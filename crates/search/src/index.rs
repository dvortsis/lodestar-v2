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
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, PoisonError, RwLock};

use lodestar_classify::{Composition, Record};
use lodestar_metainfo::InfoHash;
use lodestar_store::{Col, HotColumns, NEVER, Store};
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
use crate::population::{self, Availability, FieldAvailability, Generations, PopState};
use crate::query::{Node, When};
use crate::retry_dir::Retrying;
use crate::schema::{self, Fields, SCHEMA_VERSION, TOKENIZER_VERSION};
use crate::sorted::{self, By, SegmentCache};
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

/// What a [`SearchIndex`]'s caches hold: the `file:(…)` id sets (an
/// 8 MiB budget, `files.rs`) and the browse cache's segment tops
/// (bounded in entries, `sorted.rs`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CacheUsage {
    pub file_set_entries: u64,
    /// Retained bytes, as the budget charges them.
    pub file_set_bytes: u64,
    pub browse_entries: u64,
    /// Estimated: keys and tops at capacity, plus the map's slots.
    pub browse_bytes: u64,
}

#[derive(Clone)]
pub struct SearchIndex {
    index: Index,
    reader: IndexReader,
    fields: Fields,
    cache: Arc<SegmentCache>,
    files: FileIndex,
    /// Swarm stats and last seen (§7), for filters, sorts and ranking.
    /// Without them, hot-column clauses match nothing and relevance is
    /// text only.
    hot: Option<Arc<HotColumns>>,
    weights: RankWeights,
    penalty: Option<Penalty>,
    /// Documents per parallel piece of a cold top-k ([`sorted`]).
    sort_piece: u32,
    /// An older index serving during a rebuild whose schema differs from
    /// this build's: its fields are mapped by name, and documents built
    /// for this schema can't be written to it.
    read_only: bool,
    /// The fields of [`SearchIndex::missing_fields`].
    missing: Arc<[String]>,
    /// Held by every clone, so [`SearchIndex::holders`] tells when the
    /// last one (a lease's included) is gone.
    holders: Arc<()>,
    /// The title FST beside this index (B7, `suggest::titles`).
    titles: Arc<crate::suggest::Titles>,
    /// The two indexes' mmap caches, pruned after each commit.
    mmaps: MmapCaches,
    /// The tracked fields' generations this index is built to.
    generations: Arc<Generations>,
    /// What the torrent and file indexes' latest commits record for the
    /// tracked fields, brought up to `generations`; the writer updates it
    /// at each commit ([`SearchIndex::availability`]).
    population: Arc<RwLock<PopView>>,
}

/// Tantivy's `MmapDirectory` (0.27) caches a weak entry for every file it
/// has ever mapped, keyed by path. Deleting a file leaves its entry, and
/// only `get_cache_info()` drops the dead ones, so without a prune every
/// commit's new segment files (and every merge's) stay in the map for the
/// life of the process: about 2 MiB/h at a live crawl's commit rate
/// (X-mmap-cache-prune). The directories are `Arc`s inside, so these
/// clones share the caches the indexes use.
#[derive(Clone)]
struct MmapCaches {
    /// The torrent and file indexes' directories; none for a RAM index.
    dirs: Arc<[MmapDirectory]>,
    /// Live mappings each cache kept at its latest prune.
    entries: Arc<[AtomicU64; 2]>,
    /// Off only for the regression test's and M1-exp's control.
    on: Arc<AtomicBool>,
}

impl MmapCaches {
    fn new(dirs: Vec<MmapDirectory>) -> MmapCaches {
        MmapCaches {
            dirs: dirs.into(),
            entries: Arc::default(),
            on: Arc::new(AtomicBool::new(true)),
        }
    }

    /// Drops the entries of files no longer mapped. It takes each cache's
    /// write lock for a scan of its entries (a few hundred live files).
    fn prune(&self) {
        if !self.on.load(Ordering::Relaxed) {
            return;
        }
        for (dir, n) in self.dirs.iter().zip(self.entries.iter()) {
            n.store(dir.get_cache_info().mmapped.len() as u64, Ordering::Relaxed);
        }
    }
}

/// A per-torrent ranking multiplier in `0..=1`, by torrent id: the hook
/// for penalties computed outside the index, such as the scraper's
/// swarm-consistency check (§8: one tracker claiming a huge swarm that
/// nothing else sees).
pub type Penalty = Arc<dyn Fn(u64) -> f32 + Send + Sync>;

/// The phase-2 ranking priors (§8). Each multiplies the text score; the
/// defaults keep text relevance first and let swarm health and freshness
/// order near-ties.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RankWeights {
    /// `1 + seeders · ln(1 + s)`.
    pub seeders: f32,
    /// Seeders assumed for a torrent never scraped: neutral, between a
    /// dead swarm (0) and a live one, so new torrents aren't buried
    /// before their first scrape.
    pub unscraped_seeders: u32,
    /// `floor + (1 − floor) · ½^(age / half-life)` on the time since the
    /// torrent was last seen in the DHT.
    pub freshness_floor: f32,
    pub freshness_half_life_days: f32,
    /// `1 + trackers · min(t, cap) / cap`: a nudge for trackers reporting.
    pub trackers: f32,
    pub trackers_cap: u32,
    /// Trackers assumed for a torrent never scraped.
    pub unscraped_trackers: u32,
    /// `1 − junk · score / 100` (the junk score, 0–100).
    pub junk: f32,
}

impl Default for RankWeights {
    fn default() -> Self {
        RankWeights {
            seeders: 0.2,
            unscraped_seeders: 1,
            freshness_floor: 0.75,
            freshness_half_life_days: 30.0,
            trackers: 0.1,
            trackers_cap: 5,
            unscraped_trackers: 1,
            junk: 0.5,
        }
    }
}

/// How one hit's score was made (`?explain=1`): the text score times each
/// prior gives `score`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ScoreParts {
    /// Phase 1 (BM25) plus the name and title boosts.
    pub text: f32,
    pub seeders: f32,
    pub freshness: f32,
    pub trackers: f32,
    pub junk: f32,
    pub penalty: f32,
    pub score: f32,
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
        verified: m(f.verified),
        source: m(f.source),
        network: m(f.network),
        payload_sig: m(f.payload_sig),
        group_key: m(f.group_key),
        group_minhash: m(f.group_minhash),
        name_trigram: m(f.name_trigram),
        file_bucket: m(f.file_bucket),
        file_ext_bucket: m(f.file_ext_bucket),
        title_identity: m(f.title_identity),
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
        size_bucket: m(f.size_bucket),
        category: m(f.category),
        pieces_root: m(f.pieces_root),
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

/// What an index's latest commit payload records:
/// `lodestar-watermark:{id};reindex:{seq};pop:{population}`. Indexes
/// from before `reindex:` (v1) or `pop:` (v6, SB-6) lack those parts.
#[derive(Debug, Default)]
struct Payload {
    last_id: i64,
    reindex_seq: i64,
    /// `None` without a `pop:` part, or with no commit at all.
    population: Option<PopState>,
    /// Whether the index has any commit of ours.
    committed: bool,
}

fn payload_of(index: &Index) -> Result<Payload> {
    let Some(p) = index.load_metas()?.payload else {
        return Ok(Payload::default());
    };
    let bad = || Error::Corrupt("unknown commit payload");
    let rest = p.strip_prefix(PAYLOAD_PREFIX).ok_or_else(bad)?;
    let (rest, population) = match rest.split_once(";pop:") {
        Some((rest, pop)) => (rest, Some(PopState::decode(pop).ok_or_else(bad)?)),
        None => (rest, None),
    };
    let (id, seq) = match rest.split_once(";reindex:") {
        Some((id, seq)) => (id, seq),
        None => (rest, "0"),
    };
    Ok(Payload {
        last_id: id.parse().map_err(|_| bad())?,
        reindex_seq: seq.parse().map_err(|_| bad())?,
        population,
        committed: true,
    })
}

fn watermarks_of(index: &Index) -> Result<(i64, i64)> {
    let p = payload_of(index)?;
    Ok((p.last_id, p.reindex_seq))
}

/// An index's population state brought up to `gens`: a new index (no
/// commit of ours) is complete at them, since this build writes all it
/// will hold; a committed one keeps what it recorded, and a field at
/// another generation, or not recorded (an index from before `pop:`),
/// is backfilled over every torrent it holds.
fn population_of(p: &Payload, gens: &Generations, side: population::Side) -> PopState {
    if !p.committed {
        return PopState::fresh(gens, side);
    }
    let mut state = p.population.clone().unwrap_or_default();
    state.reconcile(gens, side, p.last_id);
    state
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
    index
        .tokenizers()
        .register(schema::TRIGRAM_TOKENIZER, schema::trigram_analyzer());
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

/// Result order. Relevance is phase-1 BM25, re-ranked in phase 2 with the
/// name boosts and, with hot columns, the swarm and freshness priors (§8).
/// Never-scraped torrents sort after every scraped one, either way.
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
    Seeders {
        desc: bool,
    },
    Leechers {
        desc: bool,
    },
    Trackers {
        desc: bool,
    },
    /// Last seen in the DHT.
    Seen {
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
    /// The search time (unix seconds), for freshness.
    pub(crate) now: i64,
    /// The query reads hot columns, so cached results expire with them.
    pub(crate) reads_hot: bool,
    /// The hot columns its swarm clauses, sorts and priors read.
    pub(crate) hot: Option<Arc<HotColumns>>,
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
    /// One per hit when asked for ([`SearchIndex::search_page_explained`])
    /// and the order is relevance; empty otherwise.
    pub explain: Vec<ScoreParts>,
}

/// Whether `node` has a date relative to now.
pub(crate) fn relative(node: &Node) -> bool {
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

/// The swarm and freshness priors for phase 2.
pub(crate) struct Priors<'a> {
    hot: &'a HotColumns,
    weights: &'a RankWeights,
    now: i64,
    penalty: Option<&'a Penalty>,
}

impl Priors<'_> {
    /// The multipliers for torrent `id` with junk score `junk`, into
    /// `parts` (whose `text` is already set).
    fn apply(&self, id: u64, junk: Option<u64>, parts: &mut ScoreParts) {
        let w = self.weights;
        let hot = |c| Some(self.hot.get(c, id)).filter(|v| *v != NEVER);
        let s = hot(Col::Seeders).unwrap_or(w.unscraped_seeders);
        parts.seeders = 1.0 + w.seeders * (1.0 + s as f32).ln();
        // Unknown last seen counts as one half-life ago.
        let age_days = hot(Col::LastSeen).map_or(w.freshness_half_life_days, |t| {
            (self.now - i64::from(t)).max(0) as f32 / 86_400.0
        });
        let decay = 0.5f32.powf(age_days / w.freshness_half_life_days.max(f32::MIN_POSITIVE));
        parts.freshness = w.freshness_floor + (1.0 - w.freshness_floor) * decay;
        let t = hot(Col::Trackers).unwrap_or(w.unscraped_trackers);
        let cap = w.trackers_cap.max(1);
        parts.trackers = 1.0 + w.trackers * t.min(cap) as f32 / cap as f32;
        parts.junk = 1.0 - w.junk * junk.unwrap_or(0).min(100) as f32 / 100.0;
        parts.penalty = self.penalty.map_or(1.0, |p| p(id).clamp(0.0, 1.0));
        parts.score = parts.text
            * parts.seeders
            * parts.freshness
            * parts.trackers
            * parts.junk
            * parts.penalty;
    }
}

/// Phase 2 (§8): adds each candidate's score under `boosts`, multiplies
/// in the `priors`, and sorts best first, ties by address.
pub(crate) fn rerank(
    searcher: &Searcher,
    boosts: Option<&dyn Query>,
    priors: Option<&Priors<'_>>,
    hits: Vec<(f32, DocAddress)>,
) -> Result<Vec<(DocAddress, ScoreParts)>> {
    let weight = boosts
        .map(|b| b.weight(EnableScoring::enabled_from_searcher(searcher)))
        .transpose()?;
    let mut hits: Vec<(DocAddress, ScoreParts)> = hits
        .into_iter()
        .map(|(score, addr)| {
            let parts = ScoreParts {
                text: score,
                seeders: 1.0,
                freshness: 1.0,
                trackers: 1.0,
                junk: 1.0,
                penalty: 1.0,
                score,
            };
            (addr, parts)
        })
        .collect();
    hits.sort_unstable_by_key(|(a, _)| *a);
    let mut i = 0;
    while i < hits.len() {
        let seg = hits[i].0.segment_ord;
        let reader = searcher.segment_reader(seg);
        let mut scorer = weight.as_ref().map(|w| w.scorer(reader, 1.0)).transpose()?;
        let columns = match priors {
            Some(_) => {
                let fast = reader.fast_fields();
                Some((fast.u64("id")?, fast.u64("junk").ok()))
            }
            None => None,
        };
        while let Some((addr, parts)) = hits.get_mut(i).filter(|(a, _)| a.segment_ord == seg) {
            if let Some(scorer) = &mut scorer {
                if scorer.doc() < addr.doc_id {
                    scorer.seek(addr.doc_id);
                }
                if scorer.doc() == addr.doc_id {
                    parts.text += scorer.score();
                }
            }
            parts.score = parts.text;
            if let (Some(p), Some((ids, junk))) = (priors, &columns)
                && let Some(id) = ids.first(addr.doc_id)
            {
                let junk = junk.as_ref().and_then(|j| j.first(addr.doc_id));
                p.apply(id, junk, parts);
            }
            i += 1;
        }
    }
    hits.sort_by(|a, b| b.1.score.total_cmp(&a.1.score).then(a.0.cmp(&b.0)));
    Ok(hits)
}

/// The fast field a sort orders by, and whether it's signed.
pub(crate) fn sort_field(sort: Sort) -> Option<(&'static str, bool)> {
    match sort {
        Sort::Relevance => None,
        Sort::Discovered { .. } => Some(("discovered", true)),
        Sort::Size { .. } => Some(("size", false)),
        Sort::Files { .. } => Some(("files", false)),
        // Hot columns, not index fields.
        Sort::Seeders { .. }
        | Sort::Leechers { .. }
        | Sort::Trackers { .. }
        | Sort::Seen { .. } => None,
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
        let (main, file) = (MmapDirectory::open(dir)?, MmapDirectory::open(&files_dir)?);
        let index = Index::open_or_create(Retrying::new(main.clone()), schema)?;
        let (file_schema, file_fields) = files::schema();
        let file_index = Index::builder()
            .schema(file_schema)
            .settings(files::settings())
            .open_or_create(Retrying::new(file.clone()))?;
        SearchIndex::wrap(index, fields, file_index, file_fields, vec![main, file])
    }

    /// Opens the existing index in `dir` with the schema it was built
    /// with, for serving while a newer one is built. Fields this build
    /// added (or retyped) are absent there: clauses on them match nothing
    /// and sorts on them are skipped ([`SearchIndex::missing_fields`]).
    /// `None` when it lacks a field no search works without, which is a
    /// schema mismatch; an error opening it is just an error.
    pub(crate) fn open_existing(dir: &Path) -> Result<Option<(SearchIndex, Compat)>> {
        let main = MmapDirectory::open(dir)?;
        let file = MmapDirectory::open(dir.join("files"))?;
        let index = Index::open(Retrying::new(main.clone()))?;
        let file_index = Index::open(Retrying::new(file.clone()))?;
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
        let mut s = SearchIndex::wrap(index, fields, file_index, file_fields, vec![main, file])?;
        s.read_only = compat == Compat::ReadOnly;
        if s.read_only {
            s.population = Arc::new(RwLock::new(PopView::new(read_only_population(
                &s.index,
                &s.files.index,
            )?)));
        }
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

    /// The file index and its reader, for tests that write raw file
    /// documents.
    #[cfg(test)]
    pub(crate) fn raw_files(&self) -> (&Index, &IndexReader) {
        (&self.files.index, &self.files.reader)
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
            Vec::new(),
        )
    }

    fn wrap(
        mut index: Index,
        fields: Fields,
        mut file_index: Index,
        file_fields: FileFields,
        mmaps: Vec<MmapDirectory>,
    ) -> Result<SearchIndex> {
        let reader = prepare(&mut index)?;
        let file_reader = prepare(&mut file_index)?;
        let mmaps = MmapCaches::new(mmaps);
        // Opening read every live segment's files: start from them.
        mmaps.prune();
        let generations = Arc::new(Generations::current());
        let population = Arc::new(RwLock::new(PopView::new(read_population(
            &index,
            &file_index,
            &generations,
        )?)));
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
            hot: None,
            weights: RankWeights::default(),
            penalty: None,
            sort_piece: sorted::PIECE_DOCS,
            read_only: false,
            missing: Arc::new([]),
            holders: Arc::default(),
            titles: Arc::default(),
            mmaps,
            generations,
            population,
        })
    }

    /// Live mappings each mmap cache (torrent index, file index) kept at
    /// its latest prune; `None` for a RAM index. It can't count dead
    /// entries: a prune drops them. Compare it with the live segment files.
    pub fn mmap_cache_entries(&self) -> Option<[u64; 2]> {
        (!self.mmaps.dirs.is_empty()).then(|| {
            let [a, b] = &*self.mmaps.entries;
            [a.load(Ordering::Relaxed), b.load(Ordering::Relaxed)]
        })
    }

    /// Live segments in each index's current searcher (torrent index,
    /// file index), for `/metrics`. Loads each reader's current searcher:
    /// an atomic pointer load and an `Arc` clone, no lock.
    pub fn segment_counts(&self) -> [u64; 2] {
        [
            self.reader.searcher().segment_readers().len() as u64,
            self.files.reader.searcher().segment_readers().len() as u64,
        ]
    }

    /// Turns the prune after each commit off (or back on), for every clone
    /// of this index. Only the regression test and M1-exp's control arm
    /// turn it off.
    #[doc(hidden)]
    pub fn set_mmap_cache_pruning(&self, on: bool) {
        self.mmaps.on.store(on, Ordering::Relaxed);
    }

    /// Searches with these hot columns (§7). Set it before cloning the
    /// index for other threads.
    pub fn with_hot(mut self, hot: Arc<HotColumns>) -> SearchIndex {
        self.hot = Some(hot);
        self
    }

    pub fn hot(&self) -> Option<&Arc<HotColumns>> {
        self.hot.as_ref()
    }

    /// Ranking priors other than the defaults (config, §8: "all weights
    /// live in config").
    pub fn with_weights(mut self, weights: RankWeights) -> SearchIndex {
        self.weights = weights;
        self
    }

    /// Ranks every torrent by this multiplier as well (see [`Penalty`]).
    pub fn with_penalty(mut self, penalty: Penalty) -> SearchIndex {
        self.penalty = Some(penalty);
        self
    }

    /// The phase-2 priors at `now`, when hot columns are attached.
    pub(crate) fn priors(&self, now: i64) -> Option<Priors<'_>> {
        self.hot.as_deref().map(|hot| Priors {
            hot,
            weights: &self.weights,
            now,
            penalty: self.penalty.as_ref(),
        })
    }

    /// Documents per parallel piece of a cold sorted search.
    pub(crate) fn sort_piece(&self) -> u32 {
        self.sort_piece
    }

    /// Splits cold sorted searches into pieces of `docs` documents, for
    /// tests: small indexes are otherwise one piece per segment.
    #[doc(hidden)]
    pub fn with_sort_pieces(mut self, docs: u32) -> SearchIndex {
        self.sort_piece = docs.max(1);
        self.cache.clear();
        self
    }

    pub fn fields(&self) -> &Fields {
        &self.fields
    }

    /// Builds to these generations instead of this build's, for tests
    /// that play a producer shipping (and the bench that measures a
    /// backfill). Set it before taking a writer or cloning the index.
    #[doc(hidden)]
    pub fn with_generations(mut self, generations: Generations) -> Result<SearchIndex> {
        let state = if self.read_only {
            read_only_population(&self.index, &self.files.index)?
        } else {
            read_population(&self.index, &self.files.index, &generations)?
        };
        self.generations = Arc::new(generations);
        self.population = Arc::new(RwLock::new(PopView::new(state)));
        Ok(self)
    }

    /// Whether the v7 bucket terms answer `file:(size:…)` here: both are
    /// in this index and complete ([`crate::population`]) in the snapshot
    /// the query runs on (`snapshot`, a searcher's generation; `None` for
    /// the latest, which a search without a lease reads). An older index
    /// serving during the rebuild lacks them, and a backfill of them
    /// isn't complete until it is, in that snapshot: either way the file
    /// index answers.
    pub(crate) fn bucket_terms_ready(&self, snapshot: Option<u64>) -> bool {
        ["file_bucket", "file_ext_bucket"]
            .iter()
            .all(|f| self.availability_at(f, snapshot) == Availability::Ready)
    }

    /// How much of `field` this index can answer for (file-index fields
    /// as `file.<name>`). A field this build added that the index lacks
    /// is [`Availability::Missing`], and so is a name neither index has;
    /// a field v6 added is tracked
    /// ([`crate::population`]); any other is [`Availability::Ready`].
    /// It follows the writer's latest commit, which a lease's older
    /// snapshot may predate: a prefilter asks [`SearchIndex::prefilter`],
    /// which checks the snapshot it runs on.
    pub fn availability(&self, field: &str) -> Availability {
        let known = match field.strip_prefix("file.") {
            Some(name) => self.files.index.schema().get_field(name).is_ok(),
            None => self.index.schema().get_field(field).is_ok(),
        };
        // A name no index has (a typo, say) fails closed: it's missing,
        // so nothing ever narrows by it.
        if !known || self.missing.iter().any(|m| m == field) {
            return Availability::Missing;
        }
        self.population
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .availability(field)
            .unwrap_or(Availability::Ready)
    }

    /// Every field that isn't [`Availability::Ready`]: the missing ones
    /// ([`SearchIndex::missing_fields`]) first, then the tracked ones
    /// without a producer or being backfilled, so a query or the status
    /// page can say what's partial and how far along it is.
    pub fn unpopulated(&self) -> Vec<FieldAvailability> {
        let mut out: Vec<FieldAvailability> = self
            .missing
            .iter()
            .map(|f| FieldAvailability {
                field: f.clone(),
                availability: Availability::Missing,
            })
            .collect();
        let view = self
            .population
            .read()
            .unwrap_or_else(PoisonError::into_inner);
        for s in view.states.iter() {
            for f in s.fields() {
                if out.iter().any(|o| o.field == f) {
                    continue;
                }
                match s.availability(f) {
                    Some(Availability::Ready) | None => {}
                    Some(availability) => out.push(FieldAvailability {
                        field: f.to_owned(),
                        availability,
                    }),
                }
            }
        }
        out
    }

    /// `query` as a prefilter on `field`, to run on `searcher` (a
    /// snapshot of the index the field is in: the file index's for a
    /// `file.` field), or `None` when the field isn't complete in that
    /// snapshot: a document a backfill hadn't reached there (or written
    /// before any producer) would fail it and look like "no match", so
    /// the caller must not narrow by it and verifies every candidate
    /// instead. Every prefilter on a tracked field goes through here.
    pub(crate) fn prefilter(
        &self,
        field: &str,
        searcher: &Searcher,
        query: Box<dyn Query>,
    ) -> Option<Box<dyn Query>> {
        if !self.availability(field).complete() {
            return None;
        }
        let view = self
            .population
            .read()
            .unwrap_or_else(PoisonError::into_inner);
        let seen = searcher.generation().generation_id();
        view.ready_since
            .get(field)
            .is_none_or(|&(since, _)| seen >= since)
            .then_some(query)
    }

    /// [`SearchIndex::availability`] as `lease`'s snapshot sees it: a
    /// torrent-index field that completed after the lease's snapshot is
    /// still what it was there (SRC-3: a later page of a lease from
    /// mid-backfill keeps its "partial" notice). File-index fields
    /// follow the latest commit.
    pub fn availability_in(&self, field: &str, lease: &crate::Lease) -> Availability {
        self.availability_at(field, Some(lease.searcher().generation().generation_id()))
    }

    /// [`SearchIndex::availability`] as of the snapshot of generation
    /// `snapshot` (`None`: the latest).
    fn availability_at(&self, field: &str, snapshot: Option<u64>) -> Availability {
        let now = self.availability(field);
        let Some(seen) = snapshot else {
            return now;
        };
        if now != Availability::Ready || field.starts_with("file.") {
            return now;
        }
        let view = self
            .population
            .read()
            .unwrap_or_else(PoisonError::into_inner);
        match view.ready_since.get(field) {
            Some(&(since, before)) if seen < since => before,
            _ => now,
        }
    }

    /// Alive while any clone of this index is: a retired index's
    /// directory is deleted only after that (leases, `lease.rs`).
    pub(crate) fn holders(&self) -> std::sync::Weak<()> {
        Arc::downgrade(&self.holders)
    }

    /// The title set typeahead completes from (B7), rebuilt as the live
    /// indexer commits.
    pub fn titles(&self) -> &Arc<crate::suggest::Titles> {
        &self.titles
    }

    /// The current snapshot, which a lease pins.
    pub(crate) fn searcher(&self) -> Searcher {
        self.reader.searcher()
    }

    /// The file index's current snapshot (the result cache's key).
    pub(crate) fn files_searcher(&self) -> Searcher {
        self.files.reader.searcher()
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

    /// Makes everything committed so far visible to this handle's
    /// searches (the live indexer does this after each commit).
    pub fn refresh(&self) -> Result<()> {
        self.reader.reload()?;
        Ok(())
    }

    /// The highest torrent id indexed (0 for a new index).
    pub fn watermark(&self) -> Result<i64> {
        Ok(watermarks_of(&self.index)?.0)
    }

    /// The last `reindex_log` entry both indexes have committed (0 for a
    /// new index). A torrent id that entry names is searchable once this
    /// handle is [refreshed](Self::refresh): the file index commits and
    /// reloads before the torrent index commits.
    pub fn reindex_watermark(&self) -> Result<i64> {
        Ok(watermarks_of(&self.index)?
            .1
            .min(watermarks_of(&self.files.index)?.1))
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

    /// Adult documents (`type:adult`) with ids in `(after, upto]`, in
    /// the current searcher: what an index swap made visible that's
    /// hidden while Adult is.
    pub(crate) fn adult_between(&self, after: i64, upto: i64) -> Result<u64> {
        let f = &self.fields;
        // Torrent ids come from SQLite's rowid sequence and start at 1;
        // a watermark is 0 at the lowest, so `max(0)` never changes one.
        let id = |v: i64| Term::from_field_u64(f.id, v.max(0) as u64);
        let adult: Box<dyn Query> = Box::new(TermQuery::new(
            Term::from_field_text(f.tag, &adult_tag()),
            IndexRecordOption::Basic,
        ));
        let range: Box<dyn Query> = Box::new(tantivy::query::RangeQuery::new(
            std::ops::Bound::Excluded(id(after)),
            std::ops::Bound::Included(id(upto)),
        ));
        let q = BooleanQuery::new(vec![(Occur::Must, adult), (Occur::Must, range)]);
        Ok(self.searcher().search(&q, &Count)? as u64)
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
        let gens = &self.generations;
        let torrents = Side::new(
            &self.index,
            heap_bytes.saturating_sub(file_heap).max(15 << 20),
            gens,
            population::Side::Torrents,
        )?;
        let files = Side::new(&self.files.index, file_heap, gens, population::Side::Files)?;
        Ok(Indexer {
            index: self.clone(),
            torrents,
            files,
            pending: 0,
            sources: SourceNames::default(),
        })
    }

    /// Compiles a parsed query; `now` (unix seconds) resolves relative
    /// dates. Times of day are UTC; see [`SearchIndex::query_at`].
    pub fn query(&self, node: &Node, now: i64) -> Compiled {
        self.query_at(node, now, 0)
    }

    /// Compiles a parsed query for a user whose zone is `utc_offset`
    /// seconds east of UTC (it only affects `time:`).
    pub fn query_at(&self, node: &Node, now: i64, utc_offset: i32) -> Compiled {
        self.compile(node, now, utc_offset, false)
    }

    /// [`SearchIndex::query_at`] under `cancel`: its `file:(…)` clauses
    /// stop scanning the file index once the token stops, and nothing is
    /// cached for a scan cut short. `Err(Cancelled)` or `Err(OutOfTime)`
    /// then.
    pub fn query_cancelled(
        &self,
        node: &Node,
        now: i64,
        utc_offset: i32,
        cancel: &crate::CancelToken,
    ) -> Result<Compiled> {
        self.compile_cancelled(node, now, utc_offset, false, cancel)
    }

    /// `node`, compiled, and narrowed to the torrents whose v7
    /// `title_identity` term is `key` ([`crate::doc::TitleIdentityParts::key`]):
    /// "other versions of this" by one term (B13b), where a phrase query
    /// over a short title can match too many torrents to rank them all.
    /// The term adds nothing to the score, so `node` ranks as it would
    /// alone. `None` when this index can't answer it: the field is
    /// missing (an older index serving during the rebuild) or not
    /// complete (a backfill of it running), so the caller searches
    /// without it rather than read an unpopulated field as "none".
    /// Decided by [`SearchIndex::prefilter`] on the current snapshot: a
    /// search without a lease reads that one or a newer one, and a field
    /// complete in a snapshot stays complete in every later one.
    pub fn query_title_identity(
        &self,
        node: &Node,
        key: u64,
        now: i64,
        utc_offset: i32,
        cancel: &crate::CancelToken,
    ) -> Result<Option<Compiled>> {
        if self.fields.title_identity == compile::ABSENT {
            return Ok(None);
        }
        let term: Box<dyn Query> = Box::new(tantivy::query::ConstScoreQuery::new(
            Box::new(TermQuery::new(
                Term::from_field_u64(self.fields.title_identity, key),
                IndexRecordOption::Basic,
            )),
            0.0,
        ));
        let Some(term) = self.prefilter("title_identity", &self.searcher(), term) else {
            return Ok(None);
        };
        let mut compiled = self.compile_cancelled(node, now, utc_offset, false, cancel)?;
        let query = std::mem::replace(&mut compiled.query, Box::new(tantivy::query::EmptyQuery));
        compiled.query = Box::new(BooleanQuery::new(vec![
            (Occur::Must, query),
            (Occur::Must, term),
        ]));
        compiled.key.push_str(&format!("@identity{key}"));
        Ok(Some(compiled))
    }

    /// The same query with the stem tier: each stemmable word also
    /// matches by its light stem in the title (§8). `None` when no word
    /// has a stem alternative, so it would match the same torrents.
    pub fn query_stemmed(&self, node: &Node, now: i64, utc_offset: i32) -> Option<Compiled> {
        compile::has_stem_alternative(node).then(|| self.compile(node, now, utc_offset, true))
    }

    /// One page of results, falling back to the stem tier when the exact
    /// query finds fewer than [`STEM_FALLBACK_BELOW`] torrents and the
    /// tier finds more. The flag says whether it did. Stem-only matches
    /// rank after exact ones, and a query's total decides, so every page
    /// of it takes the same branch. Times of day are UTC.
    pub fn search_with_fallback(
        &self,
        node: &Node,
        now: i64,
        sort: Sort,
        limit: usize,
        offset: usize,
    ) -> Result<(Results, bool)> {
        self.search_with_fallback_at(node, now, 0, sort, limit, offset)
    }

    /// [`SearchIndex::search_with_fallback`] for a user `utc_offset`
    /// seconds east of UTC.
    pub fn search_with_fallback_at(
        &self,
        node: &Node,
        now: i64,
        utc_offset: i32,
        sort: Sort,
        limit: usize,
        offset: usize,
    ) -> Result<(Results, bool)> {
        let exact = self.search_page(&self.query_at(node, now, utc_offset), sort, limit, offset)?;
        if exact.total >= STEM_FALLBACK_BELOW {
            return Ok((exact, false));
        }
        let Some(stemmed) = self.query_stemmed(node, now, utc_offset) else {
            return Ok((exact, false));
        };
        let wider = self.search_page(&stemmed, sort, limit, offset)?;
        Ok(if wider.total > exact.total {
            (wider, true)
        } else {
            (exact, false)
        })
    }

    pub(crate) fn compile(&self, node: &Node, now: i64, utc_offset: i32, stems: bool) -> Compiled {
        self.compile_cancelled(node, now, utc_offset, stems, &crate::CancelToken::new())
            .expect("uncancelled compilation preserves file-query fallback")
    }

    pub(crate) fn compile_cancelled(
        &self,
        node: &Node,
        now: i64,
        utc_offset: i32,
        stems: bool,
        cancel: &crate::CancelToken,
    ) -> Result<Compiled> {
        self.compile_at(node, now, utc_offset, stems, cancel, None)
    }

    /// [`SearchIndex::compile_cancelled`] for a query that runs on the
    /// snapshot of generation `snapshot` (a lease's): what it may read
    /// is decided as of that snapshot, not the latest commit.
    pub(crate) fn compile_at(
        &self,
        node: &Node,
        now: i64,
        utc_offset: i32,
        stems: bool,
        cancel: &crate::CancelToken,
        snapshot: Option<u64>,
    ) -> Result<Compiled> {
        cancel.check()?;
        // `file:(…)` clauses run on the file index first, but for what
        // the torrent index's bucket terms answer (X-search-files-5).
        let buckets = self.bucket_terms_ready(snapshot);
        let mut clauses = Vec::new();
        compile::search_file_clauses(node, buckets, &mut clauses);
        let mut sets = compile::FileSets::new();
        let files_searcher = self.files.reader.searcher();
        for of in &clauses {
            let key = format!("{of:?}");
            cancel.check()?;
            // A clause of extensions less rare words: the bad torrents
            // only, read through the words (X-search-files-6).
            if let Some(neg) = compile::negated_words(of).filter(|_| buckets) {
                match files::bad_torrents(
                    &files_searcher,
                    &self.files.fields,
                    &neg,
                    &self.files.cache,
                    cancel,
                ) {
                    Ok(Some(bad)) => {
                        sets.insert(compile::rescued_key(of), Arc::new(bad));
                        continue;
                    }
                    Ok(None) => {}
                    Err(e @ (Error::Cancelled | Error::OutOfTime)) => return Err(e),
                    // The whole clause on the file index, below.
                    Err(_) => {}
                }
            }
            let query = files::compile(of, &self.files.fields);
            // Tests swap in a query that cancels partway (`testing`).
            #[cfg(test)]
            let query = crate::testing::FILE_QUERY
                .with(|q| q.borrow_mut().take())
                .or(query);
            let ids = match query {
                Some(q) => match files::torrent_ids(
                    &files_searcher,
                    q.as_ref(),
                    &key,
                    &self.files.cache,
                    cancel,
                ) {
                    Ok(ids) => ids,
                    Err(e @ (Error::Cancelled | Error::OutOfTime)) => return Err(e),
                    Err(_) => {
                        lodestar_log::log_event!(
                            crate::log_events::FILE_SEARCH_FAILED,
                            error = lodestar_log::Error::kind("tantivy")
                        );
                        files::IdSet::default()
                    }
                },
                None => files::IdSet::default(),
            };
            sets.insert(key, Arc::new(ids));
        }
        let cx = compile::Cx {
            fields: &self.fields,
            files: &sets,
            stems,
            hot: self.hot.as_ref(),
            utc_offset,
            buckets,
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
        if compile::has_time_of_day(node) {
            key.push_str(&format!("@tz{utc_offset}"));
        }
        // A bucket clause is a different query with the bucket terms than
        // without them (X-search-files-5).
        if buckets {
            key.push_str("@buckets");
        }
        cancel.check()?;
        Ok(Compiled {
            query,
            filters,
            boosts,
            key,
            now,
            reads_hot: compile::reads_hot(node),
            hot: self.hot.clone(),
        })
    }

    /// What this index's own caches hold, for `/metrics`.
    pub fn cache_usage(&self) -> CacheUsage {
        let (file_set_entries, file_set_bytes) = self.files.cache.usage();
        let (browse_entries, browse_bytes) = self.cache.usage();
        CacheUsage {
            file_set_entries,
            file_set_bytes,
            browse_entries,
            browse_bytes,
        }
    }

    /// The file-set cache's hits and misses since start (one per clause
    /// and file-index segment), for benchmarks.
    pub fn file_cache_hits(&self) -> (u64, u64) {
        self.files.cache.hits()
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
            now: 0,
            reads_hot: false,
            hot: None,
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
        self.page(compiled, sort, limit, offset, false)
    }

    /// [`SearchIndex::search_page`] with each relevance hit's score parts
    /// in [`Results::explain`] (`?explain=1`).
    pub fn search_page_explained(
        &self,
        compiled: &Compiled,
        sort: Sort,
        limit: usize,
        offset: usize,
    ) -> Result<Results> {
        self.page(compiled, sort, limit, offset, true)
    }

    fn page(
        &self,
        compiled: &Compiled,
        sort: Sort,
        limit: usize,
        offset: usize,
        explain: bool,
    ) -> Result<Results> {
        let searcher = self.reader.searcher();
        let query = compiled.query.as_ref();
        let ranges = &compiled.filters;
        let hot = self.hot.as_ref();
        let top = TopDocs::with_limit(limit.max(1)).and_offset(offset);
        let mut parts = Vec::new();
        let (scored, total): (Vec<(f32, DocAddress)>, usize) = match sort {
            Sort::Relevance => {
                let priors = self.priors(compiled.now);
                if compiled.boosts.is_none() && priors.is_none() && !explain {
                    let inner = (top.order_by_score(), Count);
                    searcher.search(query, &PostFilter { ranges, hot, inner })?
                } else {
                    // Two-phase (§8): the best candidates by phase-1
                    // score, re-ranked with the name boosts and the
                    // priors. Order is exact within the window.
                    let window = (offset + limit).max(RERANK_WINDOW);
                    let inner = (TopDocs::with_limit(window).order_by_score(), Count);
                    let (candidates, n) =
                        searcher.search(query, &PostFilter { ranges, hot, inner })?;
                    // Only the first window is re-ranked, whatever page is
                    // asked: re-ranking a deeper window would reorder hits
                    // other pages already showed. Past it, phase-1 order
                    // holds (with the parts still computed for explain).
                    let mut candidates = candidates;
                    let tail = candidates.split_off(candidates.len().min(RERANK_WINDOW));
                    let mut ranked = rerank(
                        &searcher,
                        compiled.boosts.as_deref(),
                        priors.as_ref(),
                        candidates,
                    )?;
                    if !tail.is_empty() {
                        let order: std::collections::HashMap<DocAddress, usize> =
                            tail.iter().enumerate().map(|(i, (_, a))| (*a, i)).collect();
                        let mut rest =
                            rerank(&searcher, compiled.boosts.as_deref(), priors.as_ref(), tail)?;
                        rest.sort_by_key(|(a, _)| order[a]);
                        ranked.extend(rest);
                    }
                    let page: Vec<_> = ranked.into_iter().skip(offset).take(limit).collect();
                    if explain {
                        parts = page.iter().map(|(_, p)| *p).collect();
                    }
                    (page.into_iter().map(|(a, p)| (p.score, a)).collect(), n)
                }
            }
            Sort::Discovered { desc }
            | Sort::Size { desc }
            | Sort::Files { desc }
            | Sort::Seeders { desc }
            | Sort::Leechers { desc }
            | Sort::Trackers { desc }
            | Sort::Seen { desc } => {
                // An older index serving during a rebuild may lack the
                // field: relevance instead (B5b).
                if !self.sort_available(sort) {
                    return self.page(compiled, Sort::Relevance, limit, offset, explain);
                }
                let by = match (sort_field(sort), sort) {
                    (Some((field, signed)), _) => By::Fast { field, signed },
                    (None, Sort::Seeders { .. }) => By::Hot(Col::Seeders),
                    (None, Sort::Leechers { .. }) => By::Hot(Col::Leechers),
                    (None, Sort::Trackers { .. }) => By::Hot(Col::Trackers),
                    _ => By::Hot(Col::LastSeen),
                };
                let order = sorted::Order { by, desc };
                let (page, n) = sorted::search(
                    &searcher,
                    &self.cache,
                    compiled,
                    order,
                    offset..offset + limit,
                    self.sort_piece,
                )?;
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
        Ok(Results {
            hits,
            total,
            explain: parts,
        })
    }

    /// The ids of every torrent `compiled` matches, at most `limit`, in no
    /// particular order: for jobs over a query's whole result set (a
    /// classification rule's torrents, §20.9), not for display.
    /// `true` with them when more matched than `limit`.
    pub fn matching_ids(&self, compiled: &Compiled, limit: usize) -> Result<(Vec<u64>, bool)> {
        let searcher = self.reader.searcher();
        let left = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(limit));
        let over = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let collector = PostFilter {
            ranges: &compiled.filters,
            hot: compiled.hot.as_ref(),
            inner: IdsCollector {
                left: left.clone(),
                over: over.clone(),
            },
        };
        let ids = searcher.search(compiled.query.as_ref(), &collector)?;
        Ok((ids, over.load(std::sync::atomic::Ordering::Relaxed)))
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
fn torrent_doc(store: &mut Store, t: &Loaded, sources: &mut SourceNames) -> Result<TorrentDoc> {
    let mut doc = TorrentDoc::new(&t.row, &t.tree);
    doc.enriched = doc_enrichment(store, t.row.id)?;
    doc.provenance.sources = sources.of(store, t.row.sources)?;
    Ok(doc)
}

/// The `source:` term of each source bit (SRC-3), read from the store's
/// registry when a torrent names a bit not seen yet. Ids are never
/// reused and a plugin's id never changes, so an entry never goes stale.
#[derive(Debug, Default)]
struct SourceNames {
    names: Vec<Option<String>>,
    /// Bits looked up: registered, or found unregistered once (a
    /// corrupt bit, read again only after the next new source).
    known: u64,
}

impl SourceNames {
    fn of(&mut self, store: &Store, bits: u64) -> Result<Vec<String>> {
        if bits & !self.known != 0 {
            let rows = store.sources()?;
            self.names = vec![None; usize::from(lodestar_store::MAX_SOURCES)];
            for r in rows {
                self.names[usize::from(r.id.get())] = crate::doc::source_term(&r.plugin);
            }
            self.known |= bits;
            self.known |= self
                .names
                .iter()
                .enumerate()
                .filter(|(_, n)| n.is_some())
                .fold(0, |m, (i, _)| m | 1 << i);
        }
        Ok((0..64)
            .filter(|b| bits >> b & 1 == 1)
            .filter_map(|b| self.names.get(b).cloned().flatten())
            .collect())
    }
}

/// The enrichment half of torrent `id`'s document.
fn doc_enrichment(store: &mut Store, id: i64) -> Result<DocEnrichment> {
    if let Some(e) = store.enrichment(id)? {
        match Record::decode(&e.data) {
            Ok(record) => {
                // An empty or unreadable `comp` only loses the shares.
                let comp = Composition::decode(&e.comp).ok();
                return Ok(DocEnrichment::new(&record, comp.as_ref()));
            }
            Err(_) => lodestar_log::log_event!(
                crate::log_events::ENRICHMENT_UNREADABLE,
                torrent_id = id as u64
            ),
        }
    }
    Ok(DocEnrichment::default())
}

/// The term an Adult document is indexed with, its effective class
/// (§20.9): what B17's `-type:adult` visibility clause excludes.
fn adult_tag() -> String {
    format!(
        "{}:{}",
        crate::query::TagKey::Type.as_str(),
        lodestar_classify::ContentType::Adult.as_str()
    )
}

/// Whether a document with this enrichment is indexed with `type:adult`.
fn is_adult(e: &DocEnrichment) -> bool {
    e.tags.contains(&adult_tag())
}

/// Whether torrent `id`, indexed from the store as it is now, would be
/// `type:adult`.
fn stored_adult(store: &mut Store, id: i64) -> Result<bool> {
    Ok(is_adult(&doc_enrichment(store, id)?))
}

/// The tracked fields' population as the latest commits record it, and
/// for each field that became complete while this index was open, the
/// first searcher generation (of its own index's reader) that sees it
/// complete: an older snapshot, such as a lease's, still lacks it there.
#[derive(Debug, Default)]
struct PopView {
    states: [PopState; 2],
    /// The generation, and what the field was just before (its walk's
    /// last `Backfilling`), for what [`SearchIndex::availability_in`]
    /// tells a lease on an older snapshot.
    ready_since: std::collections::BTreeMap<String, (u64, Availability)>,
}

impl PopView {
    fn new(states: [PopState; 2]) -> PopView {
        PopView {
            states,
            ready_since: Default::default(),
        }
    }

    fn availability(&self, field: &str) -> Option<Availability> {
        self.states.iter().find_map(|s| s.availability(field))
    }
}

/// What a read-only index (another schema's, serving during a rebuild)
/// records, as recorded: it takes no writes, so nothing is reconciled
/// with this build's generations. A field it recorded at generation 0 has
/// no producer there, an open walk is still open. Without a `pop:` part
/// it predates the contract, when a field only ever came with a full
/// rebuild: each it has is complete, so nothing is recorded.
fn read_only_population(index: &Index, files: &Index) -> Result<[PopState; 2]> {
    Ok([
        payload_of(index)?.population.unwrap_or_default(),
        payload_of(files)?.population.unwrap_or_default(),
    ])
}

/// Both indexes' population states, brought up to `gens`.
fn read_population(index: &Index, files: &Index, gens: &Generations) -> Result<[PopState; 2]> {
    Ok([
        population_of(&payload_of(index)?, gens, population::Side::Torrents),
        population_of(&payload_of(files)?, gens, population::Side::Files),
    ])
}

/// One index's writer and how far it has followed the store.
struct Side {
    writer: IndexWriter,
    last_id: i64,
    /// The last `reindex_log` entry applied.
    reindex_seq: i64,
    /// Its tracked fields' population, committed with the watermarks.
    population: PopState,
}

impl Side {
    fn new(
        index: &Index,
        heap_bytes: usize,
        gens: &Generations,
        side: population::Side,
    ) -> Result<Side> {
        let p = payload_of(index)?;
        Ok(Side {
            writer: index.writer_with_num_threads(1, heap_bytes)?,
            last_id: p.last_id,
            reindex_seq: p.reindex_seq,
            population: population_of(&p, gens, side),
        })
    }

    fn commit(&mut self, reader: &IndexReader) -> Result<()> {
        let mut prepared = self.writer.prepare_commit()?;
        prepared.set_payload(&format!(
            "{PAYLOAD_PREFIX}{};reindex:{};pop:{}",
            self.last_id,
            self.reindex_seq,
            self.population.encode()
        ));
        prepared.commit()?;
        reader.reload()?;
        Ok(())
    }
}

/// What one [`Indexer::catch_up_counted`] step did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CatchUp {
    /// Torrents new to the index. A re-indexed torrent counts only as
    /// replayed, whatever its class.
    pub added: usize,
    /// How many of `added` are Adult: indexed with `type:adult`, the term
    /// the visibility clause `-type:adult` excludes, so the count and
    /// search agree (§20.9).
    pub adult_added: usize,
    /// `reindex_log` entries replayed.
    pub replayed: usize,
    /// Torrents re-indexed by a population backfill
    /// ([`crate::population`]), counted once whichever index needed it.
    pub backfilled: usize,
}

/// Adds torrents to both indexes in id order and commits each with its
/// watermark.
pub struct Indexer {
    index: SearchIndex,
    torrents: Side,
    files: Side,
    pending: usize,
    sources: SourceNames,
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
    /// enrichment) and those a population backfill still has to visit.
    /// Returns how many torrents it added or replaced: 0 once caught up.
    pub fn catch_up(&mut self, store: &mut Store, limit: usize) -> Result<usize> {
        let c = self.catch_up_counted(store, limit)?;
        Ok(c.added + c.replayed + c.backfilled)
    }

    /// [`Indexer::catch_up`], counting the torrents added and the
    /// `reindex_log` entries replayed apart (see [`CatchUp`]).
    pub fn catch_up_counted(&mut self, store: &mut Store, limit: usize) -> Result<CatchUp> {
        for side in [&mut self.torrents, &mut self.files] {
            if side.last_id == 0 && side.reindex_seq == 0 {
                // A new index reads every torrent's current data, so the
                // changes logged before it started are already in it.
                side.reindex_seq = store.reindex_head()?;
            }
        }
        let mut counted = self.catch_up_new(store, limit)?;
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
                        let doc = torrent_doc(store, t, &mut self.sources)?;
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
            counted.replayed += 1;
        }
        // A backfill takes what's left of the step, but at least an
        // eighth of it, so a busy crawl slows it without starving it.
        let budget = limit
            .saturating_sub(counted.added + counted.replayed)
            .max(limit / 8)
            .max(1);
        counted.backfilled = self.backfill(store, budget)?;
        Ok(counted)
    }

    /// Re-indexes up to `budget` torrents for the indexes with a
    /// population walk under way, from the store as it is now: a torrent
    /// deleted meanwhile is dropped, one changed since is also replayed
    /// from its `reindex_log` entry. The walks' cursors commit with the
    /// documents, so a restart resumes them. Returns the torrents
    /// re-indexed.
    ///
    /// When both indexes walk, they go in lockstep from the lower cursor:
    /// each torrent is read from the store once and written to each index
    /// whose walk covers it, and only that one torrent is held at a time
    /// (its metadata, from untrusted peers, can be large).
    fn backfill(&mut self, store: &mut Store, budget: usize) -> Result<usize> {
        let ranges = [
            self.torrents.population.pending(),
            self.files.population.pending(),
        ];
        let Some(from) = ranges.iter().flatten().map(|r| r.0).min() else {
            return Ok(0);
        };
        let high = ranges.iter().flatten().map(|r| r.1).max().unwrap_or(from);
        let covers = |side: usize, id: i64| ranges[side].is_some_and(|(a, h)| id > a && id <= h);
        let ids = store.ids_after(from, budget)?;
        // Fewer than asked for: nothing more is stored up to `high`.
        let mut to = if ids.len() < budget { high } else { from };
        let (fields, ff) = (self.index.fields, self.index.files.fields);
        let mut n = 0;
        for id in ids {
            if id > high {
                to = high;
                break;
            }
            to = to.max(id);
            let (torrents, files) = (covers(0, id), covers(1, id));
            if !torrents && !files {
                continue;
            }
            let t = load(store, id)?;
            if torrents {
                let w = &mut self.torrents.writer;
                w.delete_term(Term::from_field_u64(fields.id, id as u64));
                if let Some(t) = &t {
                    w.add_document(torrent_doc(store, t, &mut self.sources)?.to_document(&fields))?;
                }
            }
            if files {
                let w = &mut self.files.writer;
                w.delete_term(Term::from_field_u64(ff.torrent, id as u64));
                if let Some(t) = &t {
                    for d in files::file_docs(id as u64, &t.tree) {
                        w.add_document(d.to_document(&ff))?;
                    }
                }
            }
            n += 1;
        }
        // Every id up to `to` that a walk covers was just re-indexed.
        let mut moved = false;
        for side in [&mut self.torrents, &mut self.files] {
            let before = side.population.clone();
            side.population.advance(to);
            moved |= side.population != before;
        }
        self.pending += n;
        // A step that only moved a cursor (the walk's last ids were
        // deleted) is still work to commit: otherwise an idle indexer,
        // which commits only what's pending, would leave the field one
        // step short of complete for good.
        if moved {
            self.pending = self.pending.max(1);
        }
        Ok(n)
    }

    /// Whether both indexes have followed the store up to torrent `id`
    /// and `reindex_log` entry `seq`.
    pub fn reached(&self, id: i64, seq: i64) -> bool {
        [&self.torrents, &self.files]
            .iter()
            .all(|s| s.last_id >= id && s.reindex_seq >= seq)
    }

    /// Adds the torrents after the watermarks and counts them.
    fn catch_up_new(&mut self, store: &mut Store, limit: usize) -> Result<CatchUp> {
        let from = self.torrents.last_id.min(self.files.last_id);
        let ids = store.ids_after(from, limit)?;
        let mut c = CatchUp::default();
        for id in ids {
            // Deleted between the reads: nothing to index.
            let t = load(store, id)?;
            // The class of a torrent counted here, from its document when
            // the torrent index takes it.
            let mut class = None;
            if id > self.torrents.last_id {
                if let Some(t) = &t {
                    let doc = torrent_doc(store, t, &mut self.sources)?;
                    class = Some(is_adult(&doc.enriched));
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
                c.added += 1;
                let adult = match class {
                    Some(a) => a,
                    // Only the file index was behind.
                    None => stored_adult(store, id)?,
                };
                c.adult_added += usize::from(adult);
            }
        }
        Ok(c)
    }

    /// Commits what's pending with the watermarks and makes it
    /// searchable. The file index commits first, so a `file:(…)` clause
    /// rarely names a torrent the torrent index doesn't have yet; if it
    /// does, that torrent simply isn't found until it's indexed.
    pub fn commit(&mut self) -> Result<()> {
        self.files.commit(&self.index.files.reader)?;
        self.torrents.commit(&self.index.reader)?;
        self.pending = 0;
        let states = [
            self.torrents.population.clone(),
            self.files.population.clone(),
        ];
        let generation = |r: &IndexReader| r.searcher().generation().generation_id();
        let seen = [
            generation(&self.index.reader),
            generation(&self.index.files.reader),
        ];
        let mut view = self
            .index
            .population
            .write()
            .unwrap_or_else(PoisonError::into_inner);
        for (i, state) in states.iter().enumerate() {
            for f in state.fields() {
                let ready = state.availability(f) == Some(Availability::Ready);
                let was = view.availability(f);
                if ready && was != Some(Availability::Ready) {
                    let before = was.unwrap_or(Availability::NoProducer);
                    view.ready_since.insert(f.to_owned(), (seen[i], before));
                }
            }
        }
        view.states = states;
        drop(view);
        // After both reloads, so the files they replaced are unmapped
        // unless a searcher still holds them.
        self.index.mmaps.prune();
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

/// Collects torrent ids from the `id` fast field: at most `left` across
/// all segments, setting `over` when a match had to be left out.
struct IdsCollector {
    left: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    over: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

struct IdsSegment {
    col: tantivy::columnar::Column<u64>,
    ids: Vec<u64>,
    left: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    over: std::sync::Arc<std::sync::atomic::AtomicBool>,
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
            left: self.left.clone(),
            over: self.over.clone(),
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
        use std::sync::atomic::Ordering::Relaxed;
        let Some(id) = self.col.first(doc) else {
            return;
        };
        let mut left = self.left.load(Relaxed);
        let taken = loop {
            if left == 0 {
                break false;
            }
            match self
                .left
                .compare_exchange_weak(left, left - 1, Relaxed, Relaxed)
            {
                Ok(_) => break true,
                Err(now) => left = now,
            }
        };
        if taken {
            self.ids.push(id);
        } else {
            self.over.store(true, Relaxed);
        }
    }

    fn harvest(self) -> Vec<u64> {
        self.ids
    }
}

#[cfg(test)]
mod population_tests {
    use super::*;
    use tantivy::query::AllQuery;

    const HEAP: usize = 15 << 20;

    fn store(n: usize) -> Store {
        use lodestar_bencode::{OwnedValue, to_vec};
        let mut store = Store::open_in_memory(lodestar_store::StoreConfig::default()).unwrap();
        for i in 0..n {
            let b = |s: &str| OwnedValue::Bytes(s.as_bytes().to_vec());
            let raw = to_vec(&OwnedValue::Dict(
                [
                    ("length", OwnedValue::Int(1 << 20)),
                    ("name", b(&format!("Walk.{i}.mkv"))),
                    ("piece length", OwnedValue::Int(1 << 16)),
                    ("pieces", OwnedValue::Bytes(vec![0; 20])),
                ]
                .into_iter()
                .map(|(k, v)| (k.as_bytes().to_vec(), v))
                .collect(),
            ));
            let info = lodestar_metainfo::Info::parse(&raw).unwrap();
            store
                .insert(&[lodestar_store::NewTorrent {
                    info: &info,
                    raw_info: Some(&raw),
                    discovered_at: 1_700_000_000,
                    metadata_at: 1_700_000_001,
                    extra_flags: 0,
                    source: lodestar_store::SourceId::new(0).unwrap(),
                }])
                .unwrap();
        }
        store
    }

    /// The bucket terms answer a query only when they're complete in the
    /// snapshot it runs on (X-search-files-5): not while they're
    /// backfilled, and not on a snapshot from before the walk completed,
    /// though the latest has them (a lease taken mid-walk).
    #[test]
    fn bucket_clauses_are_decided_by_the_snapshot_they_run_on() {
        let mut s = store(20);
        let index = SearchIndex::in_memory().unwrap();
        let mut w = index.writer(HEAP).unwrap();
        while w.catch_up(&mut s, 100).unwrap() > 0 {}
        w.commit().unwrap();
        drop(w);
        assert!(index.bucket_terms_ready(None));
        let gens = Generations::current();
        let next = gens.get("file_bucket").unwrap() + 1;
        let index = index
            .with_generations(gens.with("file_bucket", next))
            .unwrap();
        let mut w = index.writer(HEAP).unwrap();
        w.catch_up(&mut s, 5).unwrap();
        w.commit().unwrap();
        assert!(!index.bucket_terms_ready(None), "mid-walk");
        let node = crate::parse("file:(size:>1MB)").unwrap();
        let cancel = crate::CancelToken::new();
        let key = |snapshot| {
            index
                .compile_at(&node, 0, 0, false, &cancel, snapshot)
                .unwrap()
                .key
        };
        assert!(!key(None).contains("@buckets"));
        let before = index.searcher().generation().generation_id();
        while w.catch_up(&mut s, 100).unwrap() > 0 {}
        w.commit().unwrap();
        assert!(index.bucket_terms_ready(None), "the walk done");
        assert!(key(None).contains("@buckets"));
        assert!(!index.bucket_terms_ready(Some(before)), "an older snapshot");
        assert!(!key(Some(before)).contains("@buckets"));
        let after = index.searcher().generation().generation_id();
        assert!(index.bucket_terms_ready(Some(after)));
    }

    /// B13b's identity term: it narrows to the torrents with that term
    /// and leaves their scores as the query alone scores them, and it's
    /// offered only once the field is complete, never mid-backfill.
    #[test]
    fn the_identity_term_narrows_without_rescoring_and_waits_for_the_field() {
        let mut s = store(20);
        let mut after = 0;
        while let Some((last, _)) =
            lodestar_pipeline::enrich::enrich_stored(&mut s, after, 100).unwrap()
        {
            after = last;
        }
        let index = SearchIndex::in_memory().unwrap();
        let mut w = index.writer(HEAP).unwrap();
        while w.catch_up(&mut s, 100).unwrap() > 0 {}
        w.commit().unwrap();
        drop(w);
        let cancel = crate::CancelToken::new();
        let node = crate::parse("walk").unwrap();
        let plain = index.query_cancelled(&node, 0, 0, &cancel).unwrap();
        let all = index
            .search_page(&plain, crate::Sort::Relevance, 100, 0)
            .unwrap();
        assert_eq!(all.hits.len(), 20);
        let target = &all.hits[3];
        let key = doc_enrichment(&mut s, target.id as i64)
            .unwrap()
            .title_identity
            .expect("a title identity");
        let narrowed = index
            .query_title_identity(&node, key, 0, 0, &cancel)
            .unwrap()
            .expect("a complete field answers");
        assert_ne!(narrowed.key, plain.key, "cached apart");
        let got = index
            .search_page(&narrowed, crate::Sort::Relevance, 100, 0)
            .unwrap();
        // Only the torrents with that identity, scored as before.
        let want: Vec<Hit> = all
            .hits
            .iter()
            .filter(|h| doc_enrichment(&mut s, h.id as i64).unwrap().title_identity == Some(key))
            .cloned()
            .collect();
        assert!(!want.is_empty() && want.len() < 20, "{want:?}");
        assert_eq!(got.hits, want);

        // A build shipping a new generation of it: backfilling, so the
        // caller searches without it until the walk completes.
        let gens = Generations::current();
        let next = gens.get("title_identity").unwrap() + 1;
        let index = index
            .with_generations(gens.with("title_identity", next))
            .unwrap();
        let ask = || {
            index
                .query_title_identity(&node, key, 0, 0, &cancel)
                .unwrap()
        };
        assert!(ask().is_none(), "backfilling");
        let mut w = index.writer(HEAP).unwrap();
        w.catch_up(&mut s, 5).unwrap();
        w.commit().unwrap();
        assert!(ask().is_none(), "mid-walk");
        while w.catch_up(&mut s, 100).unwrap() > 0 {}
        w.commit().unwrap();
        assert!(ask().is_some(), "the walk done");
    }

    /// `store` fully indexed in RAM, then reopened by a build whose
    /// `payload_sig` producer has shipped: a walk over everything.
    fn shipped(store: &mut Store) -> SearchIndex {
        let index = SearchIndex::in_memory().unwrap();
        let mut w = index.writer(HEAP).unwrap();
        while w.catch_up(store, 100).unwrap() > 0 {}
        w.commit().unwrap();
        drop(w);
        index
            .with_generations(Generations::current().with("payload_sig", 1))
            .unwrap()
    }

    #[test]
    fn segment_counts_follow_each_commit_in_both_indexes() {
        let mut store = store(4);
        let index = SearchIndex::in_memory().unwrap();
        assert_eq!(index.segment_counts(), [0, 0]);
        let mut w = index.writer(HEAP).unwrap();
        w.catch_up(&mut store, 2).unwrap();
        w.commit().unwrap();
        let [main, files] = index.segment_counts();
        assert_eq!(main, index.segments() as u64);
        assert!(main >= 1 && files >= 1, "{main} {files}");
    }

    #[test]
    fn a_prefilter_waits_for_the_snapshot_that_sees_the_field_complete() {
        let mut store = store(10);
        let index = shipped(&mut store);
        let mut w = index.writer(HEAP).unwrap();
        w.catch_up(&mut store, 6).unwrap();
        w.commit().unwrap();
        let q = || -> Box<dyn Query> { Box::new(AllQuery) };
        let partial = index.searcher();
        assert!(index.prefilter("payload_sig", &partial, q()).is_none());
        while w.catch_up(&mut store, 6).unwrap() > 0 {}
        w.commit().unwrap();
        assert_eq!(index.availability("payload_sig"), Availability::Ready);
        // A lease still reading the snapshot from mid-backfill must not
        // prefilter; a search on the new one may.
        assert!(index.prefilter("payload_sig", &partial, q()).is_none());
        assert!(
            index
                .prefilter("payload_sig", &index.searcher(), q())
                .is_some()
        );
        // Fields complete since the index opened, or not tracked, are
        // complete in every snapshot; one without a producer in none.
        assert!(index.prefilter("file.category", &partial, q()).is_some());
        assert!(index.prefilter("name", &partial, q()).is_some());
        assert!(
            index
                .prefilter("group_key", &index.searcher(), q())
                .is_none()
        );
    }

    #[test]
    fn a_step_that_only_moves_the_cursor_is_committed() {
        let mut store = store(10);
        // The highest id is deleted before the walk starts: watermarks
        // never go down, so the walk runs to 10 with 9 torrents stored.
        let top = store.torrent(10).unwrap().unwrap().info_hash.0;
        let index = SearchIndex::in_memory().unwrap();
        let mut w = index.writer(HEAP).unwrap();
        while w.catch_up(&mut store, 100).unwrap() > 0 {}
        store.delete_torrents(&[top]).unwrap();
        while w.catch_up(&mut store, 100).unwrap() > 0 {}
        w.commit().unwrap();
        drop(w);
        let index = index
            .with_generations(Generations::current().with("payload_sig", 1))
            .unwrap();
        let mut w = index.writer(HEAP).unwrap();
        // Exactly the 9 stored: the cursor stops at 9.
        assert_eq!(w.catch_up_counted(&mut store, 9).unwrap().backfilled, 9);
        w.commit().unwrap();
        let backfilling = Availability::Backfilling { done: 9, upto: 10 };
        assert_eq!(index.availability("payload_sig"), backfilling);
        // Nothing left to read, but the walk completes: pending, so the
        // live indexer commits it.
        assert_eq!(w.catch_up_counted(&mut store, 9).unwrap().backfilled, 0);
        assert!(w.pending() > 0);
        w.commit().unwrap();
        assert_eq!(index.availability("payload_sig"), Availability::Ready);
        assert_eq!(w.catch_up(&mut store, 9).unwrap(), 0);
        assert_eq!(w.pending(), 0);
    }

    #[test]
    fn a_name_no_index_has_fails_closed() {
        let index = SearchIndex::in_memory().unwrap();
        for typo in [
            "payload_sgi",
            "file.categry",
            "file.",
            "",
            "file.payload_sig",
        ] {
            assert_eq!(index.availability(typo), Availability::Missing, "{typo:?}");
            let q: Box<dyn Query> = Box::new(AllQuery);
            assert!(index.prefilter(typo, &index.searcher(), q).is_none());
        }
        assert_eq!(index.availability("name"), Availability::Ready);
        assert_eq!(index.availability("file.name"), Availability::Ready);
        assert_eq!(index.availability("file.category"), Availability::Ready);
    }

    #[test]
    fn both_indexes_walk_in_lockstep_from_the_lower_cursor() {
        let mut store = store(30);
        let index = SearchIndex::in_memory().unwrap();
        let mut w = index.writer(HEAP).unwrap();
        while w.catch_up(&mut store, 100).unwrap() > 0 {}
        w.commit().unwrap();
        drop(w);
        // One producer ships and walks a third of the way...
        let first = Generations::current().with("payload_sig", 1);
        let index = index.with_generations(first.clone()).unwrap();
        let mut w = index.writer(HEAP).unwrap();
        assert_eq!(w.catch_up_counted(&mut store, 10).unwrap().backfilled, 10);
        w.commit().unwrap();
        drop(w);
        // ...then a second, on the other index, starts from the beginning.
        let index = index
            .with_generations(first.with("file.category", 2))
            .unwrap();
        let backfilling = |done, upto| Availability::Backfilling { done, upto };
        assert_eq!(index.availability("payload_sig"), backfilling(10, 30));
        assert_eq!(index.availability("file.category"), backfilling(0, 30));
        let mut w = index.writer(HEAP).unwrap();
        // One pass from the lower cursor: 20 torrents read once each, the
        // first ten only into the file index.
        assert_eq!(w.catch_up_counted(&mut store, 20).unwrap().backfilled, 20);
        w.commit().unwrap();
        assert_eq!(index.availability("payload_sig"), backfilling(20, 30));
        assert_eq!(index.availability("file.category"), backfilling(20, 30));
        while w.catch_up(&mut store, 20).unwrap() > 0 {}
        w.commit().unwrap();
        assert_eq!(index.availability("payload_sig"), Availability::Ready);
        assert_eq!(index.availability("file.category"), Availability::Ready);
        assert_eq!((index.num_docs(), index.num_files()), (30, 30));
    }

    #[test]
    fn a_read_only_index_reports_what_it_recorded() {
        let index = Index::create_in_ram(schema::build().0);
        let mut w = index
            .writer_with_num_threads::<TantivyDocument>(1, HEAP)
            .unwrap();
        let mut prepared = w.prepare_commit().unwrap();
        prepared.set_payload(&format!(
            "{PAYLOAD_PREFIX}9;reindex:0;pop:payload_sig=1@3/9,verified=0"
        ));
        prepared.commit().unwrap();
        let [t, _] = read_only_population(&index, &index).unwrap();
        // As recorded, not reconciled with this build's generations.
        assert_eq!(
            t.availability("payload_sig"),
            Some(Availability::Backfilling { done: 3, upto: 9 })
        );
        assert_eq!(t.availability("verified"), Some(Availability::NoProducer));
        // Without a `pop:` part it predates tracking: nothing recorded.
        let bare = Index::create_in_ram(schema::build().0);
        assert_eq!(
            read_only_population(&bare, &bare).unwrap()[0],
            PopState::default()
        );
    }
}
