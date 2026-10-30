//! The file index (architecture §8): one document per file, for clauses
//! about a single file (`file:(ext:flac size:>50MB)`) and phrases within
//! one file name, which the torrent index's position-less paths can't
//! answer.
//!
//! It's a second Tantivy index beside the torrent index, following the
//! store with its own watermark. A `file:(…)` clause runs here first and
//! gives the set of torrent ids that have a matching file, which then
//! filters the torrent query.
//!
//! "Smart" coverage keeps it small: every file of a torrent with at most
//! [`SMART_ALL_FILES`] files; in larger packs, only files of at least
//! [`SMART_MIN_SIZE`] or of a media or software category. Padding and
//! hidden files (BEP 47) are never indexed. The torrent index's paths
//! still cover every file.

use std::collections::HashMap;
use std::ops::Bound;
use std::sync::{Arc, Mutex, OnceLock};

use lodestar_classify::Category;
use lodestar_classify::ext::category_of;
use lodestar_metainfo::{File, FileAttrs, Text};
use lodestar_store::FileTree;
use tantivy::collector::{Collector, SegmentCollector};
use tantivy::columnar::Column;
use tantivy::index::SegmentId;
use tantivy::query::{
    AllQuery, BitSetDocSet, BooleanQuery, ConstScorer, EmptyQuery, EnableScoring, Explanation,
    FuzzyTermQuery, Occur, Query, RangeQuery, RegexQuery, Scorer, TermQuery, Weight,
};
use tantivy::schema::{
    FAST, Field, IndexRecordOption, NumericOptions, Schema, TextFieldIndexing, TextOptions,
};
use tantivy::store::{Compressor, ZstdCompressor};
use tantivy::{
    BitSet, DocId, DocSet, IndexSettings, Score, Searcher, SegmentOrdinal, SegmentReader,
    TERMINATED, TantivyDocument, Term,
};

use crate::compile::{ABSENT, folded_word, parts_query, regex_escape, units, word_query};
use crate::doc::extension;
use crate::query::{Node, NumField, TextField};
use crate::tokenizer::{self, NAME_TOKENIZER};

type Q = Box<dyn Query>;

/// Torrents with at most this many files have every file indexed.
pub const SMART_ALL_FILES: usize = 1000;
/// In larger packs, files at least this big are indexed whatever they
/// are.
pub const SMART_MIN_SIZE: u64 = 5 << 20;

#[derive(Debug, Clone, Copy)]
pub struct FileFields {
    pub torrent: Field,
    /// The file's position in the torrent's file list (padding included),
    /// for "matched files".
    pub index: Field,
    /// The file name, with positions.
    pub name: Field,
    /// Its directory names, words only.
    pub dir: Field,
    pub ext: Field,
    pub size: Field,
}

pub fn schema() -> (Schema, FileFields) {
    let mut b = Schema::builder();
    // File clauses select torrents and are never scored, so no field has
    // norms: they would cost a byte per file each.
    let text = |positions: bool| {
        TextOptions::default().set_indexing_options(
            TextFieldIndexing::default()
                .set_tokenizer(NAME_TOKENIZER)
                .set_fieldnorms(false)
                .set_index_option(if positions {
                    IndexRecordOption::WithFreqsAndPositions
                } else {
                    IndexRecordOption::WithFreqs
                }),
        )
    };
    let keyword = TextOptions::default().set_indexing_options(
        TextFieldIndexing::default()
            .set_tokenizer("raw")
            .set_index_option(IndexRecordOption::Basic)
            .set_fieldnorms(false),
    );
    let fields = FileFields {
        torrent: b.add_u64_field(
            "torrent",
            NumericOptions::default().set_indexed().set_fast(),
        ),
        index: b.add_u64_field("index", FAST),
        name: b.add_text_field("name", text(true)),
        dir: b.add_text_field("dir", text(false)),
        ext: b.add_text_field("ext", keyword),
        size: b.add_u64_field("size", NumericOptions::default().set_fast()),
    };
    (b.build(), fields)
}

/// The file index's settings. It stores nothing, but Tantivy's docstore
/// still keeps a 4-byte offset per document, which lz4 barely shrinks;
/// zstd over large blocks takes most of it away.
pub fn settings() -> IndexSettings {
    IndexSettings {
        docstore_compression: Compressor::Zstd(ZstdCompressor {
            compression_level: Some(19),
        }),
        docstore_blocksize: 1 << 20,
        ..IndexSettings::default()
    }
}

/// One indexed file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileDoc {
    pub torrent: u64,
    pub index: u64,
    pub name: String,
    pub dirs: Vec<String>,
    pub ext: Option<String>,
    pub size: u64,
}

impl FileDoc {
    pub fn to_document(&self, f: &FileFields) -> TantivyDocument {
        let mut d = TantivyDocument::new();
        d.add_u64(f.torrent, self.torrent);
        d.add_u64(f.index, self.index);
        d.add_text(f.name, &self.name);
        for dir in &self.dirs {
            d.add_text(f.dir, dir);
        }
        if let Some(e) = &self.ext {
            d.add_text(f.ext, e);
        }
        d.add_u64(f.size, self.size);
        d
    }
}

/// The files of a torrent that smart coverage indexes.
pub fn file_docs(torrent: u64, tree: &FileTree) -> Vec<FileDoc> {
    let skipped =
        |f: &File| f.attrs.contains(FileAttrs::PADDING) || f.attrs.contains(FileAttrs::HIDDEN);
    let real = tree.files.iter().filter(|f| !skipped(f)).count();
    let pack = real > SMART_ALL_FILES;
    let mut out = Vec::new();
    for (i, f) in tree.files.iter().enumerate() {
        if skipped(f) {
            continue;
        }
        let mut segments: Vec<String> = f
            .path
            .iter()
            .map(|seg| {
                Text {
                    raw: seg.clone(),
                    encoding: tree.encoding,
                }
                .decode()
                .into_owned()
            })
            .collect();
        let Some(name) = segments.pop() else {
            continue;
        };
        let ext = extension(&name);
        if pack && !worth_it_in_a_pack(ext.as_deref(), f.size) {
            continue;
        }
        out.push(FileDoc {
            torrent,
            index: i as u64,
            name,
            dirs: segments,
            ext,
            size: f.size,
        });
    }
    out
}

/// In a large pack, big files and media or software files are indexed;
/// small documents, images (covers, thumbnails) and everything else are
/// left to the torrent's paths.
fn worth_it_in_a_pack(ext: Option<&str>, size: u64) -> bool {
    if size >= SMART_MIN_SIZE {
        return true;
    }
    let Some(ext) = ext else {
        return false;
    };
    !matches!(
        category_of(ext),
        Category::Other | Category::Document | Category::Image
    )
}

/// Whether a clause can be asked of a single file: words, phrases,
/// prefixes and fuzzy words on the name or path, `ext:`, `size:`, and
/// their combinations.
pub fn file_clause_ok(node: &Node) -> bool {
    match node {
        Node::All => true,
        Node::And { of } | Node::Or { of } => of.iter().all(file_clause_ok),
        Node::Not { of } => file_clause_ok(of),
        Node::Word { field, .. }
        | Node::Phrase { field, .. }
        | Node::Near { field, .. }
        | Node::Prefix { field, .. }
        | Node::Fuzzy { field, .. } => *field != TextField::Title,
        Node::Ext { .. } => true,
        Node::Range { field, .. } => *field == NumField::Size,
        _ => false,
    }
}

/// The fields a text clause covers in the file index, and whether each
/// has positions. A plain word matches the name or a directory.
fn targets(field: TextField, f: &FileFields, phrase: bool) -> Vec<(Field, bool)> {
    match field {
        TextField::Path => vec![(f.dir, false)],
        TextField::Any if !phrase => vec![(f.name, true), (f.dir, false)],
        _ => vec![(f.name, true)],
    }
}

fn any_of(targets: Vec<(Field, bool)>, build: impl Fn(Field, bool) -> Q) -> Q {
    let mut clauses: Vec<(Occur, Q)> = targets
        .into_iter()
        .map(|(fld, positions)| {
            let q = if fld == ABSENT {
                Box::new(EmptyQuery)
            } else {
                build(fld, positions)
            };
            (Occur::Should, q)
        })
        .collect();
    if clauses.len() == 1 {
        return clauses.pop().expect("one").1;
    }
    Box::new(BooleanQuery::new(clauses))
}

/// The inner clause of `file:(…)` as a file-index query. `None` for a
/// clause that constrains nothing.
pub fn compile(node: &Node, f: &FileFields) -> Option<Q> {
    Some(match node {
        Node::All => Box::new(AllQuery),
        Node::And { of } => {
            let mut clauses: Vec<(Occur, Q)> = Vec::new();
            for n in of {
                match n {
                    Node::Not { of } => {
                        if let Some(q) = compile(of, f) {
                            clauses.push((Occur::MustNot, q));
                        }
                    }
                    n => {
                        if let Some(q) = compile(n, f) {
                            clauses.push((Occur::Must, q));
                        }
                    }
                }
            }
            if clauses.iter().all(|(o, _)| *o == Occur::MustNot) {
                clauses.push((Occur::Must, Box::new(AllQuery)));
            }
            Box::new(BooleanQuery::new(clauses))
        }
        Node::Or { of } => {
            let clauses: Vec<(Occur, Q)> = of
                .iter()
                .filter_map(|n| compile(n, f))
                .map(|q| (Occur::Should, q))
                .collect();
            if clauses.is_empty() {
                return None;
            }
            Box::new(BooleanQuery::new(clauses))
        }
        Node::Not { of } => {
            let q = compile(of, f)?;
            Box::new(BooleanQuery::new(vec![
                (Occur::Must, Box::new(AllQuery) as Q),
                (Occur::MustNot, q),
            ]))
        }
        Node::Word { field, word } => {
            let toks = tokenizer::tokenize(word);
            if toks.is_empty() {
                return None;
            }
            any_of(targets(*field, f, false), |fld, positions| {
                word_query(fld, &toks, positions)
            })
        }
        Node::Phrase { field, text } => {
            let parts = units(&tokenizer::tokenize(text));
            if parts.is_empty() {
                return None;
            }
            any_of(targets(*field, f, true), |fld, positions| {
                parts_query(fld, &parts, positions, 0)
            })
        }
        Node::Near { field, words, slop } => {
            let mut parts = Vec::new();
            let mut next = 0;
            for w in words {
                let mut toks = Vec::new();
                next = tokenizer::tokenize_into(w, next, &mut toks, 512);
                parts.extend(units(&toks));
            }
            if parts.is_empty() {
                return None;
            }
            any_of(targets(*field, f, true), |fld, positions| {
                parts_query(fld, &parts, positions, *slop)
            })
        }
        Node::Prefix { field, prefix } => {
            let pattern = format!("{}.*", regex_escape(&folded_word(prefix)?));
            any_of(
                targets(*field, f, false),
                |fld, _| match RegexQuery::from_pattern(&pattern, fld) {
                    Ok(q) => Box::new(q),
                    Err(_) => Box::new(EmptyQuery),
                },
            )
        }
        Node::Fuzzy {
            field,
            word,
            distance,
        } => {
            let folded = folded_word(word)?;
            any_of(targets(*field, f, false), |fld, _| {
                Box::new(FuzzyTermQuery::new(
                    Term::from_field_text(fld, &folded),
                    (*distance).min(2),
                    true,
                ))
            })
        }
        // A field an older index lacks (`compile::ABSENT`) matches nothing.
        Node::Ext { .. } if f.ext == ABSENT => Box::new(EmptyQuery),
        Node::Range { .. } if f.size == ABSENT => Box::new(EmptyQuery),
        Node::Ext { any } => Box::new(BooleanQuery::new(
            any.iter()
                .map(|e| {
                    (
                        Occur::Should,
                        Box::new(TermQuery::new(
                            Term::from_field_text(f.ext, e),
                            IndexRecordOption::Basic,
                        )) as Q,
                    )
                })
                .collect(),
        )),
        Node::Range {
            field: NumField::Size,
            lo,
            hi,
        } => {
            let bound = |v: &Option<u64>| match v {
                Some(v) => Bound::Included(Term::from_field_u64(f.size, *v)),
                None => Bound::Unbounded,
            };
            Box::new(RangeQuery::new(bound(lo), bound(hi)))
        }
        // The parser only lets the clauses above in; anything else (from
        // AST JSON) matches nothing.
        _ => Box::new(EmptyQuery),
    })
}

/// The torrents with a file matching one clause: a sorted id list per
/// file-index segment. Membership checks each list, so they're never
/// merged unless a query needs the union.
#[derive(Debug, Default, Clone)]
pub(crate) struct IdSet {
    parts: Vec<Arc<Vec<u64>>>,
    union: OnceLock<Option<BitSet>>,
}

impl PartialEq for IdSet {
    fn eq(&self, other: &IdSet) -> bool {
        self.parts == other.parts
    }
}

impl IdSet {
    pub fn contains(&self, id: u64) -> bool {
        self.parts.iter().any(|p| p.binary_search(&id).is_ok())
    }

    pub fn is_empty(&self) -> bool {
        self.parts.iter().all(|p| p.is_empty())
    }

    /// Every id as a bitset, for checking each document of a segment in
    /// constant time; `None` when empty or past the bitset's range.
    fn union(&self) -> Option<&BitSet> {
        self.union
            .get_or_init(|| {
                let max = self.parts.iter().filter_map(|p| p.last()).max()?;
                let max = u32::try_from(*max).ok().filter(|m| *m < u32::MAX)?;
                let mut set = BitSet::with_max_value(max + 1);
                for id in self.parts.iter().flat_map(|p| p.iter()) {
                    set.insert(*id as u32);
                }
                Some(set)
            })
            .as_ref()
    }
}

/// Independent retained file-id cache budget, not the result/lease budget.
const CACHE_BYTES: usize = 8 << 20;
const CACHE_ENTRIES: usize = 4096;
type CacheKey = (String, SegmentId, u32);

struct CacheEntry {
    ids: Arc<Vec<u64>>,
    bytes: usize,
    used: u64,
}

#[derive(Default)]
struct CacheState {
    map: HashMap<CacheKey, CacheEntry>,
    bytes: usize,
    clock: u64,
}

impl CacheState {
    fn retained_bytes(&self) -> usize {
        // Charge bucket capacity conservatively (including control bytes and
        // alignment), plus String/Vec capacities and the Arc allocation.
        self.bytes + self.map.capacity() * (std::mem::size_of::<(CacheKey, CacheEntry)>() + 16) * 2
    }
}

pub(crate) struct FileSetCache {
    state: Mutex<CacheState>,
    budget: usize,
}

impl Default for FileSetCache {
    fn default() -> Self {
        Self {
            state: Mutex::new(CacheState::default()),
            budget: CACHE_BYTES,
        }
    }
}

impl FileSetCache {
    pub fn clear(&self) {
        *self.state.lock().expect("cache lock") = CacheState::default();
    }

    /// Entries held and their retained bytes, as the budget charges them.
    pub fn usage(&self) -> (u64, u64) {
        let state = self.state.lock().expect("cache lock");
        (state.map.len() as u64, state.retained_bytes() as u64)
    }

    fn get(&self, key: &CacheKey) -> Option<Arc<Vec<u64>>> {
        let mut state = self.state.lock().expect("cache lock");
        state.clock = state.clock.saturating_add(1);
        let used = state.clock;
        let entry = state.map.get_mut(key)?;
        entry.used = used;
        Some(entry.ids.clone())
    }

    fn insert(&self, key: CacheKey, ids: Arc<Vec<u64>>) {
        let bytes = key
            .0
            .capacity()
            .saturating_add(ids.capacity().saturating_mul(std::mem::size_of::<u64>()))
            .saturating_add(std::mem::size_of::<Vec<u64>>() + 2 * std::mem::size_of::<usize>());
        if bytes > self.budget {
            return;
        }
        let mut state = self.state.lock().expect("cache lock");
        state.clock = state.clock.saturating_add(1);
        let used = state.clock;
        if let Some(old) = state.map.remove(&key) {
            state.bytes -= old.bytes;
        }
        state.bytes += bytes;
        state.map.insert(key, CacheEntry { ids, bytes, used });
        while state.map.len() > CACHE_ENTRIES || state.retained_bytes() > self.budget {
            let Some(key) = state
                .map
                .iter()
                .min_by_key(|(_, v)| v.used)
                .map(|(k, _)| k.clone())
            else {
                *state = CacheState::default();
                break;
            };
            let old = state.map.remove(&key).expect("present cache entry");
            state.bytes -= old.bytes;
            if state.map.is_empty() {
                *state = CacheState::default();
            }
        }
    }
}

/// The torrents with a file matching `query`, the clause known as `key`.
pub(crate) fn torrent_ids(
    searcher: &Searcher,
    query: &dyn Query,
    key: &str,
    cache: &FileSetCache,
    cancel: &crate::CancelToken,
) -> crate::index::Result<IdSet> {
    cancel.check()?;
    let mut weight: Option<Box<dyn Weight>> = None;
    let mut parts = Vec::new();
    for seg in searcher.segment_readers() {
        cancel.check()?;
        let k = (key.to_owned(), seg.segment_id(), seg.num_deleted_docs());
        if let Some(hit) = cache.get(&k) {
            parts.push(hit);
            continue;
        }
        let w = match &mut weight {
            Some(w) => w,
            None => weight.insert(query.weight(EnableScoring::disabled_from_searcher(searcher))?),
        };
        let column = seg.fast_fields().u64("torrent")?;
        let alive = seg.alive_bitset();
        let mut ids = Vec::new();
        let mut scorer = w.scorer(seg, 1.0)?;
        let mut doc = scorer.doc();
        let mut visited = 0u32;
        while doc != TERMINATED {
            if visited.is_multiple_of(crate::cancel::CHECK_EVERY) {
                cancel.check()?;
            }
            visited = visited.wrapping_add(1);
            if alive.is_none_or(|a| a.is_alive(doc)) {
                // A torrent's files are added together, so repeats are
                // mostly adjacent.
                if let Some(id) = column.first(doc).filter(|id| ids.last() != Some(id)) {
                    ids.push(id);
                }
            }
            doc = scorer.advance();
        }
        cancel.check()?;
        ids.sort_unstable();
        cancel.check()?;
        ids.dedup();
        cancel.check()?;
        let ids = Arc::new(ids);
        cache.insert(k, ids.clone());
        parts.push(ids);
    }
    Ok(IdSet {
        parts,
        union: OnceLock::new(),
    })
}

/// The torrent-index documents whose id is in a set: a `file:(…)` clause
/// that has to select by itself (alone, or under `|` or `-`). It reads
/// each segment's id column into a bitset, one pass per segment.
#[derive(Debug, Clone)]
pub(crate) struct IdsQuery {
    pub ids: Arc<IdSet>,
}

impl Query for IdsQuery {
    fn weight(&self, _: EnableScoring<'_>) -> tantivy::Result<Box<dyn Weight>> {
        Ok(Box::new(IdsWeight(
            self.ids.clone(),
            crate::filter::SegmentSets::default(),
        )))
    }
}

/// The set, and its matches per segment (computed once per search; see
/// [`crate::filter::SegmentSets`]).
struct IdsWeight(Arc<IdSet>, crate::filter::SegmentSets);

impl IdsWeight {
    fn matches(&self, reader: &SegmentReader) -> tantivy::Result<BitSet> {
        let column = reader.fast_fields().u64("id")?;
        let mut set = BitSet::with_max_value(reader.max_doc());
        match self.0.union() {
            Some(ids) => {
                let max = ids.max_value();
                for doc in 0..reader.max_doc() {
                    if column
                        .first(doc)
                        .is_some_and(|id| id < u64::from(max) && ids.contains(id as u32))
                    {
                        set.insert(doc);
                    }
                }
            }
            // Ids past 4 billion: slower, but still right.
            None if !self.0.is_empty() => {
                for doc in 0..reader.max_doc() {
                    if column.first(doc).is_some_and(|id| self.0.contains(id)) {
                        set.insert(doc);
                    }
                }
            }
            None => {}
        }
        Ok(set)
    }
}

impl Weight for IdsWeight {
    fn scorer(&self, reader: &SegmentReader, boost: Score) -> tantivy::Result<Box<dyn Scorer>> {
        let set = self.1.get(reader, |r| self.matches(r))?;
        Ok(Box::new(ConstScorer::new(
            BitSetDocSet::from(BitSet::clone(&set)),
            boost,
        )))
    }

    fn explain(&self, reader: &SegmentReader, doc: DocId) -> tantivy::Result<Explanation> {
        if doc < reader.max_doc() && self.1.get(reader, |r| self.matches(r))?.contains(doc) {
            Ok(Explanation::new("file clause", 1.0))
        } else {
            Err(tantivy::TantivyError::InvalidArgument(format!(
                "document {doc} doesn't match"
            )))
        }
    }
}

/// Collects the file positions of the matching documents.
pub(crate) struct FileIndexes;

pub(crate) struct FileIndexesSegment {
    column: Column<u64>,
    found: Vec<u64>,
}

impl Collector for FileIndexes {
    type Fruit = Vec<u64>;
    type Child = FileIndexesSegment;

    fn for_segment(
        &self,
        _ord: SegmentOrdinal,
        reader: &SegmentReader,
    ) -> tantivy::Result<FileIndexesSegment> {
        Ok(FileIndexesSegment {
            column: reader.fast_fields().u64("index")?,
            found: Vec::new(),
        })
    }

    fn requires_scoring(&self) -> bool {
        false
    }

    fn merge_fruits(&self, fruits: Vec<Vec<u64>>) -> tantivy::Result<Vec<u64>> {
        let mut all: Vec<u64> = fruits.into_iter().flatten().collect();
        all.sort_unstable();
        all.dedup();
        Ok(all)
    }
}

impl SegmentCollector for FileIndexesSegment {
    type Fruit = Vec<u64>;

    fn collect(&mut self, doc: DocId, _score: Score) {
        self.found.extend(self.column.first(doc));
    }

    fn harvest(self) -> Vec<u64> {
        self.found
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lodestar_metainfo::TextEncoding;

    fn tree(files: &[(&str, u64, FileAttrs)]) -> FileTree {
        FileTree {
            files: files
                .iter()
                .map(|(p, size, attrs)| File {
                    path: p.split('/').map(|s| s.as_bytes().to_vec()).collect(),
                    size: *size,
                    attrs: *attrs,
                    symlink_target: None,
                    pieces_root: None,
                    hashes: Default::default(),
                })
                .collect(),
            encoding: TextEncoding::UTF8,
        }
    }

    #[test]
    fn small_torrents_index_every_real_file() {
        let t = tree(&[
            ("Album/01 Song.flac", 30 << 20, FileAttrs::empty()),
            ("Album/cover.jpg", 50_000, FileAttrs::empty()),
            (".pad/1234", 1000, FileAttrs::PADDING),
            ("Album/.hidden", 10, FileAttrs::HIDDEN),
        ]);
        let docs = file_docs(7, &t);
        let names: Vec<_> = docs.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(names, ["01 Song.flac", "cover.jpg"]);
        assert_eq!(docs[0].dirs, ["Album"]);
        assert_eq!(docs[0].ext.as_deref(), Some("flac"));
        assert_eq!((docs[1].torrent, docs[1].index), (7, 1));
    }

    #[test]
    fn large_packs_keep_big_and_media_files() {
        let mut files = Vec::new();
        for i in 0..SMART_ALL_FILES {
            files.push((format!("pics/{i}.jpg"), 100_000));
        }
        files.push(("pics/big.jpg".into(), SMART_MIN_SIZE));
        files.push(("music/a.mp3".into(), 4 << 20));
        files.push(("info.nfo".into(), 2_000));
        let refs: Vec<(&str, u64, FileAttrs)> = files
            .iter()
            .map(|(p, s)| (p.as_str(), *s, FileAttrs::empty()))
            .collect();
        let names: Vec<_> = file_docs(1, &tree(&refs))
            .into_iter()
            .map(|d| d.name)
            .collect();
        assert_eq!(names, ["big.jpg", "a.mp3"]);
    }

    #[test]
    fn file_clauses() {
        let ok = |q: &str| file_clause_ok(&crate::parse(q).unwrap());
        assert!(ok(
            "\"dark knight\" ext:mkv size:>1GB -sample (a | path:b) co* ~knigth"
        ));
        assert!(!ok("res:1080p"));
        assert!(!ok("title:x"));
        assert!(!ok("files:>2"));
    }
}

#[cfg(test)]
mod cache_regressions {
    use super::*;

    fn key(n: usize) -> CacheKey {
        (format!("clause-{n}"), SegmentId::generate_random(), 0)
    }

    #[test]
    fn retained_capacity_is_bounded_and_oversized_results_still_belong_to_caller() {
        let cache = FileSetCache {
            state: Mutex::new(CacheState::default()),
            budget: 4096,
        };
        for n in 0..100 {
            let mut ids = Vec::with_capacity(128);
            ids.push(n as u64);
            cache.insert(key(n), Arc::new(ids));
            assert!(cache.state.lock().unwrap().retained_bytes() <= cache.budget);
        }
        let large = Arc::new(vec![1; 1024]);
        let k = key(101);
        cache.insert(k.clone(), large.clone());
        assert!(cache.get(&k).is_none());
        assert_eq!(large.len(), 1024);
        cache.clear();
        assert_eq!(cache.state.lock().unwrap().retained_bytes(), 0);
    }

    #[test]
    fn usage_reports_entries_and_the_charged_bytes() {
        let cache = FileSetCache::default();
        assert_eq!(cache.usage(), (0, 0));
        cache.insert(key(1), Arc::new(vec![1; 100]));
        let (entries, bytes) = cache.usage();
        assert_eq!(entries, 1);
        assert_eq!(bytes, cache.state.lock().unwrap().retained_bytes() as u64);
        assert!(bytes >= 800, "{bytes}");
    }

    #[test]
    fn hits_refresh_recency_and_replacement_does_not_double_charge() {
        let cache = FileSetCache {
            state: Mutex::new(CacheState::default()),
            // Two 1 KiB vectors and table overhead fit; three vectors do not.
            budget: 3072,
        };
        let a = key(1);
        let b = key(2);
        let c = key(3);
        let value = || Arc::new(vec![0; 128]);
        cache.insert(a.clone(), value());
        cache.insert(b.clone(), value());
        let bytes = cache.state.lock().unwrap().retained_bytes();
        cache.insert(b.clone(), value());
        assert_eq!(cache.state.lock().unwrap().retained_bytes(), bytes);
        assert!(cache.get(&a).is_some());
        cache.insert(c.clone(), value());
        assert!(cache.get(&a).is_some());
        assert!(cache.get(&b).is_none());
        assert!(cache.get(&c).is_some());
    }

    #[test]
    fn cancellation_during_enumeration_stops_before_scanning_every_file() {
        use std::sync::atomic::Ordering;
        let mut builder = Schema::builder();
        let torrent = builder.add_u64_field("torrent", FAST);
        let index = tantivy::Index::create_in_ram(builder.build());
        let mut writer = index
            .writer_with_num_threads::<TantivyDocument>(1, 15 << 20)
            .unwrap();
        for id in 0..10_000u64 {
            let mut doc = TantivyDocument::new();
            doc.add_u64(torrent, id);
            writer.add_document(doc).unwrap();
        }
        writer.commit().unwrap();
        let reader = index.reader().unwrap();
        let token = crate::CancelToken::new();
        let (query, visits) = crate::testing::tripwire(100, &token);
        let cache = FileSetCache::default();
        let result = torrent_ids(
            &reader.searcher(),
            query.query.as_ref(),
            "cold",
            &cache,
            &token,
        );
        assert!(matches!(result, Err(crate::index::Error::Cancelled)));
        assert!(visits.load(Ordering::Relaxed) <= crate::cancel::CHECK_EVERY as usize);
        assert_eq!(cache.state.lock().unwrap().retained_bytes(), 0);
    }

    #[test]
    fn cancelled_file_scan_does_not_build_or_cache_a_scorer() {
        let (index, _) = crate::testing::index_with(10, 1);
        let token = crate::CancelToken::new();
        token.cancel();
        let cache = FileSetCache::default();
        let result = torrent_ids(&index.searcher(), &AllQuery, "cold", &cache, &token);
        assert!(matches!(result, Err(crate::index::Error::Cancelled)));
        assert_eq!(cache.state.lock().unwrap().retained_bytes(), 0);
    }
}
