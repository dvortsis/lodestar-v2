//! The pipeline (architecture §5–§7, §18.4, §19): source plugins → intake
//! (the have-filter, then a lease from the coordinator) → the shared fetch
//! pool → prepare → the single writer.
//!
//! ```text
//!   worker side (WorkerId::LOCAL)                         │ coordinator
//!  plugins ──emit──▶ intake ──claim──────────────────────▶│ leases (harvested at grant)
//!     ▲               │  ◀──grant/held──────────────────── │
//!     │               └──new──▶ fetch pool ──▶ prepare ──Results──▶ writer ──▶ store
//!     └── peer asks ◀─────────────┘   └──lost──────────────▶│ leases (lost, once)
//!                     stored? (confirm, add source) ─────────────────▶ writer
//! ```
//!
//! The coordinator–worker boundary (§19.4) runs in-process with one
//! built-in worker, through the same leases and records remote workers
//! will use. Each stage is one task with bounded channels between them.
//! Live sources never wait (a full pipeline drops and counts), importers
//! do; the pool waits on prepare and prepare on the writer, which is the
//! backpressure that keeps memory flat.

pub mod control;
mod coordinator;
mod dht;
mod egress;
pub mod enrich;
pub mod gate;
mod have;
pub mod health;
pub mod log_events;
mod rollups;
pub mod source_adult;
pub mod torrent_cache;
pub mod unverified;
mod worker;
mod writer;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use lodestar_cluster::{Claim, Claimed, RestoredLease, WorkerId};
use lodestar_dht::node::NodeHandle;
use lodestar_filter::{
    Core, CoreRule, CsList, Engine, Handle, HeldSet, Lists, Origin, Sealer, Switches,
};
use lodestar_peer_wire::FetchOptions;
use lodestar_sources::pool::{self, PeerWireFetcher};
use lodestar_sources::{
    Discovery, Emitter, Fetcher, FunnelCounts, Kind, Loss, Manifest, PluginError, PoolConfig,
    PoolHandle, PoolIo, PoolStats, Priority, Shutdown, SourceCtx, SourcePlugin,
};
use lodestar_store::{
    Event, FilterState, HotColumns, QueuedKey, RuleSet, SourceId, SourceState, Store, StoreConfig,
    WriteBatch,
};
use lodestar_tracker_scrape::TrackerUrl;
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::JoinHandle;

use crate::coordinator::Coordinator;
use crate::health::{HealthConfig, HealthHandle};
use crate::worker::{PrepareStage, WorkerFunnel, WorkerLeases};

pub use control::Control;
pub use coordinator::LeaseStats;
pub use dht::{DhtSource, strategy};
pub use have::Have;
pub use rollups::{Rollups, metric};
pub use writer::WriterStats;

/// `events.kind` codes. Persisted: append only.
pub mod event {
    pub const START: u32 = 1;
    pub const STOP: u32 = 2;
    /// The crawler was paused (B14); Insights draws the gap.
    pub const PAUSE: u32 = 3;
    pub const RESUME: u32 = 4;
    /// The crawler's intensity changed; `detail` is the new one.
    pub const INTENSITY: u32 = 5;
}

const FIRST_RUN_SETTING: &str = "first_run_at";
const PLUGIN_STOP_TIMEOUT: Duration = Duration::from_secs(15);
/// A worker silent this long loses its leases (§19.3).
const LEASE_TTL_SECS: i64 = 600;
const HEARTBEAT_EVERY: Duration = Duration::from_secs(5);
/// Incomplete imports queued for re-fetching per start; the rest follow
/// on later starts.
const PARTIAL_REFETCH: usize = 50_000;

/// The cohort a queued refetch carries instead of an hour. A refetch
/// completes an import's record; it isn't a crawl discovery, so no funnel
/// hook counts it (no cohort row, no `FUNNEL_EVENTS`, no Fetch stage).
/// The pool reports progress by cohort alone, so the cohort is the flag
/// that reaches every hook. Far in the future, so it sorts after real
/// hours and `cohort * 3600` can't overflow.
pub(crate) const REFETCH_COHORT: i64 = i64::MAX / 3600;

/// Whether a key under `cohort` is the crawl's, and counted in the funnel.
pub(crate) fn in_funnel(cohort: i64) -> bool {
    cohort != REFETCH_COHORT
}

#[derive(Debug, Clone)]
pub struct PipelineConfig {
    pub pool: PoolConfig,
    pub fetch: FetchOptions,
    /// Per-plugin priority overrides, by manifest id (`config.toml`).
    /// One saved in the app (`PATCH /sources/{id}`) wins over these.
    pub priorities: HashMap<String, Priority>,
    /// The content filter (§20), shared with the API so rule changes
    /// apply live. The store's CS-1 entries and switches are added at
    /// start.
    pub filter: Arc<Handle>,
    /// Seals quarantined names (§20.3); without it none are kept.
    pub sealer: Option<Arc<Sealer>>,
    /// How long quarantine holds a torrent before purging it.
    pub quarantine: Duration,
    /// How long filter hits are kept (0–30 days, §20.11).
    pub filter_hits: Duration,
    /// Tombstoned and quarantined keys, shared with the API so a release
    /// takes effect at once. The store's are added at start.
    pub held: Arc<HeldSet>,
    /// Set by the API when stored torrents were marked for classifying
    /// again (a classification rule changed): background enrichment
    /// starts another pass.
    pub reenrich: Arc<AtomicBool>,
    /// The crawler warm-up window: torrents found this soon after the
    /// first run are flagged `BACKFILL` (§6).
    pub backfill: Duration,
    pub flush_every: Duration,
    pub save_queue_every: Duration,
    pub batch_max: usize,
    pub batch_wait: Duration,
    pub intake_capacity: usize,
    /// The writer keeps these up to date after each commit (§7).
    pub hot: Option<Arc<HotColumns>>,
    /// The scrape scheduler; `None` turns scraping off.
    pub health: Option<HealthSetup>,
    /// A search index will report its commits to the gate
    /// ([`gate::GateStats::searchable`]), so the writer hands new ids
    /// over from its first commit. Off with search off.
    pub gate_follows_index: bool,
    /// `sources.verify_unverified` (§18.4, plan SRC-2): fetch unverified
    /// items' metadata through the shared pool so they can be promoted.
    /// Off by default until the crawler acceptance soak closes.
    pub verify_unverified: bool,
    /// Builds each enabled plugin's HTTP client on its egress setting
    /// (§18.4, §18.7). `None` (tests): plugins get no egress at all
    /// ([`lodestar_sources::NoEgress`]), so nothing leaves the machine.
    pub egress: Option<lodestar_sources::EgressFactory>,
}

/// How often a running plugin's non-direct route is probed, so a route
/// that goes down abandons the requests in flight (SRC-5's kill switch).
pub const EGRESS_PROBE_EVERY: Duration = Duration::from_secs(15);

/// What the scrape scheduler needs besides its settings.
#[derive(Debug, Clone)]
pub struct HealthSetup {
    pub config: HealthConfig,
    pub trackers: Vec<TrackerUrl>,
    /// The database, for the scheduler's own read-only connection.
    pub db: PathBuf,
    pub store: StoreConfig,
    /// The DHT plugin's node, for BEP 33 scrapes, while it runs.
    pub dht: Option<Arc<Mutex<Option<NodeHandle>>>>,
}

impl Default for PipelineConfig {
    fn default() -> Self {
        PipelineConfig {
            pool: PoolConfig::default(),
            fetch: FetchOptions::default(),
            priorities: HashMap::new(),
            filter: Arc::new(default_filter()),
            sealer: None,
            quarantine: Duration::from_secs(30 * 86_400),
            filter_hits: Duration::from_secs(30 * 86_400),
            held: Arc::new(HeldSet::default()),
            reenrich: Arc::new(AtomicBool::new(false)),
            backfill: Duration::from_secs(30 * 86_400),
            flush_every: Duration::from_secs(5),
            save_queue_every: Duration::from_secs(300),
            batch_max: 256,
            batch_wait: Duration::from_millis(250),
            intake_capacity: 16_384,
            hot: None,
            health: None,
            gate_follows_index: false,
            verify_unverified: false,
            egress: None,
        }
    }
}

/// The shipped child-safety core with nothing added.
pub fn default_filter() -> Handle {
    let lists = Lists::shipped().expect("shipped term lists parse");
    Handle::new(Engine::new(Core::new(
        lists,
        CsList::default(),
        Switches::default(),
    )))
}

/// What the filter is built from in the store: CS-1 entries, CS-8
/// signatures and your switches ([`FilterState`]), packs and your rules,
/// and your classification rules. The floor's config files and the
/// subscribed lists are synced into the store and applied before this
/// (the binary does that, at the server's start and in `lodestar import`).
#[derive(Debug, Clone)]
pub struct StoreFilter {
    pub state: FilterState,
    pub rules: RuleSet,
    pub class_rules: Vec<(i64, String, bool)>,
}

impl StoreFilter {
    pub fn read(store: &Store) -> Result<StoreFilter, lodestar_store::Error> {
        Ok(StoreFilter {
            state: store.filter_state()?,
            rules: store.rule_set()?,
            class_rules: store.class_rules()?,
        })
    }
}

/// Loads the store's filter into `filter`, the one way both the server
/// and `lodestar import` build it: CS-1 entries, CS-8 signatures and your
/// switches into the core, then packs, your rules and your
/// classification rules into the engine. Returns the notes for rules
/// that no longer parse; they're skipped.
pub fn load_filter(filter: &Handle, from: &StoreFilter) -> Vec<String> {
    let state = &from.state;
    filter.update(|core| {
        for (hash, origin) in &state.cs_hashes {
            let origin = Origin::from_code(*origin).unwrap_or_else(|| {
                lodestar_log::log_event!(
                    log_events::CS_ORIGIN_UNKNOWN,
                    origin = u64::from(*origin)
                );
                Origin::Imported
            });
            core.cs.add_hash(*hash, origin);
        }
        for sig in &state.cs_sigs {
            core.cs.add_sig(*sig);
        }
        for (rule, on) in &state.switches {
            if let Some(r) = CoreRule::from_id(rule) {
                let _ = core.switches.set(r, *on);
            }
        }
    });
    filter.update_engine(|e| {
        let mut notes = e.load_rules(&from.rules);
        notes.extend(e.load_class_rules(&from.class_rules));
        notes
    })
}

#[derive(Debug)]
pub enum Error {
    Store(lodestar_store::Error),
    Join(tokio::task::JoinError),
    /// Reading or writing the hot-column snapshot.
    Io(std::io::Error),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Store(e) => e.fmt(f),
            Error::Join(e) => write!(f, "pipeline task failed: {e}"),
            Error::Io(e) => write!(f, "hot columns: {e}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<lodestar_store::Error> for Error {
    fn from(e: lodestar_store::Error) -> Self {
        Error::Store(e)
    }
}

/// Counters for the intake stage.
#[derive(Debug, Default)]
pub struct IntakeStats {
    pub received: AtomicU64,
    /// Already stored according to the have-filter (confirmed by the writer).
    pub known: AtomicU64,
    /// New keys leased to this worker and sent to the pool.
    pub to_pool: AtomicU64,
    /// Rediscoveries dropped because the writer was behind.
    pub dropped: AtomicU64,
    /// Keys refused before fetching: CS-1 hashes, tombstones, quarantine.
    pub refused: AtomicU64,
    /// Discoveries with claims sent to the writer to judge and store as
    /// unverified items (SRC-2), not to the pool.
    pub claims: AtomicU64,
}

/// One plugin of this build, running or not.
#[derive(Debug, Clone, PartialEq)]
pub struct SourceStatus {
    pub id: SourceId,
    pub plugin: &'static str,
    pub name: &'static str,
    pub running: bool,
    /// Its stored state allowed it to run at this start (§18.1): a
    /// disabled, paused or purged source isn't started at all.
    pub enabled: bool,
    /// Keys the plugin dropped because the pipeline was full.
    pub dropped: u64,
    pub manifest: Manifest,
    /// The priority it started with.
    pub priority: Priority,
    /// Why it stopped, if it stopped with an error.
    pub last_error: Option<String>,
    /// Its politeness, config and placement (`SourcePlugin::policy`).
    pub policy: lodestar_sources::Policy,
    /// Its egress as it runs: `None` when it isn't running or has no
    /// egress configured.
    pub egress: Option<EgressLive>,
}

/// A running plugin's egress: its kill switch's view, the route it was
/// built with (`None` when it couldn't be: a proxy name that didn't
/// resolve, an unreadable setting), and the saved setting it was built
/// from.
#[derive(Debug, Clone, PartialEq)]
pub struct EgressLive {
    pub status: lodestar_sources::EgressStatus,
    pub route: Option<lodestar_sources::Egress>,
    pub setting: control::SavedEgress,
}

struct SourceEntry {
    id: SourceId,
    plugin: Arc<dyn SourcePlugin>,
    emitter: Emitter,
    running: Arc<AtomicBool>,
    enabled: bool,
    priority: Priority,
    error: Arc<Mutex<Option<String>>>,
    egress: Option<egress::Built>,
    /// Stops this plugin alone ([`Monitor::stop_source`]); `None` for one
    /// that never started. The pipeline's own shutdown sets it too.
    stop: Option<Arc<watch::Sender<bool>>>,
    /// The plugin's task, to abort one that ignores its stop.
    task: Option<tokio::task::AbortHandle>,
}

/// A cheap, cloneable view of a running pipeline's counters, for metrics
/// and the status page.
#[derive(Clone)]
pub struct Monitor {
    pool: PoolHandle,
    intake: Arc<IntakeStats>,
    writer: Arc<WriterStats>,
    leases: Arc<LeaseStats>,
    have: Arc<Have>,
    sources: Arc<Vec<SourceEntry>>,
    stored_at_start: u64,
    started_at: i64,
    health: Option<HealthHandle>,
    hot: Option<Arc<HotColumns>>,
    rollups: Arc<Rollups>,
    gate: Arc<gate::GateStats>,
}

impl std::fmt::Debug for Monitor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Monitor")
            .field("started_at", &self.started_at)
            .finish_non_exhaustive()
    }
}

impl Monitor {
    pub fn pool(&self) -> &PoolHandle {
        &self.pool
    }

    /// Stops source `id`'s plugin alone, for a purge (SRC-14b): signals
    /// its stop, waits up to the plugin stop timeout for it to end, then
    /// aborts it, and drops every key it queued in the pool (a key others
    /// found too keeps them). Returns the number of keys dropped. A
    /// source that isn't running only has its keys dropped. Its egress
    /// monitor ends with it.
    pub async fn stop_source(&self, id: SourceId) -> usize {
        if let Some(s) = self.sources.iter().find(|s| s.id == id) {
            if let Some(stop) = &s.stop {
                let _ = stop.send(true);
            }
            let running = Arc::clone(&s.running);
            let ended = tokio::time::timeout(PLUGIN_STOP_TIMEOUT, async {
                while running.load(Ordering::Relaxed) {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await;
            if ended.is_err() {
                lodestar_log::log_event!(
                    log_events::SOURCE_STOP_TIMEOUT,
                    plugin = s.plugin.manifest().id
                );
                if let Some(task) = &s.task {
                    task.abort();
                }
                s.running.store(false, Ordering::Relaxed);
            }
        }
        self.pool.drop_source(id).await
    }

    pub fn pool_stats(&self) -> &PoolStats {
        self.pool.stats()
    }

    pub fn intake_stats(&self) -> &IntakeStats {
        &self.intake
    }

    pub fn writer_stats(&self) -> &WriterStats {
        &self.writer
    }

    /// The crawler gate's counters (owner-decisions §2.25).
    pub fn gate(&self) -> &Arc<gate::GateStats> {
        &self.gate
    }

    /// The coordinator's in-flight leases.
    pub fn lease_stats(&self) -> &LeaseStats {
        &self.leases
    }

    pub fn have(&self) -> &Have {
        &self.have
    }

    /// Torrents stored, without a table scan: the count at start plus
    /// what the writer has added since.
    pub fn torrents(&self) -> u64 {
        self.stored_at_start + self.writer.stored.load(Ordering::Relaxed)
    }

    /// Unix seconds.
    pub fn started_at(&self) -> i64 {
        self.started_at
    }

    /// The scrape scheduler, when scraping is on.
    pub fn health(&self) -> Option<&HealthHandle> {
        self.health.as_ref()
    }

    pub fn hot(&self) -> Option<&Arc<HotColumns>> {
        self.hot.as_ref()
    }

    /// Counters the writer flushes into the Insights rollups; for stages
    /// outside the pipeline (the API's Serve counters).
    pub fn rollups(&self) -> &Arc<Rollups> {
        &self.rollups
    }

    pub fn sources(&self) -> Vec<SourceStatus> {
        self.sources
            .iter()
            .map(|s| SourceStatus {
                id: s.id,
                plugin: s.plugin.manifest().id,
                name: s.plugin.manifest().name,
                running: s.running.load(Ordering::Relaxed),
                enabled: s.enabled,
                dropped: s.emitter.dropped(),
                manifest: s.plugin.manifest().clone(),
                priority: s.priority,
                last_error: s.error.lock().expect("source error lock").clone(),
                policy: *s.plugin.policy(),
                // A plugin that stopped has no live route.
                egress: s
                    .egress
                    .as_ref()
                    .filter(|_| s.running.load(Ordering::Relaxed))
                    .map(|b| EgressLive {
                        status: b.egress.status(),
                        route: b.route.clone(),
                        setting: b.setting.clone(),
                    }),
            })
            .collect()
    }
}

pub struct Pipeline {
    monitor: Monitor,
    tasks: Vec<JoinHandle<Result<(), PluginError>>>,
    /// The egress monitors (`egress::watch`).
    egress_tasks: Vec<JoinHandle<()>>,
    stop_plugins: watch::Sender<bool>,
    stop_intake: watch::Sender<bool>,
    stop_pool: watch::Sender<bool>,
    intake: JoinHandle<()>,
    pool_task: JoinHandle<Vec<QueuedKey>>,
    prepare: JoinHandle<()>,
    heartbeat: JoinHandle<()>,
    saver: JoinHandle<()>,
    finish: oneshot::Sender<Vec<QueuedKey>>,
    writer: JoinHandle<Store>,
    /// The scrape scheduler, stopped first.
    health: Option<(watch::Sender<bool>, JoinHandle<Store>)>,
    /// The registered always-adult sources ([`Pipeline::adult_sources`]).
    adult: source_adult::AdultSources,
}

impl std::fmt::Debug for Pipeline {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Pipeline")
            .field("sources", &self.tasks.len())
            .finish_non_exhaustive()
    }
}

impl Pipeline {
    /// Starts every stage and the given plugins, with the real peer-wire
    /// fetcher.
    pub async fn start(
        config: PipelineConfig,
        store: Store,
        plugins: Vec<Arc<dyn SourcePlugin>>,
    ) -> Result<Pipeline, Error> {
        let fetcher = Arc::new(PeerWireFetcher(config.fetch.clone()));
        Pipeline::start_with(config, store, plugins, fetcher).await
    }

    pub async fn start_with(
        config: PipelineConfig,
        store: Store,
        plugins: Vec<Arc<dyn SourcePlugin>>,
        fetcher: Arc<dyn Fetcher>,
    ) -> Result<Pipeline, Error> {
        let manifests: Vec<Registering> = plugins
            .iter()
            .map(|p| {
                let m = p.manifest();
                Registering {
                    manifest: m.clone(),
                    peer_finder: m.is(Kind::PeerFinder),
                }
            })
            .collect();
        let backfill = config.backfill.as_secs() as i64;
        let (store, registered, queue, have, backfill_until, stored_at_start, store_filter) =
            tokio::task::spawn_blocking(move || prepare(store, &manifests, backfill))
                .await
                .map_err(Error::Join)??;
        // The sources whose every find is classified adult (X-source-adult).
        let adult = source_adult::AdultSources::of(
            registered
                .iter()
                .zip(&plugins)
                .map(|((id, ..), p)| (*id, p.manifest())),
        );
        lodestar_log::log_event!(
            log_events::STARTING,
            stored = have.len(),
            queued = queue.len()
        );
        let have = Arc::new(have);
        for note in load_filter(&config.filter, &store_filter) {
            let (what, rule, reason) = log_events::filter_note(&note);
            lodestar_log::log_event!(
                log_events::FILTER_RULE_SKIPPED,
                what = what,
                rule = rule,
                reason = reason
            );
        }
        let held = config.held.clone();
        for (h, _) in &store_filter.state.held {
            held.insert(*h);
        }
        let funnel = Arc::new(FunnelCounts::default());
        let rollups = Arc::new(Rollups::default());
        let started_at = unix_now();
        let gate = Arc::new(gate::GateStats::new(started_at));
        if config.gate_follows_index {
            gate.follow();
        }

        // The coordinator's lease table. Keys saved in the queue were
        // leased in the last run; they're leased again here, without
        // counting a new harvest.
        let coordinator = Arc::new(Coordinator::new(
            started_at as u32,
            LEASE_TTL_SECS,
            funnel.clone(),
        ));
        let leases = Arc::new(WorkerLeases::default());
        for q in &queue {
            // A refetch has no discovery hour: it was found at this start.
            let discovered_at = if in_funnel(q.cohort) {
                q.cohort * 3600
            } else {
                started_at.div_euclid(3600) * 3600
            };
            let token = coordinator.restore(
                WorkerId::LOCAL,
                q.info_hash,
                RestoredLease {
                    cohort: q.cohort,
                    source: q.source,
                    sources: 1u64 << q.source.get(),
                    strategy: q.strategy,
                    discovered_at,
                },
                started_at,
            );
            leases.insert(q.info_hash, token);
        }
        let preparer = store.preparer();

        let (stop_plugins, plugin_shutdown) = Shutdown::new();
        let (stop_intake, intake_shutdown) = Shutdown::new();
        let (stop_pool, pool_shutdown) = Shutdown::new();
        let (emit_tx, emit_rx) = mpsc::channel(config.intake_capacity);
        let (pool_tx, pool_rx) = mpsc::channel(config.intake_capacity);
        let (touch_tx, touch_rx) = mpsc::channel(config.intake_capacity);
        let (retry_tx, retry_rx) = mpsc::channel(config.intake_capacity);
        let (claims_tx, claims_rx) = mpsc::channel(config.intake_capacity);
        let claims_budget = Arc::new(tokio::sync::Semaphore::new(
            unverified::CLAIMS_BUDGET_KIB as usize,
        ));
        // Small: each entry holds a whole info dictionary or prepared row.
        let (out_tx, out_rx) = mpsc::channel(64);
        let (results_tx, results_rx) = mpsc::channel(64);
        let (queue_tx, queue_rx) = mpsc::channel(1);
        let (finish_tx, finish_rx) = oneshot::channel();

        // Plugins, with peer-request channels for the peer finders.
        let mut finders = Vec::new();
        let mut weights = HashMap::new();
        let mut sources = Vec::new();
        let mut tasks = Vec::new();
        // Each non-direct route's monitor; they end at the plugins'
        // shutdown.
        let mut egress_tasks = Vec::new();
        // Every enabled plugin's client, built at once: a proxy's name may
        // take a few seconds to resolve, or to fail to.
        let mut built = HashMap::new();
        if let Some(factory) = &config.egress {
            let mut set = tokio::task::JoinSet::new();
            for (plugin, (id, state, _, setting)) in plugins.iter().zip(&registered) {
                if *state != SourceState::Enabled {
                    continue;
                }
                let (factory, plugin, setting, id) =
                    (factory.clone(), plugin.clone(), setting.clone(), *id);
                set.spawn(async move {
                    (
                        id,
                        egress::build(&factory, plugin.manifest(), &setting).await,
                    )
                });
            }
            while let Some(done) = set.join_next().await {
                let (id, b) = done.map_err(Error::Join)?;
                built.insert(id, b);
            }
        }
        for (plugin, (id, state, saved, _)) in plugins.into_iter().zip(registered) {
            let m = plugin.manifest();
            let priority = saved
                .or_else(|| config.priorities.get(m.id).copied())
                .unwrap_or(m.default_priority);
            weights.insert(id, priority.weight());
            // Only an enabled source runs (§18.1); the rest are listed,
            // stopped, and a disabled peer finder isn't asked for peers.
            let enabled = state == SourceState::Enabled;
            let emitter = Emitter::new(id, emit_tx.clone());
            let error = Arc::default();
            if !enabled {
                lodestar_log::log_event!(
                    log_events::SOURCE_NOT_STARTED,
                    plugin = m.id,
                    state = log_events::state(state)
                );
                tasks.push(tokio::spawn(async { Ok(()) }));
                sources.push(SourceEntry {
                    id,
                    plugin,
                    emitter,
                    running: Arc::new(AtomicBool::new(false)),
                    enabled,
                    priority,
                    error,
                    egress: None,
                    stop: None,
                    task: None,
                });
                continue;
            }
            // Its own stop, which the pipeline's plugin shutdown also
            // sets, so one plugin can be stopped alone (a purge).
            let (stop, own_shutdown) = Shutdown::new();
            let stop = Arc::new(stop);
            {
                let (stop, mut all) = (Arc::clone(&stop), plugin_shutdown.clone());
                // Ends with the pipeline's plugin shutdown; holds `stop`
                // so the plugin's receiver never sees it dropped early.
                egress_tasks.push(tokio::spawn(async move {
                    all.requested().await;
                    let _ = stop.send(true);
                }));
            }
            let egress = built.remove(&id);
            if let Some(b) = &egress {
                egress_tasks.extend(egress::watch(
                    m.id,
                    Arc::clone(&b.egress),
                    EGRESS_PROBE_EVERY,
                    own_shutdown.clone(),
                ));
            }
            let kit = match &egress {
                Some(b) => lodestar_sources::Kit::for_plugin(
                    &*plugin,
                    Arc::new(lodestar_sources::MemoryState::new()),
                    Arc::clone(&b.egress) as Arc<dyn lodestar_sources::Fetch>,
                    &lodestar_sources::UserLimits::default(),
                ),
                None => lodestar_sources::Kit::default(),
            };
            let peer_requests = m.is(Kind::PeerFinder).then(|| {
                let (tx, rx) = mpsc::channel(1024);
                finders.push(tx);
                rx
            });
            let peer_replies = m
                .is(Kind::PeerFinder)
                .then(|| Emitter::replies(id, pool_tx.clone()));
            let ctx = SourceCtx {
                source: id,
                emitter: emitter.clone(),
                peer_requests,
                peer_replies,
                shutdown: own_shutdown,
                kit,
            };
            let p = plugin.clone();
            let running = Arc::new(AtomicBool::new(true));
            let flag = running.clone();
            let last_error = Arc::clone(&error);
            let task = tokio::spawn(async move {
                let result = p.run(ctx).await;
                flag.store(false, Ordering::Relaxed);
                if let Err(e) = &result {
                    lodestar_log::log_event!(
                        log_events::SOURCE_FAILED,
                        plugin = p.manifest().id,
                        error = lodestar_log::Error::new(e)
                    );
                    *last_error.lock().expect("source error lock") = Some(e.to_string());
                }
                result
            });
            let abort = task.abort_handle();
            tasks.push(task);
            sources.push(SourceEntry {
                id,
                plugin,
                emitter,
                running,
                enabled,
                priority,
                error,
                egress,
                stop: Some(stop),
                task: Some(abort),
            });
        }
        drop(emit_tx);
        egress::warn_proxy_env(&sources_egress_routes(&sources));

        let (pool, pool_task) = pool::spawn(
            config.pool.clone(),
            fetcher,
            PoolIo {
                discoveries: pool_rx,
                peer_finders: finders,
                out: out_tx,
                funnel: Arc::new(WorkerFunnel {
                    counts: funnel.clone(),
                    coordinator: coordinator.clone(),
                    leases: leases.clone(),
                    gate: gate.clone(),
                }),
                shutdown: pool_shutdown,
            },
            weights,
            queue,
        );

        let intake_stats = Arc::new(IntakeStats::default());
        let intake = tokio::spawn(intake(
            Intake {
                emitted: emit_rx,
                retry: retry_rx,
                have: have.clone(),
                coordinator: coordinator.clone(),
                leases: leases.clone(),
                to_pool: pool_tx.clone(),
                touches: touch_tx,
                claims: claims_tx,
                claims_budget,
                rollups: rollups.clone(),
                stats: intake_stats.clone(),
                filter: config.filter.clone(),
                held: held.clone(),
            },
            intake_shutdown,
        ));

        let writer_stats = Arc::new(WriterStats::default());
        let prepare = tokio::spawn(
            PrepareStage {
                preparer,
                filter: config.filter.clone(),
                coordinator: coordinator.clone(),
                leases: leases.clone(),
                stats: writer_stats.clone(),
                gate: gate.clone(),
                adult,
                batch_max: 64,
                batch_wait: Duration::from_millis(20),
            }
            .run(out_rx, results_tx),
        );

        // The built-in worker's heartbeat. It can't really go silent, but
        // if its leases ever expired they'd come straight back to it.
        let heartbeat = {
            let coordinator = coordinator.clone();
            let leases = leases.clone();
            tokio::spawn(async move {
                loop {
                    let now = unix_now();
                    coordinator.heartbeat(WorkerId::LOCAL, now);
                    for (key, token) in coordinator.expire_to(WorkerId::LOCAL, now) {
                        leases.insert(key, token);
                    }
                    tokio::time::sleep(HEARTBEAT_EVERY).await;
                }
            })
        };

        let saver = {
            let pool = pool.clone();
            let every = config.save_queue_every;
            tokio::spawn(async move {
                loop {
                    tokio::time::sleep(every).await;
                    let queue = pool.snapshot().await;
                    if queue_tx.send(queue).await.is_err() {
                        break;
                    }
                }
            })
        };

        // The scrape scheduler: its trackers registered while we still
        // hold the writer's store, and a read-only connection of its own.
        let (swarm_tx, swarm_rx) = mpsc::channel(16);
        let mut store = store;
        let (health, health_task) = match config.health.clone() {
            Some(h) => {
                let urls = h.trackers.clone();
                let (back, trackers) = tokio::task::spawn_blocking(move || {
                    let r = health::register_trackers(&mut store, &urls);
                    (store, r)
                })
                .await
                .map_err(Error::Join)?;
                store = back;
                let trackers = trackers?;
                let (db, cfg) = (h.db.clone(), h.store.clone());
                let reader = tokio::task::spawn_blocking(move || Store::open_reader(db, cfg))
                    .await
                    .map_err(Error::Join)??;
                let (stop, shutdown) = Shutdown::new();
                let (handle, task) = health::spawn(health::Setup {
                    cfg: h.config,
                    reader,
                    trackers,
                    dht: h.dht.unwrap_or_default(),
                    out: swarm_tx,
                    rollups: rollups.clone(),
                    shutdown,
                });
                (Some(handle), Some((stop, task)))
            }
            None => {
                drop(swarm_tx);
                (None, None)
            }
        };

        let lease_stats = coordinator.stats.clone();
        let rollups_handle = rollups.clone();
        let writer = writer::Writer {
            store: Some(store),
            adult,
            backfill_after: 0,
            backfill_done: false,
            backfill_backoff: Default::default(),
            reenrich: config.reenrich.clone(),
            cfg: writer::WriterConfig {
                flush_every: config.flush_every,
                batch_max: config.batch_max,
                batch_wait: config.batch_wait,
                backfill_until,
                quarantine_secs: config.quarantine.as_secs() as i64,
                hits_secs: config.filter_hits.as_secs() as i64,
            },
            have: have.clone(),
            funnel,
            rollups,
            filter: config.filter.clone(),
            sealer: config.sealer.clone(),
            held: held.clone(),
            last_purge: 0,
            coordinator,
            stats: writer_stats.clone(),
            gate: gate.clone(),
            hot: config.hot.clone(),
            health: health.clone(),
            candidates: Default::default(),
            sweep: Default::default(),
            verify: unverified::Verify {
                on: config.verify_unverified,
                ..Default::default()
            },
            #[cfg(test)]
            between: None,
            #[cfg(test)]
            marked: None,
        };
        drop(pool_tx);
        let writer = tokio::spawn(writer.run(writer::WriterIo {
            results: results_rx,
            touches: touch_rx,
            queues: queue_rx,
            retry: retry_tx,
            finish: finish_rx,
            swarm: swarm_rx,
            claims: claims_rx,
        }));

        Ok(Pipeline {
            monitor: Monitor {
                pool,
                intake: intake_stats,
                writer: writer_stats,
                leases: lease_stats,
                have,
                sources: Arc::new(sources),
                stored_at_start,
                started_at,
                health,
                hot: config.hot.clone(),
                rollups: rollups_handle,
                gate,
            },
            tasks,
            egress_tasks,
            stop_plugins,
            stop_intake,
            stop_pool,
            intake,
            pool_task,
            prepare,
            heartbeat,
            saver,
            finish: finish_tx,
            writer,
            health: health_task,
            adult,
        })
    }

    pub fn monitor(&self) -> Monitor {
        self.monitor.clone()
    }

    /// The registered sources whose every find is classified adult
    /// (X-source-adult), for the other places that classify a stored
    /// torrent: the API's corrections.
    pub fn adult_sources(&self) -> source_adult::AdultSources {
        self.adult
    }

    pub fn pool(&self) -> &PoolHandle {
        self.monitor.pool()
    }

    pub fn pool_stats(&self) -> &PoolStats {
        self.monitor.pool_stats()
    }

    pub fn intake_stats(&self) -> &IntakeStats {
        self.monitor.intake_stats()
    }

    pub fn writer_stats(&self) -> &WriterStats {
        self.monitor.writer_stats()
    }

    pub fn have(&self) -> &Have {
        self.monitor.have()
    }

    pub fn sources(&self) -> Vec<SourceStatus> {
        self.monitor.sources()
    }

    /// Stops the stages in order (plugins, intake, pool, prepare, writer), saving
    /// the DHT state and the fetch queue, and returns the store.
    pub async fn shutdown(self) -> Result<Store, Error> {
        // The scheduler first: its last rounds reach the writer, which
        // commits them on its way out.
        if let Some((stop, task)) = self.health {
            let _ = stop.send(true);
            task.await.map_err(Error::Join)?;
        }
        let _ = self.stop_plugins.send(true);
        for (s, mut task) in self.monitor.sources.iter().zip(self.tasks) {
            let id = s.plugin.manifest().id;
            // A plugin that ignores shutdown mustn't hold up the rest.
            match tokio::time::timeout(PLUGIN_STOP_TIMEOUT, &mut task).await {
                Ok(Ok(Err(e))) => lodestar_log::log_event!(
                    log_events::SOURCE_STOP_FAILED,
                    plugin = id,
                    error = lodestar_log::Error::new(&e)
                ),
                Ok(_) => {}
                Err(_) => {
                    lodestar_log::log_event!(log_events::SOURCE_STOP_TIMEOUT, plugin = id);
                    task.abort();
                }
            }
        }
        // The egress monitors end at the same signal.
        for mut task in self.egress_tasks {
            if tokio::time::timeout(PLUGIN_STOP_TIMEOUT, &mut task)
                .await
                .is_err()
            {
                task.abort();
            }
        }
        let _ = self.stop_intake.send(true);
        self.intake.await.map_err(Error::Join)?;
        let _ = self.stop_pool.send(true);
        let queue = self.pool_task.await.map_err(Error::Join)?;
        // Prepare finishes what the pool fetched, then closes the writer's
        // input.
        self.prepare.await.map_err(Error::Join)?;
        self.heartbeat.abort();
        self.saver.abort();
        let _ = self.finish.send(queue);
        self.writer.await.map_err(Error::Join)
    }
}

/// A refetch saved in the fetch queue: strategy code 200 under
/// `REFETCH_COHORT`, or from a peer finder (queues saved before the cohort).
fn saved_refetch(q: &QueuedKey, finders: &std::collections::HashSet<SourceId>) -> bool {
    q.strategy == strategy::REFETCH && (q.cohort == REFETCH_COHORT || finders.contains(&q.source))
}

/// The plugins that run and connect directly, for the proxy warning.
fn sources_egress_routes(sources: &[SourceEntry]) -> Vec<&'static str> {
    sources
        .iter()
        .filter(|s| {
            s.egress
                .as_ref()
                .is_some_and(|b| b.route == Some(lodestar_sources::Egress::Direct))
        })
        .map(|s| s.plugin.manifest().id)
        .collect()
}

/// What [`prepare`] needs of a plugin's manifest.
struct Registering {
    manifest: lodestar_sources::Manifest,
    peer_finder: bool,
}

/// How a plugin seen for the first time is registered: only a plugin on
/// by default (the Mainline DHT, §18.1) whose class needs nothing more
/// than a toggle starts enabled; the rest wait for the user, and their
/// consent (§18.7).
fn first_state(m: &lodestar_sources::Manifest) -> SourceState {
    if m.enabled_by_default && lodestar_sources::consent::need(m, None, &[]).is_none() {
        SourceState::Enabled
    } else {
        SourceState::Disabled
    }
}

/// Registers the plugins, loads the queue, builds the have-filter and
/// records the start (blocking).
#[allow(clippy::type_complexity)]
fn prepare(
    mut store: Store,
    manifests: &[Registering],
    backfill: i64,
) -> Result<
    (
        Store,
        Vec<(
            SourceId,
            SourceState,
            Option<Priority>,
            control::SavedEgress,
        )>,
        Vec<QueuedKey>,
        Have,
        i64,
        u64,
        StoreFilter,
    ),
    Error,
> {
    let now = unix_now();
    // One write per plugin: a new one is registered in its first state,
    // so a crash can't leave a consent-gated plugin enabled.
    let ids = manifests
        .iter()
        .map(|r| {
            let m = &r.manifest;
            store.register_source_as(m.id, m.version, first_state(m), now)
        })
        .collect::<Result<Vec<_>, _>>()?;
    // Every start checks each enabled source's class condition again
    // (§18.7, §18.9): one whose consent text or endpoints changed since
    // it was acknowledged, whose credentials are gone, or that was
    // enabled some other way, is switched off before it can run.
    let rows = store.sources()?;
    for (r, id) in manifests.iter().zip(&ids) {
        let enabled = rows
            .iter()
            .any(|row| row.id == *id && row.state == SourceState::Enabled);
        if enabled && lodestar_sources::consent::need_in(&store, &r.manifest)?.is_some() {
            store.set_source_state(*id, SourceState::Disabled)?;
            lodestar_log::log_event!(
                log_events::SOURCE_STATE_CHANGED,
                plugin = r.manifest.id,
                state = log_events::state(SourceState::Disabled)
            );
        }
    }
    let states: HashMap<SourceId, SourceState> = store
        .sources()?
        .into_iter()
        .map(|r| (r.id, r.state))
        .collect();
    let state = |id: &SourceId| states.get(id).copied().unwrap_or(SourceState::Enabled);
    let first_run = match store.setting_i64(FIRST_RUN_SETTING)? {
        Some(t) => t,
        None => {
            store.set_setting_i64(FIRST_RUN_SETTING, now)?;
            now
        }
    };
    let mut queue = store.load_fetch_queue()?;
    // Refetches are queued afresh from the store below, so a saved one
    // whose import was removed while the server was stopped isn't fetched
    // into a new, crawled-looking row (and one saved before they had their
    // own cohort doesn't land in the funnel). Matched by strategy code and
    // owner: `REFETCH_COHORT`, or, for queues saved before it, a peer
    // finder's key (refetches were queued under one). A plugin's own
    // strategy codes are its own, so a non-finder's keys under 200 stay.
    let finders: std::collections::HashSet<SourceId> = manifests
        .iter()
        .zip(&ids)
        .filter(|(m, _)| m.peer_finder)
        .map(|(_, id)| *id)
        .collect();
    queue.retain(|q| !saved_refetch(q, &finders));
    // Imports with incomplete file lists get their info dicts fetched, if
    // there's a peer finder to find them peers.
    if let Some(finder) = manifests
        .iter()
        .zip(&ids)
        .find(|(m, id)| m.peer_finder && state(id) == SourceState::Enabled)
        .map(|(_, id)| *id)
    {
        let queued: std::collections::HashSet<_> = queue.iter().map(|q| q.info_hash).collect();
        for hash in store.partial_hashes(PARTIAL_REFETCH)? {
            if !queued.contains(&hash) {
                queue.push(QueuedKey {
                    info_hash: hash,
                    source: finder,
                    cohort: REFETCH_COHORT,
                    strategy: strategy::REFETCH,
                    priority: 0,
                    attempts: 0,
                    private_source: false,
                });
            }
        }
    }
    let stored = store.count()?;
    let have = Have::new(have::capacity_for(stored));
    store.for_each_swarm_hash(|h| have.insert(&h))?;
    store.write(&WriteBatch {
        events: &[Event {
            at: now,
            kind: event::START,
            detail: format!("v{}", env!("CARGO_PKG_VERSION")),
        }],
        ..WriteBatch::default()
    })?;
    // RP-0: pack versions are seeded once, before the filter is first read.
    let packs = lodestar_filter::packs::manifest::Manifest::builtin();
    lodestar_filter::packs::upgrade::ensure_seeded(&mut store, &packs, now)?;
    let filter = StoreFilter::read(&store)?;
    let mut sources = Vec::with_capacity(ids.len());
    for (m, id) in manifests.iter().zip(&ids) {
        sources.push((
            *id,
            state(id),
            control::saved_priority(&store, m.manifest.id)?,
            control::saved_settings(&store, m.manifest.id, true)?.egress,
        ));
    }
    Ok((
        store,
        sources,
        queue,
        have,
        first_run + backfill,
        stored,
        filter,
    ))
}

struct Intake {
    emitted: mpsc::Receiver<Discovery>,
    /// Have-filter false positives from the writer, to claim.
    retry: mpsc::Receiver<Discovery>,
    have: Arc<Have>,
    coordinator: Arc<Coordinator>,
    leases: Arc<WorkerLeases>,
    to_pool: mpsc::Sender<Discovery>,
    touches: mpsc::Sender<Discovery>,
    /// Discoveries with claims, to the writer (SRC-2), each holding its
    /// share of `claims_budget` so the channel is bounded in bytes.
    claims: mpsc::Sender<unverified::Claimed>,
    claims_budget: Arc<tokio::sync::Semaphore>,
    rollups: Arc<Rollups>,
    stats: Arc<IntakeStats>,
    filter: Arc<Handle>,
    held: Arc<HeldSet>,
}

/// The worker's intake: known keys go to the writer as rediscoveries; new
/// ones are claimed from the coordinator (§19.3) and go to the pool.
///
/// The built-in worker claims key by key: the lease table is a function
/// call away, so batching (≤ 512 or 250 ms) is a concern of the wire
/// protocol, not of this boundary.
async fn intake(mut io: Intake, mut shutdown: Shutdown) {
    loop {
        let (mut d, retried) = tokio::select! {
            biased;
            _ = shutdown.requested() => break,
            d = io.emitted.recv() => match d {
                Some(d) => (d, false),
                None => break,
            },
            Some(d) = io.retry.recv() => (d, true),
        };
        if !retried {
            io.stats.received.fetch_add(1, Ordering::Relaxed);
            io.rollups.add(
                d.at,
                metric::HARVESTED,
                d.source,
                i64::from(d.found.strategy),
                1,
            );
            // A key with claims is judged and stored by the writer, as an
            // unverified item or as claims on the torrent that holds it
            // (SRC-2), never fetched from here: verification fetches go
            // through the writer's cursor, behind its switch. A held key
            // is refused first, as below, and the store refuses it too.
            if d.found.claims.is_some() {
                let key = d.found.key.0;
                if io.held.contains(&key) || io.filter.current().blocked_hash(&[key]).is_some() {
                    io.stats.refused.fetch_add(1, Ordering::Relaxed);
                    continue;
                }
                io.stats.claims.fetch_add(1, Ordering::Relaxed);
                // Waiting for budget or room backs up into the emitters,
                // as for the pool.
                let kib = unverified::claims_kib(&d);
                let permit = tokio::select! {
                    biased;
                    _ = shutdown.requested() => break,
                    p = io.claims_budget.clone().acquire_many_owned(kib) => match p {
                        Ok(p) => p,
                        Err(_) => break,
                    },
                };
                tokio::select! {
                    biased;
                    _ = shutdown.requested() => break,
                    sent = io.claims.send((d, permit)) => if sent.is_err() { break },
                }
                continue;
            }
            if io.have.contains(&d.found.key) {
                io.stats.known.fetch_add(1, Ordering::Relaxed);
                if io.touches.try_send(d.clone()).is_err() {
                    io.stats.dropped.fetch_add(1, Ordering::Relaxed);
                    io.rollups.add(d.at, metric::DROPPED, d.source, 0, 1);
                }
                continue;
            }
        }
        let claim = Claim {
            key: d.found.key,
            source: d.source,
            strategy: d.found.strategy,
            at: d.at,
        };
        // Known-bad keys are claimed (so the harvest is counted once) and
        // lost straight away, never fetched (§20.3, §20.5.2).
        let key = d.found.key.0;
        let refuse = io.held.contains(&key) || io.filter.current().blocked_hash(&[key]).is_some();
        match io.coordinator.claim(WorkerId::LOCAL, claim, unix_now()) {
            Claimed::Granted(token) if refuse => {
                io.stats.refused.fetch_add(1, Ordering::Relaxed);
                io.coordinator.lost(&d.found.key, token, Loss::Blocked);
                continue;
            }
            Claimed::Granted(token) => {
                io.leases.insert(d.found.key, token);
                io.stats.to_pool.fetch_add(1, Ordering::Relaxed);
            }
            // Leased already (in M3 always to this worker): its source is
            // on the lease, and its peers go to the holder's pool as a
            // reply, so the pool merges them but never admits the key
            // again once it has left.
            Claimed::Held { .. } => d.reply = true,
        }
        // Waiting here backs up into the emitters: live sources drop and
        // count, importers wait.
        tokio::select! {
            biased;
            _ = shutdown.requested() => break,
            sent = io.to_pool.send(d) => if sent.is_err() { break },
        }
    }
}

pub(crate) fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

/// `settings` key holding the stamp of the hot-column snapshot that's
/// current, or 0 while the columns are live and the snapshot is stale.
const HOT_STAMP: &str = "hot_stamp";

/// Loads the hot columns (blocking): the snapshot in `dir` when it's the
/// one the last clean shutdown recorded, else a rebuild from SQLite. The
/// recorded stamp is cleared either way, so a crash from here on forces a
/// rebuild. Returns whether the snapshot was used.
pub fn open_hot(store: &mut Store, dir: &Path) -> Result<(HotColumns, bool), Error> {
    let stamp = store.setting_i64(HOT_STAMP)?.unwrap_or(0);
    let snapshot = match stamp {
        0 => None,
        // Unreadable is treated like corrupt: rebuilt, never fatal.
        s => HotColumns::load(dir, s as u64).unwrap_or_else(|e| {
            lodestar_log::log_event!(
                crate::log_events::HOT_SNAPSHOT_UNREADABLE,
                error = lodestar_log::Error::io(&e)
            );
            None
        }),
    };
    store.set_setting_i64(HOT_STAMP, 0)?;
    match snapshot {
        Some(hot) => Ok((hot, true)),
        None => {
            let hot = HotColumns::new();
            store.load_hot(&hot)?;
            Ok((hot, false))
        }
    }
}

/// Snapshots the hot columns into `dir` and records the stamp (blocking).
/// Call after the pipeline has stopped, so nothing changes them meanwhile.
pub fn save_hot(store: &mut Store, hot: &HotColumns, dir: &Path) -> Result<(), Error> {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(1, |d| d.as_nanos() as u64)
        ^ u64::from(std::process::id()).rotate_left(32);
    let stamp = (stamp & (i64::MAX as u64)).max(1);
    hot.save(dir, stamp).map_err(Error::Io)?;
    store.set_setting_i64(HOT_STAMP, stamp as i64)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use lodestar_metainfo::InfoHash;

    #[test]
    fn a_saved_refetch_is_matched_by_code_and_owner() {
        let (finder, feed) = (SourceId::new(1).unwrap(), SourceId::new(2).unwrap());
        let finders = std::collections::HashSet::from([finder]);
        let key = |source, cohort, strategy| QueuedKey {
            info_hash: InfoHash([1; 20]),
            source,
            cohort,
            strategy,
            priority: 0,
            attempts: 0,
            private_source: false,
        };
        // This build's refetch, whoever's peers it took.
        assert!(saved_refetch(
            &key(finder, REFETCH_COHORT, strategy::REFETCH),
            &finders
        ));
        assert!(saved_refetch(
            &key(feed, REFETCH_COHORT, strategy::REFETCH),
            &finders
        ));
        // One saved before `REFETCH_COHORT`, under an hour.
        assert!(saved_refetch(
            &key(finder, 400_000, strategy::REFETCH),
            &finders
        ));
        // Another plugin's strategy 200, and a finder's own crawl.
        assert!(!saved_refetch(
            &key(feed, 400_000, strategy::REFETCH),
            &finders
        ));
        assert!(!saved_refetch(
            &key(finder, 400_000, strategy::SAMPLE),
            &finders
        ));
        assert!(!saved_refetch(
            &key(finder, REFETCH_COHORT, strategy::SAMPLE),
            &finders
        ));
    }
}
