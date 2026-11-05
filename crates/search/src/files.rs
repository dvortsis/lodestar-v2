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
use tantivy::index::InvertedIndexReader;
use tantivy::index::SegmentId;
use tantivy::postings::SegmentPostings;
use tantivy::query::{
    AllQuery, BitSetDocSet, BooleanQuery, ConstScorer, EmptyQuery, EnableScoring, Explanation,
    FuzzyTermQuery, Occur, Query, RangeQuery, RegexQuery, Scorer, TermQuery, TermSetQuery, Weight,
};
use tantivy::schema::{
    BytesOptions, FAST, Field, IndexRecordOption, NumericOptions, Schema, TextFieldIndexing,
    TextOptions,
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
    /// v6. [`size_bucket`] of the size, in the inverted index, so a
    /// `size:` range can read the postings of the buckets inside it and
    /// check the column only in the two at its edges, instead of every
    /// file's value (X-search-files-2). Indexing `size` itself would
    /// cost a term per distinct size: 307 B per torrent, over §3's
    /// file-index budget on its own.
    pub size_bucket: Field,
    /// v6. The file's category ([`Category::as_str`], keyword), from its
    /// extension: `video` for `.mkv`, `other` without one.
    pub category: Field,
    /// v6. The v2 Merkle root (32 bytes), for "same file" across
    /// torrents. Only v2 and hybrid torrents have one.
    pub pieces_root: Field,
}

/// The fields v6 added to the file index, by name (as
/// [`crate::schema::V6_FIELDS`]).
pub const V6_FIELDS: [&str; 3] = ["size_bucket", "category", "pieces_root"];

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
        ext: b.add_text_field("ext", keyword.clone()),
        size: b.add_u64_field("size", NumericOptions::default().set_fast()),
        size_bucket: b.add_u64_field("size_bucket", NumericOptions::default().set_indexed()),
        category: b.add_text_field("category", keyword),
        pieces_root: b.add_bytes_field("pieces_root", BytesOptions::default().set_indexed()),
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
    /// From the extension; [`Category::Other`] without one.
    pub category: Category,
    pub pieces_root: Option<[u8; 32]>,
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
        // Exactly one bucket a file: `SizeRange::bucketed` relies on it.
        d.add_u64(f.size_bucket, size_bucket(self.size));
        d.add_text(f.category, self.category.as_str());
        if let Some(root) = &self.pieces_root {
            d.add_bytes(f.pieces_root, root);
        }
        d
    }
}

/// The bucket of a file size: two per power of two (the highest bit and
/// the one below it), so each spans at most half its lowest size and
/// there are at most 128. Sizes below 2 share bucket 0 and 1. Monotonic:
/// a larger size never has a smaller bucket.
///
/// Changing it needs a new field name (or a generation check in
/// `SizeRange::bucketed`): that check finds files without a bucket, not
/// files with one from an older definition (X-search-files-3).
pub fn size_bucket(size: u64) -> u64 {
    if size < 2 {
        return size;
    }
    let top = 63 - u64::from(size.leading_zeros());
    let half = (size >> (top - 1)) & 1;
    top * 2 + half
}

/// The smallest size in `bucket`, the inverse of [`size_bucket`]'s
/// lower edge.
pub fn bucket_floor(bucket: u64) -> u64 {
    if bucket < 2 {
        return bucket;
    }
    let (top, half) = (bucket / 2, bucket % 2);
    (1 << top) | (half << (top - 1))
}

/// Padding and hidden files (BEP 47), which no index covers.
fn skipped(f: &File) -> bool {
    f.attrs.contains(FileAttrs::PADDING) || f.attrs.contains(FileAttrs::HIDDEN)
}

/// Whether smart coverage treats `tree` as a large pack.
fn is_pack(tree: &FileTree) -> bool {
    tree.files.iter().filter(|f| !skipped(f)).count() > SMART_ALL_FILES
}

/// The files of a torrent that smart coverage indexes.
pub fn file_docs(torrent: u64, tree: &FileTree) -> Vec<FileDoc> {
    let pack = is_pack(tree);
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
            category: ext.as_deref().map_or(Category::Other, category_of),
            ext,
            size: f.size,
            pieces_root: f.pieces_root,
        });
    }
    out
}

/// The distinct [`size_code`]s of the files [`file_docs`] indexes, sorted,
/// and their distinct `{ext}:{code}` pairs, sorted: the torrent index's
/// v7 `file_bucket` and `file_ext_bucket` (X-search-files-5). Decided by
/// exactly `file_docs`' rule (test `covered_buckets_are_the_file_docs`),
/// so a bucket or pair term holds for a torrent exactly when one of its
/// file documents is in that bucket, with that extension. Only each
/// file's name is decoded, not its directories.
pub fn covered_buckets(tree: &FileTree) -> (Vec<u64>, Vec<String>) {
    let pack = is_pack(tree);
    let mut buckets = Vec::new();
    let mut pairs = Vec::new();
    for f in tree.files.iter().filter(|f| !skipped(f)) {
        let Some(name) = f.path.last() else {
            continue;
        };
        let text = Text {
            raw: name.clone(),
            encoding: tree.encoding,
        };
        let ext = extension(&text.decode());
        if pack && !worth_it_in_a_pack(ext.as_deref(), f.size) {
            continue;
        }
        let c = size_code(f.size);
        buckets.push(c);
        if let Some(e) = ext {
            pairs.push(ext_bucket_term(&e, c));
        }
    }
    buckets.sort_unstable();
    buckets.dedup();
    pairs.sort_unstable();
    pairs.dedup();
    (buckets, pairs)
}

/// The smallest sizes of the torrent's size codes, sorted and distinct:
/// [`size_bucket`]'s floors (each power of two and one and a half times
/// it, so a binary bound such as `size:>1GB` falls on one), and 5, 10,
/// 20, 50, 100, 200 and 500 of each binary unit (B, KB, MB, GB, TB, PB,
/// EB), so a decimal-looking bound such as `size:>100MB` (100 × 2^20)
/// falls on one too (X-search-files-6). 1 and 2 of a unit are powers of
/// two already.
fn code_floors() -> &'static [u64] {
    static FLOORS: std::sync::OnceLock<Vec<u64>> = std::sync::OnceLock::new();
    FLOORS.get_or_init(|| {
        let mut f: Vec<u64> = (0..128).map(bucket_floor).collect();
        for unit in (0..64).step_by(10) {
            for m in [5u64, 10, 20, 50, 100, 200, 500] {
                if let Some(v) = m.checked_mul(1 << unit) {
                    f.push(v);
                }
            }
        }
        f.sort_unstable();
        f.dedup();
        f
    })
}

/// The floor of each [`size_code`] pair, for tests that probe their
/// boundaries.
pub fn code_floor(i: usize) -> Option<u64> {
    code_floors().get(i).copied()
}

/// How many floors [`size_code`] has (its codes are `0..2 × that`).
pub fn code_floor_count() -> usize {
    code_floors().len()
}

/// A file's code in the torrent's `file_bucket` and `file_ext_bucket`:
/// the index of the largest [`code_floors`] entry at or below its size,
/// twice, plus one when it's above that floor. Splitting at each floor
/// makes a range with a strict lower bound on a floor (`size:>1GiB`,
/// `size:>100MB`) or an upper bound on one (`size:<=1GiB`) whole codes,
/// where it would otherwise check every file of the edge code in the
/// file index (X-search-files-5). Sizes exactly on a floor are rare, so
/// the split costs almost no terms. Monotonic: a larger size never has
/// a smaller code.
///
/// Changing it changes what `file_bucket` and `file_ext_bucket` hold:
/// bump both in [`crate::population::GENERATIONS`].
pub fn size_code(size: u64) -> u64 {
    let floors = code_floors();
    // `floors[0]` is 0, so there is always one at or below.
    let i = floors.partition_point(|&f| f <= size) - 1;
    i as u64 * 2 + u64::from(size > floors[i])
}

/// The sizes a [`size_code`] holds, as a closed range; `None` for the
/// upper code of two adjacent floors, which holds none.
fn code_range(code: u64) -> Option<(u64, u64)> {
    let floors = code_floors();
    let i = (code / 2) as usize;
    let floor = *floors.get(i)?;
    if code.is_multiple_of(2) {
        return Some((floor, floor));
    }
    let ceil = floors.get(i + 1).map_or(u64::MAX, |n| n - 1);
    (floor < ceil).then_some((floor + 1, ceil))
}

/// `file_ext_bucket`'s term for a file of extension `ext` and
/// [`size_code`] `code`. Extensions are ASCII letters and digits
/// (`doc::extension`), so the colon can't be part of one.
pub fn ext_bucket_term(ext: &str, code: u64) -> String {
    format!("{ext}:{code}")
}

/// How the torrent's codes answer a size range: the [`size_code`]s the
/// range holds whole, and the parts of it left for the file index (at
/// most two, the first and last codes it touches, each within one code
/// and not all of it).
pub(crate) fn code_plan(lo: u64, hi: u64) -> (Vec<u64>, Vec<(u64, u64)>) {
    let (mut codes, mut left) = (Vec::new(), Vec::new());
    if lo > hi {
        return (codes, left);
    }
    for c in size_code(lo)..=size_code(hi) {
        let Some((a, b)) = code_range(c) else {
            continue;
        };
        if lo <= a && b <= hi {
            codes.push(c);
        } else {
            left.push((lo.max(a), hi.min(b)));
        }
    }
    (codes, left)
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
/// clause that constrains nothing. A clause the torrent index answers
/// ([`crate::compile::file_exts`]) comes back as [`TorrentLevel`]: its
/// matched files are still read here, but its torrents aren't.
pub fn compile(node: &Node, f: &FileFields) -> Option<Q> {
    let q = compile_node(node, f)?;
    Some(match crate::compile::file_exts(node) {
        Some(_) => Box::new(TorrentLevel(q)),
        None => q,
    })
}

/// A `file:(…)` clause the torrent index answers. [`torrent_ids`] gives
/// no set for it, and so scans nothing, since the torrent query never
/// reads one; as a query it's the clause itself, for a result's matched
/// files.
#[derive(Debug)]
pub struct TorrentLevel(Q);

impl Clone for TorrentLevel {
    fn clone(&self) -> Self {
        TorrentLevel(self.0.box_clone())
    }
}

impl Query for TorrentLevel {
    fn weight(&self, scoring: EnableScoring<'_>) -> tantivy::Result<Box<dyn Weight>> {
        self.0.weight(scoring)
    }
}

fn compile_node(node: &Node, f: &FileFields) -> Option<Q> {
    Some(match node {
        Node::All => Box::new(AllQuery),
        Node::And { of } => {
            let mut clauses: Vec<(Occur, Q)> = Vec::new();
            for n in of {
                match n {
                    Node::Not { of } => {
                        if let Some(q) = compile_node(of, f) {
                            clauses.push((Occur::MustNot, q));
                        }
                    }
                    n => {
                        if let Some(q) = compile_node(n, f) {
                            clauses.push((Occur::Must, q));
                        }
                    }
                }
            }
            if clauses.iter().all(|(o, _)| *o == Occur::MustNot) {
                clauses.push((Occur::Must, Box::new(AllQuery)));
            }
            size_checked(clauses)
        }
        Node::Or { of } => {
            let clauses: Vec<(Occur, Q)> = of
                .iter()
                .filter_map(|n| compile_node(n, f))
                .map(|q| (Occur::Should, q))
                .collect();
            if clauses.is_empty() {
                return None;
            }
            Box::new(BooleanQuery::new(clauses))
        }
        Node::Not { of } => {
            let q = compile_node(of, f)?;
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
            let range = lo.unwrap_or(0)..=hi.unwrap_or(u64::MAX);
            if range.is_empty() {
                return Some(Box::new(EmptyQuery));
            }
            Box::new(SizeRange {
                field: f.size,
                bucket: f.size_bucket,
                range,
            })
        }
        // The parser only lets the clauses above in; anything else (from
        // AST JSON) matches nothing.
        _ => Box::new(EmptyQuery),
    })
}

/// A `size:` range. Where a segment's documents all carry a
/// [`size_bucket`], it reads the postings of the buckets the range
/// covers and checks the `size` column only in its (at most two) edge
/// buckets, so it never reads every file's size (X-search-files-3).
/// Elsewhere (an older index without the field, or a segment a backfill
/// hasn't finished) it's Tantivy's fast-field range over the column.
///
/// It keeps its bounds so a scan in pieces can read each piece's
/// documents straight from the postings or the column: Tantivy's range
/// doc set searches from document 0 for its first match when it's made,
/// before any seek, so each piece of a sparse range would read the
/// segment's head again.
#[derive(Debug, Clone)]
pub(crate) struct SizeRange {
    field: Field,
    /// `size_bucket`, or [`ABSENT`] on an index without it.
    bucket: Field,
    range: std::ops::RangeInclusive<u64>,
}

/// The largest size in `bucket`.
fn bucket_ceil(bucket: u64) -> u64 {
    if bucket >= 127 {
        u64::MAX
    } else {
        bucket_floor(bucket + 1) - 1
    }
}

impl SizeRange {
    /// The buckets the range touches, each with whether its files need
    /// the column check (an edge bucket the range only partly covers).
    /// Only the first and the last can be partial.
    fn buckets(&self) -> impl Iterator<Item = (u64, bool)> + '_ {
        let (lo, hi) = (*self.range.start(), *self.range.end());
        (size_bucket(lo)..=size_bucket(hi))
            .map(move |b| (b, bucket_floor(b) < lo || bucket_ceil(b) > hi))
    }

    /// Whether every document of `seg`, deleted ones included, has a
    /// bucket, so the buckets answer for the whole segment. A document
    /// has exactly one (`FileDoc::to_document`), so their postings'
    /// lengths sum to `max_doc` exactly when none is missing; a document
    /// a backfill hasn't reached, or one from before the field, makes it
    /// short. This reads the segment itself, so it holds for the
    /// snapshot being searched whatever the population record says.
    fn bucketed(&self, seg: &SegmentReader) -> tantivy::Result<Option<Arc<InvertedIndexReader>>> {
        if self.bucket == ABSENT
            || seg
                .schema()
                .get_field_entry(self.bucket)
                .field_type()
                .get_index_record_option()
                .is_none()
        {
            return Ok(None);
        }
        let inv = seg.inverted_index(self.bucket)?;
        let mut stream = inv.terms().stream()?;
        let mut docs = 0u64;
        while stream.advance() {
            docs += u64::from(stream.value().doc_freq);
        }
        Ok((docs == u64::from(seg.max_doc())).then_some(inv))
    }
}

impl SizeRange {
    /// How many files the range's buckets hold in a segment they cover
    /// (edge buckets whole).
    fn expected(&self, inv: &InvertedIndexReader) -> tantivy::Result<u32> {
        let mut n = 0u32;
        for (b, _) in self.buckets() {
            if let Some(info) = inv.get_term_info(&Term::from_field_u64(self.bucket, b))? {
                n = n.saturating_add(info.doc_freq);
            }
        }
        Ok(n)
    }
}

impl Query for SizeRange {
    fn weight(&self, scoring: EnableScoring<'_>) -> tantivy::Result<Box<dyn Weight>> {
        let bound = |v: u64| Bound::Included(Term::from_field_u64(self.field, v));
        let column = RangeQuery::new(bound(*self.range.start()), bound(*self.range.end()))
            .weight(scoring)?;
        let buckets = match self.bucket {
            ABSENT => None,
            _ => {
                let clauses: Vec<(Occur, Q)> = self
                    .buckets()
                    .map(|(b, edge)| {
                        let term = Term::from_field_u64(self.bucket, b);
                        let q: Q = if edge {
                            Box::new(EdgeBucket {
                                term,
                                size: self.field,
                                range: self.range.clone(),
                            })
                        } else {
                            Box::new(TermQuery::new(term, IndexRecordOption::Basic))
                        };
                        (Occur::Should, q)
                    })
                    .collect();
                Some(BooleanQuery::new(clauses).weight(scoring)?)
            }
        };
        Ok(Box::new(SizeWeight {
            range: self.clone(),
            column,
            buckets,
        }))
    }
}

/// [`SizeRange`] under a boolean: the union of its buckets' postings in
/// a segment that has them all, else the column range.
struct SizeWeight {
    range: SizeRange,
    column: Box<dyn Weight>,
    buckets: Option<Box<dyn Weight>>,
}

impl SizeWeight {
    fn pick(&self, seg: &SegmentReader) -> tantivy::Result<&dyn Weight> {
        Ok(match &self.buckets {
            Some(b) if self.range.bucketed(seg)?.is_some() => b.as_ref(),
            _ => self.column.as_ref(),
        })
    }
}

impl Weight for SizeWeight {
    fn scorer(&self, seg: &SegmentReader, boost: Score) -> tantivy::Result<Box<dyn Scorer>> {
        self.pick(seg)?.scorer(seg, boost)
    }

    fn explain(&self, seg: &SegmentReader, doc: DocId) -> tantivy::Result<Explanation> {
        self.pick(seg)?.explain(seg, doc)
    }
}

/// Fast-field columns opened once per query and segment, shared by the
/// segment's pieces and scorers. Opening a blockwise-linear column reads
/// its whole block table (a few bytes per 512 documents): at 10M
/// torrents, opened once a piece and once an edge scorer, that was about
/// a tenth of a size clause (X-search-files-4).
#[derive(Default)]
pub(crate) struct SegColumns(Mutex<HashMap<(SegmentId, Field), Arc<ColumnCell>>>);

type ColumnCell = OnceLock<tantivy::Result<Option<Column<u64>>>>;

impl SegColumns {
    /// `field`'s column in `seg`, `None` if the segment has none.
    fn get(&self, seg: &SegmentReader, field: Field) -> tantivy::Result<Option<Column<u64>>> {
        let cell = self
            .0
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .entry((seg.segment_id(), field))
            .or_default()
            .clone();
        cell.get_or_init(|| {
            seg.fast_fields()
                .column_opt::<u64>(seg.schema().get_field_name(field))
        })
        .clone()
    }
}

/// A conjunction's clauses as a query: [`SizeChecked`] when a required
/// `size:` range sits beside another required clause, else the plain
/// boolean.
fn size_checked(clauses: Vec<(Occur, Q)>) -> Q {
    let is_range = |o: &Occur, q: &Q| *o == Occur::Must && q.downcast_ref::<SizeRange>().is_some();
    let ranges: Vec<SizeRange> = clauses
        .iter()
        .filter(|(o, q)| is_range(o, q))
        .filter_map(|(_, q)| q.downcast_ref::<SizeRange>().cloned())
        .collect();
    let others = clauses
        .iter()
        .filter(|(o, q)| *o == Occur::Must && !is_range(o, q))
        .count();
    if ranges.is_empty() || others == 0 {
        return Box::new(BooleanQuery::new(clauses));
    }
    let rest: Vec<(Occur, Q)> = clauses
        .iter()
        .filter(|(o, q)| !is_range(o, q))
        .map(|(o, q)| (*o, q.box_clone()))
        .collect();
    Box::new(SizeChecked {
        rest: Box::new(BooleanQuery::new(rest)),
        ranges,
        whole: Box::new(BooleanQuery::new(clauses)),
        ratio: CHECK_RATIO,
    })
}

/// A conjunction with required `size:` ranges and other required
/// clauses (`ext:mkv size:>1GB`). Where the other clauses match fewer
/// files than the ranges' buckets hold, as an extension beside a broad
/// size usually does, they lead and each of their files has its size
/// checked in the column, a block at a time. Intersected with a
/// bucketed range instead, every bucket's postings were decoded and
/// merged through a union just to be skipped through (X-search-files-4).
/// Elsewhere it's the plain conjunction (`whole`). Both give the same
/// files: the check is exact.
#[derive(Debug)]
struct SizeChecked {
    /// The conjunction without its ranges.
    rest: Q,
    ranges: Vec<SizeRange>,
    whole: Q,
    /// [`CHECK_RATIO`]; tests force either path with 0 (never check) or
    /// `u64::MAX` (always).
    ratio: u64,
}

impl Clone for SizeChecked {
    fn clone(&self) -> Self {
        SizeChecked {
            rest: self.rest.box_clone(),
            ranges: self.ranges.clone(),
            whole: self.whole.box_clone(),
            ratio: self.ratio,
        }
    }
}

/// The column check leads unless the ranges' buckets hold at most
/// `1 / CHECK_RATIO` of the files the other clauses match: reading a
/// file's size from the column costs about what merging a file through
/// the bucket union does, and the union's files must be decoded besides.
const CHECK_RATIO: u64 = 4;

impl SizeChecked {
    fn checked_weight(&self, scoring: EnableScoring<'_>) -> tantivy::Result<SizeCheckedWeight> {
        let lo = self.ranges.iter().map(|r| *r.range.start()).max();
        let hi = self.ranges.iter().map(|r| *r.range.end()).min();
        // The other clauses as plain terms, if they are: their files are
        // then counted from the term dictionary, without opening postings.
        let mut terms = Vec::new();
        let rest: &dyn Query = match self.rest.downcast_ref::<BooleanQuery>() {
            Some(b) if b.clauses().len() == 1 && b.clauses()[0].0 == Occur::Must => {
                b.clauses()[0].1.as_ref()
            }
            _ => self.rest.as_ref(),
        };
        let rest_terms = TermUnion::terms(rest, &mut terms).then_some(terms);
        Ok(SizeCheckedWeight {
            rest: self.rest.weight(scoring)?,
            rest_terms,
            whole: self.whole.weight(scoring)?,
            ranges: self.ranges.clone(),
            bounds: (lo.unwrap_or(0), hi.unwrap_or(u64::MAX)),
            ratio: self.ratio,
            columns: SegColumns::default(),
        })
    }
}

impl Query for SizeChecked {
    fn weight(&self, scoring: EnableScoring<'_>) -> tantivy::Result<Box<dyn Weight>> {
        Ok(Box::new(self.checked_weight(scoring)?))
    }
}

struct SizeCheckedWeight {
    rest: Box<dyn Weight>,
    /// `rest` when it's any of some terms (`ext:mkv,mp4`).
    rest_terms: Option<Vec<Term>>,
    whole: Box<dyn Weight>,
    ranges: Vec<SizeRange>,
    /// Every range's bounds at once: their intersection, `lo > hi` when
    /// it's empty.
    bounds: (u64, u64),
    ratio: u64,
    columns: SegColumns,
}

/// How a [`SizeCheckedWeight`] runs in one segment.
enum Plan {
    /// The plain conjunction: the ranges' buckets hold few files.
    Whole,
    /// Nothing matches: the segment has no sizes.
    Empty,
    /// The other clauses' files, each size checked in the column.
    Check(Box<dyn Scorer>, Column<u64>),
}

impl SizeCheckedWeight {
    /// Which way this segment runs. Only the scorer that runs is built:
    /// where the other clauses are plain terms their files are counted
    /// from the term dictionary, so a segment that runs the plain
    /// conjunction opens no postings for the check it didn't take
    /// (X-search-files-5, #468's review P2).
    fn plan(&self, seg: &SegmentReader, boost: Score) -> tantivy::Result<Plan> {
        let field = self.ranges[0].field;
        // A segment without sizes matches none, as the range would.
        let Some(sizes) = self.columns.get(seg, field)? else {
            return Ok(Plan::Empty);
        };
        // The fewest files any range's buckets hold. Without buckets a
        // range reads the column for every file, so checking is better.
        let mut fewest: Option<u64> = None;
        for r in &self.ranges {
            if let Some(inv) = r.bucketed(seg)? {
                let n = u64::from(r.expected(&inv)?);
                fewest = Some(fewest.map_or(n, |f| f.min(n)));
            }
        }
        let Some(fewest) = fewest else {
            return Ok(Plan::Check(self.rest.scorer(seg, boost)?, sizes));
        };
        let (lead, rest) = match &self.rest_terms {
            Some(terms) => (self.term_files(seg, terms)?, None),
            None => {
                let rest = self.rest.scorer(seg, boost)?;
                (u64::from(rest.size_hint()), Some(rest))
            }
        };
        if fewest.saturating_mul(self.ratio) < lead {
            return Ok(Plan::Whole);
        }
        let rest = match rest {
            Some(r) => r,
            None => self.rest.scorer(seg, boost)?,
        };
        Ok(Plan::Check(rest, sizes))
    }

    /// The files `terms` hold in `seg` (deleted ones included), from the
    /// term dictionary: their sum, at least the union's size and no less
    /// than Tantivy's estimate of it, so the plan leans towards the plain
    /// conjunction. Either plan gives the same files.
    fn term_files(&self, seg: &SegmentReader, terms: &[Term]) -> tantivy::Result<u64> {
        let mut n = 0u64;
        for t in terms {
            if seg
                .schema()
                .get_field_entry(t.field())
                .field_type()
                .get_index_record_option()
                .is_none()
            {
                continue;
            }
            if let Some(info) = seg.inverted_index(t.field())?.get_term_info(t)? {
                n += u64::from(info.doc_freq);
            }
        }
        Ok(n)
    }
}

impl Weight for SizeCheckedWeight {
    fn scorer(&self, seg: &SegmentReader, boost: Score) -> tantivy::Result<Box<dyn Scorer>> {
        Ok(match self.plan(seg, boost)? {
            Plan::Whole => self.whole.scorer(seg, boost)?,
            Plan::Empty => Box::new(tantivy::query::EmptyScorer),
            Plan::Check(rest, sizes) => Box::new(CheckedScorer::new(rest, sizes, self.bounds)),
        })
    }

    fn explain(&self, seg: &SegmentReader, doc: DocId) -> tantivy::Result<Explanation> {
        // The same files either way: the plain conjunction explains.
        self.whole.explain(seg, doc)
    }
}

/// The other clauses' files whose size every range contains.
struct CheckedScorer {
    rest: Box<dyn Scorer>,
    sizes: Column<u64>,
    /// Every range's bounds at once: their intersection, `lo > hi` when
    /// it's empty.
    lo: u64,
    hi: u64,
}

impl CheckedScorer {
    fn new(rest: Box<dyn Scorer>, sizes: Column<u64>, (lo, hi): (u64, u64)) -> CheckedScorer {
        let mut scorer = CheckedScorer {
            rest,
            sizes,
            lo,
            hi,
        };
        scorer.settle();
        scorer
    }

    #[inline(always)]
    fn fits(&self, size: Option<u64>) -> bool {
        size.is_some_and(|s| self.lo <= s && s <= self.hi)
    }

    /// Moves to the first document from here whose size fits.
    fn settle(&mut self) -> DocId {
        loop {
            let doc = self.rest.doc();
            if doc == TERMINATED || self.fits(self.sizes.first(doc)) {
                return doc;
            }
            self.rest.advance();
        }
    }
}

impl DocSet for CheckedScorer {
    fn advance(&mut self) -> DocId {
        self.rest.advance();
        self.settle()
    }

    fn seek(&mut self, target: DocId) -> DocId {
        self.rest.seek(target);
        self.settle()
    }

    // `fill_buffer` is Tantivy's own, a document at a time, which keeps
    // its contract (a short buffer is the end). The scan reads `rest`
    // itself, a block at a time within its piece (`Source::Checked`).

    fn doc(&self) -> DocId {
        self.rest.doc()
    }

    fn size_hint(&self) -> u32 {
        self.rest.size_hint()
    }
}

impl Scorer for CheckedScorer {
    fn score(&mut self) -> Score {
        self.rest.score()
    }
}

/// Keeps, at the front of `docs`, those whose size is in `[lo, hi]`, in
/// order, and returns how many; their sizes are read in one call.
fn keep_fitting(
    sizes: &Column<u64>,
    (lo, hi): (u64, u64),
    docs: &mut [DocId],
    raw: &mut [u64; tantivy::COLLECT_BLOCK_BUFFER_LEN],
    vals: &mut [Option<u64>; tantivy::COLLECT_BLOCK_BUFFER_LEN],
) -> usize {
    let n = docs.len();
    let mut kept = 0;
    if matches!(sizes.index, tantivy::columnar::ColumnIndex::Full) {
        // Every file has a size (`FileDoc::to_document`).
        let raw = &mut raw[..n];
        sizes.values.get_vals(docs, raw);
        for i in 0..n {
            docs[kept] = docs[i];
            kept += usize::from(lo <= raw[i] && raw[i] <= hi);
        }
    } else {
        let vals = &mut vals[..n];
        vals.fill(None);
        sizes.first_vals(docs, vals);
        for i in 0..n {
            if vals[i].is_some_and(|s| lo <= s && s <= hi) {
                docs[kept] = docs[i];
                kept += 1;
            }
        }
    }
    kept
}

/// One edge bucket of a [`SizeRange`]: its postings, less the files
/// whose size is outside the range.
#[derive(Debug, Clone)]
struct EdgeBucket {
    term: Term,
    size: Field,
    range: std::ops::RangeInclusive<u64>,
}

impl Query for EdgeBucket {
    fn weight(&self, _: EnableScoring<'_>) -> tantivy::Result<Box<dyn Weight>> {
        Ok(Box::new(self.clone()))
    }
}

impl Weight for EdgeBucket {
    fn scorer(&self, seg: &SegmentReader, boost: Score) -> tantivy::Result<Box<dyn Scorer>> {
        let inv = seg.inverted_index(self.term.field())?;
        let sizes = seg
            .fast_fields()
            .column_opt::<u64>(seg.schema().get_field_name(self.size))?;
        let (Some(postings), Some(sizes)) = (
            inv.read_postings(&self.term, IndexRecordOption::Basic)?,
            sizes,
        ) else {
            return Ok(Box::new(tantivy::query::EmptyScorer));
        };
        let mut scorer = EdgeScorer {
            postings,
            sizes,
            range: self.range.clone(),
            boost,
        };
        scorer.settle();
        Ok(Box::new(scorer))
    }

    fn explain(&self, seg: &SegmentReader, doc: DocId) -> tantivy::Result<Explanation> {
        // The scorer starts on its first match: one past `doc` means
        // `doc` doesn't match, and seeking back isn't allowed.
        let mut s = self.scorer(seg, 1.0)?;
        if s.doc() <= doc && s.seek(doc) == doc {
            Ok(Explanation::new("size in an edge bucket", 1.0))
        } else {
            Err(tantivy::TantivyError::InvalidArgument(format!(
                "document {doc} doesn't match"
            )))
        }
    }
}

struct EdgeScorer {
    postings: SegmentPostings,
    sizes: Column<u64>,
    range: std::ops::RangeInclusive<u64>,
    boost: Score,
}

impl EdgeScorer {
    /// Moves to the first document from here whose size is in range.
    fn settle(&mut self) -> DocId {
        loop {
            let doc = self.postings.doc();
            if doc == TERMINATED
                || self
                    .sizes
                    .first(doc)
                    .is_some_and(|s| self.range.contains(&s))
            {
                return doc;
            }
            self.postings.advance();
        }
    }
}

impl DocSet for EdgeScorer {
    fn advance(&mut self) -> DocId {
        self.postings.advance();
        self.settle()
    }

    fn seek(&mut self, target: DocId) -> DocId {
        self.postings.seek(target);
        self.settle()
    }

    fn doc(&self) -> DocId {
        self.postings.doc()
    }

    fn size_hint(&self) -> u32 {
        self.postings.size_hint()
    }
}

impl Scorer for EdgeScorer {
    fn score(&mut self) -> Score {
        self.boost
    }
}

/// The torrents with a file matching one clause: a sorted id set per
/// file-index segment. Membership checks each one, so they're never
/// merged unless a query needs the union.
#[derive(Debug, Default, Clone)]
pub(crate) struct IdSet {
    parts: Vec<Arc<IdPart>>,
    union: OnceLock<Option<Union>>,
}

impl PartialEq for IdSet {
    fn eq(&self, other: &IdSet) -> bool {
        self.parts == other.parts
    }
}

impl IdSet {
    /// A set of the given ids, in any order.
    pub(crate) fn from_ids(mut ids: Vec<u64>) -> IdSet {
        ids.sort_unstable();
        ids.dedup();
        IdSet {
            parts: vec![Arc::new(IdPart::from_sorted(ids))],
            union: OnceLock::new(),
        }
    }

    /// Every id, sorted and distinct.
    pub(crate) fn to_vec(&self) -> Vec<u64> {
        let mut out = Vec::new();
        for p in &self.parts {
            p.for_each(|id| out.push(id));
        }
        out.sort_unstable();
        out.dedup();
        out
    }

    pub fn contains(&self, id: u64) -> bool {
        self.parts.iter().any(|p| p.contains(id))
    }

    pub fn is_empty(&self) -> bool {
        self.parts.iter().all(|p| p.is_empty())
    }

    /// Every id as a bitset, for checking each document of a segment in
    /// constant time; `None` when empty or past the bitset's range.
    /// Built a word at a time from the parts kept as bitsets (most of a
    /// broad clause's), rather than an insert per id: at 10M torrents
    /// that was about 10 ms a query (X-search-files-4).
    fn union(&self) -> Option<&Union> {
        self.union
            .get_or_init(|| {
                let max = self.parts.iter().filter_map(|p| p.last()).max()?;
                let max = u32::try_from(max).ok().filter(|m| *m < u32::MAX)?;
                // One spare word, for a part's last word shifted across.
                let mut words = vec![0u64; max as usize / 64 + 2];
                for p in &self.parts {
                    match p.as_ref() {
                        IdPart::List(ids) => {
                            for id in ids {
                                words[(id / 64) as usize] |= 1 << (id % 64);
                            }
                        }
                        IdPart::Bits { base, words: from } => {
                            let (at, shift) = ((base / 64) as usize, base % 64);
                            for (i, w) in from.iter().enumerate() {
                                words[at + i] |= w << shift;
                                if shift > 0 {
                                    words[at + i + 1] |= w >> (64 - shift);
                                }
                            }
                        }
                    }
                }
                Some(Union {
                    words,
                    end: u64::from(max) + 1,
                })
            })
            .as_ref()
    }
}

/// [`IdSet::union`]: bit `i` of word `i / 64` is id `i`.
#[derive(Debug, Clone)]
pub(crate) struct Union {
    words: Vec<u64>,
    /// One past the greatest id.
    end: u64,
}

impl Union {
    #[inline]
    fn contains(&self, id: u64) -> bool {
        id < self.end && (self.words[(id / 64) as usize] >> (id % 64)) & 1 == 1
    }

    #[cfg(test)]
    fn max_value(&self) -> u64 {
        self.end
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
    let len = max_doc
        .div_ceil(max_doc.div_ceil(piece_docs.max(1)).clamp(1, MAX_PIECES))
        .max(1);
    // From `len`, so no piece is left empty (5 docs in 4 pieces is 3).
    let n = max_doc.div_ceil(len).max(1);
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
    if query.downcast_ref::<TorrentLevel>().is_some() {
        return Ok(IdSet::default());
    }
    let segs = searcher.segment_readers();
    let keys: Vec<CacheKey> = segs
        .iter()
        .map(|seg| (key.to_owned(), seg.segment_id(), seg.num_deleted_docs()))
        .collect();
    let mut parts: Vec<Option<Arc<IdPart>>> = keys.iter().map(|k| cache.get(k)).collect();
    let missed: Vec<usize> = (0..segs.len()).filter(|i| parts[*i].is_none()).collect();
    if !missed.is_empty() {
        cancel.check()?;
        let size = query.downcast_ref::<SizeRange>();
        let scoring = EnableScoring::disabled_from_searcher(searcher);
        let checked = query
            .downcast_ref::<SizeChecked>()
            .map(|c| c.checked_weight(scoring))
            .transpose()?;
        let union = match (size, &checked) {
            (None, None) => TermUnion::of(query),
            _ => None,
        };
        // The weight of the clause itself, or of a term union's exclusions.
        let weight = match (size, &checked, &union) {
            (Some(_), _, _) | (None, Some(_), _) => None,
            (None, None, Some(u)) => u.exclude.as_ref().map(|q| q.weight(scoring)).transpose()?,
            (None, None, None) => Some(query.weight(scoring)?),
        };
        let source = match (size, &checked, &union, &weight) {
            (Some(r), _, _, _) => Source::Size(r),
            (None, Some(c), _, _) => Source::Checked(c),
            (None, None, Some(u), w) => Source::Terms(u, w.as_deref()),
            (None, None, None, Some(w)) => Source::Weight(w.as_ref()),
            (None, None, None, None) => unreachable!("a weight is built for every other query"),
        };
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
        // Each segment's `torrent` column, opened by its first piece and
        // shared with the rest: opening a blockwise-linear column reads
        // its block table (a few bytes per 512 documents), which at 10M
        // torrents' files was several milliseconds a piece.
        let columns: Vec<OnceLock<tantivy::Result<Column<u64>>>> =
            segs.iter().map(|_| OnceLock::new()).collect();
        // A bare size range's `size` columns, likewise.
        let sizes = SegColumns::default();
        let scanned = searcher
            .index()
            .search_executor()
            .map(
                |(i, lo, hi): (usize, DocId, DocId)| {
                    let column = columns[i]
                        .get_or_init(|| segs[i].fast_fields().u64("torrent"))
                        .clone();
                    column
                        .and_then(|column| {
                            scan_segment(
                                &source,
                                &segs[i],
                                &column,
                                &sizes,
                                (lo, hi),
                                cancel,
                                &failed,
                            )
                        })
                        .map_err(|e| {
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
                        })
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
    #[inline(always)]
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
            // Pieces sized apart can differ in form (a short last piece
            // can fall under the bitset's threshold): the list goes into
            // the bitset, rather than the bitset into a list to sort.
            (mut bits @ Gather::Bits { .. }, Gather::List(ids))
            | (Gather::List(ids), mut bits @ Gather::Bits { .. }) => {
                for id in ids {
                    bits.push(id);
                }
                bits
            }
            (Gather::List(mut ids), b) => {
                b.for_each(|id| ids.push(id));
                Gather::List(ids)
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

/// Where a scan's matching documents come from.
enum Source<'a> {
    /// The clause's scorer, seeked to the piece's start.
    Weight(&'a dyn Weight),
    /// A bare `size:` range, read from its buckets' postings, or from the
    /// column a block at a time where the segment's buckets are
    /// incomplete.
    Size(&'a SizeRange),
    /// Any of some terms (`file:mkv`, `file:(ext:mkv,mp4)`), less what
    /// `exclude` matches (`-path:sample`): see [`TermUnion`].
    Terms(&'a TermUnion, Option<&'a dyn Weight>),
    /// A conjunction with a size check ([`SizeChecked`]): where it checks
    /// the column, the other clauses' documents a block at a time within
    /// the piece, their sizes checked in one call.
    Checked(&'a SizeCheckedWeight),
}

/// A clause that is any of some terms, perhaps less some others'
/// matches. The scan reads each term's postings in turn instead of
/// merging them through Tantivy's union, which sorts documents the
/// gather doesn't need sorted: at 10M torrents the merge was over a
/// third of `ext:mkv,mp4,avi -path:sample` (X-search-files-4). The
/// excluded documents of each piece are read into a bitset first.
pub(crate) struct TermUnion {
    terms: Vec<Term>,
    exclude: Option<Q>,
}

impl TermUnion {
    /// The terms of a query that is one term, or `Should`-only
    /// alternatives that are each such a query.
    fn terms(q: &dyn Query, out: &mut Vec<Term>) -> bool {
        if let Some(t) = q.downcast_ref::<TermQuery>() {
            out.push(t.term().clone());
            return true;
        }
        match q.downcast_ref::<BooleanQuery>() {
            Some(b) if !b.clauses().is_empty() => b
                .clauses()
                .iter()
                .all(|(o, c)| *o == Occur::Should && TermUnion::terms(c.as_ref(), out)),
            _ => false,
        }
    }

    /// `q` as a union of terms with exclusions, if it's one: a term
    /// union alone, or one required term union and only `MustNot`
    /// clauses beside it.
    fn of(q: &dyn Query) -> Option<TermUnion> {
        let mut terms = Vec::new();
        if TermUnion::terms(q, &mut terms) {
            return Some(TermUnion {
                terms,
                exclude: None,
            });
        }
        let b = q.downcast_ref::<BooleanQuery>()?;
        let mut exclude = Vec::new();
        let mut must = 0;
        for (o, c) in b.clauses() {
            match o {
                Occur::Must => {
                    must += 1;
                    if must > 1 || !TermUnion::terms(c.as_ref(), &mut terms) {
                        return None;
                    }
                }
                Occur::MustNot => exclude.push((Occur::Should, c.box_clone())),
                Occur::Should => return None,
            }
        }
        (must == 1).then(|| TermUnion {
            terms,
            exclude: (!exclude.is_empty()).then(|| Box::new(BooleanQuery::new(exclude)) as Q),
        })
    }
}

/// The torrent ids of one file-index segment's documents in `[lo, hi)`.
/// Documents come 64 at a time, with their torrent ids read in one call.
fn scan_segment(
    source: &Source<'_>,
    seg: &SegmentReader,
    column: &Column<u64>,
    sizes: &SegColumns,
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
    let alive = seg.alive_bitset();
    let span = column.max_value().saturating_sub(column.min_value()) / 64 + 1;
    // `hint` is the scorer's estimate of matching files. Tantivy 0.27's
    // range doc set gives `num_docs / 10` (an upstream TODO), and a bare
    // `size:` range on the column fallback uses the same, so the form
    // chosen for those can be the costlier one; either is correct. The
    // bucket path passes its postings' lengths instead. Each counts the
    // whole segment's matches, so it's scaled to the piece's share of the
    // documents: a piece's bitset spans the segment's ids all the same,
    // and pays only for the matches the piece itself holds.
    let gather_for = |hint: u32| {
        let docs = u64::from(hi.saturating_sub(lo));
        let hint = u64::from(hint) * docs / u64::from(seg.max_doc().max(1));
        if seg.max_doc() > 0 && hint >= span {
            Gather::Bits {
                lo: column.min_value(),
                words: vec![0; span as usize],
            }
        } else {
            Gather::List(Vec::new())
        }
    };
    const BLOCK: usize = tantivy::COLLECT_BLOCK_BUFFER_LEN;
    let mut ids = [None; BLOCK];
    let mut raw = [0u64; BLOCK];
    let full = matches!(column.index, tantivy::columnar::ColumnIndex::Full);
    // A clause's scorer, or one term's postings, gives a torrent's
    // matching files together, so
    // a run of one id is pushed once; a size range's buckets give them
    // bucket by bucket, where runs are rare and the check costs more than
    // it saves.
    let runs = !matches!(source, Source::Size(_));
    let mut add = |docs: &[DocId], gather: &mut Gather| {
        for docs in docs.chunks(BLOCK) {
            if full {
                // Every file has its torrent (as `FileDoc::to_document`
                // writes it): read the values directly, without the
                // optional slots.
                let raw = &mut raw[..docs.len()];
                column.values.get_vals(docs, raw);
                let mut last = u64::MAX;
                for (doc, id) in docs.iter().zip(raw.iter()) {
                    if (!runs || last != *id) && alive.is_none_or(|a| a.is_alive(*doc)) {
                        gather.push(*id);
                        last = *id;
                    }
                }
                continue;
            }
            let ids = &mut ids[..docs.len()];
            // A multivalued column would leave a valueless doc's slot as
            // it was; `torrent` has one value a file, but don't rely on it.
            ids.fill(None);
            column.first_vals(docs, ids);
            for (doc, id) in docs.iter().zip(ids.iter()) {
                if let Some(id) = *id
                    && alive.is_none_or(|a| a.is_alive(*doc))
                {
                    gather.push(id);
                }
            }
        }
    };
    // A scorer's documents within the piece, a block at a time, those of
    // a size outside `check`'s bounds left out.
    let mut scan_scorer = |mut scorer: Box<dyn Scorer>,
                           check: Option<(&Column<u64>, (u64, u64))>|
     -> tantivy::Result<Gather> {
        const CHECK_BLOCKS: u32 = crate::cancel::CHECK_EVERY / 2 / BLOCK as u32;
        const _: () = assert!(CHECK_BLOCKS > 0);
        let mut gather = gather_for(scorer.size_hint());
        if scorer.doc() < lo {
            scorer.seek(lo);
        }
        let mut docs = [0 as DocId; BLOCK];
        let mut raw = [0u64; BLOCK];
        let mut vals = [None; BLOCK];
        let mut blocks = 0u32;
        loop {
            if blocks.is_multiple_of(CHECK_BLOCKS) {
                stopped()?;
            }
            blocks = blocks.wrapping_add(1);
            let filled = scorer.fill_buffer(&mut docs);
            // Docs come in order; those from `hi` on are the next piece's.
            let n = docs[..filled].partition_point(|d| *d < hi);
            let keep = match check {
                Some((sizes, bounds)) => {
                    keep_fitting(sizes, bounds, &mut docs[..n], &mut raw, &mut vals)
                }
                None => n,
            };
            add(&docs[..keep], &mut gather);
            if n < filled || filled == 0 {
                break;
            }
        }
        Ok(gather)
    };
    let gather = match source {
        Source::Weight(weight) => scan_scorer(weight.scorer(seg, 1.0)?, None)?,
        Source::Checked(w) => match w.plan(seg, 1.0)? {
            Plan::Whole => scan_scorer(w.whole.scorer(seg, 1.0)?, None)?,
            Plan::Empty => gather_for(0),
            Plan::Check(rest, sizes) => scan_scorer(rest, Some((&sizes, w.bounds)))?,
        },
        Source::Terms(union, exclude) => {
            let gather = scan_terms(
                seg,
                union,
                *exclude,
                (lo, hi),
                &stopped,
                gather_for,
                &mut add,
            )?;
            stopped()?;
            return Ok(gather);
        }
        Source::Size(range) => {
            // A segment without sizes matches none, as Tantivy's range does.
            // Belt and braces: every file document is written with a size
            // (`FileDoc::to_document`), so only a foreign index lacks the
            // column. The clamp below is the guard results depend on.
            let Some(sizes) = sizes.get(seg, range.field)? else {
                return Ok(gather_for(0));
            };
            if let Some(inv) = range.bucketed(seg)? {
                let gather = scan_buckets(
                    range,
                    &inv,
                    &sizes,
                    (lo, hi),
                    &stopped,
                    gather_for,
                    &mut add,
                )?;
                stopped()?;
                return Ok(gather);
            }
            let mut gather = gather_for(seg.num_docs() / 10);
            // The column's range search assumes bounds within its values
            // (Tantivy's own range query clamps them first): unclamped, a
            // range wholly below the segment's sizes, or a segment of one
            // distinct size, gets documents outside it.
            let (min, max) = (sizes.min_value(), sizes.max_value());
            let (start, end) = (*range.range.start(), *range.range.end());
            if end < min || start > max {
                return Ok(gather);
            }
            let clamped = start.max(min)..=end.min(max);
            let mut docs = Vec::with_capacity(BLOCK);
            let mut at = lo;
            while at < hi {
                stopped()?;
                let end = at.saturating_add(crate::cancel::CHECK_EVERY).min(hi);
                docs.clear();
                sizes.get_docids_for_value_range(clamped.clone(), at..end, &mut docs);
                add(&docs, &mut gather);
                at = end;
            }
            gather
        }
    };
    stopped()?;
    Ok(gather)
}

/// A [`TermUnion`] over one piece of a segment: each term's postings from
/// `lo` in turn, less the documents `exclude` matches in the piece. Ids
/// come in runs, one a term; the gather doesn't need them in order. Its
/// form follows the terms' postings lengths.
fn scan_terms(
    seg: &SegmentReader,
    union: &TermUnion,
    exclude: Option<&dyn Weight>,
    (lo, hi): (DocId, DocId),
    stopped: &dyn Fn() -> tantivy::Result<()>,
    gather_for: impl Fn(u32) -> Gather,
    mut add: impl FnMut(&[DocId], &mut Gather),
) -> tantivy::Result<Gather> {
    const BLOCK: usize = tantivy::COLLECT_BLOCK_BUFFER_LEN;
    const CHECK_BLOCKS: u32 = crate::cancel::CHECK_EVERY / 2 / BLOCK as u32;
    let mut docs = [0 as DocId; BLOCK];
    let mut blocks = 0u32;
    // The piece's excluded documents, `doc - lo` in the bitset.
    let excluded = match exclude {
        None => None,
        Some(w) => {
            let mut set = BitSet::with_max_value(hi.saturating_sub(lo).max(1));
            let mut scorer = w.scorer(seg, 1.0)?;
            if scorer.doc() < lo {
                scorer.seek(lo);
            }
            loop {
                if blocks.is_multiple_of(CHECK_BLOCKS) {
                    stopped()?;
                }
                blocks = blocks.wrapping_add(1);
                let filled = scorer.fill_buffer(&mut docs);
                let n = docs[..filled].partition_point(|d| *d < hi);
                for d in &docs[..n] {
                    set.insert(d - lo);
                }
                if n < filled || filled == 0 {
                    break;
                }
            }
            Some(set)
        }
    };
    let mut lists = Vec::with_capacity(union.terms.len());
    let mut expected = 0u32;
    for term in &union.terms {
        // A field the segment's schema doesn't index matches nothing.
        if seg
            .schema()
            .get_field_entry(term.field())
            .field_type()
            .get_index_record_option()
            .is_none()
        {
            continue;
        }
        let inv = seg.inverted_index(term.field())?;
        if let Some(info) = inv.get_term_info(term)? {
            expected = expected.saturating_add(info.doc_freq);
            lists.push((inv, info));
        }
    }
    let mut gather = gather_for(expected);
    let mut kept = Vec::with_capacity(BLOCK);
    for (inv, info) in lists {
        let mut postings = inv.read_postings_from_terminfo(&info, IndexRecordOption::Basic)?;
        if postings.doc() < lo {
            postings.seek(lo);
        }
        loop {
            if blocks.is_multiple_of(CHECK_BLOCKS) {
                stopped()?;
            }
            blocks = blocks.wrapping_add(1);
            let filled = postings.fill_buffer(&mut docs);
            let n = docs[..filled].partition_point(|d| *d < hi);
            match &excluded {
                Some(set) => {
                    kept.clear();
                    kept.extend(docs[..n].iter().filter(|d| !set.contains(**d - lo)));
                    add(&kept, &mut gather);
                }
                None => add(&docs[..n], &mut gather),
            }
            if n < filled || filled == 0 {
                break;
            }
        }
    }
    Ok(gather)
}

/// A bare `size:` range over one piece of a segment whose documents all
/// have a bucket: each bucket's postings from `lo`, the edge buckets'
/// filtered by the column. Buckets are read one after another, so ids
/// come in runs; the gather doesn't need them in order. Its form follows
/// the buckets' postings lengths, which count the files matched (edge
/// buckets' whole), rather than the range's guessed tenth.
fn scan_buckets(
    range: &SizeRange,
    inv: &InvertedIndexReader,
    sizes: &Column<u64>,
    (lo, hi): (DocId, DocId),
    stopped: &dyn Fn() -> tantivy::Result<()>,
    gather_for: impl Fn(u32) -> Gather,
    mut add: impl FnMut(&[DocId], &mut Gather),
) -> tantivy::Result<Gather> {
    const BLOCK: usize = tantivy::COLLECT_BLOCK_BUFFER_LEN;
    const CHECK_BLOCKS: u32 = crate::cancel::CHECK_EVERY / 2 / BLOCK as u32;
    let mut lists = Vec::new();
    let mut expected = 0u32;
    for (b, edge) in range.buckets() {
        let term = Term::from_field_u64(range.bucket, b);
        if let Some(info) = inv.get_term_info(&term)? {
            expected = expected.saturating_add(info.doc_freq);
            lists.push((info, edge));
        }
    }
    let mut gather = gather_for(expected);
    let mut docs = [0 as DocId; BLOCK];
    let mut vals = [None; BLOCK];
    let mut kept = Vec::with_capacity(BLOCK);
    let mut blocks = 0u32;
    for (info, edge) in lists {
        let mut postings = inv.read_postings_from_terminfo(&info, IndexRecordOption::Basic)?;
        if postings.doc() < lo {
            postings.seek(lo);
        }
        loop {
            if blocks.is_multiple_of(CHECK_BLOCKS) {
                stopped()?;
            }
            blocks = blocks.wrapping_add(1);
            let filled = postings.fill_buffer(&mut docs);
            let n = docs[..filled].partition_point(|d| *d < hi);
            if edge {
                let vals = &mut vals[..n];
                vals.fill(None);
                sizes.first_vals(&docs[..n], vals);
                kept.clear();
                kept.extend(
                    docs[..n]
                        .iter()
                        .zip(vals.iter())
                        .filter(|(_, v)| v.is_some_and(|v| range.range.contains(&v)))
                        .map(|(d, _)| *d),
                );
                add(&kept, &mut gather);
            } else {
                add(&docs[..n], &mut gather);
            }
            if n < filled || filled == 0 {
                break;
            }
        }
    }
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
        if self.0.is_empty() {
            return Ok(set);
        }
        let union = self.0.union();
        let hit = |id: u64| match union {
            Some(ids) => ids.contains(id),
            // Ids past 4 billion: slower, but still right.
            None => self.0.contains(id),
        };
        // Every torrent has one id, so the column is normally full: read
        // its values a block at a time rather than one lookup a document
        // (at 10M, the per-document lookups were most of the join,
        // X-search-files-3).
        const JOIN_BLOCK: usize = 1024;
        if matches!(column.index, tantivy::columnar::ColumnIndex::Full) {
            let mut vals = [0u64; JOIN_BLOCK];
            let mut at = 0u32;
            while at < reader.max_doc() {
                let n = (reader.max_doc() - at).min(JOIN_BLOCK as u32) as usize;
                column.values.get_range(u64::from(at), &mut vals[..n]);
                for (i, id) in vals[..n].iter().enumerate() {
                    if hit(*id) {
                        set.insert(at + i as u32);
                    }
                }
                at += n as u32;
            }
        } else {
            for doc in 0..reader.max_doc() {
                if column.first(doc).is_some_and(hit) {
                    set.insert(doc);
                }
            }
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

/// The negated words of a [`crate::compile::negated_words`] clause go
/// through the file index's postings, not the extensions', only when
/// their terms' files are at most this fraction of the extensions'
/// files: the suspects' pass reads about as many files as the words
/// have, and the whole clause's pass as many as the extensions have
/// (X-search-files-6).
pub(crate) const NEGATED_RATIO: u64 = 8;

/// The bad torrents of a [`crate::compile::negated_words`] clause: those
/// with a covered file of the extensions that has one of the words (the
/// suspects, driven by the words' postings), and none of the
/// extensions without them (checked on the suspects' files only). The
/// clause's torrents are the candidates (`file_ext_bucket`) less these.
/// `None` when the words aren't rare enough ([`NEGATED_RATIO`]) or
/// compile to nothing: the clause then runs on the file index whole.
pub(crate) fn bad_torrents(
    searcher: &Searcher,
    f: &FileFields,
    neg: &crate::compile::NegatedWords,
    cache: &FileSetCache,
    cancel: &crate::CancelToken,
) -> crate::index::Result<Option<IdSet>> {
    if f.torrent == ABSENT || f.ext == ABSENT {
        return Ok(None);
    }
    let ext = Node::Ext {
        any: neg.exts.clone(),
    };
    let words = match neg.words.as_slice() {
        [one] => one.clone(),
        many => Node::Or { of: many.to_vec() },
    };
    let (Some(ext_q), Some(words_q)) = (compile_node(&ext, f), compile_node(&words, f)) else {
        return Ok(None);
    };
    let ext_terms: Vec<Term> = neg
        .exts
        .iter()
        .map(|e| Term::from_field_text(f.ext, e))
        .collect();
    let mut word_terms = Vec::new();
    for w in &neg.words {
        if let Node::Word { field, word } = w {
            for (fld, _) in targets(*field, f, false) {
                if fld != ABSENT {
                    word_terms.extend(
                        tokenizer::tokenize(word)
                            .iter()
                            .map(|t| Term::from_field_text(fld, &t.text)),
                    );
                }
            }
        }
    }
    let (exts, words_n) = (
        doc_freq(searcher, &ext_terms)?,
        doc_freq(searcher, &word_terms)?,
    );
    if words_n.saturating_mul(NEGATED_RATIO) > exts {
        return Ok(None);
    }
    cancel.check()?;
    let suspects_node = Node::And {
        of: vec![ext, words],
    };
    let suspects_q = compile_node(&suspects_node, f).expect("an extension compiles");
    let suspects = torrent_ids(
        searcher,
        suspects_q.as_ref(),
        &format!("{suspects_node:?}"),
        cache,
        cancel,
    )?
    .to_vec();
    if suspects.is_empty() {
        return Ok(Some(IdSet::default()));
    }
    cancel.check()?;
    // The suspects' files with an extension and none of the words.
    let theirs = TermSetQuery::new(
        suspects
            .iter()
            .map(|&id| Term::from_field_u64(f.torrent, id)),
    );
    let rescue = BooleanQuery::new(vec![
        (Occur::Must, Box::new(theirs) as Q),
        (Occur::Must, ext_q),
        (Occur::MustNot, words_q),
    ]);
    let rescued = searcher.search(&rescue, &Torrents)?;
    cancel.check()?;
    Ok(Some(IdSet::from_ids(
        suspects
            .into_iter()
            .filter(|id| rescued.binary_search(id).is_err())
            .collect(),
    )))
}

/// The documents holding any of `terms`, summed over the searcher's
/// segments (deleted ones included): a bound on the postings a clause
/// on them reads.
fn doc_freq(searcher: &Searcher, terms: &[Term]) -> crate::index::Result<u64> {
    let mut n = 0u64;
    for seg in searcher.segment_readers() {
        for t in terms {
            let inv = seg.inverted_index(t.field())?;
            n += u64::from(inv.doc_freq(t).map_err(tantivy::TantivyError::from)?);
        }
    }
    Ok(n)
}

/// Collects the torrent ids of the matching documents, sorted and
/// distinct.
struct Torrents;

impl Collector for Torrents {
    type Fruit = Vec<u64>;
    type Child = FileIndexesSegment;

    fn for_segment(
        &self,
        _ord: SegmentOrdinal,
        reader: &SegmentReader,
    ) -> tantivy::Result<FileIndexesSegment> {
        Ok(FileIndexesSegment {
            column: reader.fast_fields().u64("torrent")?,
            found: Vec::new(),
        })
    }

    fn requires_scoring(&self) -> bool {
        false
    }

    fn merge_fruits(&self, fruits: Vec<Vec<u64>>) -> tantivy::Result<Vec<u64>> {
        FileIndexes.merge_fruits(fruits)
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
    fn size_buckets_are_monotonic_and_invert_at_their_floors() {
        assert_eq!((0..4).map(size_bucket).collect::<Vec<_>>(), [0, 1, 2, 3]);
        assert_eq!(size_bucket(5), 4);
        assert_eq!(size_bucket(6), 5);
        assert_eq!(size_bucket(u64::MAX), 127);
        let mut sizes: Vec<u64> = (0..64)
            .flat_map(|b| {
                let p = 1u64 << b;
                [p - 1, p, p + 1, p + p / 2 - 1, p + p / 2]
            })
            .collect();
        sizes.sort_unstable();
        let mut last = 0;
        for s in sizes {
            let k = size_bucket(s);
            assert!(k >= last, "{s}");
            last = k;
            assert!(bucket_floor(k) <= s, "{s}");
            assert_eq!(size_bucket(bucket_floor(k)), k, "{s}");
            if k < 127 {
                assert!(bucket_floor(k + 1) > s, "{s}");
            }
        }
    }

    #[test]
    fn file_docs_carry_category_and_pieces_root() {
        use tantivy::schema::Value;
        let mut t = tree(&[
            ("Show/E01.MKV", 1 << 30, FileAttrs::empty()),
            ("Show/README", 10, FileAttrs::empty()),
            ("Show/subs.srt", 10, FileAttrs::empty()),
        ]);
        t.files[0].pieces_root = Some([7; 32]);
        let docs = file_docs(1, &t);
        let cats: Vec<_> = docs.iter().map(|d| d.category).collect();
        assert_eq!(cats, [Category::Video, Category::Other, Category::Subtitle]);
        assert_eq!(docs[0].pieces_root, Some([7; 32]));
        assert_eq!(docs[1].pieces_root, None);

        // Written as a keyword and 32 bytes; the root only when present.
        let (_, f) = schema();
        let d = docs[0].to_document(&f);
        let text = |field| {
            d.get_first(field)
                .and_then(|v| v.as_str().map(str::to_owned))
        };
        assert_eq!(text(f.category).as_deref(), Some("video"));
        assert_eq!(
            d.get_first(f.pieces_root).and_then(|v| v.as_bytes()),
            Some(&[7u8; 32][..])
        );
        assert!(docs[1].to_document(&f).get_first(f.pieces_root).is_none());
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

    /// The torrent's bucket terms come from exactly the files
    /// `file_docs` indexes: in a small torrent every real file, in a
    /// large pack only the big and media ones, never padding, hidden
    /// files or a file without a path; a file without an extension has a
    /// bucket but no pair.
    #[test]
    fn covered_buckets_are_the_file_docs() {
        let want = |t: &FileTree| {
            let docs = file_docs(1, t);
            let mut b: Vec<u64> = docs.iter().map(|d| size_code(d.size)).collect();
            let mut p: Vec<String> = docs
                .iter()
                .filter_map(|d| {
                    d.ext
                        .as_ref()
                        .map(|e| ext_bucket_term(e, size_code(d.size)))
                })
                .collect();
            b.sort_unstable();
            b.dedup();
            p.sort_unstable();
            p.dedup();
            (b, p)
        };
        let small = tree(&[
            ("a/movie.mkv", 4 << 30, FileAttrs::empty()),
            ("a/movie.srt", 40_000, FileAttrs::empty()),
            ("README", 900, FileAttrs::empty()),
            ("pad", 1 << 20, FileAttrs::PADDING),
            ("hidden.exe", 5 << 20, FileAttrs::HIDDEN),
            ("", 7, FileAttrs::empty()),
        ]);
        let (b, p) = covered_buckets(&small);
        assert_eq!((b.clone(), p.clone()), want(&small));
        assert!(b.contains(&size_code(900)), "README has a bucket");
        assert!(!p.iter().any(|t| t.starts_with("exe:")), "{p:?}");
        let mut files: Vec<(String, u64)> = (0..SMART_ALL_FILES + 5)
            .map(|i| (format!("pics/{i}.jpg"), 100_000 + i as u64))
            .collect();
        files.push(("pics/big.jpg".into(), SMART_MIN_SIZE));
        files.push(("music/a.mp3".into(), 4 << 20));
        files.push(("info.nfo".into(), 2_000));
        files.push(("noext".into(), 3_000));
        let refs: Vec<(&str, u64, FileAttrs)> = files
            .iter()
            .map(|(p, s)| (p.as_str(), *s, FileAttrs::empty()))
            .collect();
        let pack = tree(&refs);
        let (b, p) = covered_buckets(&pack);
        assert_eq!((b.clone(), p.clone()), want(&pack));
        assert_eq!(
            p,
            [
                ext_bucket_term("jpg", size_code(SMART_MIN_SIZE)),
                ext_bucket_term("mp3", size_code(4 << 20)),
            ]
        );
    }

    /// A strict lower bound on a floor, or an upper bound on one, is
    /// whole codes, binary (`1GiB`) or decimal-looking (`100MiB`); any
    /// other edge is left to the file index.
    #[test]
    fn floors_split_ranges_into_whole_codes() {
        assert_eq!(size_code(0), 0);
        assert_eq!(size_code(1), 2);
        let last = code_floor_count() as u64 * 2 - 1;
        assert_eq!(size_code(u64::MAX), last);
        for g in [1u64 << 30, 100 << 20, 500 << 20, 3 << 29, 5 << 40] {
            let c = size_code(g);
            assert_eq!(c % 2, 0, "{g} is a floor");
            assert_eq!(size_code(g + 1), c + 1);
            assert_eq!(size_code(g - 1), c - 1);
            // `size:>g`: every code from g's upper one on, no file check.
            let (codes, left) = code_plan(g + 1, u64::MAX);
            assert!(left.is_empty(), "{g}");
            assert_eq!(codes[0], c + 1);
            assert_eq!(*codes.last().unwrap(), last);
            // `size:<=g`: up to g's floor code.
            let (codes, left) = code_plan(0, g);
            assert!(left.is_empty(), "{g}");
            assert_eq!(codes.last(), Some(&c));
        }
        // `size:>700MiB`: inside a code, left to the file index up to
        // the next floor (768 MiB).
        let m = 700u64 << 20;
        let (codes, left) = code_plan(m + 1, u64::MAX);
        assert_eq!(left, [(m + 1, (768 << 20) - 1)]);
        assert_eq!(codes[0], size_code(768 << 20));
        // Inside one code: an edge only; empty: nothing.
        assert_eq!(code_plan(m + 1, m + 2), (vec![], vec![(m + 1, m + 2)]));
        assert_eq!(code_plan(5, 4), (vec![], vec![]));
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
        for id in 0..600u64 {
            let want = (10..200).contains(&id) || id == 3 || id == 500;
            assert_eq!(union.contains(id), want, "{id}");
            assert_eq!(set.contains(id), want, "{id}");
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
                    for id in 0..u.max_value() + 64 {
                        if u.contains(id) {
                            got.push(id);
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
        for (max_doc, piece) in [
            (0, 10),
            (1, 10),
            (5, 1),
            (10, 10),
            (11, 10),
            (35, 10),
            (1000, 10),
        ] {
            let got: Vec<_> = pieces(max_doc, piece).collect();
            assert!(!got.is_empty() && got.len() <= MAX_PIECES as usize);
            assert_eq!(got[0].0, 0);
            assert_eq!(got.last().unwrap().1, max_doc);
            assert!(got.windows(2).all(|w| w[0].1 == w[1].0));
            assert!(max_doc == 0 || got.iter().all(|(lo, hi)| lo < hi));
        }
        assert_eq!(pieces(11, 10).collect::<Vec<_>>(), [(0, 6), (6, 11)]);
        assert_eq!(pieces(5, 10).count(), 1);
        // Not four: the last would be empty.
        assert_eq!(pieces(5, 1).collect::<Vec<_>>(), [(0, 2), (2, 4), (4, 5)]);
    }

    /// A bare `size:` range compiles to [`SizeRange`], which a scan in
    /// pieces reads from the column; an empty one matches nothing.
    #[test]
    fn a_bare_size_range_is_read_from_the_column() {
        let (_, f) = schema();
        let range = |lo, hi| Node::Range {
            field: NumField::Size,
            lo,
            hi,
        };
        let q = compile(&range(Some(5), None), &f).unwrap();
        let r = q.downcast_ref::<SizeRange>().expect("a SizeRange");
        assert_eq!(r.range, 5..=u64::MAX);
        let q = compile(&range(None, Some(9)), &f).unwrap();
        assert_eq!(q.downcast_ref::<SizeRange>().unwrap().range, 0..=9);
        let q = compile(&range(Some(9), Some(5)), &f).unwrap();
        assert!(q.downcast_ref::<EmptyQuery>().is_some());
    }

    /// Pieces joined in every pairing of forms give the part one
    /// gather of all their ids gives, overlaps and repeats included.
    #[test]
    fn joined_gathers_equal_one_gather() {
        let bits = |ids: &[u64]| {
            let mut g = Gather::Bits {
                lo: 100,
                words: vec![0; 4],
            };
            ids.iter().for_each(|id| g.push(*id));
            g
        };
        let list = |ids: &[u64]| {
            let mut g = Gather::List(Vec::new());
            ids.iter().for_each(|id| g.push(*id));
            g
        };
        let (a, b) = ([100u64, 101, 164, 200], [101u64, 150, 150, 355]);
        let mut all: Vec<u64> = a.iter().chain(&b).copied().collect();
        all.sort_unstable();
        all.dedup();
        let want = IdPart::from_sorted(all);
        for (x, y) in [
            (bits(&a), bits(&b)),
            (list(&a), list(&b)),
            (bits(&a), list(&b)),
            (list(&a), bits(&b)),
        ] {
            assert_eq!(x.join(y).finish(), want);
        }
    }

    /// Scanned in pieces (two to four a segment, ending inside a
    /// torrent's files), a clause finds what a whole-segment scan finds:
    /// by its scorer and by the column (a bare `size:` range), gathered
    /// as bitsets and as lists.
    #[test]
    fn a_scan_in_pieces_finds_what_a_whole_scan_does() {
        let mut builder = Schema::builder();
        let torrent = builder.add_u64_field(
            "torrent",
            NumericOptions::default().set_indexed().set_fast(),
        );
        let size = builder.add_u64_field("size", FAST);
        let mut index = tantivy::Index::create_in_ram(builder.build());
        index.set_multithread_executor(3).unwrap();
        let mut writer = index
            .writer_with_num_threads::<TantivyDocument>(1, 15 << 20)
            .unwrap();
        writer.set_merge_policy(Box::new(tantivy::indexer::NoMergePolicy));
        // 1,000 and 1,500 files.
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
        // No `size_bucket` here: the column path.
        let sized = |lo: u64, hi: u64| SizeRange {
            field: size,
            bucket: ABSENT,
            range: lo..=hi,
        };
        let few: Q = Box::new(BooleanQuery::new(
            [3u64, 401, 777]
                .iter()
                .map(|id| {
                    let t = TermQuery::new(
                        Term::from_field_u64(torrent, *id),
                        IndexRecordOption::Basic,
                    );
                    (Occur::Should, Box::new(t) as Q)
                })
                .collect(),
        ));
        let queries: [(&str, Q, Option<bool>); 5] = [
            ("all", Box::new(AllQuery), Some(true)),
            ("big", Box::new(sized(3_000, u64::MAX)), Some(true)),
            ("band", Box::new(sized(10, 40)), Some(true)),
            // The scorer path for a range, under a boolean. Its form
            // follows the intersection's size hint, which is 0 in a
            // segment the range can't match, so it isn't checked.
            (
                "band and all",
                Box::new(BooleanQuery::new(vec![
                    (Occur::Must, Box::new(sized(2_000, 2_500)) as Q),
                    (Occur::Must, Box::new(AllQuery) as Q),
                ])),
                None,
            ),
            ("few", few, Some(false)),
        ];
        for (key, q, dense) in &queries {
            let whole =
                torrent_ids(&searcher, q.as_ref(), key, &FileSetCache::default(), &token).unwrap();
            assert!(whole.parts.iter().any(|p| !p.is_empty()), "{key}");
            assert!(!whole.contains(401));
            let mut forms = Vec::new();
            for seg in searcher.segment_readers() {
                let w = q
                    .weight(EnableScoring::disabled_from_searcher(&searcher))
                    .unwrap();
                let source = match q.downcast_ref::<SizeRange>() {
                    Some(r) => Source::Size(r),
                    None => Source::Weight(w.as_ref()),
                };
                let g = scan_segment(
                    &source,
                    seg,
                    &seg.fast_fields().u64("torrent").unwrap(),
                    &SegColumns::default(),
                    (0, seg.max_doc()),
                    &token,
                    &AtomicBool::new(false),
                )
                .unwrap();
                forms.push(matches!(g, Gather::Bits { .. }));
            }
            assert!(
                dense.is_none_or(|d| forms.iter().all(|b| *b == d)),
                "{key}: {forms:?}"
            );
            // 1 cuts each segment in four, 400 in three and four, 500 in
            // two and three, and `PIECE_DOCS` leaves them whole.
            for piece_docs in [1, 400, 500, PIECE_DOCS] {
                let cache = FileSetCache::default();
                let cut =
                    torrent_ids_in_pieces(&searcher, q.as_ref(), key, &cache, &token, piece_docs)
                        .unwrap();
                assert_eq!(cut, whole, "{key} in pieces of {piece_docs}");
            }
        }
    }

    /// A query `TermUnion::of` doesn't recognise, so the scan runs its
    /// scorer: the reference for the term-union path.
    #[derive(Debug, Clone)]
    struct Opaque(Arc<dyn Query>);

    impl Query for Opaque {
        fn weight(&self, scoring: EnableScoring<'_>) -> tantivy::Result<Box<dyn Weight>> {
            self.0.weight(scoring)
        }
    }

    /// A union of terms, alone or less some exclusions, read term by
    /// term finds what its scorer does: whole and in pieces, across
    /// segments, with a deleted torrent and a term absent from a
    /// segment. Shapes it mustn't take (a second required clause, a
    /// `Should` beside a `Must`) keep the scorer.
    #[test]
    fn a_term_union_scan_finds_what_its_scorer_does() {
        let mut builder = Schema::builder();
        let torrent = builder.add_u64_field(
            "torrent",
            NumericOptions::default().set_indexed().set_fast(),
        );
        let ext = builder.add_text_field("ext", tantivy::schema::STRING);
        let mut index = tantivy::Index::create_in_ram(builder.build());
        index.set_multithread_executor(3).unwrap();
        let mut writer = index
            .writer_with_num_threads::<TantivyDocument>(1, 15 << 20)
            .unwrap();
        writer.set_merge_policy(Box::new(tantivy::indexer::NoMergePolicy));
        let exts = ["mkv", "mp4", "avi", "srt", "nfo"];
        // `avi` only in the first segment.
        for ids in [0..400u64, 400..1_000] {
            for id in ids.clone() {
                for f in 0..(1 + id % 4) {
                    let e = exts[((id * 3 + f) % 5) as usize];
                    if e == "avi" && id >= 400 {
                        continue;
                    }
                    let mut doc = TantivyDocument::new();
                    doc.add_u64(torrent, id);
                    doc.add_text(ext, e);
                    writer.add_document(doc).unwrap();
                }
            }
            writer.commit().unwrap();
        }
        writer.delete_term(Term::from_field_u64(torrent, 401));
        writer.commit().unwrap();
        let searcher = index.reader().unwrap().searcher();
        let token = crate::CancelToken::new();
        let t = |e: &str| -> Q {
            Box::new(TermQuery::new(
                Term::from_field_text(ext, e),
                IndexRecordOption::Basic,
            ))
        };
        let any = |es: &[&str]| -> Q {
            Box::new(BooleanQuery::new(
                es.iter().map(|e| (Occur::Should, t(e))).collect(),
            ))
        };
        let queries: Vec<(Q, bool)> = vec![
            (t("mkv"), true),
            (any(&["mkv", "mp4", "avi"]), true),
            (any(&["avi", "nothing"]), true),
            (
                Box::new(BooleanQuery::new(vec![
                    (Occur::Must, any(&["mkv", "mp4", "avi"])),
                    (Occur::MustNot, t("srt")),
                    (Occur::MustNot, t("nfo")),
                ])),
                true,
            ),
            (
                Box::new(BooleanQuery::new(vec![
                    (Occur::Must, any(&["mkv", "avi"])),
                    (Occur::Must, t("mp4")),
                ])),
                false,
            ),
            (
                Box::new(BooleanQuery::new(vec![
                    (Occur::Must, t("mkv")),
                    (Occur::Should, t("mp4")),
                ])),
                false,
            ),
        ];
        for (i, (q, union)) in queries.iter().enumerate() {
            assert_eq!(TermUnion::of(q.as_ref()).is_some(), *union, "query {i}");
            let opaque = Opaque(Arc::from(q.box_clone()));
            let want =
                torrent_ids(&searcher, &opaque, "o", &FileSetCache::default(), &token).unwrap();
            assert!(!want.contains(401));
            for piece_docs in [1, 300, 500, PIECE_DOCS] {
                let got = torrent_ids_in_pieces(
                    &searcher,
                    q.as_ref(),
                    "q",
                    &FileSetCache::default(),
                    &token,
                    piece_docs,
                )
                .unwrap();
                assert_eq!(got, want, "query {i} in pieces of {piece_docs}");
            }
        }
    }

    /// The join reads the torrent index's id column a block at a time:
    /// across block edges, with a partial last block, it marks exactly
    /// the documents whose id is in the set.
    #[test]
    fn the_join_marks_each_document_whose_id_is_in_the_set() {
        let mut builder = Schema::builder();
        let id = builder.add_u64_field("id", FAST);
        let index = tantivy::Index::create_in_ram(builder.build());
        let mut writer = index
            .writer_with_num_threads::<TantivyDocument>(1, 15 << 20)
            .unwrap();
        // 2,500 documents (two full blocks and a partial one), ids out of
        // order, as a merged segment's runs are.
        let ids: Vec<u64> = (0..2_500u64).map(|d| (d * 7_919) % 10_007).collect();
        for i in &ids {
            let mut doc = TantivyDocument::new();
            doc.add_u64(id, *i);
            writer.add_document(doc).unwrap();
        }
        writer.commit().unwrap();
        let searcher = index.reader().unwrap().searcher();
        let seg = &searcher.segment_readers()[0];
        // The block path is the one under test: every document has an id.
        assert!(matches!(
            seg.fast_fields().u64("id").unwrap().index,
            tantivy::columnar::ColumnIndex::Full
        ));
        let wanted: Vec<u64> = (0..10_007).filter(|i| i % 3 == 0).collect();
        let weight = ids_weight(vec![wanted]);
        let got = weight.matches(seg).unwrap();
        for (doc, i) in ids.iter().enumerate() {
            assert_eq!(got.contains(doc as u32), i % 3 == 0, "doc {doc} id {i}");
        }
        assert_eq!(got.len(), ids.iter().filter(|i| *i % 3 == 0).count());
        // An empty set marks nothing.
        let none = IdsWeight(Arc::default(), crate::filter::SegmentSets::default());
        assert_eq!(none.matches(seg).unwrap().len(), 0);
    }

    fn ids_weight(parts: Vec<Vec<u64>>) -> IdsWeight {
        let set = IdSet {
            parts: parts
                .into_iter()
                .map(|p| Arc::new(IdPart::from_sorted(p)))
                .collect(),
            union: OnceLock::new(),
        };
        IdsWeight(Arc::new(set), crate::filter::SegmentSets::default())
    }

    /// Ids past `u32::MAX` have no union bitset: the join checks each
    /// part instead, on the block path, and still marks exactly the
    /// documents in the set.
    #[test]
    fn the_join_handles_ids_past_u32() {
        let mut builder = Schema::builder();
        let id = builder.add_u64_field("id", FAST);
        let index = tantivy::Index::create_in_ram(builder.build());
        let mut writer = index
            .writer_with_num_threads::<TantivyDocument>(1, 15 << 20)
            .unwrap();
        let big = u64::from(u32::MAX);
        let ids: Vec<u64> = (0..1_500u64)
            .map(|d| if d % 2 == 0 { d } else { big - 2 + d })
            .collect();
        for i in &ids {
            let mut doc = TantivyDocument::new();
            doc.add_u64(id, *i);
            writer.add_document(doc).unwrap();
        }
        writer.commit().unwrap();
        let searcher = index.reader().unwrap().searcher();
        let seg = &searcher.segment_readers()[0];
        assert!(matches!(
            seg.fast_fields().u64("id").unwrap().index,
            tantivy::columnar::ColumnIndex::Full
        ));
        let mut wanted: Vec<u64> = ids.iter().copied().filter(|i| i % 3 == 0).collect();
        wanted.sort_unstable();
        // Two parts, as two file-index segments give.
        let (a, b) = wanted.split_at(wanted.len() / 2);
        let weight = ids_weight(vec![a.to_vec(), b.to_vec()]);
        assert!(weight.0.union().is_none());
        let got = weight.matches(seg).unwrap();
        for (doc, i) in ids.iter().enumerate() {
            assert_eq!(got.contains(doc as u32), i % 3 == 0, "doc {doc} id {i}");
        }
    }

    /// A column that some documents lack (never written by the indexer,
    /// which gives every torrent an id) takes the per-document path and
    /// leaves the documents without one unmarked.
    #[test]
    fn the_join_handles_documents_without_an_id() {
        let mut builder = Schema::builder();
        let id = builder.add_u64_field("id", FAST);
        let other = builder.add_u64_field("other", FAST);
        let index = tantivy::Index::create_in_ram(builder.build());
        let mut writer = index
            .writer_with_num_threads::<TantivyDocument>(1, 15 << 20)
            .unwrap();
        // Every fifth document has no id.
        let ids: Vec<Option<u64>> = (0..2_100u64)
            .map(|d| (d % 5 != 0).then_some(d * 3))
            .collect();
        for i in &ids {
            let mut doc = TantivyDocument::new();
            doc.add_u64(other, 1);
            if let Some(i) = i {
                doc.add_u64(id, *i);
            }
            writer.add_document(doc).unwrap();
        }
        writer.commit().unwrap();
        let searcher = index.reader().unwrap().searcher();
        let seg = &searcher.segment_readers()[0];
        assert!(!matches!(
            seg.fast_fields().u64("id").unwrap().index,
            tantivy::columnar::ColumnIndex::Full
        ));
        // Every id, so only a missing one leaves a document unmarked.
        let weight = ids_weight(vec![(0..2_100 * 3).collect()]);
        let got = weight.matches(seg).unwrap();
        for (doc, i) in ids.iter().enumerate() {
            assert_eq!(got.contains(doc as u32), i.is_some(), "doc {doc} id {i:?}");
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

#[cfg(test)]
mod bucket_props {
    use super::*;
    use proptest::prelude::*;

    /// Sizes that sit on, just below and just above bucket floors, the
    /// extremes, and anything else.
    fn size() -> impl Strategy<Value = u64> {
        prop_oneof![
            (0u64..128, -2i64..=2).prop_map(|(b, d)| bucket_floor(b).saturating_add_signed(d)),
            (0..code_floor_count(), -2i64..=2)
                .prop_map(|(i, d)| code_floor(i).unwrap().saturating_add_signed(d)),
            Just(0u64),
            Just(u64::MAX),
            0u64..10_000,
            any::<u64>(),
        ]
    }

    /// The file index's own schema, in RAM, a segment per entry of
    /// `segments` (a torrent's file sizes each). In segments listed in
    /// `unbucketed`, every other file is written without its bucket, as
    /// a document a backfill hasn't reached.
    fn index(segments: &[Vec<Vec<u64>>], unbucketed: &[usize]) -> (Searcher, FileFields) {
        let (schema, f) = schema();
        let mut index = ram(schema);
        index.set_multithread_executor(2).unwrap();
        let mut writer = index
            .writer_with_num_threads::<TantivyDocument>(1, 15 << 20)
            .unwrap();
        writer.set_merge_policy(Box::new(tantivy::indexer::NoMergePolicy));
        let mut torrent = 0u64;
        for (s, seg) in segments.iter().enumerate() {
            for files in seg {
                torrent += 1;
                for (i, size) in files.iter().enumerate() {
                    let doc = FileDoc {
                        torrent,
                        index: i as u64,
                        name: "f.bin".into(),
                        dirs: Vec::new(),
                        // Two extensions, so a conjunction has a clause
                        // besides the size to lead.
                        ext: Some(if size % 3 == 0 { "dat" } else { "bin" }.into()),
                        size: *size,
                        category: Category::Other,
                        pieces_root: None,
                    };
                    let mut d = doc.to_document(&f);
                    if unbucketed.contains(&s) && i % 2 == 0 {
                        d = TantivyDocument::new();
                        d.add_u64(f.torrent, torrent);
                        d.add_u64(f.index, i as u64);
                        d.add_u64(f.size, *size);
                        d.add_text(f.ext, if size % 3 == 0 { "dat" } else { "bin" });
                    }
                    writer.add_document(d).unwrap();
                }
            }
            writer.commit().unwrap();
        }
        // Torrent 2's files are deleted (still in the postings).
        writer.delete_term(Term::from_field_u64(f.torrent, 2));
        writer.commit().unwrap();
        (index.reader().unwrap().searcher(), f)
    }

    fn ram(schema: Schema) -> tantivy::Index {
        let index = tantivy::Index::create_in_ram(schema);
        index
            .tokenizers()
            .register(NAME_TOKENIZER, tokenizer::NameTokenizer::default());
        index
    }

    fn ids(searcher: &Searcher, q: &dyn Query, piece_docs: DocId) -> Vec<u64> {
        let set = torrent_ids_in_pieces(
            searcher,
            q,
            "q",
            &FileSetCache::default(),
            &crate::CancelToken::new(),
            piece_docs,
        )
        .unwrap();
        let mut out = Vec::new();
        for p in &set.parts {
            IdPart::for_each(p, |id| out.push(id));
        }
        out.sort_unstable();
        out.dedup();
        out
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(512))]

        /// Every size has one code, and codes are ordered: a larger size
        /// never has a smaller code, the code's range holds the size,
        /// and a floor starts its own code (X-search-files-6).
        #[test]
        fn codes_are_ordered_and_hold_their_sizes(a in size(), b in size()) {
            let (a, b) = (a.min(b), a.max(b));
            prop_assert!(size_code(a) <= size_code(b));
            for s in [a, b] {
                let (lo, hi) = code_range(size_code(s)).expect("a size's code holds it");
                prop_assert!(lo <= s && s <= hi, "{} outside {}..={}", s, lo, hi);
            }
        }

        /// The torrent's codes and the file index's edges answer a range
        /// exactly: every size inside it is in a whole code or an edge,
        /// none outside is in either, an edge is never also a whole code,
        /// and edges are inside the range, each within one code and not
        /// all of it, at most two (X-search-files-5, -6).
        #[test]
        fn a_code_plan_covers_exactly_its_range(lo in size(), hi in size(), probes in prop::collection::vec(size(), 0..40)) {
            let (codes, edges) = code_plan(lo, hi);
            prop_assert!(codes.windows(2).all(|w| w[0] < w[1]));
            prop_assert!(edges.len() <= 2);
            for &(a, b) in &edges {
                prop_assert!(lo <= a && a <= b && b <= hi, "{a}..={b} outside {lo}..={hi}");
                prop_assert_eq!(size_code(a), size_code(b));
                prop_assert_ne!(code_range(size_code(a)), Some((a, b)), "a whole code as an edge");
            }
            let near = |x: u64| [x.saturating_sub(1), x, x.saturating_add(1)];
            let mut sizes: Vec<u64> = probes;
            sizes.extend(near(lo).into_iter().chain(near(hi)));
            for &(a, b) in &edges {
                sizes.extend(near(a).into_iter().chain(near(b)));
            }
            for &c in &codes {
                let (a, b) = code_range(c).expect("a whole code holds sizes");
                sizes.extend(near(a).into_iter().chain(near(b)));
            }
            for s in sizes {
                let coded = codes.binary_search(&size_code(s)).is_ok();
                let edged = edges.iter().any(|&(a, b)| (a..=b).contains(&s));
                prop_assert!(!(coded && edged), "{s} in a code and an edge");
                prop_assert_eq!(coded || edged, lo <= s && s <= hi, "{} for {}..={}", s, lo, hi);
            }
        }
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(64))]

        /// Bucketed and scanned, bare and under a boolean, whole and in
        /// pieces, a size range finds exactly the torrents a scan of
        /// every size does, and a segment with files lacking a bucket
        /// falls back to the scan.
        #[test]
        fn bucketed_and_scanned_results_agree(
            segments in prop::collection::vec(
                prop::collection::vec(prop::collection::vec(size(), 1..4), 1..40),
                1..4,
            ),
            unbucketed in prop::collection::vec(0usize..4, 0..2),
            lo in prop::option::of(size()),
            hi in prop::option::of(size()),
            piece_docs in prop_oneof![Just(PIECE_DOCS), 1u32..20],
        ) {
            let (searcher, f) = index(&segments, &unbucketed);
            let r = lo.unwrap_or(0)..=hi.unwrap_or(u64::MAX);
            let want: Vec<u64> = segments
                .iter()
                .flatten()
                .enumerate()
                .map(|(i, files)| (i as u64 + 1, files))
                .filter(|(id, files)| *id != 2 && files.iter().any(|s| r.contains(s)))
                .map(|(id, _)| id)
                .collect();
            let node = Node::Range { field: NumField::Size, lo, hi };
            let q = compile(&node, &f).unwrap();
            let scan = SizeRange { field: f.size, bucket: ABSENT, range: r.clone() };
            let nested = |q: Q| -> Q {
                Box::new(BooleanQuery::new(vec![
                    (Occur::Must, q),
                    (Occur::Must, Box::new(AllQuery) as Q),
                ]))
            };
            prop_assert_eq!(&ids(&searcher, q.as_ref(), piece_docs), &want, "bucketed");
            prop_assert_eq!(&ids(&searcher, &scan, piece_docs), &want, "scanned");
            prop_assert_eq!(&ids(&searcher, nested(q.box_clone()).as_ref(), piece_docs), &want, "nested");
            prop_assert_eq!(
                &ids(&searcher, nested(Box::new(scan.clone())).as_ref(), piece_docs),
                &want,
                "nested scan"
            );
            // A file outside the range: the complement, per file.
            let outside: Vec<u64> = segments
                .iter()
                .flatten()
                .enumerate()
                .map(|(i, files)| (i as u64 + 1, files))
                .filter(|(id, files)| *id != 2 && files.iter().any(|s| !r.contains(s)))
                .map(|(id, _)| id)
                .collect();
            // Beside an extension: compiled, and with the size checked in
            // the column or intersected with the buckets, whatever the
            // estimate says.
            let with_ext: Vec<u64> = segments
                .iter()
                .flatten()
                .enumerate()
                .map(|(i, files)| (i as u64 + 1, files))
                .filter(|(id, files)| *id != 2 && files.iter().any(|s| s % 3 != 0 && r.contains(s)))
                .map(|(id, _)| id)
                .collect();
            let ext = Node::Ext { any: vec!["bin".into()] };
            let and = Node::And { of: vec![ext.clone(), node.clone()] };
            let compiled = compile(&and, &f).unwrap();
            // An empty range compiles to nothing to check.
            prop_assert_eq!(
                compiled.downcast_ref::<SizeChecked>().is_some(),
                q.downcast_ref::<SizeRange>().is_some()
            );
            prop_assert_eq!(&ids(&searcher, compiled.as_ref(), piece_docs), &with_ext, "with ext");
            for ratio in [0, u64::MAX] {
                let ext_q = compile_node(&ext, &f).unwrap();
                let range = q.downcast_ref::<SizeRange>().cloned();
                let Some(range) = range else { break };
                let forced = SizeChecked {
                    rest: Box::new(BooleanQuery::new(vec![(Occur::Must, ext_q.box_clone())])),
                    ranges: vec![range.clone()],
                    whole: Box::new(BooleanQuery::new(vec![
                        (Occur::Must, ext_q),
                        (Occur::Must, Box::new(range) as Q),
                    ])),
                    ratio,
                };
                prop_assert_eq!(&ids(&searcher, &forced, piece_docs), &with_ext, "ratio {}", ratio);
                // Its scorer (not the scan's own reading) wherever it's
                // nested: negated, in an alternative, beside a torrent's
                // term as matched files ask, and collected whole, which
                // takes a short buffer for the end.
                let not_fit: Vec<u64> = segments
                    .iter()
                    .flatten()
                    .enumerate()
                    .map(|(i, files)| (i as u64 + 1, files))
                    .filter(|(id, files)| {
                        *id != 2 && files.iter().any(|s| s % 3 == 0 || !r.contains(s))
                    })
                    .map(|(id, _)| id)
                    .collect();
                let negated = BooleanQuery::new(vec![
                    (Occur::Must, Box::new(AllQuery) as Q),
                    (Occur::MustNot, Box::new(forced.clone()) as Q),
                ]);
                prop_assert_eq!(&ids(&searcher, &negated, piece_docs), &not_fit, "not, ratio {}", ratio);
                let mut or_one = with_ext.clone();
                or_one.push(1);
                or_one.sort_unstable();
                or_one.dedup();
                let or = BooleanQuery::new(vec![
                    (Occur::Should, Box::new(forced.clone()) as Q),
                    (
                        Occur::Should,
                        Box::new(TermQuery::new(
                            Term::from_field_u64(f.torrent, 1),
                            IndexRecordOption::Basic,
                        )) as Q,
                    ),
                ]);
                prop_assert_eq!(&ids(&searcher, &or, piece_docs), &or_one, "or, ratio {}", ratio);
                let fitting: Vec<(u64, u64)> = segments
                    .iter()
                    .flatten()
                    .enumerate()
                    .flat_map(|(i, files)| {
                        files.iter().enumerate().map(move |(at, s)| (i as u64 + 1, at as u64, *s))
                    })
                    .filter(|(id, _, s)| *id != 2 && s % 3 != 0 && r.contains(s))
                    .map(|(id, at, _)| (id, at))
                    .collect();
                prop_assert_eq!(
                    searcher.search(&forced, &tantivy::collector::Count).unwrap(),
                    fitting.len(),
                    "count, ratio {}", ratio
                );
                let first = BooleanQuery::new(vec![
                    (Occur::Must, Box::new(forced.clone()) as Q),
                    (
                        Occur::Must,
                        Box::new(TermQuery::new(
                            Term::from_field_u64(f.torrent, 1),
                            IndexRecordOption::Basic,
                        )) as Q,
                    ),
                ]);
                let want_files: Vec<u64> =
                    fitting.iter().filter(|(id, _)| *id == 1).map(|(_, at)| *at).collect();
                prop_assert_eq!(
                    searcher.search(&first, &FileIndexes).unwrap(),
                    want_files,
                    "matched files, ratio {}", ratio
                );
            }
            let not = compile(&Node::Not { of: Box::new(node) }, &f).unwrap();
            prop_assert_eq!(&ids(&searcher, not.as_ref(), piece_docs), &outside, "negated");
        }
    }

    /// Which buckets a range reads, and which of them it checks.
    #[test]
    fn only_edge_buckets_are_checked() {
        let r = |lo: u64, hi: u64| SizeRange {
            field: ABSENT,
            bucket: ABSENT,
            range: lo..=hi,
        };
        let got = |s: SizeRange| s.buckets().collect::<Vec<_>>();
        // [4096, 8191] is buckets 24 and 25 whole.
        assert_eq!(got(r(4096, 8191)), [(24, false), (25, false)]);
        // One past each end: both edges partial, the middle whole.
        assert_eq!(
            got(r(4095, 8192)),
            [(23, true), (24, false), (25, false), (26, true)]
        );
        // Inside one bucket.
        assert_eq!(got(r(4097, 4098)), [(24, true)]);
        // Open at the top: the last bucket ends at the largest size.
        let top: Vec<_> = got(r(1 << 62, u64::MAX));
        assert_eq!(top.first(), Some(&(124, false)));
        assert_eq!(top.last(), Some(&(127, false)));
        assert!(top.iter().all(|(_, edge)| !edge));
        assert_eq!(got(r(0, 0)), [(0, false)]);
        assert_eq!(got(r(0, u64::MAX)).len(), 128);
        assert!(got(r(0, u64::MAX)).iter().all(|(_, e)| !e));
    }

    /// The bare path never reads the column for a bucket wholly inside:
    /// a segment whose sizes column disagrees with its buckets shows
    /// which was read.
    #[test]
    fn inner_buckets_come_from_postings() {
        let (schema, f) = schema();
        let index = ram(schema);
        let mut writer = index
            .writer_with_num_threads::<TantivyDocument>(1, 15 << 20)
            .unwrap();
        // Bucket 25 ([6144, 8191]) but a stored size of 1.
        let mut d = TantivyDocument::new();
        d.add_u64(f.torrent, 1);
        d.add_u64(f.index, 0);
        d.add_u64(f.size, 1);
        d.add_u64(f.size_bucket, 25);
        writer.add_document(d).unwrap();
        writer.commit().unwrap();
        let searcher = index.reader().unwrap().searcher();
        let bucketed = SizeRange {
            field: f.size,
            bucket: f.size_bucket,
            range: 4096..=8191,
        };
        assert_eq!(ids(&searcher, &bucketed, PIECE_DOCS), [1]);
        let scan = SizeRange {
            bucket: ABSENT,
            ..bucketed.clone()
        };
        assert!(ids(&searcher, &scan, PIECE_DOCS).is_empty());
        // Under a boolean, the same: the nested weight reads the bucket.
        let nested = |q: Q| -> Q {
            Box::new(BooleanQuery::new(vec![
                (Occur::Must, q),
                (Occur::Must, Box::new(AllQuery) as Q),
            ]))
        };
        assert_eq!(
            ids(&searcher, nested(Box::new(bucketed)).as_ref(), PIECE_DOCS),
            [1]
        );
        assert!(ids(&searcher, nested(Box::new(scan)).as_ref(), PIECE_DOCS).is_empty());
    }

    /// An edge bucket explains a file before its first match (no seek
    /// back) and its match: 4,097 and 5,000 bytes share bucket 24, which
    /// `4500..` only partly covers.
    #[test]
    fn an_edge_bucket_explains_without_seeking_back() {
        let (searcher, f) = index(&[vec![vec![4097], vec![5000]]], &[]);
        let seg = &searcher.segment_readers()[0];
        let edge = EdgeBucket {
            term: Term::from_field_u64(f.size_bucket, size_bucket(4500)),
            size: f.size,
            range: 4500..=8191,
        };
        assert!(Weight::explain(&edge, seg, 0).is_err());
        assert!(Weight::explain(&edge, seg, 1).is_ok());
    }

    /// A weight that counts the scorers built from it.
    struct Counting(Box<dyn Weight>, Arc<std::sync::atomic::AtomicUsize>);

    impl Weight for Counting {
        fn scorer(&self, seg: &SegmentReader, boost: Score) -> tantivy::Result<Box<dyn Scorer>> {
            self.1.fetch_add(1, Ordering::Relaxed);
            self.0.scorer(seg, boost)
        }

        fn explain(&self, seg: &SegmentReader, doc: DocId) -> tantivy::Result<Explanation> {
            self.0.explain(seg, doc)
        }
    }

    /// Where the ranges' buckets hold far fewer files than the other
    /// clauses' terms, the plain conjunction runs, and the other clauses'
    /// own scorer is never built (#468's review P2): their files are
    /// counted from the term dictionary. Where the check runs, it's built
    /// once a segment and piece. The results are the same either way.
    #[test]
    fn the_plain_conjunction_builds_no_scorer_for_the_check() {
        // Every file is `bin`; one in fifty is big.
        let sizes: Vec<Vec<u64>> = (0..2_000u64)
            .map(|i| vec![if i % 50 == 0 { 1 << 30 } else { 1_000 + i }])
            .collect();
        let (searcher, f) = index(&[sizes], &[]);
        let ext = Node::Ext {
            any: vec!["bin".into()],
        };
        let checked = |ratio: u64| {
            let ext_q = compile_node(&ext, &f).unwrap();
            let range = SizeRange {
                field: f.size,
                bucket: f.size_bucket,
                range: (1 << 29)..=u64::MAX,
            };
            SizeChecked {
                rest: Box::new(BooleanQuery::new(vec![(Occur::Must, ext_q.box_clone())])),
                ranges: vec![range.clone()],
                whole: Box::new(BooleanQuery::new(vec![
                    (Occur::Must, ext_q),
                    (Occur::Must, Box::new(range) as Q),
                ])),
                ratio,
            }
        };
        let seg = &searcher.segment_readers()[0];
        let scoring = EnableScoring::disabled_from_searcher(&searcher);
        let mut want = None;
        for (ratio, whole) in [(CHECK_RATIO, true), (u64::MAX, false)] {
            let q = checked(ratio);
            let mut w = q.checked_weight(scoring).unwrap();
            assert!(w.rest_terms.is_some(), "an extension is plain terms");
            let built = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            w.rest = Box::new(Counting(w.rest, built.clone()));
            let plan = w.plan(seg, 1.0).unwrap();
            assert_eq!(matches!(plan, Plan::Whole), whole, "ratio {ratio}");
            assert_eq!(
                built.load(Ordering::Relaxed),
                usize::from(!whole),
                "ratio {ratio}"
            );
            let got = ids(&searcher, &q, PIECE_DOCS);
            assert_eq!(
                *want.get_or_insert_with(|| got.clone()),
                got,
                "ratio {ratio}"
            );
        }
        // One torrent in fifty, torrent 2 (deleted) not among them.
        assert_eq!(want.unwrap().len(), 40);
    }

    /// The size check where some files have no size (never written by
    /// the indexer, which gives every file one): the column with gaps
    /// takes the optional path, and a file without a size never fits.
    #[test]
    fn the_size_check_skips_files_without_a_size() {
        let (schema, f) = schema();
        let index = ram(schema);
        let mut writer = index
            .writer_with_num_threads::<TantivyDocument>(1, 15 << 20)
            .unwrap();
        // Torrent t has one `bin` file, of size 10 * t, sized when t is
        // even.
        for t in 1..=300u64 {
            let mut d = TantivyDocument::new();
            d.add_u64(f.torrent, t);
            d.add_u64(f.index, 0);
            d.add_text(f.ext, "bin");
            if t % 2 == 0 {
                d.add_u64(f.size, t * 10);
            }
            writer.add_document(d).unwrap();
        }
        writer.commit().unwrap();
        let searcher = index.reader().unwrap().searcher();
        let seg = &searcher.segment_readers()[0];
        assert!(!matches!(
            seg.fast_fields().u64("size").unwrap().index,
            tantivy::columnar::ColumnIndex::Full
        ));
        let ext_q = compile_node(
            &Node::Ext {
                any: vec!["bin".into()],
            },
            &f,
        )
        .unwrap();
        let range = SizeRange {
            field: f.size,
            bucket: ABSENT,
            range: 1_000..=2_000,
        };
        let forced = SizeChecked {
            rest: Box::new(BooleanQuery::new(vec![(Occur::Must, ext_q.box_clone())])),
            ranges: vec![range.clone()],
            whole: Box::new(BooleanQuery::new(vec![
                (Occur::Must, ext_q),
                (Occur::Must, Box::new(range) as Q),
            ])),
            ratio: u64::MAX,
        };
        let want: Vec<u64> = (100..=200).filter(|t| t % 2 == 0).collect();
        for piece_docs in [7, PIECE_DOCS] {
            assert_eq!(ids(&searcher, &forced, piece_docs), want, "{piece_docs}");
        }
        assert_eq!(
            searcher
                .search(&forced, &tantivy::collector::Count)
                .unwrap(),
            want.len()
        );
    }

    /// A clause the torrent index answers scans nothing and caches
    /// nothing: the search's file pass gives it no set.
    #[test]
    fn a_torrent_level_clause_is_not_scanned() {
        let (searcher, f) = index(&[vec![vec![7], vec![5]]], &[]);
        let ext = Node::Ext {
            any: vec!["bin".into()],
        };
        let q = compile(&ext, &f).unwrap();
        assert!(q.downcast_ref::<TorrentLevel>().is_some());
        let cache = FileSetCache::default();
        let set = torrent_ids(
            &searcher,
            q.as_ref(),
            "q",
            &cache,
            &crate::CancelToken::new(),
        )
        .unwrap();
        assert!(set.is_empty());
        assert_eq!(cache.usage().0, 0);
        // Inside a conjunction it's a file-index clause like any other.
        let and = Node::And {
            of: vec![
                ext,
                Node::Range {
                    field: NumField::Size,
                    lo: Some(6),
                    hi: None,
                },
            ],
        };
        let q = compile(&and, &f).unwrap();
        assert!(q.downcast_ref::<TorrentLevel>().is_none());
        assert_eq!(ids(&searcher, q.as_ref(), PIECE_DOCS), vec![1]);
    }

    /// The column fallback clamps its bounds to the segment's values: a
    /// range wholly below them, or a segment of one distinct size, used
    /// to match files outside the range.
    #[test]
    fn the_column_fallback_matches_nothing_outside_the_segments_sizes() {
        for (sizes, range) in [
            (vec![5u64, 9], 0..=3u64),
            (vec![u64::MAX], 0..=0),
            (vec![7, 7, 7], 8..=100),
            (vec![7, 7, 7], 0..=6),
        ] {
            let segments = vec![sizes.iter().map(|s| vec![*s]).collect::<Vec<_>>()];
            // `bucket: ABSENT` below sends the range to the column path,
            // whatever buckets the files carry.
            let (searcher, f) = index(&segments, &[]);
            let scan = SizeRange {
                field: f.size,
                bucket: ABSENT,
                range: range.clone(),
            };
            assert!(
                ids(&searcher, &scan, PIECE_DOCS).is_empty(),
                "{sizes:?} {range:?}"
            );
        }
    }

    /// A bucket scan checks its stop at least every `CHECK_EVERY` files,
    /// counted across buckets, and gives up when told.
    #[test]
    fn a_bucket_scan_stops_when_cancelled() {
        let n = crate::cancel::CHECK_EVERY as usize * 3;
        let sizes: Vec<Vec<u64>> = (0..n).map(|i| vec![4096 + (i as u64 % 4096)]).collect();
        let (searcher, f) = index(&[sizes], &[]);
        let seg = &searcher.segment_readers()[0];
        let range = SizeRange {
            field: f.size,
            bucket: f.size_bucket,
            range: 4000..=u64::MAX,
        };
        let inv = range
            .bucketed(seg)
            .unwrap()
            .expect("every file has a bucket");
        let sizes = seg.fast_fields().u64("size").unwrap();
        let checks = std::cell::Cell::new(0u32);
        let stopped = || {
            checks.set(checks.get() + 1);
            if checks.get() >= 2 {
                Err(crate::cancel::interrupted())
            } else {
                Ok(())
            }
        };
        let mut visited = 0usize;
        let got = scan_buckets(
            &range,
            &inv,
            &sizes,
            (0, seg.max_doc()),
            &stopped,
            |_| Gather::List(Vec::new()),
            |docs: &[DocId], _: &mut Gather| visited += docs.len(),
        );
        assert!(got.is_err());
        assert_eq!(checks.get(), 2);
        assert!(
            visited > 0 && visited <= crate::cancel::CHECK_EVERY as usize,
            "{visited}"
        );
    }
}
