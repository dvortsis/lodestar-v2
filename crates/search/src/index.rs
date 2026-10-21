//! The torrent index on disk, near-real-time indexing and searching.
//!
//! Indexing follows the store in id order. Ids are allocated in the single
//! writer's transactions and committed in order, so "everything up to
//! the watermark is indexed" is all the state there is. The watermark is
//! saved in the Tantivy commit payload, atomically with the documents it
//! covers: a crash can only lose work, never skip or duplicate a torrent.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use lodestar_metainfo::InfoHash;
use lodestar_store::Store;
use tantivy::collector::{Count, TopDocs};
use tantivy::directory::MmapDirectory;
use tantivy::query::{EnableScoring, Query, TermQuery};
use tantivy::schema::{IndexRecordOption, Value};
use tantivy::{
    DocAddress, DocSet, Index, IndexReader, IndexWriter, ReloadPolicy, Searcher, TantivyDocument,
    Term,
};

use crate::compile;
use crate::doc::TorrentDoc;
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
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Tantivy(e) => write!(f, "index: {e}"),
            Error::Store(e) => e.fmt(f),
            Error::Io(e) => write!(f, "index directory: {e}"),
            Error::Corrupt(what) => write!(f, "corrupt index: {what}"),
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
const RERANK_WINDOW: usize = 200;

/// Adds each candidate's score under `boosts` and sorts best first,
/// ties by address.
fn rerank(
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

/// Segments searched at once, at most: the §3 targets assume 4 cores.
const SEARCH_THREADS: usize = 4;

impl SearchIndex {
    /// Opens the index in `dir`, creating it if absent.
    pub fn open(dir: &Path) -> Result<SearchIndex> {
        std::fs::create_dir_all(dir).map_err(Error::Io)?;
        let (schema, fields) = schema::build();
        let index = Index::open_or_create(MmapDirectory::open(dir)?, schema)?;
        SearchIndex::wrap(index, fields)
    }

    /// A RAM index, for tests.
    pub fn in_memory() -> Result<SearchIndex> {
        let (schema, fields) = schema::build();
        SearchIndex::wrap(Index::create_in_ram(schema), fields)
    }

    fn wrap(mut index: Index, fields: Fields) -> Result<SearchIndex> {
        // Segments are searched in parallel: broad queries visit every
        // match, so this is most of the latency budget at 10M (§16).
        let threads =
            std::thread::available_parallelism().map_or(1, |n| n.get().min(SEARCH_THREADS));
        if threads > 1 {
            index.set_multithread_executor(threads)?;
        }
        index
            .tokenizers()
            .register(NAME_TOKENIZER, NameTokenizer::default());
        let reader = index
            .reader_builder()
            .reload_policy(ReloadPolicy::Manual)
            .try_into()?;
        Ok(SearchIndex {
            index,
            reader,
            fields,
            cache: Arc::default(),
        })
    }

    pub fn fields(&self) -> &Fields {
        &self.fields
    }

    /// The highest torrent id indexed (0 for a new index).
    pub fn watermark(&self) -> Result<i64> {
        Ok(self.watermarks()?.0)
    }

    /// `(highest id indexed, last reindex_log seq applied)`.
    fn watermarks(&self) -> Result<(i64, i64)> {
        let Some(p) = self.index.load_metas()?.payload else {
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

    /// Documents in the current searcher.
    pub fn num_docs(&self) -> u64 {
        self.reader.searcher().num_docs()
    }

    /// Bytes by (component, field) across live segments, largest first,
    /// for the size report.
    pub fn space_by_field(&self) -> Result<Vec<(String, u64)>> {
        let mut totals: std::collections::BTreeMap<String, u64> = Default::default();
        let searcher = self.reader.searcher();
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

    pub fn writer(&self, heap_bytes: usize) -> Result<Indexer> {
        let writer = self.index.writer_with_num_threads(1, heap_bytes)?;
        let (last_id, reindex_seq) = self.watermarks()?;
        Ok(Indexer {
            index: self.clone(),
            writer,
            last_id,
            reindex_seq,
            pending: 0,
        })
    }

    /// Compiles a parsed query; `now` (unix seconds) resolves relative
    /// dates.
    pub fn query(&self, node: &Node, now: i64) -> Compiled {
        let compile::Parts {
            query,
            filters,
            boosts,
        } = compile::compile(node, &self.fields, now);
        // Relative dates resolve against `now`, so they're part of the key.
        let key = if relative(node) {
            format!("{node:?}@{now}")
        } else {
            format!("{node:?}")
        };
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
    }

    /// The torrent with this swarm hash, if indexed.
    pub fn find_hash(&self, hash: &InfoHash) -> Result<Option<u64>> {
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
                let (field, signed) = match sort {
                    Sort::Discovered { .. } => ("discovered", true),
                    Sort::Size { .. } => ("size", false),
                    _ => ("files", false),
                };
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

/// Adds torrents to the index in id order and commits with the watermark.
pub struct Indexer {
    index: SearchIndex,
    writer: IndexWriter,
    last_id: i64,
    /// The last `reindex_log` entry applied.
    reindex_seq: i64,
    pending: usize,
}

impl std::fmt::Debug for Indexer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Indexer")
            .field("last_id", &self.last_id)
            .field("pending", &self.pending)
            .finish()
    }
}

impl Indexer {
    /// The highest id added (committed or not).
    pub fn last_id(&self) -> i64 {
        self.last_id
    }

    /// Documents added since the last commit.
    pub fn pending(&self) -> usize {
        self.pending
    }

    pub fn add(&mut self, doc: &TorrentDoc) -> Result<()> {
        debug_assert!(doc.id as i64 > self.last_id, "ids arrive in order");
        self.writer
            .add_document(doc.to_document(&self.index.fields))?;
        self.last_id = doc.id as i64;
        self.pending += 1;
        Ok(())
    }

    /// Indexes up to `limit` torrents stored after the watermark, then
    /// re-indexes torrents changed since (upgraded partial imports).
    /// Returns how many documents it added or replaced.
    pub fn catch_up(&mut self, store: &mut Store, limit: usize) -> Result<usize> {
        let mut added = self.catch_up_new(store, limit)?;
        for (seq, id) in store.reindex_after(self.reindex_seq, limit)? {
            self.reindex_seq = seq;
            // Ids past the watermark get their current data when reached.
            if id > self.last_id {
                continue;
            }
            self.writer
                .delete_term(Term::from_field_u64(self.index.fields.id, id as u64));
            if let (Some(row), Some(tree)) = (store.torrent(id)?, store.files(id)?) {
                self.writer
                    .add_document(TorrentDoc::new(&row, &tree).to_document(&self.index.fields))?;
            }
            self.pending += 1;
            added += 1;
        }
        Ok(added)
    }

    fn catch_up_new(&mut self, store: &mut Store, limit: usize) -> Result<usize> {
        let ids = store.ids_after(self.last_id, limit)?;
        let mut added = 0;
        for id in ids {
            let (Some(row), Some(tree)) = (store.torrent(id)?, store.files(id)?) else {
                // Deleted between the two reads: nothing to index.
                self.last_id = id;
                continue;
            };
            self.add(&TorrentDoc::new(&row, &tree))?;
            added += 1;
        }
        Ok(added)
    }

    /// Commits what's pending with the watermark and makes it searchable.
    pub fn commit(&mut self) -> Result<()> {
        let mut prepared = self.writer.prepare_commit()?;
        prepared.set_payload(&format!(
            "{PAYLOAD_PREFIX}{};reindex:{}",
            self.last_id, self.reindex_seq
        ));
        prepared.commit()?;
        self.index.reader.reload()?;
        self.pending = 0;
        Ok(())
    }

    /// Waits for background merges, for benchmarks and shutdown.
    pub fn finish(self) -> Result<()> {
        self.writer.wait_merging_threads()?;
        self.index.reader.reload()?;
        Ok(())
    }
}
