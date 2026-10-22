//! The torrent and file indexes on disk, near-real-time indexing and
//! searching.
//!
//! Indexing follows the store in id order. Ids are allocated in the single
//! writer's transactions and committed in order, so "everything up to
//! the watermark is indexed" is all the state there is. Each index saves
//! its watermark in its own Tantivy commit payload, atomically with the
//! documents it covers: a crash can only lose work, never skip or
//! duplicate a torrent. If it strikes between the two commits, the next
//! catch-up brings the index that's behind up to the other.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use lodestar_classify::{Composition, Record};
use lodestar_metainfo::InfoHash;
use lodestar_store::Store;
use tantivy::collector::{Count, TopDocs};
use tantivy::directory::MmapDirectory;
use tantivy::query::{BooleanQuery, EnableScoring, Occur, Query, TermQuery};
use tantivy::schema::{Field, IndexRecordOption, Schema, Value};
use tantivy::{
    DocAddress, DocSet, Index, IndexReader, IndexWriter, ReloadPolicy, Searcher, TantivyDocument,
    Term,
};

use crate::compile;
use crate::doc::{DocEnrichment, TorrentDoc};
use crate::files::{self, FileFields};
use crate::filter::{FastRange, PostFilter};
use crate::query::{Node, When};
use crate::schema::{self, Fields, SCHEMA_VERSION, TOKENIZER_VERSION};
use crate::sorted::{self, SegmentCache};
use crate::tokenizer::{NAME_TOKENIZER, NameTokenizer};

pub type Result<T, E = Error> = std::result::Result<T, E>;

#[derive(Debug)]
pub enum Error {
    Tantivy(tantivy::TantivyError),
    Store(lodestar_store::Error),
    Io(std::io::Error),
    /// The index's commit payload isn't ours.
    Corrupt(&'static str),
    /// Another process (or handle) is serving the index root.
    Locked(PathBuf),
    /// The index root's lock couldn't be taken at all, which some network
    /// shares (SMB, some NAS mounts) don't support.
    LockFailed {
        root: PathBuf,
        error: std::io::Error,
    },
    /// An older index kept serving during a rebuild takes no writes.
    ReadOnly,
    /// The search's [`CancelToken`](crate::CancelToken) was cancelled.
    Cancelled,
    /// The search ran out of its time budget.
    OutOfTime,
    /// The searcher lease a cursor names expired or never existed.
    LeaseExpired,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Tantivy(e) => write!(f, "index: {e}"),
            Error::Store(e) => e.fmt(f),
            Error::Io(e) => write!(f, "index directory: {e}"),
            Error::Corrupt(what) => write!(f, "corrupt index: {what}"),
            Error::Locked(root) => write!(f, "{} is in use by another instance", root.display()),
            Error::LockFailed { root, error } => write!(
                f,
                "can't lock {}: {error} (network shares may not support file locks; use a local disk for the index)",
                root.display()
            ),
            Error::ReadOnly => write!(f, "this index is read-only while it's rebuilt"),
            Error::Cancelled => write!(f, "the search was cancelled"),
            Error::OutOfTime => write!(f, "the search ran out of time"),
            Error::LeaseExpired => write!(f, "the searcher lease expired"),
        }
    }
}

impl std::error::Error for Error {}

impl From<tantivy::TantivyError> for Error {
    fn from(e: tantivy::TantivyError) -> Self {
        Error::Tantivy(e)
    }
}

impl From<lodestar_store::Error> for Error {
    fn from(e: lodestar_store::Error) -> Self {
        Error::Store(e)
    }
}

impl From<tantivy::directory::error::OpenDirectoryError> for Error {
    fn from(e: tantivy::directory::error::OpenDirectoryError) -> Self {
        Error::Tantivy(e.into())
    }
}

const PAYLOAD_PREFIX: &str = "lodestar-watermark:";

/// Below this many exact matches, a search also tries the stem tier
/// ([`SearchIndex::search_with_fallback`]). The second pass roughly
/// doubles the cost of only these queries; broad ones never pay for it.
pub const STEM_FALLBACK_BELOW: usize = 10;

/// The index directory for this build's schema and tokenizer, under
/// `root` (e.g. `data/index`).
pub fn index_dir(root: &Path) -> PathBuf {
    root.join(format!("v{SCHEMA_VERSION}-t{TOKENIZER_VERSION}"))
}

#[derive(Clone)]
pub struct SearchIndex {
    index: Index,
    reader: IndexReader,
    fields: Fields,
    cache: Arc<SegmentCache>,
    files: FileIndex,
    /// An older index serving during a rebuild whose schema differs from
    /// this build's: its fields are mapped by name, and documents built
    /// for this schema can't be written to it.
    read_only: bool,
    /// The fields of [`SearchIndex::missing_fields`].
    missing: Arc<[String]>,
    /// Held by every clone, so [`SearchIndex::holders`] tells when the
    /// last one (a lease's included) is gone.
    holders: Arc<()>,
}

/// How an existing index can serve this build ([`SearchIndex::open_existing`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Compat {
    /// Same schema: it can keep following the store.
    Exact,
    /// Another schema, read by field name: it serves queries and takes
    /// no writes.
    ReadOnly,
}

/// Fields an index can't serve without: ids, names, words and dates.
/// Every schema since v1 has them.
const REQUIRED: [&str; 4] = ["id", "name", "text", "discovered"];
/// The file index's: what a clause's result is made of.
const FILE_REQUIRED: [&str; 2] = ["torrent", "index"];

/// Resolves this build's fields (in `from`) in an older index's schema
/// (`to`) by name. A field it lacks, or has with another value type,
/// becomes [`compile::ABSENT`] and its name is recorded.
struct Resolver<'a> {
    from: &'a Schema,
    to: &'a Schema,
    absent: std::cell::RefCell<Vec<String>>,
}

impl Resolver<'_> {
    fn get(&self, f: Field) -> Field {
        let entry = self.from.get_field_entry(f);
        let found = self.to.get_field(entry.name()).ok().filter(|g| {
            self.to.get_field_entry(*g).field_type().value_type() == entry.field_type().value_type()
        });
        found.unwrap_or_else(|| {
            self.absent.borrow_mut().push(entry.name().to_owned());
            compile::ABSENT
        })
    }
}

/// This build's fields resolved in another schema. The literal names
/// every field, so a new one can't be forgotten here.
fn resolve_fields(f: &Fields, r: &Resolver<'_>) -> Fields {
    let m = |f: Field| r.get(f);
    Fields {
        id: m(f.id),
        hash: m(f.hash),
        name: m(f.name),
        path: m(f.path),
        text: m(f.text),
        ext: m(f.ext),
        flag: m(f.flag),
        size: m(f.size),
        files: m(f.files),
        largest: m(f.largest),
        piece_len: m(f.piece_len),
        discovered: m(f.discovered),
        metadata: m(f.metadata),
        title: m(f.title),
        title_stem: m(f.title_stem),
        tag: m(f.tag),
        year: m(f.year),
        season: m(f.season),
        episode: m(f.episode),
        release_year: m(f.release_year),
        junk: m(f.junk),
        cat_count: f.cat_count.map(m),
        cat_bytes: f.cat_bytes.map(m),
    }
}

fn resolve_file_fields(f: &FileFields, r: &Resolver<'_>) -> FileFields {
    let m = |f: Field| r.get(f);
    FileFields {
        torrent: m(f.torrent),
        index: m(f.index),
        name: m(f.name),
        dir: m(f.dir),
        ext: m(f.ext),
        size: m(f.size),
    }
}

/// The file index (§8, [`crate::files`]), in the `files` directory
/// inside the torrent index's.
#[derive(Clone)]
struct FileIndex {
    index: Index,
    reader: IndexReader,
    fields: FileFields,
    cache: Arc<files::FileSetCache>,
}

fn watermarks_of(index: &Index) -> Result<(i64, i64)> {
    let Some(p) = index.load_metas()?.payload else {
        return Ok((0, 0));
    };
    let bad = || Error::Corrupt("unknown commit payload");
    let rest = p.strip_prefix(PAYLOAD_PREFIX).ok_or_else(bad)?;
    let (id, seq) = match rest.split_once(";reindex:") {
        Some((id, seq)) => (id, seq),
        None => (rest, "0"),
    };
    Ok((
        id.parse().map_err(|_| bad())?,
        seq.parse().map_err(|_| bad())?,
    ))
}

/// Bytes by (component, field) across an index's live segments, largest
/// first.
fn space_by_field(reader: &IndexReader) -> Result<Vec<(String, u64)>> {
    let mut totals: std::collections::BTreeMap<String, u64> = Default::default();
    let searcher = reader.searcher();
    for seg in searcher.segment_readers() {
        let u = seg.space_usage().map_err(Error::Io)?;
        let parts = [
            ("terms", u.termdict()),
            ("postings", u.postings()),
            ("positions", u.positions()),
            ("fast", u.fast_fields()),
            ("norms", u.fieldnorms()),
        ];
        for (component, per_field) in parts {
            for f in per_field.fields() {
                *totals
                    .entry(format!("{component} {}", f.field_name()))
                    .or_default() += f.total().get_bytes();
            }
        }
        *totals.entry("docstore".into()).or_default() += u.store().total().get_bytes();
    }
    let mut out: Vec<(String, u64)> = totals.into_iter().collect();
    out.sort_by_key(|e| std::cmp::Reverse(e.1));
    Ok(out)
}

/// Segments searched in parallel, and the name tokenizer registered.
fn prepare(index: &mut Index) -> Result<IndexReader> {
    // Broad queries visit every match, so searching segments in parallel
    // is most of the latency budget at 10M (§16).
    let threads = std::thread::available_parallelism().map_or(1, |n| n.get().min(SEARCH_THREADS));
    if threads > 1 {
        index.set_multithread_executor(threads)?;
    }
    index
        .tokenizers()
        .register(NAME_TOKENIZER, NameTokenizer::default());
    Ok(index
        .reader_builder()
        .reload_policy(ReloadPolicy::Manual)
        .try_into()?)
}

impl std::fmt::Debug for SearchIndex {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SearchIndex").finish_non_exhaustive()
    }
}

/// One result.
#[derive(Debug, Clone, PartialEq)]
pub struct Hit {
    pub id: u64,
    pub score: f32,
}

/// Result order. Relevance is phase-1 BM25 for now; the re-ranker with
/// swarm and freshness priors arrives with swarm health (§8, M4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Sort {
    #[default]
    Relevance,
    Discovered {
        desc: bool,
    },
    Size {
        desc: bool,
    },
    Files {
        desc: bool,
    },
}

/// A query ready to run: the Tantivy query, plus top-level ranges that
/// the collector checks against fast fields (see `filter`).
pub struct Compiled {
    pub(crate) query: Box<dyn Query>,
    pub(crate) filters: Vec<FastRange>,
    /// Scores added to the phase-1 candidates (name boosts).
    pub(crate) boosts: Option<Box<dyn Query>>,
    /// Identifies the query for the segment cache.
    pub(crate) key: String,
}

impl std::fmt::Debug for Compiled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Compiled")
            .field("query", &self.query)
            .field("filters", &self.filters)
            .field("boosts", &self.boosts)
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Results {
    pub hits: Vec<Hit>,
    /// Exact number of matches.
    pub total: usize,
}

/// Whether `node` has a date relative to now.
fn relative(node: &Node) -> bool {
    match node {
        Node::And { of } | Node::Or { of } => of.iter().any(relative),
        Node::Not { of } => relative(of),
        Node::Date { from, to, .. } => [from, to].iter().any(|w| matches!(w, Some(When::Ago(_)))),
        _ => false,
    }
}

/// Phase-1 candidates re-ranked for a relevance page: four pages of 50.
/// §8 sketched ~1,000; at 10M torrents a window that size costs about a
/// quarter of the keyword budget in top-k upkeep alone (§16).
pub(crate) const RERANK_WINDOW: usize = 200;

/// Adds each candidate's score under `boosts` and sorts best first,
/// ties by address.
pub(crate) fn rerank(
    searcher: &Searcher,
    boosts: &dyn Query,
    mut hits: Vec<(f32, DocAddress)>,
) -> Result<Vec<(f32, DocAddress)>> {
    let weight = boosts.weight(EnableScoring::enabled_from_searcher(searcher))?;
    hits.sort_unstable_by_key(|(_, a)| *a);
    let mut i = 0;
    while i < hits.len() {
        let seg = hits[i].1.segment_ord;
        let mut scorer = weight.scorer(searcher.segment_reader(seg), 1.0)?;
        while let Some((score, addr)) = hits.get_mut(i).filter(|(_, a)| a.segment_ord == seg) {
            if scorer.doc() < addr.doc_id {
                scorer.seek(addr.doc_id);
            }
            if scorer.doc() == addr.doc_id {
                *score += scorer.score();
            }
            i += 1;
        }
    }
    hits.sort_by(|a, b| b.0.total_cmp(&a.0).then(a.1.cmp(&b.1)));
    Ok(hits)
}

/// The fast field a sort orders by, and whether it's signed.
pub(crate) fn sort_field(sort: Sort) -> Option<(&'static str, bool)> {
    match sort {
        Sort::Relevance => None,
        Sort::Discovered { .. } => Some(("discovered", true)),
        Sort::Size { .. } => Some(("size", false)),
        Sort::Files { .. } => Some(("files", false)),
    }
}

/// Segments searched at once, at most: the §3 targets assume 4 cores.
const SEARCH_THREADS: usize = 4;

impl SearchIndex {
    /// Opens the index in `dir`, creating it if absent.
    pub fn open(dir: &Path) -> Result<SearchIndex> {
        let files_dir = dir.join("files");
        std::fs::create_dir_all(&files_dir).map_err(Error::Io)?;
        let (schema, fields) = schema::build();
        let index = Index::open_or_create(MmapDirectory::open(dir)?, schema)?;
        let (file_schema, file_fields) = files::schema();
        let file_index = Index::builder()
            .schema(file_schema)
            .settings(files::settings())
            .open_or_create(MmapDirectory::open(&files_dir)?)?;
        SearchIndex::wrap(index, fields, file_index, file_fields)
    }

    /// Opens the existing index in `dir` with the schema it was built
    /// with, for serving while a newer one is built. Fields this build
    /// added (or retyped) are absent there: clauses on them match nothing
    /// and sorts on them are skipped ([`SearchIndex::missing_fields`]).
    /// `None` when it lacks a field no search works without, which is a
    /// schema mismatch; an error opening it is just an error.
    pub(crate) fn open_existing(dir: &Path) -> Result<Option<(SearchIndex, Compat)>> {
        let index = Index::open(MmapDirectory::open(dir)?)?;
        let file_index = Index::open(MmapDirectory::open(dir.join("files"))?)?;
        let (schema, fields) = schema::build();
        let (file_schema, file_fields) = files::schema();
        let compat = if index.schema() == schema && file_index.schema() == file_schema {
            Compat::Exact
        } else {
            Compat::ReadOnly
        };
        let (old, old_files) = (index.schema(), file_index.schema());
        let r = Resolver {
            from: &schema,
            to: &old,
            absent: Default::default(),
        };
        let fields = resolve_fields(&fields, &r);
        let fr = Resolver {
            from: &file_schema,
            to: &old_files,
            absent: Default::default(),
        };
        let file_fields = resolve_file_fields(&file_fields, &fr);
        let absent = r.absent.into_inner();
        let file_absent = fr.absent.into_inner();
        if absent.iter().any(|n| REQUIRED.contains(&n.as_str()))
            || file_absent
                .iter()
                .any(|n| FILE_REQUIRED.contains(&n.as_str()))
        {
            return Ok(None);
        }
        let mut s = SearchIndex::wrap(index, fields, file_index, file_fields)?;
        s.read_only = compat == Compat::ReadOnly;
        s.missing = absent
            .into_iter()
            .chain(file_absent.into_iter().map(|n| format!("file.{n}")))
            .collect();
        Ok(Some((s, compat)))
    }

    /// The torrent index and its reader, for tests that write raw
    /// documents.
    #[cfg(test)]
    pub(crate) fn raw(&self) -> (&Index, &IndexReader) {
        (&self.index, &self.reader)
    }

    /// The file index's fields, for tests.
    #[cfg(test)]
    pub(crate) fn file_fields(&self) -> &FileFields {
        &self.files.fields
    }

    /// A RAM index, for tests.
    pub fn in_memory() -> Result<SearchIndex> {
        let (schema, fields) = schema::build();
        let (file_schema, file_fields) = files::schema();
        SearchIndex::wrap(
            Index::create_in_ram(schema),
            fields,
            Index::builder()
                .schema(file_schema)
                .settings(files::settings())
                .create_in_ram()?,
            file_fields,
        )
    }

    fn wrap(
        mut index: Index,
        fields: Fields,
        mut file_index: Index,
        file_fields: FileFields,
    ) -> Result<SearchIndex> {
        let reader = prepare(&mut index)?;
        let file_reader = prepare(&mut file_index)?;
        Ok(SearchIndex {
            index,
            reader,
            fields,
            cache: Arc::default(),
            files: FileIndex {
                index: file_index,
                reader: file_reader,
                fields: file_fields,
                cache: Arc::default(),
            },
            read_only: false,
            missing: Arc::new([]),
            holders: Arc::default(),
        })
    }

    pub fn fields(&self) -> &Fields {
        &self.fields
    }

    /// Alive while any clone of this index is: a retired index's
    /// directory is deleted only after that (leases, `lease.rs`).
    pub(crate) fn holders(&self) -> std::sync::Weak<()> {
        Arc::downgrade(&self.holders)
    }

    /// The current snapshot, which a lease pins.
    pub(crate) fn searcher(&self) -> Searcher {
        self.reader.searcher()
    }

    pub(crate) fn segment_cache(&self) -> &SegmentCache {
        &self.cache
    }

    /// Fields of this build's schema this index lacks: an older index
    /// serving during a rebuild (file-index fields as `file.<name>`).
    /// Clauses on them match nothing and sorts on them fall back to
    /// relevance, so the API can say "index rebuilding; … not yet
    /// available". Empty for this build's own index.
    pub fn missing_fields(&self) -> &[String] {
        &self.missing
    }

    /// Whether `sort` works on this index; otherwise it's by relevance.
    pub fn sort_available(&self, sort: Sort) -> bool {
        sort_field(sort).is_none_or(|(field, _)| !self.missing.iter().any(|m| m == field))
    }

    /// The highest torrent id indexed (0 for a new index).
    pub fn watermark(&self) -> Result<i64> {
        Ok(watermarks_of(&self.index)?.0)
    }

    /// The highest torrent id whose files are indexed.
    pub fn files_watermark(&self) -> Result<i64> {
        Ok(watermarks_of(&self.files.index)?.0)
    }

    /// Documents in the file index's current searcher.
    pub fn num_files(&self) -> u64 {
        self.files.reader.searcher().num_docs()
    }

    /// Bytes on disk across the file index's live segments.
    pub fn files_disk_bytes(&self) -> Result<u64> {
        let searcher = self.files.reader.searcher();
        let mut total = 0;
        for seg in searcher.segment_readers() {
            total += seg.space_usage().map_err(Error::Io)?.total().get_bytes();
        }
        Ok(total)
    }

    /// Documents in the current searcher.
    pub fn num_docs(&self) -> u64 {
        self.reader.searcher().num_docs()
    }

    /// Bytes by (component, field) across live segments, largest first,
    /// for the size report.
    pub fn space_by_field(&self) -> Result<Vec<(String, u64)>> {
        space_by_field(&self.reader)
    }

    /// The same for the file index.
    pub fn files_space_by_field(&self) -> Result<Vec<(String, u64)>> {
        space_by_field(&self.files.reader)
    }

    /// Live segments in the current searcher.
    pub fn segments(&self) -> usize {
        self.reader.searcher().segment_readers().len()
    }

    /// Bytes on disk across the live segments.
    pub fn disk_bytes(&self) -> Result<u64> {
        let searcher = self.reader.searcher();
        let mut total = 0;
        for seg in searcher.segment_readers() {
            total += seg.space_usage().map_err(Error::Io)?.total().get_bytes();
        }
        Ok(total)
    }

    /// The writer for both indexes. `heap_bytes` is split between them:
    /// a quarter goes to the file index, which has smaller documents.
    pub fn writer(&self, heap_bytes: usize) -> Result<Indexer> {
        if self.read_only {
            return Err(Error::ReadOnly);
        }
        // Tantivy wants at least 15 MB per writer.
        let file_heap = (heap_bytes / 4).max(15 << 20);
        let torrents = Side::new(
            &self.index,
            heap_bytes.saturating_sub(file_heap).max(15 << 20),
        )?;
        let files = Side::new(&self.files.index, file_heap)?;
        Ok(Indexer {
            index: self.clone(),
            torrents,
            files,
            pending: 0,
        })
    }

    /// Compiles a parsed query; `now` (unix seconds) resolves relative
    /// dates.
    pub fn query(&self, node: &Node, now: i64) -> Compiled {
        self.compile(node, now, false)
    }

    /// The same query with the stem tier: each stemmable word also
    /// matches by its light stem in the title (§8). `None` when no word
    /// has a stem alternative, so it would match the same torrents.
    pub fn query_stemmed(&self, node: &Node, now: i64) -> Option<Compiled> {
        compile::has_stem_alternative(node).then(|| self.compile(node, now, true))
    }

    /// One page of results, falling back to the stem tier when the exact
    /// query finds fewer than [`STEM_FALLBACK_BELOW`] torrents and the
    /// tier finds more. The flag says whether it did. Stem-only matches
    /// rank after exact ones, and a query's total decides, so every page
    /// of it takes the same branch.
    pub fn search_with_fallback(
        &self,
        node: &Node,
        now: i64,
        sort: Sort,
        limit: usize,
        offset: usize,
    ) -> Result<(Results, bool)> {
        let exact = self.search_page(&self.query(node, now), sort, limit, offset)?;
        if exact.total >= STEM_FALLBACK_BELOW {
            return Ok((exact, false));
        }
        let Some(stemmed) = self.query_stemmed(node, now) else {
            return Ok((exact, false));
        };
        let wider = self.search_page(&stemmed, sort, limit, offset)?;
        Ok(if wider.total > exact.total {
            (wider, true)
        } else {
            (exact, false)
        })
    }

    pub(crate) fn compile(&self, node: &Node, now: i64, stems: bool) -> Compiled {
        // `file:(…)` clauses run on the file index first.
        let mut clauses = Vec::new();
        compile::file_clauses(node, &mut clauses);
        let mut sets = compile::FileSets::new();
        let files_searcher = self.files.reader.searcher();
        for of in &clauses {
            let key = format!("{of:?}");
            let ids = match files::compile(of, &self.files.fields) {
                Some(q) => files::torrent_ids(&files_searcher, q.as_ref(), &key, &self.files.cache)
                    .unwrap_or_else(|e| {
                        tracing::error!("file index search failed: {e}");
                        files::IdSet::default()
                    }),
                None => files::IdSet::default(),
            };
            sets.insert(key, Arc::new(ids));
        }
        let cx = compile::Cx {
            fields: &self.fields,
            files: &sets,
            stems,
        };
        let compile::Parts {
            query,
            filters,
            boosts,
        } = compile::compile(node, &cx, now);
        // Relative dates resolve against `now`, so they're part of the key,
        // and file clauses against the file index's generation.
        let mut key = format!("{node:?}");
        if stems {
            key.push_str("@stems");
        }
        if relative(node) {
            key.push_str(&format!("@{now}"));
        }
        if !clauses.is_empty() {
            key.push_str(&format!(
                "@files{}",
                files_searcher.generation().generation_id()
            ));
        }
        Compiled {
            query,
            filters,
            boosts,
            key,
        }
    }

    /// Empties the browse cache (for benchmarks).
    pub fn clear_cache(&self) {
        self.cache.clear();
        self.files.cache.clear();
    }

    /// The torrent with this swarm hash, if indexed.
    pub fn find_hash(&self, hash: &InfoHash) -> Result<Option<u64>> {
        if self.fields.hash == compile::ABSENT {
            return Ok(None);
        }
        let q = TermQuery::new(
            Term::from_field_bytes(self.fields.hash, &hash.0),
            IndexRecordOption::Basic,
        );
        let q = Compiled {
            query: Box::new(q),
            filters: Vec::new(),
            boosts: None,
            key: String::new(),
        };
        Ok(self.search(&q, 1)?.hits.first().map(|h| h.id))
    }

    pub fn search(&self, query: &Compiled, limit: usize) -> Result<Results> {
        self.search_page(query, Sort::Relevance, limit, 0)
    }

    /// One page of results in `sort` order, with the exact total.
    pub fn search_page(
        &self,
        compiled: &Compiled,
        sort: Sort,
        limit: usize,
        offset: usize,
    ) -> Result<Results> {
        let searcher = self.reader.searcher();
        let query = compiled.query.as_ref();
        let ranges = &compiled.filters;
        let top = TopDocs::with_limit(limit.max(1)).and_offset(offset);
        let (scored, total): (Vec<(f32, DocAddress)>, usize) = match sort {
            Sort::Relevance => match &compiled.boosts {
                None => {
                    let inner = (top.order_by_score(), Count);
                    searcher.search(query, &PostFilter { ranges, inner })?
                }
                Some(boosts) => {
                    // Two-phase (§8): the best candidates by phase-1
                    // score, re-ranked with the name boosts. Order is
                    // exact within the window.
                    let window = (offset + limit).max(RERANK_WINDOW);
                    let inner = (TopDocs::with_limit(window).order_by_score(), Count);
                    let (candidates, n) = searcher.search(query, &PostFilter { ranges, inner })?;
                    let ranked = rerank(&searcher, boosts.as_ref(), candidates)?;
                    (ranked.into_iter().skip(offset).take(limit).collect(), n)
                }
            },
            Sort::Discovered { desc } | Sort::Size { desc } | Sort::Files { desc } => {
                let Some((field, signed)) = sort_field(sort) else {
                    unreachable!("a field sort");
                };
                if !self.sort_available(sort) {
                    return self.search_page(compiled, Sort::Relevance, limit, offset);
                }
                let order = sorted::Order {
                    field,
                    signed,
                    desc,
                };
                let (page, n) =
                    sorted::search(&searcher, &self.cache, compiled, order, limit, offset)?;
                (page.into_iter().map(|a| (0.0, a)).collect(), n)
            }
        };
        // Ids come from the fast field: the docstore is only for names.
        let mut columns = std::collections::HashMap::new();
        let mut hits = Vec::with_capacity(scored.len());
        for (score, addr) in scored {
            let col = match columns.entry(addr.segment_ord) {
                std::collections::hash_map::Entry::Occupied(e) => e.into_mut(),
                std::collections::hash_map::Entry::Vacant(e) => e.insert(
                    searcher
                        .segment_reader(addr.segment_ord)
                        .fast_fields()
                        .u64("id")?,
                ),
            };
            let id = col
                .first(addr.doc_id)
                .ok_or(Error::Corrupt("document without an id"))?;
            hits.push(Hit { id, score });
        }
        Ok(Results { hits, total })
    }

    /// The ids of every torrent `compiled` matches, at most `limit`, in no
    /// particular order: for jobs over a query's whole result set (a
    /// classification rule's torrents, §20.9), not for display.
    pub fn matching_ids(&self, compiled: &Compiled, limit: usize) -> Result<Vec<u64>> {
        let searcher = self.reader.searcher();
        let collector = PostFilter {
            ranges: &compiled.filters,
            inner: IdsCollector { limit },
        };
        let mut ids = searcher.search(compiled.query.as_ref(), &collector)?;
        ids.truncate(limit);
        Ok(ids)
    }

    /// The positions (in the torrent's file list) of the files of torrent
    /// `id` that match any `file:(…)` clause of `node`, at most `limit`:
    /// the "matched files" a result shows (§8).
    pub fn matched_files(&self, node: &Node, id: u64, limit: usize) -> Result<Vec<u64>> {
        let mut clauses = Vec::new();
        compile::file_clauses(node, &mut clauses);
        let searcher = self.files.reader.searcher();
        let mut out = Vec::new();
        for of in &clauses {
            let Some(q) = files::compile(of, &self.files.fields) else {
                continue;
            };
            let this = TermQuery::new(
                Term::from_field_u64(self.files.fields.torrent, id),
                IndexRecordOption::Basic,
            );
            let both = BooleanQuery::new(vec![
                (Occur::Must, q),
                (Occur::Must, Box::new(this) as Box<dyn Query>),
            ]);
            out.extend(searcher.search(&both, &files::FileIndexes)?);
        }
        out.sort_unstable();
        out.dedup();
        out.truncate(limit);
        Ok(out)
    }

    /// The stored names of some hits (for tests and tools; the API reads
    /// names from SQLite).
    pub fn names(&self, hits: &[Hit]) -> Result<Vec<String>> {
        let searcher = self.reader.searcher();
        let mut out = Vec::with_capacity(hits.len());
        for h in hits {
            let q = TermQuery::new(
                Term::from_field_u64(self.fields.id, h.id),
                IndexRecordOption::Basic,
            );
            let top = searcher.search(&q, &TopDocs::with_limit(1).order_by_score())?;
            let name = match top.first() {
                Some((_, addr)) => {
                    let doc: TantivyDocument = searcher.doc(*addr)?;
                    doc.get_first(self.fields.name)
                        .and_then(|v| v.as_str())
                        .unwrap_or_default()
                        .to_string()
                }
                None => String::new(),
            };
            out.push(name);
        }
        Ok(out)
    }
}

/// A stored torrent: its row and file tree, or `None` if it was deleted.
struct Loaded {
    row: lodestar_store::TorrentRow,
    tree: lodestar_store::FileTree,
}

fn load(store: &mut Store, id: i64) -> Result<Option<Loaded>> {
    Ok(match (store.torrent(id)?, store.files(id)?) {
        (Some(row), Some(tree)) => Some(Loaded { row, tree }),
        _ => None,
    })
}

/// The torrent's document, with its enrichment when it has a readable
/// one. A record the codec rejects is left out: the writer's backfill
/// derives it again and logs the torrent for reindexing.
fn torrent_doc(store: &mut Store, t: &Loaded) -> Result<TorrentDoc> {
    let mut doc = TorrentDoc::new(&t.row, &t.tree);
    if let Some(e) = store.enrichment(t.row.id)? {
        match Record::decode(&e.data) {
            Ok(record) => {
                // An empty or unreadable `comp` only loses the shares.
                let comp = Composition::decode(&e.comp).ok();
                doc.enriched = DocEnrichment::new(&record, comp.as_ref());
            }
            Err(err) => tracing::debug!(id = t.row.id, "unreadable enrichment: {err}"),
        }
    }
    Ok(doc)
}

/// One index's writer and how far it has followed the store.
struct Side {
    writer: IndexWriter,
    last_id: i64,
    /// The last `reindex_log` entry applied.
    reindex_seq: i64,
}

impl Side {
    fn new(index: &Index, heap_bytes: usize) -> Result<Side> {
        let (last_id, reindex_seq) = watermarks_of(index)?;
        Ok(Side {
            writer: index.writer_with_num_threads(1, heap_bytes)?,
            last_id,
            reindex_seq,
        })
    }

    fn commit(&mut self, reader: &IndexReader) -> Result<()> {
        let mut prepared = self.writer.prepare_commit()?;
        prepared.set_payload(&format!(
            "{PAYLOAD_PREFIX}{};reindex:{}",
            self.last_id, self.reindex_seq
        ));
        prepared.commit()?;
        reader.reload()?;
        Ok(())
    }
}

/// Adds torrents to both indexes in id order and commits each with its
/// watermark.
pub struct Indexer {
    index: SearchIndex,
    torrents: Side,
    files: Side,
    pending: usize,
}

impl std::fmt::Debug for Indexer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Indexer")
            .field("last_id", &self.torrents.last_id)
            .field("files_last_id", &self.files.last_id)
            .field("pending", &self.pending)
            .finish()
    }
}

impl Indexer {
    /// The highest id added to the torrent index (committed or not).
    pub fn last_id(&self) -> i64 {
        self.torrents.last_id
    }

    /// Torrents added or replaced since the last commit.
    pub fn pending(&self) -> usize {
        self.pending
    }

    /// Indexes up to `limit` torrents stored after the watermarks, then
    /// re-indexes torrents changed since (upgraded partial imports, new
    /// enrichment). Returns how many torrents it added or replaced.
    pub fn catch_up(&mut self, store: &mut Store, limit: usize) -> Result<usize> {
        let (added, replayed) = self.catch_up_counted(store, limit)?;
        Ok(added + replayed)
    }

    /// [`Indexer::catch_up`], counting the torrents added and the
    /// `reindex_log` entries replayed apart.
    pub fn catch_up_counted(&mut self, store: &mut Store, limit: usize) -> Result<(usize, usize)> {
        for side in [&mut self.torrents, &mut self.files] {
            if side.last_id == 0 && side.reindex_seq == 0 {
                // A new index reads every torrent's current data, so the
                // changes logged before it started are already in it.
                side.reindex_seq = store.reindex_head()?;
            }
        }
        let added = self.catch_up_new(store, limit)?;
        let mut replayed = 0;
        let after = self.torrents.reindex_seq.min(self.files.reindex_seq);
        for (seq, id) in store.reindex_after(after, limit)? {
            let t = load(store, id)?;
            let fields = &self.index.fields;
            let torrents = &mut self.torrents;
            // Ids past a watermark get their current data when reached.
            if seq > torrents.reindex_seq {
                torrents.reindex_seq = seq;
                if id <= torrents.last_id {
                    torrents
                        .writer
                        .delete_term(Term::from_field_u64(fields.id, id as u64));
                    if let Some(t) = &t {
                        let doc = torrent_doc(store, t)?;
                        torrents.writer.add_document(doc.to_document(fields))?;
                    }
                }
            }
            let ff = &self.index.files.fields;
            let files = &mut self.files;
            if seq > files.reindex_seq {
                files.reindex_seq = seq;
                if id <= files.last_id {
                    files
                        .writer
                        .delete_term(Term::from_field_u64(ff.torrent, id as u64));
                    if let Some(t) = &t {
                        for d in files::file_docs(id as u64, &t.tree) {
                            files.writer.add_document(d.to_document(ff))?;
                        }
                    }
                }
            }
            self.pending += 1;
            replayed += 1;
        }
        Ok((added, replayed))
    }

    /// Whether both indexes have followed the store up to torrent `id`
    /// and `reindex_log` entry `seq`.
    pub fn reached(&self, id: i64, seq: i64) -> bool {
        [&self.torrents, &self.files]
            .iter()
            .all(|s| s.last_id >= id && s.reindex_seq >= seq)
    }

    fn catch_up_new(&mut self, store: &mut Store, limit: usize) -> Result<usize> {
        let from = self.torrents.last_id.min(self.files.last_id);
        let ids = store.ids_after(from, limit)?;
        let mut added = 0;
        for id in ids {
            // Deleted between the reads: nothing to index.
            let t = load(store, id)?;
            if id > self.torrents.last_id {
                if let Some(t) = &t {
                    let doc = torrent_doc(store, t)?;
                    self.torrents
                        .writer
                        .add_document(doc.to_document(&self.index.fields))?;
                }
                self.torrents.last_id = id;
            }
            if id > self.files.last_id {
                if let Some(t) = &t {
                    for d in files::file_docs(id as u64, &t.tree) {
                        self.files
                            .writer
                            .add_document(d.to_document(&self.index.files.fields))?;
                    }
                }
                self.files.last_id = id;
            }
            if t.is_some() {
                self.pending += 1;
                added += 1;
            }
        }
        Ok(added)
    }

    /// Commits what's pending with the watermarks and makes it
    /// searchable. The file index commits first, so a `file:(…)` clause
    /// rarely names a torrent the torrent index doesn't have yet; if it
    /// does, that torrent simply isn't found until it's indexed.
    pub fn commit(&mut self) -> Result<()> {
        self.files.commit(&self.index.files.reader)?;
        self.torrents.commit(&self.index.reader)?;
        self.pending = 0;
        Ok(())
    }

    /// Waits for background merges, for benchmarks and shutdown.
    pub fn finish(self) -> Result<()> {
        self.files.writer.wait_merging_threads()?;
        self.torrents.writer.wait_merging_threads()?;
        self.index.files.reader.reload()?;
        self.index.reader.reload()?;
        Ok(())
    }
}

/// Collects torrent ids from the `id` fast field, at most `limit` per
/// segment (the merge truncates the total).
struct IdsCollector {
    limit: usize,
}

struct IdsSegment {
    col: tantivy::columnar::Column<u64>,
    ids: Vec<u64>,
    limit: usize,
}

impl tantivy::collector::Collector for IdsCollector {
    type Fruit = Vec<u64>;
    type Child = IdsSegment;

    fn for_segment(
        &self,
        _: tantivy::SegmentOrdinal,
        reader: &tantivy::SegmentReader,
    ) -> tantivy::Result<IdsSegment> {
        Ok(IdsSegment {
            col: reader.fast_fields().u64("id")?,
            ids: Vec::new(),
            limit: self.limit,
        })
    }

    fn requires_scoring(&self) -> bool {
        false
    }

    fn merge_fruits(&self, fruits: Vec<Vec<u64>>) -> tantivy::Result<Vec<u64>> {
        Ok(fruits.concat())
    }
}

impl tantivy::collector::SegmentCollector for IdsSegment {
    type Fruit = Vec<u64>;

    fn collect(&mut self, doc: tantivy::DocId, _: tantivy::Score) {
        if self.ids.len() < self.limit {
            if let Some(id) = self.col.first(doc) {
                self.ids.push(id);
            }
        }
    }

    fn harvest(self) -> Vec<u64> {
        self.ids
    }
}
