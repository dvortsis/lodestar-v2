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

mod coordinator;
mod dht;
pub mod enrich;
mod have;
pub mod health;
mod rollups;
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
    Discovery, Emitter, Fetcher, FunnelCounts, Kind, Loss, PluginError, PoolConfig, PoolHandle,
    PoolIo, PoolStats, Priority, Shutdown, SourceCtx, SourcePlugin,
};
use lodestar_store::{
    Event, FilterState, HotColumns, QueuedKey, RuleSet, SourceId, Store, StoreConfig, WriteBatch,
};
use lodestar_tracker_scrape::TrackerUrl;
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::JoinHandle;

use crate::coordinator::Coordinator;
use crate::health::{HealthConfig, HealthHandle};
use crate::worker::{PrepareStage, WorkerFunnel, WorkerLeases};

pub use coordinator::LeaseStats;
pub use dht::{DhtSource, strategy};
pub use have::Have;
pub use rollups::{Rollups, metric};
pub use writer::WriterStats;

/// `events.kind` codes. Persisted: append only.
pub mod event {
    pub const START: u32 = 1;
    pub const STOP: u32 = 2;
}

const FIRST_RUN_SETTING: &str = "first_run_at";
const PLUGIN_STOP_TIMEOUT: Duration = Duration::from_secs(15);
/// A worker silent this long loses its leases (§19.3).
const LEASE_TTL_SECS: i64 = 600;
const HEARTBEAT_EVERY: Duration = Duration::from_secs(5);
/// Incomplete imports queued for re-fetching per start; the rest follow
/// on later starts.
const PARTIAL_REFETCH: usize = 50_000;

#[derive(Debug, Clone)]
pub struct PipelineConfig {
    pub pool: PoolConfig,
    pub fetch: FetchOptions,
    /// Per-plugin priority overrides, by manifest id.
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
}

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

/// Adds the store's filter state to the engine: CS-1 entries, CS-8
/// signatures and your switches.
fn load_filter(filter: &Handle, state: &FilterState) {
    filter.update(|core| {
        for (hash, origin) in &state.cs_hashes {
            let origin = Origin::from_code(*origin).unwrap_or_else(|| {
                tracing::warn!(origin, "unknown CS-1 origin; kept as imported");
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
}

/// One running plugin.
#[derive(Debug)]
pub struct SourceStatus {
    pub id: SourceId,
    pub plugin: &'static str,
    pub name: &'static str,
    pub running: bool,
    /// Keys the plugin dropped because the pipeline was full.
    pub dropped: u64,
}

struct SourceEntry {
    id: SourceId,
    plugin: Arc<dyn SourcePlugin>,
    emitter: Emitter,
    running: Arc<AtomicBool>,
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

    pub fn pool_stats(&self) -> &PoolStats {
        self.pool.stats()
    }

    pub fn intake_stats(&self) -> &IntakeStats {
        &self.intake
    }

    pub fn writer_stats(&self) -> &WriterStats {
        &self.writer
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
                dropped: s.emitter.dropped(),
            })
            .collect()
    }
}

pub struct Pipeline {
    monitor: Monitor,
    tasks: Vec<JoinHandle<Result<(), PluginError>>>,
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
        let manifests: Vec<(&'static str, &'static str, bool)> = plugins
            .iter()
            .map(|p| {
                let m = p.manifest();
                (m.id, m.version, m.is(Kind::PeerFinder))
            })
            .collect();
        let backfill = config.backfill.as_secs() as i64;
        let (store, ids, queue, have, backfill_until, stored_at_start, filter_state, rule_set) =
            tokio::task::spawn_blocking(move || prepare(store, &manifests, backfill))
                .await
                .map_err(Error::Join)??;
        tracing::info!(
            stored = have.len(),
            queued = queue.len(),
            "pipeline starting"
        );
        let have = Arc::new(have);
        load_filter(&config.filter, &filter_state);
        let (rule_set, class_rules) = rule_set;
        for note in config.filter.update_engine(|e| {
            let mut notes = e.load_rules(&rule_set);
            notes.extend(e.load_class_rules(&class_rules));
            notes
        }) {
            tracing::warn!("filter: {note}");
        }
        let held = config.held.clone();
        for (h, _) in &filter_state.held {
            held.insert(*h);
        }
        let funnel = Arc::new(FunnelCounts::default());
        let rollups = Arc::new(Rollups::default());
        let started_at = unix_now();

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
            let token = coordinator.restore(
                WorkerId::LOCAL,
                q.info_hash,
                RestoredLease {
                    cohort: q.cohort,
                    source: q.source,
                    sources: 1u64 << q.source.get(),
                    strategy: q.strategy,
                    discovered_at: q.cohort * 3600,
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
        for (plugin, id) in plugins.into_iter().zip(ids) {
            let m = plugin.manifest();
            let priority = config
                .priorities
                .get(m.id)
                .copied()
                .unwrap_or(m.default_priority);
            weights.insert(id, priority.weight());
            let peer_requests = m.is(Kind::PeerFinder).then(|| {
                let (tx, rx) = mpsc::channel(1024);
                finders.push(tx);
                rx
            });
            let peer_replies = m
                .is(Kind::PeerFinder)
                .then(|| Emitter::replies(id, pool_tx.clone()));
            let emitter = Emitter::new(id, emit_tx.clone());
            let ctx = SourceCtx {
                source: id,
                emitter: emitter.clone(),
                peer_requests,
                peer_replies,
                shutdown: plugin_shutdown.clone(),
            };
            let p = plugin.clone();
            let running = Arc::new(AtomicBool::new(true));
            let flag = running.clone();
            tasks.push(tokio::spawn(async move {
                let result = p.run(ctx).await;
                flag.store(false, Ordering::Relaxed);
                if let Err(e) = &result {
                    tracing::error!(plugin = p.manifest().id, "source stopped: {e}");
                }
                result
            }));
            sources.push(SourceEntry {
                id,
                plugin,
                emitter,
                running,
            });
        }
        drop(emit_tx);

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
            backfill_after: 0,
            backfill_done: false,
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
            hot: config.hot.clone(),
            health: health.clone(),
        };
        drop(pool_tx);
        let writer = tokio::spawn(writer.run(writer::WriterIo {
            results: results_rx,
            touches: touch_rx,
            queues: queue_rx,
            retry: retry_tx,
            finish: finish_rx,
            swarm: swarm_rx,
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
            },
            tasks,
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
        })
    }

    pub fn monitor(&self) -> Monitor {
        self.monitor.clone()
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
                Ok(Ok(Err(e))) => tracing::warn!(plugin = id, "source stopped with an error: {e}"),
                Ok(_) => {}
                Err(_) => {
                    tracing::warn!(plugin = id, "source didn't stop in time; aborting it");
                    task.abort();
                }
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

/// Registers the plugins, loads the queue, builds the have-filter and
/// records the start (blocking).
#[allow(clippy::type_complexity)]
fn prepare(
    mut store: Store,
    manifests: &[(&'static str, &'static str, bool)],
    backfill: i64,
) -> Result<
    (
        Store,
        Vec<SourceId>,
        Vec<QueuedKey>,
        Have,
        i64,
        u64,
        FilterState,
        (RuleSet, Vec<(i64, String, bool)>),
    ),
    Error,
> {
    let now = unix_now();
    let ids = manifests
        .iter()
        .map(|(id, version, _)| store.register_source(id, version, now))
        .collect::<Result<Vec<_>, _>>()?;
    let first_run = match store.setting_i64(FIRST_RUN_SETTING)? {
        Some(t) => t,
        None => {
            store.set_setting_i64(FIRST_RUN_SETTING, now)?;
            now
        }
    };
    let mut queue = store.load_fetch_queue()?;
    // Imports with incomplete file lists get their info dicts fetched, if
    // there's a peer finder to find them peers.
    if let Some(finder) = manifests
        .iter()
        .zip(&ids)
        .find(|((_, _, pf), _)| *pf)
        .map(|(_, id)| *id)
    {
        let queued: std::collections::HashSet<_> = queue.iter().map(|q| q.info_hash).collect();
        for hash in store.partial_hashes(PARTIAL_REFETCH)? {
            if !queued.contains(&hash) {
                queue.push(QueuedKey {
                    info_hash: hash,
                    source: finder,
                    cohort: now.div_euclid(3600),
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
    let filter = store.filter_state()?;
    let rules = (store.rule_set()?, store.class_rules()?);
    Ok((
        store,
        ids,
        queue,
        have,
        first_run + backfill,
        stored,
        filter,
        rules,
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
        s => HotColumns::load(dir, s as u64).map_err(Error::Io)?,
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
