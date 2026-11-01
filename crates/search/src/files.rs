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

use std::collections::{BTreeMap, HashMap};
use std::ops::Bound;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
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
    TantivyDocument, Term,
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

/// The torrents with a file matching one clause: a sorted id set per
/// file-index segment. Membership checks each one, so they're never
/// merged unless a query needs the union.
#[derive(Debug, Default, Clone)]
pub(crate) struct IdSet {
    parts: Vec<Arc<IdPart>>,
    union: OnceLock<Option<BitSet>>,
}

impl PartialEq for IdSet {
    fn eq(&self, other: &IdSet) -> bool {
        self.parts == other.parts
    }
}

impl IdSet {
    pub fn contains(&self, id: u64) -> bool {
        self.parts.iter().any(|p| p.contains(id))
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
                let max = u32::try_from(max).ok().filter(|m| *m < u32::MAX)?;
                let mut set = BitSet::with_max_value(max + 1);
                for p in &self.parts {
                    p.for_each(|id| {
                        set.insert(id as u32);
                    });
                }
                Some(set)
            })
            .as_ref()
    }
}

/// One file-index segment's torrent ids, in whichever form is smaller: a
/// sorted list (64 bits an id), or a bitset over the span from the least
/// id to the greatest (one bit an id of the span). Torrent ids are given
/// out in order, so a broad clause's are dense and its bitset a small
/// fraction of the list: at 200,000 torrents, `file:(ext:mkv)` takes 25 KB
/// rather than 1.5 MB (X-search-files-perf).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum IdPart {
    List(Vec<u64>),
    /// Bit `i` of `words` is id `base + i`; the last word's top bit set
    /// is the greatest id.
    Bits {
        base: u64,
        words: Vec<u64>,
    },
}

impl IdPart {
    /// From sorted, deduplicated ids.
    fn from_sorted(mut ids: Vec<u64>) -> IdPart {
        let (Some(&first), Some(&last)) = (ids.first(), ids.last()) else {
            return IdPart::List(Vec::new());
        };
        let words = (last - first) / 64 + 1;
        if words >= ids.len() as u64 {
            ids.shrink_to_fit();
            return IdPart::List(ids);
        }
        let mut bits = vec![0u64; words as usize];
        for id in ids {
            let i = id - first;
            bits[(i / 64) as usize] |= 1 << (i % 64);
        }
        IdPart::Bits {
            base: first,
            words: bits,
        }
    }

    /// From a bitset whose bit `i` is id `lo + i`: the same part
    /// [`IdPart::from_sorted`] makes from those ids, without sorting them.
    fn from_bits(lo: u64, mut words: Vec<u64>) -> IdPart {
        let (Some(fw), Some(lw)) = (
            words.iter().position(|w| *w != 0),
            words.iter().rposition(|w| *w != 0),
        ) else {
            return IdPart::List(Vec::new());
        };
        let count: u64 = words.iter().map(|w| u64::from(w.count_ones())).sum();
        let shift = fw as u64 * 64 + u64::from(words[fw].trailing_zeros());
        let first = lo + shift;
        let last = lo + lw as u64 * 64 + u64::from(63 - words[lw].leading_zeros());
        let need = ((last - first) / 64 + 1) as usize;
        if need as u64 >= count {
            let mut ids = Vec::with_capacity(count as usize);
            for_each_bit(lo, &words, |id| ids.push(id));
            return IdPart::List(ids);
        }
        // Move bit `shift` to bit 0, in place: each word reads only words
        // at or after its own index.
        let (skip, s) = ((shift / 64) as usize, (shift % 64) as u32);
        for i in 0..need {
            let hi = match s {
                0 => 0,
                s => words.get(skip + i + 1).map_or(0, |w| w << (64 - s)),
            };
            words[i] = (words[skip + i] >> s) | hi;
        }
        words.truncate(need);
        words.shrink_to_fit();
        IdPart::Bits { base: first, words }
    }

    fn contains(&self, id: u64) -> bool {
        match self {
            IdPart::List(ids) => ids.binary_search(&id).is_ok(),
            IdPart::Bits { base, words } => id.checked_sub(*base).is_some_and(|i| {
                words
                    .get((i / 64) as usize)
                    .is_some_and(|w| w & (1 << (i % 64)) != 0)
            }),
        }
    }

    fn is_empty(&self) -> bool {
        match self {
            IdPart::List(ids) => ids.is_empty(),
            // A bitset is only made from at least one id.
            IdPart::Bits { .. } => false,
        }
    }

    fn last(&self) -> Option<u64> {
        match self {
            IdPart::List(ids) => ids.last().copied(),
            IdPart::Bits { base, words } => {
                let (i, w) = words.iter().enumerate().rev().find(|(_, w)| **w != 0)?;
                Some(base + i as u64 * 64 + u64::from(63 - w.leading_zeros()))
            }
        }
    }

    /// Calls `f` with each id, in order.
    fn for_each(&self, mut f: impl FnMut(u64)) {
        match self {
            IdPart::List(ids) => ids.iter().for_each(|id| f(*id)),
            IdPart::Bits { base, words } => for_each_bit(*base, words, f),
        }
    }

    /// The heap bytes it holds, as the cache charges them.
    fn heap_bytes(&self) -> usize {
        let words = match self {
            IdPart::List(ids) => ids.capacity(),
            IdPart::Bits { words, .. } => words.capacity(),
        };
        words.saturating_mul(std::mem::size_of::<u64>())
    }
}

/// Independent retained file-id cache budget, not the result/lease budget.
const CACHE_BYTES: usize = 8 << 20;
const CACHE_ENTRIES: usize = 4096;
type CacheKey = (String, SegmentId, u32);

struct CacheEntry {
    ids: Arc<IdPart>,
    bytes: usize,
    used: u64,
}

/// The sets, and their recency: `order` maps each entry's last use to its
/// key, so the least recently used is the first, found and evicted in
/// O(log n) rather than by scanning the map.
#[derive(Default)]
struct CacheState {
    map: HashMap<CacheKey, CacheEntry>,
    order: BTreeMap<u64, CacheKey>,
    bytes: usize,
    clock: u64,
}

impl CacheState {
    fn retained_bytes(&self) -> usize {
        // Charge bucket capacity conservatively (including control bytes and
        // alignment), plus String/Vec capacities and the Arc allocation.
        // The recency index is charged three slots an entry: measured
        // under this cache's pattern (append at the end, hits moved to the
        // end, pop the first), its nodes take up to 2.5 slots an entry,
        // since B-tree leaves run half empty (X-search-files-2).
        self.bytes
            + self.map.capacity() * (std::mem::size_of::<(CacheKey, CacheEntry)>() + 16) * 2
            + self.order.len() * std::mem::size_of::<(u64, CacheKey)>() * 3
    }

    fn tick(&mut self) -> u64 {
        self.clock += 1;
        self.clock
    }

    fn remove(&mut self, key: &CacheKey) {
        if let Some(old) = self.map.remove(key) {
            self.order.remove(&old.used);
            self.bytes -= old.bytes;
        }
    }

    /// Drops the least recently used entry; `false` when empty.
    fn evict(&mut self) -> bool {
        let Some((_, key)) = self.order.pop_first() else {
            return false;
        };
        let old = self.map.remove(&key).expect("indexed cache entry");
        self.bytes -= old.bytes;
        true
    }
}

pub(crate) struct FileSetCache {
    state: Mutex<CacheState>,
    budget: usize,
    /// Lookups that found a set, and lookups that didn't, since start.
    hits: AtomicU64,
    misses: AtomicU64,
}

impl Default for FileSetCache {
    fn default() -> Self {
        Self::with_budget(CACHE_BYTES)
    }
}

impl FileSetCache {
    fn with_budget(budget: usize) -> Self {
        Self {
            state: Mutex::new(CacheState::default()),
            budget,
            hits: AtomicU64::new(0),
            misses: AtomicU64::new(0),
        }
    }

    /// Lookups that found a set, and lookups that didn't, since start.
    pub fn hits(&self) -> (u64, u64) {
        (
            self.hits.load(Ordering::Relaxed),
            self.misses.load(Ordering::Relaxed),
        )
    }

    pub fn clear(&self) {
        *self.state.lock().expect("cache lock") = CacheState::default();
    }

    /// Entries held and their retained bytes, as the budget charges them.
    pub fn usage(&self) -> (u64, u64) {
        let state = self.state.lock().expect("cache lock");
        (state.map.len() as u64, state.retained_bytes() as u64)
    }

    fn get(&self, key: &CacheKey) -> Option<Arc<IdPart>> {
        let mut state = self.state.lock().expect("cache lock");
        let used = state.tick();
        let Some(entry) = state.map.get_mut(key) else {
            self.misses.fetch_add(1, Ordering::Relaxed);
            return None;
        };
        let was = std::mem::replace(&mut entry.used, used);
        let ids = entry.ids.clone();
        let key = state.order.remove(&was).expect("indexed cache entry");
        state.order.insert(used, key);
        self.hits.fetch_add(1, Ordering::Relaxed);
        Some(ids)
    }

    fn insert(&self, key: CacheKey, ids: Arc<IdPart>) {
        // The key is held twice: in the map and in the recency index.
        let bytes = key
            .0
            .capacity()
            .saturating_mul(2)
            .saturating_add(ids.heap_bytes())
            .saturating_add(std::mem::size_of::<IdPart>() + 2 * std::mem::size_of::<usize>());
        if bytes > self.budget {
            return;
        }
        let mut state = self.state.lock().expect("cache lock");
        state.remove(&key);
        let used = state.tick();
        state.bytes += bytes;
        state.order.insert(used, key.clone());
        state.map.insert(key, CacheEntry { ids, bytes, used });
        while state.map.len() > CACHE_ENTRIES || state.retained_bytes() > self.budget {
            if !state.evict() || state.map.is_empty() {
                // Nothing left but the table's own capacity: start over.
                *state = CacheState::default();
                break;
            }
        }
    }
}

/// A segment with more file documents than this is scanned in pieces,
/// in parallel: merging leaves one segment with most of the files (at 1M
/// torrents, 17.7M of 29.5M), and scanned whole it held a cold clause
/// to one thread while the others sat idle (X-file-clause-cold).
const PIECE_DOCS: DocId = 1 << 21;

/// At most this many pieces a segment, as many as the file index's
/// search threads: each piece's gather is transient (a bitset over the
/// segment's id span when dense), so this bounds them to four a segment.
const MAX_PIECES: DocId = 4;

/// The doc ranges a segment of `max_doc` documents is scanned in.
fn pieces(max_doc: DocId, piece_docs: DocId) -> impl Iterator<Item = (DocId, DocId)> {
    let n = max_doc.div_ceil(piece_docs.max(1)).clamp(1, MAX_PIECES);
    let len = max_doc.div_ceil(n).max(1);
    (0..n).map(move |i| {
        let lo = i.saturating_mul(len).min(max_doc);
        (lo, lo.saturating_add(len).min(max_doc))
    })
}

/// The torrents with a file matching `query`, the clause known as `key`.
/// Segments the cache misses are scanned in parallel on the file index's
/// executor, as the torrent index's searches are, a large one in pieces.
pub(crate) fn torrent_ids(
    searcher: &Searcher,
    query: &dyn Query,
    key: &str,
    cache: &FileSetCache,
    cancel: &crate::CancelToken,
) -> crate::index::Result<IdSet> {
    torrent_ids_in_pieces(searcher, query, key, cache, cancel, PIECE_DOCS)
}

fn torrent_ids_in_pieces(
    searcher: &Searcher,
    query: &dyn Query,
    key: &str,
    cache: &FileSetCache,
    cancel: &crate::CancelToken,
    piece_docs: DocId,
) -> crate::index::Result<IdSet> {
    cancel.check()?;
    let segs = searcher.segment_readers();
    let keys: Vec<CacheKey> = segs
        .iter()
        .map(|seg| (key.to_owned(), seg.segment_id(), seg.num_deleted_docs()))
        .collect();
    let mut parts: Vec<Option<Arc<IdPart>>> = keys.iter().map(|k| cache.get(k)).collect();
    let missed: Vec<usize> = (0..segs.len()).filter(|i| parts[*i].is_none()).collect();
    if !missed.is_empty() {
        cancel.check()?;
        let weight = query.weight(EnableScoring::disabled_from_searcher(searcher))?;
        // One segment's failure stops its siblings: they see `failed` at
        // their next check and give up, and the first real error is the
        // one reported (the executor would report the first by position,
        // which may be a sibling's stop).
        let failed = AtomicBool::new(false);
        let first_err = Mutex::new(None);
        let tasks: Vec<(usize, DocId, DocId)> = missed
            .iter()
            .flat_map(|&i| pieces(segs[i].max_doc(), piece_docs).map(move |(lo, hi)| (i, lo, hi)))
            .collect();
        let scanned = searcher
            .index()
            .search_executor()
            .map(
                |(i, lo, hi): (usize, DocId, DocId)| {
                    scan_segment(weight.as_ref(), &segs[i], (lo, hi), cancel, &failed).map_err(
                        |e| {
                            // A stop by the token or by a sibling is not this
                            // segment's own error.
                            if !cancel.is_stopped() && !failed.load(Ordering::Acquire) {
                                let mut slot = first_err.lock().unwrap_or_else(|p| p.into_inner());
                                slot.get_or_insert(e);
                                failed.store(true, Ordering::Release);
                                crate::cancel::interrupted()
                            } else {
                                e
                            }
                        },
                    )
                },
                tasks.iter().copied(),
            )
            .map_err(|e| {
                let first = first_err.lock().unwrap_or_else(|p| p.into_inner()).take();
                crate::cancel::stopped_or(cancel, first.unwrap_or(e))
            })?;
        cancel.check()?;
        // Cached only once every segment is whole: a scan cut short
        // caches nothing.
        // A segment's pieces are adjacent in `tasks`: join them.
        let mut joined: Vec<(usize, Gather)> = Vec::with_capacity(missed.len());
        for ((i, _, _), gather) in tasks.into_iter().zip(scanned) {
            match joined.last_mut() {
                Some((at, g)) if *at == i => {
                    let first = std::mem::replace(g, Gather::List(Vec::new()));
                    *g = first.join(gather);
                }
                _ => joined.push((i, gather)),
            }
        }
        for (i, gather) in joined {
            let ids = Arc::new(gather.finish());
            cache.insert(keys[i].clone(), ids.clone());
            parts[i] = Some(ids);
        }
    }
    Ok(IdSet {
        parts: parts.into_iter().flatten().collect(),
        union: OnceLock::new(),
    })
}

/// The ids one segment's scan gathers: a list to sort, or, when the
/// scorer expects at least one matching file per 64 ids of the segment's
/// range, a bitset over that range, which needs no sort or dedup. The
/// bitset is transient (at most 8 bytes per expected file); the part kept
/// is the same either way.
enum Gather {
    List(Vec<u64>),
    Bits { lo: u64, words: Vec<u64> },
}

impl Gather {
    fn push(&mut self, id: u64) {
        match self {
            // A torrent's files are added together, so repeats are mostly
            // adjacent.
            Gather::List(ids) => {
                if ids.last() != Some(&id) {
                    ids.push(id);
                }
            }
            Gather::Bits { lo, words } => {
                match id.checked_sub(*lo).map(|i| (i / 64, i % 64)) {
                    Some((w, b)) if w < words.len() as u64 => words[w as usize] |= 1 << b,
                    // Outside the column's stated range (never expected):
                    // fall back to the list.
                    _ => {
                        let mut ids = Vec::new();
                        for_each_bit(*lo, words, |id| ids.push(id));
                        ids.push(id);
                        *self = Gather::List(ids);
                    }
                }
            }
        }
    }

    /// Both pieces' ids: their bitsets or'd when they cover the same
    /// span (a segment's pieces always do), else one list.
    fn join(self, other: Gather) -> Gather {
        match (self, other) {
            (Gather::Bits { lo, mut words }, Gather::Bits { lo: lo2, words: w2 })
                if lo == lo2 && words.len() == w2.len() =>
            {
                for (a, b) in words.iter_mut().zip(&w2) {
                    *a |= b;
                }
                Gather::Bits { lo, words }
            }
            (a, b) => {
                let mut ids = Vec::new();
                a.for_each(|id| ids.push(id));
                b.for_each(|id| ids.push(id));
                Gather::List(ids)
            }
        }
    }

    fn for_each(&self, mut f: impl FnMut(u64)) {
        match self {
            Gather::List(ids) => ids.iter().copied().for_each(f),
            Gather::Bits { lo, words } => for_each_bit(*lo, words, &mut f),
        }
    }

    fn finish(self) -> IdPart {
        match self {
            Gather::List(mut ids) => {
                ids.sort_unstable();
                ids.dedup();
                IdPart::from_sorted(ids)
            }
            Gather::Bits { lo, words } => IdPart::from_bits(lo, words),
        }
    }
}

/// The torrent ids of one file-index segment's documents in `[lo, hi)`.
/// Documents come from the scorer 64 at a time, with their torrent ids
/// read in one call.
fn scan_segment(
    weight: &dyn Weight,
    seg: &SegmentReader,
    (lo, hi): (DocId, DocId),
    cancel: &crate::CancelToken,
    failed: &AtomicBool,
) -> tantivy::Result<Gather> {
    let stopped = || {
        if cancel.is_stopped() || failed.load(Ordering::Acquire) {
            Err(crate::cancel::interrupted())
        } else {
            Ok(())
        }
    };
    stopped()?;
    let column = seg.fast_fields().u64("torrent")?;
    let alive = seg.alive_bitset();
    let mut scorer = weight.scorer(seg, 1.0)?;
    let span = column.max_value().saturating_sub(column.min_value()) / 64 + 1;
    // An estimate: tantivy 0.27's range doc set (`size:` ranges) gives
    // `num_docs / 10` as its size hint (an upstream TODO), so the form
    // chosen for those can be the costlier one; either is correct.
    let mut gather = if seg.max_doc() > 0 && u64::from(scorer.size_hint()) >= span {
        Gather::Bits {
            lo: column.min_value(),
            words: vec![0; span as usize],
        }
    } else {
        Gather::List(Vec::new())
    };
    const BLOCK: usize = tantivy::COLLECT_BLOCK_BUFFER_LEN;
    const CHECK_BLOCKS: u32 = crate::cancel::CHECK_EVERY / 2 / BLOCK as u32;
    const _: () = assert!(CHECK_BLOCKS > 0);
    if scorer.doc() < lo {
        scorer.seek(lo);
    }
    let mut docs = [0 as DocId; BLOCK];
    let mut ids = [None; BLOCK];
    let mut blocks = 0u32;
    loop {
        if blocks.is_multiple_of(CHECK_BLOCKS) {
            stopped()?;
        }
        blocks = blocks.wrapping_add(1);
        let filled = scorer.fill_buffer(&mut docs);
        // Docs come in order; those from `hi` on are the next piece's.
        let n = docs[..filled].partition_point(|d| *d < hi);
        if n == 0 {
            break;
        }
        // A multivalued column would leave a valueless doc's slot as it
        // was; `torrent` has one value a file, but don't rely on it.
        ids[..n].fill(None);
        column.first_vals(&docs[..n], &mut ids[..n]);
        for (doc, id) in docs[..n].iter().zip(&ids[..n]) {
            if let Some(id) = *id
                && alive.is_none_or(|a| a.is_alive(*doc))
            {
                gather.push(id);
            }
        }
        if n < filled {
            break;
        }
    }
    stopped()?;
    Ok(gather)
}

/// Calls `f` with the id of each bit set, in order; bit `i` is `base + i`.
fn for_each_bit(base: u64, words: &[u64], mut f: impl FnMut(u64)) {
    for (i, w) in words.iter().enumerate() {
        let mut w = *w;
        while w != 0 {
            f(base + i as u64 * 64 + u64::from(w.trailing_zeros()));
            w &= w - 1;
        }
    }
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

    /// Dense ids become a bitset, sparse ones stay a list, and both
    /// answer membership, the greatest id and iteration the same.
    #[test]
    fn id_parts_take_the_smaller_form() {
        let dense: Vec<u64> = (1_000..3_000).filter(|i| i % 3 != 0).collect();
        let sparse: Vec<u64> = (0..100).map(|i| i * 1_000 + 7).collect();
        for (ids, bits) in [(dense, true), (sparse, false), (vec![5], false)] {
            let part = IdPart::from_sorted(ids.clone());
            assert_eq!(matches!(part, IdPart::Bits { .. }), bits);
            assert!(part.heap_bytes() <= ids.len() * 8);
            let mut seen = Vec::new();
            part.for_each(|id| seen.push(id));
            assert_eq!(seen, ids);
            assert_eq!(part.last(), ids.last().copied());
            assert!(!part.is_empty());
            let lo = ids[0].saturating_sub(70);
            for id in lo..ids.last().unwrap() + 70 {
                assert_eq!(part.contains(id), ids.binary_search(&id).is_ok(), "{id}");
            }
        }
        assert!(IdPart::from_sorted(Vec::new()).is_empty());
        let set = IdSet {
            parts: vec![
                Arc::new(IdPart::from_sorted((10..200).collect())),
                Arc::new(IdPart::from_sorted(vec![3, 500])),
            ],
            union: OnceLock::new(),
        };
        let union = set.union().unwrap();
        assert_eq!(union.max_value(), 501);
        for id in 0..600u32 {
            let want = (10..200).contains(&id) || id == 3 || id == 500;
            assert_eq!(id < 501 && union.contains(id), want, "{id}");
            assert_eq!(set.contains(u64::from(id)), want, "{id}");
        }
    }

    #[test]
    fn retained_capacity_is_bounded_and_oversized_results_still_belong_to_caller() {
        let cache = FileSetCache::with_budget(4096);
        for n in 0..100 {
            let mut ids = Vec::with_capacity(128);
            ids.push(n as u64);
            cache.insert(key(n), Arc::new(IdPart::List(ids)));
            assert!(cache.state.lock().unwrap().retained_bytes() <= cache.budget);
        }
        let large = Arc::new(IdPart::List(vec![1; 1024]));
        let k = key(101);
        cache.insert(k.clone(), large.clone());
        assert!(cache.get(&k).is_none());
        assert_eq!(*large, IdPart::List(vec![1; 1024]));
        cache.clear();
        assert_eq!(cache.state.lock().unwrap().retained_bytes(), 0);
    }

    #[test]
    fn usage_reports_entries_and_the_charged_bytes() {
        let cache = FileSetCache::default();
        assert_eq!(cache.usage(), (0, 0));
        cache.insert(key(1), Arc::new(IdPart::List(vec![1; 100])));
        let (entries, bytes) = cache.usage();
        assert_eq!(entries, 1);
        assert_eq!(bytes, cache.state.lock().unwrap().retained_bytes() as u64);
        assert!(bytes >= 800, "{bytes}");
    }

    #[test]
    fn hits_refresh_recency_and_replacement_does_not_double_charge() {
        // Two 1 KiB vectors and table overhead fit; three vectors do not.
        let cache = FileSetCache::with_budget(3072);
        let a = key(1);
        let b = key(2);
        let c = key(3);
        let value = || Arc::new(IdPart::List(vec![0; 128]));
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

    /// Past [`CACHE_ENTRIES`], the least recently used set goes first,
    /// a hit counting as a use, and the recency index stays in step.
    #[test]
    fn past_the_entry_cap_the_least_recently_used_goes_first() {
        let cache = FileSetCache::default();
        let keys: Vec<CacheKey> = (0..CACHE_ENTRIES + 2).map(key).collect();
        for k in &keys[..CACHE_ENTRIES] {
            cache.insert(k.clone(), Arc::new(IdPart::List(vec![1])));
        }
        assert!(cache.get(&keys[0]).is_some());
        cache.insert(keys[CACHE_ENTRIES].clone(), Arc::new(IdPart::List(vec![1])));
        cache.insert(
            keys[CACHE_ENTRIES + 1].clone(),
            Arc::new(IdPart::List(vec![1])),
        );
        assert!(cache.get(&keys[1]).is_none());
        assert!(cache.get(&keys[2]).is_none());
        for k in [&keys[0], &keys[3], &keys[CACHE_ENTRIES + 1]] {
            assert!(cache.get(k).is_some());
        }
        let state = cache.state.lock().unwrap();
        assert_eq!(state.map.len(), CACHE_ENTRIES);
        assert_eq!(state.order.len(), CACHE_ENTRIES);
        // Oldest first: keys 4, 5, ... then the ones just read.
        let oldest: Vec<_> = state.order.values().take(2).cloned().collect();
        assert_eq!(oldest, [keys[4].clone(), keys[5].clone()]);
        let newest: Vec<_> = state.order.values().rev().take(3).cloned().collect();
        assert_eq!(
            newest,
            [
                keys[CACHE_ENTRIES + 1].clone(),
                keys[3].clone(),
                keys[0].clone()
            ]
        );
        assert!(state.retained_bytes() <= CACHE_BYTES);
    }

    #[test]
    fn cancellation_during_enumeration_stops_before_scanning_every_file() {
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

#[cfg(test)]
mod id_part_props {
    use super::*;
    use proptest::prelude::*;

    /// Sorted, deduplicated ids: runs of dense ids and lone sparse ones,
    /// from a random base, so sets cross 64-bit word boundaries at any
    /// offset and come out in either form.
    fn ids() -> impl Strategy<Value = Vec<u64>> {
        (
            0u64..1 << 40,
            prop::collection::vec((1u64..300, 0u64..200, any::<bool>()), 0..12),
        )
            .prop_map(|(base, runs)| {
                let mut out = Vec::new();
                let mut at = base;
                for (gap, len, dense) in runs {
                    at += gap;
                    for i in 0..len {
                        if dense || i % 37 == 0 {
                            out.push(at + i);
                        }
                    }
                    at += len;
                }
                out
            })
    }

    proptest! {
        /// Both forms answer `contains`, `for_each`, `last` and the
        /// union exactly as the plain sorted list does.
        #[test]
        fn an_id_part_is_its_sorted_list(a in ids(), b in ids()) {
            let part = IdPart::from_sorted(a.clone());
            let mut seen = Vec::new();
            part.for_each(|id| seen.push(id));
            prop_assert_eq!(&seen, &a);
            prop_assert_eq!(part.last(), a.last().copied());
            prop_assert_eq!(part.is_empty(), a.is_empty());
            prop_assert!(part.heap_bytes() <= a.len() * 8);
            // Gathered as a bitset from below the least id, or as a list
            // in any order with repeats, the part is the same.
            if let (Some(&lo), Some(&hi)) = (a.first(), a.last()) {
                for below in [0, 1, 63, 64, 65, 200] {
                    let lo = lo.saturating_sub(below);
                    let span = (hi - lo) / 64 + 1;
                    let mut bits = Gather::Bits { lo, words: vec![0; span as usize] };
                    a.iter().for_each(|id| bits.push(*id));
                    prop_assert_eq!(bits.finish(), part.clone());
                }
            }
            let mut list = Gather::List(Vec::new());
            a.iter().rev().chain(&a).for_each(|id| list.push(*id));
            prop_assert_eq!(list.finish(), part.clone());
            // A bitset whose range is too small falls back to the list.
            if a.len() > 1 {
                let mut short = Gather::Bits { lo: a[1], words: vec![0; 1] };
                a.iter().rev().for_each(|id| short.push(*id));
                prop_assert_eq!(short.finish(), part.clone());
            }
            if let (Some(&lo), Some(&hi)) = (a.first(), a.last()) {
                let probes = a.iter().flat_map(|&id| [id.wrapping_sub(1), id, id + 1]);
                for id in probes.chain([0, lo.saturating_sub(64), hi + 64, u64::MAX]) {
                    prop_assert_eq!(part.contains(id), a.binary_search(&id).is_ok(), "{}", id);
                }
            }
            // The union over two parts, within the bitset's range.
            let shift = |v: &[u64]| v.iter().map(|id| id % (1 << 16)).collect::<Vec<_>>();
            let (mut a, mut b) = (shift(&a), shift(&b));
            a.sort_unstable();
            a.dedup();
            b.sort_unstable();
            b.dedup();
            let set = IdSet {
                parts: vec![
                    Arc::new(IdPart::from_sorted(a.clone())),
                    Arc::new(IdPart::from_sorted(b.clone())),
                ],
                union: OnceLock::new(),
            };
            let mut all: Vec<u64> = a.iter().chain(&b).copied().collect();
            all.sort_unstable();
            all.dedup();
            prop_assert_eq!(set.is_empty(), all.is_empty());
            match set.union() {
                None => prop_assert!(all.is_empty()),
                Some(u) => {
                    let mut got = Vec::new();
                    for id in 0..u.max_value() {
                        if u.contains(id) {
                            got.push(u64::from(id));
                        }
                    }
                    prop_assert_eq!(&got, &all);
                }
            }
            for &id in all.iter().chain(&[0, 1 << 16]) {
                prop_assert_eq!(set.contains(id), all.binary_search(&id).is_ok());
            }
        }
    }
}

#[cfg(test)]
mod scan_tests {
    use super::*;

    /// Several segments, many files a torrent, some deleted, scanned in
    /// parallel: the sets are each segment's live torrents, whichever
    /// way they were gathered, and a second lookup is served from the
    /// cache.
    #[test]
    fn a_parallel_scan_finds_each_segments_live_torrents() {
        let mut builder = Schema::builder();
        let torrent = builder.add_u64_field(
            "torrent",
            NumericOptions::default().set_indexed().set_fast(),
        );
        let mut index = tantivy::Index::create_in_ram(builder.build());
        index.set_multithread_executor(3).unwrap();
        let mut writer = index
            .writer_with_num_threads::<TantivyDocument>(1, 15 << 20)
            .unwrap();
        writer.set_merge_policy(Box::new(tantivy::indexer::NoMergePolicy));
        // Dense segments (every torrent, five files each), a sparse one
        // (a torrent every 1,000), and one left with a single torrent.
        let segments: [Vec<u64>; 4] = [
            (0..5_000).collect(),
            (5_000..9_000).collect(),
            (0..50).map(|i| 9_000 + i * 1_000).collect(),
            vec![70_000, 70_001, 70_002],
        ];
        for ids in &segments {
            for id in ids {
                for _ in 0..5 {
                    let mut doc = TantivyDocument::new();
                    doc.add_u64(torrent, *id);
                    writer.add_document(doc).unwrap();
                }
            }
            writer.commit().unwrap();
        }
        let deleted = [17u64, 5_001, 19_000, 70_000, 70_001];
        for id in deleted {
            writer.delete_term(Term::from_field_u64(torrent, id));
        }
        writer.commit().unwrap();
        let reader = index.reader().unwrap();
        let searcher = reader.searcher();
        assert_eq!(searcher.segment_readers().len(), 4);
        let cache = FileSetCache::default();
        let token = crate::CancelToken::new();
        let set = torrent_ids(&searcher, &AllQuery, "all", &cache, &token).unwrap();
        let mut forms = vec![None; segments.len()];
        assert_eq!(set.parts.len(), segments.len());
        // Parts follow tantivy's segment order, which is not promised:
        // pair each with the segment that holds its first id.
        for part in &set.parts {
            let mut first = None;
            part.for_each(|id| {
                first.get_or_insert(id);
            });
            let first = first.expect("no segment is wholly deleted");
            let at = segments
                .iter()
                .position(|ids| ids.contains(&first))
                .expect("a part's ids come from a written segment");
            let ids = &segments[at];
            let want: Vec<u64> = ids
                .iter()
                .copied()
                .filter(|i| !deleted.contains(i))
                .collect();
            let mut got = Vec::new();
            part.for_each(|id| got.push(id));
            assert_eq!(got, want);
            assert!(forms[at].is_none(), "two parts for segment {at}");
            forms[at] = Some(matches!(**part, IdPart::Bits { .. }));
        }
        assert_eq!(forms, [Some(true), Some(true), Some(false), Some(false)]);
        assert!(set.contains(4_999) && !set.contains(17) && !set.contains(70_000));
        let (hits, misses) = cache.hits();
        assert_eq!((hits, misses), (0, 4));
        let again = torrent_ids(&searcher, &AllQuery, "all", &cache, &token).unwrap();
        assert_eq!(again, set);
        assert_eq!(cache.hits(), (4, 4));
    }

    /// A segment is cut into at most `MAX_PIECES` adjacent ranges that
    /// cover it exactly.
    #[test]
    fn pieces_cover_a_segment_in_order() {
        for (max_doc, piece) in [(0, 10), (1, 10), (10, 10), (11, 10), (35, 10), (1000, 10)] {
            let got: Vec<_> = pieces(max_doc, piece).collect();
            assert!(!got.is_empty() && got.len() <= MAX_PIECES as usize);
            assert_eq!(got[0].0, 0);
            assert_eq!(got.last().unwrap().1, max_doc);
            assert!(got.windows(2).all(|w| w[0].1 == w[1].0 && w[0].0 < w[0].1));
        }
        assert_eq!(pieces(11, 10).collect::<Vec<_>>(), [(0, 6), (6, 11)]);
        assert_eq!(pieces(5, 10).count(), 1);
    }

    /// Scanned in pieces (here a few hundred documents each, so every
    /// segment is cut, with pieces ending inside a torrent's files), a
    /// clause finds what a whole-segment scan finds, in both forms.
    #[test]
    fn a_scan_in_pieces_finds_what_a_whole_scan_does() {
        let mut builder = Schema::builder();
        let torrent = builder.add_u64_field("torrent", FAST);
        let size = builder.add_u64_field("size", FAST);
        let mut index = tantivy::Index::create_in_ram(builder.build());
        index.set_multithread_executor(3).unwrap();
        let mut writer = index
            .writer_with_num_threads::<TantivyDocument>(1, 15 << 20)
            .unwrap();
        writer.set_merge_policy(Box::new(tantivy::indexer::NoMergePolicy));
        for ids in [0..400u64, 400..1_000] {
            for id in ids {
                for f in 0..(1 + id % 4) {
                    let mut doc = TantivyDocument::new();
                    doc.add_u64(torrent, id);
                    doc.add_u64(size, id * 7 + f);
                    writer.add_document(doc).unwrap();
                }
            }
            writer.commit().unwrap();
        }
        writer.delete_term(Term::from_field_u64(torrent, 401));
        writer.commit().unwrap();
        let searcher = index.reader().unwrap().searcher();
        let token = crate::CancelToken::new();
        let queries: [(&str, Q); 3] = [
            ("all", Box::new(AllQuery)),
            (
                "big",
                Box::new(RangeQuery::new(
                    Bound::Included(Term::from_field_u64(size, 3_000)),
                    Bound::Unbounded,
                )),
            ),
            (
                "few",
                Box::new(RangeQuery::new(
                    Bound::Included(Term::from_field_u64(size, 10)),
                    Bound::Included(Term::from_field_u64(size, 40)),
                )),
            ),
        ];
        for (key, q) in &queries {
            let whole =
                torrent_ids(&searcher, q.as_ref(), key, &FileSetCache::default(), &token).unwrap();
            for piece_docs in [1, 97, 300] {
                let cut = torrent_ids_in_pieces(
                    &searcher,
                    q.as_ref(),
                    key,
                    &FileSetCache::default(),
                    &token,
                    piece_docs,
                )
                .unwrap();
                assert_eq!(cut, whole, "{key} in pieces of {piece_docs}");
            }
            assert!(!whole.contains(401));
        }
    }

    /// One segment's scan fails while its siblings are mid-scan: the
    /// siblings stop at their next check rather than run to their end,
    /// the failing segment's own error is the one returned, and nothing
    /// is cached.
    #[test]
    fn a_failing_segment_stops_its_siblings() {
        use std::time::{Duration, Instant};
        let mut builder = Schema::builder();
        let torrent = builder.add_u64_field("torrent", FAST);
        let mut index = tantivy::Index::create_in_ram(builder.build());
        index.set_multithread_executor(3).unwrap();
        let mut writer = index
            .writer_with_num_threads::<TantivyDocument>(1, 15 << 20)
            .unwrap();
        writer.set_merge_policy(Box::new(tantivy::indexer::NoMergePolicy));
        for seg in 0..3u64 {
            let mut doc = TantivyDocument::new();
            doc.add_u64(torrent, seg);
            writer.add_document(doc).unwrap();
            writer.commit().unwrap();
        }
        let searcher = index.reader().unwrap().searcher();
        assert_eq!(searcher.segment_readers().len(), 3);
        let probe = Probe {
            fail: searcher.segment_readers()[1].segment_id(),
            started: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            ran_out: Arc::new(AtomicBool::new(false)),
            deadline: Instant::now() + Duration::from_secs(20),
        };
        let cache = FileSetCache::default();
        let token = crate::CancelToken::new();
        let result = torrent_ids(&searcher, &probe, "probe", &cache, &token);
        match result {
            Err(crate::index::Error::Tantivy(e)) => {
                assert!(e.to_string().contains("injected"), "{e}")
            }
            other => panic!("expected the injected error, got {other:?}"),
        }
        assert_eq!(probe.started.load(Ordering::Acquire), 2);
        assert!(
            !probe.ran_out.load(Ordering::Acquire),
            "a sibling ran to its end"
        );
        assert!(!token.is_stopped());
        assert_eq!(cache.state.lock().unwrap().retained_bytes(), 0);
        assert_eq!(cache.usage().0, 0);
    }

    /// Fails segment `fail` once both siblings are scanning; the siblings
    /// match their one document over and over until `deadline`.
    #[derive(Debug, Clone)]
    struct Probe {
        fail: SegmentId,
        started: Arc<std::sync::atomic::AtomicUsize>,
        ran_out: Arc<AtomicBool>,
        deadline: std::time::Instant,
    }

    impl Query for Probe {
        fn weight(&self, _: EnableScoring<'_>) -> tantivy::Result<Box<dyn Weight>> {
            Ok(Box::new(self.clone()))
        }
    }

    impl Weight for Probe {
        fn scorer(&self, seg: &SegmentReader, _: Score) -> tantivy::Result<Box<dyn Scorer>> {
            if seg.segment_id() == self.fail {
                while self.started.load(Ordering::Acquire) < 2 {
                    assert!(
                        std::time::Instant::now() < self.deadline,
                        "siblings never started"
                    );
                    std::thread::yield_now();
                }
                return Err(tantivy::TantivyError::InternalError("injected".into()));
            }
            self.started.fetch_add(1, Ordering::AcqRel);
            Ok(Box::new(Endless(self.clone(), 0)))
        }

        fn explain(&self, _: &SegmentReader, _: DocId) -> tantivy::Result<Explanation> {
            Ok(Explanation::new("probe", 1.0))
        }
    }

    struct Endless(Probe, DocId);

    impl DocSet for Endless {
        fn advance(&mut self) -> DocId {
            if std::time::Instant::now() >= self.0.deadline {
                self.0.ran_out.store(true, Ordering::Release);
                self.1 = tantivy::TERMINATED;
            }
            self.1
        }

        fn doc(&self) -> DocId {
            self.1
        }

        fn size_hint(&self) -> u32 {
            0
        }
    }

    impl Scorer for Endless {
        fn score(&mut self) -> Score {
            1.0
        }
    }
}
