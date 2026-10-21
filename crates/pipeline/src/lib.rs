//! The pipeline (architecture §5–§7, §18.4): source plugins → intake (the
//! have-filter) → the shared fetch pool → the single writer.
//!
//! ```text
//!  plugins ──emit──▶ intake ──new──▶ fetch pool ──verified──▶ writer ──▶ store
//!     ▲                │                  │                     │
//!     └── peer asks ◀──┼──────────────────┘                     │
//!                      └──── stored? (confirm, add source) ─────┘
//! ```
//!
//! Each stage is one task with bounded channels between them. Live sources
//! never wait (a full pipeline drops and counts), importers do; the pool
//! waits on the writer, which is the backpressure that keeps memory flat.

mod dht;
mod have;
mod rollups;
mod writer;

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use lodestar_peer_wire::FetchOptions;
use lodestar_sources::pool::{self, PeerWireFetcher};
use lodestar_sources::{
    Discovery, Emitter, Fetcher, FunnelCounts, Kind, PluginError, PoolConfig, PoolHandle, PoolIo,
    PoolStats, Priority, Shutdown, SourceCtx, SourcePlugin,
};
use lodestar_store::{Event, QueuedKey, SourceId, Store, WriteBatch};
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::JoinHandle;

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

#[derive(Debug, Clone)]
pub struct PipelineConfig {
    pub pool: PoolConfig,
    pub fetch: FetchOptions,
    /// Per-plugin priority overrides, by manifest id.
    pub priorities: HashMap<String, Priority>,
    /// Info hashes never stored, whatever finds them (§18.7).
    pub blocklist: HashSet<[u8; 20]>,
    /// The crawler warm-up window: torrents found this soon after the
    /// first run are flagged `BACKFILL` (§6).
    pub backfill: Duration,
    pub flush_every: Duration,
    pub save_queue_every: Duration,
    pub batch_max: usize,
    pub batch_wait: Duration,
    pub intake_capacity: usize,
}

impl Default for PipelineConfig {
    fn default() -> Self {
        PipelineConfig {
            pool: PoolConfig::default(),
            fetch: FetchOptions::default(),
            priorities: HashMap::new(),
            blocklist: HashSet::new(),
            backfill: Duration::from_secs(30 * 86_400),
            flush_every: Duration::from_secs(5),
            save_queue_every: Duration::from_secs(300),
            batch_max: 256,
            batch_wait: Duration::from_millis(250),
            intake_capacity: 16_384,
        }
    }
}

#[derive(Debug)]
pub enum Error {
    Store(lodestar_store::Error),
    Join(tokio::task::JoinError),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Store(e) => e.fmt(f),
            Error::Join(e) => write!(f, "pipeline task failed: {e}"),
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
    pub to_pool: AtomicU64,
    /// Rediscoveries dropped because the writer was behind.
    pub dropped: AtomicU64,
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
    /// Strategy names by code, from the manifest.
    pub strategies: &'static [&'static str],
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
    have: Arc<Have>,
    sources: Arc<Vec<SourceEntry>>,
    stored_at_start: u64,
    started_at: i64,
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

    pub fn sources(&self) -> Vec<SourceStatus> {
        self.sources
            .iter()
            .map(|s| SourceStatus {
                id: s.id,
                plugin: s.plugin.manifest().id,
                name: s.plugin.manifest().name,
                running: s.running.load(Ordering::Relaxed),
                dropped: s.emitter.dropped(),
                strategies: s.plugin.manifest().strategies,
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
    saver: JoinHandle<()>,
    finish: oneshot::Sender<Vec<QueuedKey>>,
    writer: JoinHandle<Store>,
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
        let manifests: Vec<(&'static str, &'static str)> = plugins
            .iter()
            .map(|p| (p.manifest().id, p.manifest().version))
            .collect();
        let backfill = config.backfill.as_secs() as i64;
        let (store, ids, queue, have, backfill_until, stored_at_start) =
            tokio::task::spawn_blocking(move || prepare(store, &manifests, backfill))
                .await
                .map_err(Error::Join)??;
        tracing::info!(
            stored = have.len(),
            queued = queue.len(),
            "pipeline starting"
        );
        let have = Arc::new(have);
        let funnel = Arc::new(FunnelCounts::default());
        let rollups = Arc::new(Rollups::default());

        let (stop_plugins, plugin_shutdown) = Shutdown::new();
        let (stop_intake, intake_shutdown) = Shutdown::new();
        let (stop_pool, pool_shutdown) = Shutdown::new();
        let (emit_tx, emit_rx) = mpsc::channel(config.intake_capacity);
        let (pool_tx, pool_rx) = mpsc::channel(config.intake_capacity);
        let (touch_tx, touch_rx) = mpsc::channel(config.intake_capacity);
        // Small: each entry holds a whole info dictionary.
        let (out_tx, out_rx) = mpsc::channel(64);
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
            let emitter = Emitter::new(id, emit_tx.clone());
            let ctx = SourceCtx {
                source: id,
                emitter: emitter.clone(),
                peer_requests,
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
                funnel: funnel.clone(),
                shutdown: pool_shutdown,
            },
            weights,
            queue,
        );

        let intake_stats = Arc::new(IntakeStats::default());
        let intake = tokio::spawn(intake(
            emit_rx,
            have.clone(),
            pool_tx.clone(),
            touch_tx,
            rollups.clone(),
            intake_stats.clone(),
            intake_shutdown,
        ));

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

        let writer_stats = Arc::new(WriterStats::default());
        let writer = writer::Writer {
            store: Some(store),
            cfg: writer::WriterConfig {
                flush_every: config.flush_every,
                batch_max: config.batch_max,
                batch_wait: config.batch_wait,
                backfill_until,
            },
            have: have.clone(),
            funnel,
            rollups,
            blocklist: Arc::new(config.blocklist.clone()),
            stats: writer_stats.clone(),
        };
        let writer = tokio::spawn(writer.run(writer::WriterIo {
            fetched: out_rx,
            touches: touch_rx,
            queues: queue_rx,
            to_pool: pool_tx,
            finish: finish_rx,
        }));

        Ok(Pipeline {
            monitor: Monitor {
                pool,
                intake: intake_stats,
                writer: writer_stats,
                have,
                sources: Arc::new(sources),
                stored_at_start,
                started_at: unix_now(),
            },
            tasks,
            stop_plugins,
            stop_intake,
            stop_pool,
            intake,
            pool_task,
            saver,
            finish: finish_tx,
            writer,
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

    /// Stops the stages in order (plugins, intake, pool, writer), saving
    /// the DHT state and the fetch queue, and returns the store.
    pub async fn shutdown(self) -> Result<Store, Error> {
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
    manifests: &[(&'static str, &'static str)],
    backfill: i64,
) -> Result<(Store, Vec<SourceId>, Vec<QueuedKey>, Have, i64, u64), Error> {
    let now = unix_now();
    let ids = manifests
        .iter()
        .map(|(id, version)| store.register_source(id, version, now))
        .collect::<Result<Vec<_>, _>>()?;
    let first_run = match store.setting_i64(FIRST_RUN_SETTING)? {
        Some(t) => t,
        None => {
            store.set_setting_i64(FIRST_RUN_SETTING, now)?;
            now
        }
    };
    let queue = store.load_fetch_queue()?;
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
    Ok((store, ids, queue, have, first_run + backfill, stored))
}

/// Sends new keys to the pool and known ones to the writer.
async fn intake(
    mut rx: mpsc::Receiver<Discovery>,
    have: Arc<Have>,
    to_pool: mpsc::Sender<Discovery>,
    touches: mpsc::Sender<Discovery>,
    rollups: Arc<Rollups>,
    stats: Arc<IntakeStats>,
    mut shutdown: Shutdown,
) {
    loop {
        let d = tokio::select! {
            biased;
            _ = shutdown.requested() => break,
            d = rx.recv() => match d {
                Some(d) => d,
                None => break,
            },
        };
        stats.received.fetch_add(1, Ordering::Relaxed);
        rollups.add(
            d.at,
            metric::HARVESTED,
            d.source,
            i64::from(d.found.strategy),
            1,
        );
        if have.contains(&d.found.key) {
            stats.known.fetch_add(1, Ordering::Relaxed);
            if touches.try_send(d.clone()).is_err() {
                stats.dropped.fetch_add(1, Ordering::Relaxed);
                rollups.add(d.at, metric::DROPPED, d.source, 0, 1);
            }
            continue;
        }
        stats.to_pool.fetch_add(1, Ordering::Relaxed);
        // Waiting here backs up into the emitters: live sources drop and
        // count, importers wait.
        tokio::select! {
            biased;
            _ = shutdown.requested() => break,
            sent = to_pool.send(d) => if sent.is_err() { break },
        }
    }
}

pub(crate) fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}
