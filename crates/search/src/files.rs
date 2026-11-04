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
    FuzzyTermQuery, Occur, Query, RangeQuery, RegexQuery, Scorer, TermQuery, Weight,
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
            category: ext.as_deref().map_or(Category::Other, category_of),
            ext,
            size: f.size,
            pieces_root: f.pieces_root,
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
        let weight = match size {
            Some(_) => None,
            None => Some(query.weight(EnableScoring::disabled_from_searcher(searcher))?),
        };
        let source = match (size, &weight) {
            (Some(r), _) => Source::Size(r),
            (None, Some(w)) => Source::Weight(w.as_ref()),
            (None, None) => unreachable!("a weight is built for every other query"),
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
        let scanned = searcher
            .index()
            .search_executor()
            .map(
                |(i, lo, hi): (usize, DocId, DocId)| {
                    scan_segment(&source, &segs[i], (lo, hi), cancel, &failed).map_err(|e| {
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
            (Gather::List(mut ids), b) => {
                b.for_each(|id| ids.push(id));
                Gather::List(ids)
            }
            (a, Gather::List(mut ids)) => {
                a.for_each(|id| ids.push(id));
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
}

/// The torrent ids of one file-index segment's documents in `[lo, hi)`.
/// Documents come 64 at a time, with their torrent ids read in one call.
fn scan_segment(
    source: &Source<'_>,
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
    let span = column.max_value().saturating_sub(column.min_value()) / 64 + 1;
    // `hint` is the scorer's estimate of matching files. Tantivy 0.27's
    // range doc set gives `num_docs / 10` (an upstream TODO), and a bare
    // `size:` range on the column fallback uses the same, so the form
    // chosen for those can be the costlier one; either is correct. The
    // bucket path passes its postings' lengths instead.
    let gather_for = |hint: u32| {
        if seg.max_doc() > 0 && u64::from(hint) >= span {
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
    let mut add = |docs: &[DocId], gather: &mut Gather| {
        for docs in docs.chunks(BLOCK) {
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
    let gather = match source {
        Source::Weight(weight) => {
            const CHECK_BLOCKS: u32 = crate::cancel::CHECK_EVERY / 2 / BLOCK as u32;
            const _: () = assert!(CHECK_BLOCKS > 0);
            let mut scorer = weight.scorer(seg, 1.0)?;
            let mut gather = gather_for(scorer.size_hint());
            if scorer.doc() < lo {
                scorer.seek(lo);
            }
            let mut docs = [0 as DocId; BLOCK];
            let mut blocks = 0u32;
            loop {
                if blocks.is_multiple_of(CHECK_BLOCKS) {
                    stopped()?;
                }
                blocks = blocks.wrapping_add(1);
                let filled = scorer.fill_buffer(&mut docs);
                // Docs come in order; those from `hi` on are the next piece's.
                let n = docs[..filled].partition_point(|d| *d < hi);
                add(&docs[..n], &mut gather);
                if n < filled || filled == 0 {
                    break;
                }
            }
            gather
        }
        Source::Size(range) => {
            // A segment without sizes matches none, as Tantivy's range does.
            let Some(sizes) = seg
                .fast_fields()
                .column_opt::<u64>(seg.schema().get_field_name(range.field))?
            else {
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
            Some(ids) => id < u64::from(ids.max_value()) && ids.contains(id as u32),
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
        let wanted: Vec<u64> = (0..10_007).filter(|i| i % 3 == 0).collect();
        let set = IdSet {
            parts: vec![Arc::new(IdPart::from_sorted(wanted))],
            union: OnceLock::new(),
        };
        let weight = IdsWeight(Arc::new(set), crate::filter::SegmentSets::default());
        let got = weight.matches(seg).unwrap();
        for (doc, i) in ids.iter().enumerate() {
            assert_eq!(got.contains(doc as u32), i % 3 == 0, "doc {doc} id {i}");
        }
        assert_eq!(got.len(), ids.iter().filter(|i| *i % 3 == 0).count());
        // An empty set marks nothing.
        let none = IdsWeight(Arc::default(), crate::filter::SegmentSets::default());
        assert_eq!(none.matches(seg).unwrap().len(), 0);
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
                        ext: Some("bin".into()),
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
            p.for_each(|id| out.push(id));
        }
        out.sort_unstable();
        out.dedup();
        out
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
            // Every file without a bucket, so even the bucketed range scans.
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
