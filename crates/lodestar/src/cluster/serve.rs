//! The cluster port in `lodestar serve` (architecture §19.2, §19.6, §12;
//! workers plan WK-1 and Q3).
//!
//! **Off until allowed.** [`Cluster::start`] binds nothing and opens nothing
//! while "Allow workers" (`[cluster] enabled`) is off: no listener, no
//! cluster CA, no `cluster.` rows in `settings`, no secrets key opened for
//! it. Turning it on ([`ClusterHandle::set_allowed`], or the config at
//! start):
//! 1. binds `[cluster] listen` (default `0.0.0.0:8091`) first, so a port
//!    that's taken creates nothing;
//! 2. opens the cluster PKI ([`Coordinator::open`]: the CA and the
//!    coordinator's certificate are created on first use) on its own store
//!    connection, with the install's `secrets.key`;
//! 3. builds the [`ClusterServer`] on the pipeline's one
//!    [`LeaseAuthority`] (WK-0), with a store check for claimed keys, and
//!    serves the port. The server's own heartbeat tick reloads the PKI,
//!    runs `maintain` hourly, sweeps revoked and silent sessions and
//!    re-grants expired leases.
//!
//! Turning it off stops every session (`Stop(Shutdown)`) and closes the
//! port. Leases remote workers held stay in the authority and expire as
//! any silent worker's do. Nothing already created is deleted: the CA and
//! its workers' certificates are kept for when it's on again.
//!
//! **At start** a failure to bind or to open the PKI fails `serve` with the
//! reason, as any other bad configuration does. A later change that fails
//! is logged (`cluster.start_failed`) and counted; serving goes on.
//!
//! **Secrets-key rotation** ([`ClusterHandle::reseal`]) goes between
//! `SecretStore::rotate` and `SecretStore::retire_old_keys`. The server is
//! stopped first, so its hourly upkeep can't write a key sealed with the
//! old secrets key meanwhile; then the CA, previous CA and the
//! coordinator's keys are sealed again with the new primary key, and the
//! server is rebuilt on the key file as it is now. `serve` has no rotation
//! flow yet; whichever slice adds one calls this.

use std::collections::{BTreeMap, HashMap};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError, Weak};
use std::time::{Duration, Instant};

use lodestar_cluster::assign::Politeness;
use lodestar_cluster::proto::{
    HintItem, Outcome, Payload, ReverifyReply, SnapshotAck, SnapshotKind,
};
use lodestar_cluster::{
    Clock, ClusterError, ClusterServer, Coordinator, FleetSnapshot, InboundSink, KeyCheck, Lease,
    LeaseAuthority, Pki, ServerConfig, ServerHooks, StoredKeys, WorkerId,
};
use lodestar_filter::Handle;
use lodestar_integrations::SecretStore;
use lodestar_metainfo::InfoHash;
use lodestar_pipeline::remote::{RemoteIntake, RemoteSink};
use lodestar_pipeline::wire::filter_snapshot::{DictSnapshot, WireKey, WireKeys};
use lodestar_pipeline::{Have, HaveObserver, Monitor};
use lodestar_store::{Store, StoreConfig};
use tokio::net::TcpListener;
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::JoinHandle;

use super::filter::{WorkerSnapshots, day_of, read_snapshot, wire_keys};
use super::have::WorkerFilter;
use super::placement::Placement;
use crate::config;
use crate::logging::events;

/// The cluster PKI's rows in `settings` (`lodestar_cluster::Coordinator`).
const PKI_PREFIX: &str = "cluster.";

/// What the cluster port needs from the rest of `serve`.
#[derive(Clone)]
pub struct Wiring {
    /// `lodestar.db` and its settings: the PKI and the store check each
    /// open their own connection.
    pub db: PathBuf,
    pub store: StoreConfig,
    /// Holds `secrets.key`, which seals the cluster's private keys.
    pub data_dir: PathBuf,
    /// The pipeline's lease authority (`Monitor::lease_authority`).
    pub authority: Arc<LeaseAuthority>,
    /// The pipeline's intake for what workers send (`Monitor::remote`,
    /// WK-9). Without one, results and counters are refused.
    pub remote: Option<Arc<RemoteIntake>>,
    pub settings: config::Cluster,
    pub clock: Arc<dyn Clock>,
    /// The one planner (`Workers::placement`, WK-14): the tick syncs it.
    pub placement: Arc<Placement>,
    /// What the tick sends remote workers to judge with and check against
    /// (WK-3, WK-8). Without it no snapshot is built or sent and no
    /// sealed record opens.
    pub fleet: Option<Fleet>,
    /// The coordinator's own DHT node, while it runs (`DhtSource`'s
    /// handle): its address vote is the coordinator's public address for
    /// the planner (`set_coordinator_ips`, WK-5). `None`: no DHT.
    pub coordinator_dht: Option<CoordinatorDht>,
    /// Applies the coordinator's own share of the plan to its pool and its
    /// DHT (WK-10e, [`CoordinatorShare`]). `None`: nothing to apply it to.
    pub coordinator_share: Option<Arc<dyn ApplyShare>>,
    /// The built-in worker's held set (`PipelineConfig::held`): a worker's
    /// floor report goes into it as the sink takes it, so the built-in worker refuses
    /// the key too (WK-10d). `None` in tests that need none.
    pub held: Option<Arc<lodestar_filter::HeldSet>>,
}

/// The coordinator DHT's handle slot, set while its node runs.
pub type CoordinatorDht = Arc<std::sync::Mutex<Option<lodestar_dht::node::NodeHandle>>>;

/// Where the coordinator's own share of the plan goes (WK-10e): its
/// pool's per-peer-IP cap and its DHT's BEP 51 scale, so the coordinator
/// and the workers behind its public address together act as one install.
pub trait ApplyShare: Send + Sync + 'static {
    fn apply(
        &self,
        share: Politeness,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'static>>;
}

/// The coordinator's pool, and its DHT node while it runs.
pub struct CoordinatorShare {
    pub pool: lodestar_sources::PoolHandle,
    pub dht: Option<CoordinatorDht>,
}

impl ApplyShare for CoordinatorShare {
    fn apply(
        &self,
        share: Politeness,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'static>> {
        let pool = self.pool.clone();
        let dht = self
            .dht
            .as_ref()
            .and_then(|slot| slot.lock().unwrap_or_else(PoisonError::into_inner).clone());
        Box::pin(async move {
            let cap = crate::worker::runtime::ShareCap(share);
            pool.set_ip_cap(Some(Arc::new(cap))).await;
            if let Some(node) = dht {
                node.set_sample_scale(share.bep51_interval_scale).await;
            }
        })
    }
}

/// `share` if it's a share the protocol accepts (the same check an
/// `Assign` gets, `AssignError::Field("politeness")` otherwise), so the
/// coordinator never applies one a worker would refuse.
fn checked_share(share: Politeness) -> Result<Politeness, lodestar_cluster::assign::AssignError> {
    lodestar_cluster::assign::Assignment {
        revision: 1,
        dht: None,
        fetch: true,
        prepare: true,
        intensity: lodestar_cluster::assign::Intensity::default(),
        budget: lodestar_cluster::assign::Budget::default(),
        pool_cap: 1,
        dict_version: 0,
        filter_version: 0,
        politeness: share,
    }
    .validate()
    .map(|()| share)
}

impl std::fmt::Debug for Wiring {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Wiring")
            .field("listen", &self.settings.listen)
            .field("fleet", &self.fleet.is_some())
            .finish_non_exhaustive()
    }
}

/// The coordinator's stored set, as the workers' have-filter needs it.
pub trait Stored: Send + Sync + 'static {
    /// Stored swarm hashes (`Have::len`, not torrents).
    fn swarm_hashes(&self) -> u64;
    /// Sets the one insert observer (`Have::observe`).
    fn observe(&self, observer: Arc<dyn HaveObserver>) -> bool;
}

impl Stored for Monitor {
    fn swarm_hashes(&self) -> u64 {
        self.have().len()
    }

    fn observe(&self, observer: Arc<dyn HaveObserver>) -> bool {
        self.have().observe(observer)
    }
}

impl Stored for Have {
    fn swarm_hashes(&self) -> u64 {
        self.len()
    }

    fn observe(&self, observer: Arc<dyn HaveObserver>) -> bool {
        Have::observe(self, observer)
    }
}

/// What remote workers are sent and checked against (WK-3, WK-8, WK-10).
#[derive(Clone)]
pub struct Fleet {
    pub stored: Arc<dyn Stored>,
    /// The commit's filter engine: its version says when the filter
    /// snapshot is due again.
    pub filter: Arc<Handle>,
    /// `seal.key`: the day's wire keys are derived from it (WK-8). It
    /// never leaves this process.
    pub seal_key: [u8; 32],
}

/// The server configuration `[cluster]` gives (its numbers were checked at
/// load, `config::Cluster::check`). Heartbeats, lease TTL and the rest keep
/// the protocol's defaults.
pub fn server_config(c: &config::Cluster) -> ServerConfig {
    ServerConfig {
        max_skew_ms: c.max_skew_ms,
        handshake_timeout: Duration::from_secs(c.handshake_timeout_secs),
        max_pending: c.max_pending,
        max_pending_per_ip: c.max_pending_per_ip,
        session_queue_bytes: c.session_queue_mib << 20,
        ..ServerConfig::default()
    }
}

/// Why the cluster port couldn't open, for `serve`'s error and the log.
/// Its text never holds key material: `ClusterError`'s doesn't.
#[derive(Debug)]
pub struct StartError {
    /// `bind`, `store`, `secrets`, `pki` or `server`.
    step: &'static str,
    /// A fixed kind for the log line.
    kind: &'static str,
    detail: String,
}

impl std::fmt::Display for StartError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "cluster port ({}): {}", self.step, self.detail)
    }
}

impl std::error::Error for StartError {}

impl StartError {
    fn bind(listen: SocketAddr, e: &std::io::Error) -> StartError {
        StartError {
            step: "bind",
            kind: io_kind(e.kind()),
            detail: format!("binding cluster.listen {listen}: {e}"),
        }
    }

    fn pki(step: &'static str, e: &ClusterError) -> StartError {
        StartError {
            step,
            kind: cluster_kind(e),
            detail: e.to_string(),
        }
    }

    fn task() -> StartError {
        StartError {
            step: "pki",
            kind: "task_failed",
            detail: "the PKI task failed".into(),
        }
    }
}

fn io_kind(k: std::io::ErrorKind) -> &'static str {
    use std::io::ErrorKind as K;
    match k {
        K::AddrInUse => "addr_in_use",
        K::AddrNotAvailable => "addr_not_available",
        K::PermissionDenied => "permission_denied",
        _ => "io",
    }
}

fn cluster_kind(e: &ClusterError) -> &'static str {
    match e {
        ClusterError::Store(_) => "store",
        ClusterError::Secret(_) => "secrets",
        ClusterError::Tls(_) | ClusterError::Verifier(_) => "tls",
        ClusterError::Io(..) => "io",
        ClusterError::Corrupt(_) => "corrupt",
        ClusterError::Random => "random",
        _ => "pki",
    }
}

/// Floor drops remote workers reported (`Lost(Blocked)`, WK-10d), until
/// each is a `worker-floor:<id>` tombstone in the store
/// ([`Store::tombstone_from_worker`]).
///
/// - **Refused as soon as the sink sees it, everywhere:** a report goes
///   into this map, and into the built-in worker's `HeldSet` (its intake,
///   parked keys and commit refuse what that set holds), on the sink's
///   path: short locks, nothing that waits. The claim check
///   ([`StoreCheck`]) reads the map before the store, so a re-claim is
///   denied before the row lands.
/// - **Never dropped within the budget:** a report leaves the map only
///   once its row is written; a failed write keeps it for the next try.
/// - **Bounded:** each worker may report [`FLOOR_BUDGET`] drops per
///   [`FLOOR_WINDOW`] (rolling). The first report past that is the sink's
///   cue to pause the worker and take its leases back; that one and any
///   later ones are dropped (counted, nowhere else): the worker is
///   untrusted by then, and the floor still applies to whoever fetches
///   the key next. So only reports within a budget are in the map, and at
///   most [`FLOOR_PENDING_CAP`] of them (reached only if writes keep
///   failing); past that a report is turned away, counted, and doesn't
///   use up a budget slot. (The leases a worker holds don't bound it: a
///   worker can claim and report again and again, #607's reviews.)
#[derive(Debug)]
pub(crate) struct FloorReports {
    pending: Mutex<BTreeMap<[u8; 20], Report>>,
    /// Each worker's reports in the rolling window (oldest first).
    recent: Mutex<HashMap<u32, std::collections::VecDeque<Instant>>>,
    /// The built-in worker's held set (`None` in tests that need none).
    held: Option<Arc<lodestar_filter::HeldSet>>,
    /// [`FLOOR_PENDING_CAP`], smaller in tests.
    cap: usize,
    wake: tokio::sync::Notify,
    /// Reports taken, rows written, writes that failed (kept), reports
    /// over a worker's budget (dropped), and reports the full map turned
    /// away.
    pub reported: AtomicU64,
    pub written: AtomicU64,
    pub write_failures: AtomicU64,
    pub over_budget: AtomicU64,
    pub turned_away: AtomicU64,
}

/// One pending report: who reported it, and when.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Report {
    worker: u32,
    at: i64,
}

/// What became of a report.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Reported {
    /// Within the worker's budget: refused, and written soon.
    Taken,
    /// Over its budget: dropped; `first` the first report over it in this
    /// window (the sink pauses the worker and evicts its leases then).
    OverBudget { first: bool },
    /// Within its budget, but the map is full: counted, nothing else, and
    /// no budget slot used.
    TurnedAway,
}

/// A worker's floor reports per rolling window before it's paused. An
/// honest worker reports a floor drop rarely: the hash lists refuse most
/// floor keys before a claim, and a CS-8a repost is rare.
pub(crate) const FLOOR_BUDGET: usize = 100;
pub(crate) const FLOOR_WINDOW: Duration = Duration::from_secs(3600);
/// Reports the map holds at most (about 5 MB).
pub(crate) const FLOOR_PENDING_CAP: usize = 100_000;

impl Default for FloorReports {
    fn default() -> FloorReports {
        FloorReports::new(None)
    }
}

impl FloorReports {
    pub(crate) fn new(held: Option<Arc<lodestar_filter::HeldSet>>) -> FloorReports {
        FloorReports::with_cap(held, FLOOR_PENDING_CAP)
    }

    fn with_cap(held: Option<Arc<lodestar_filter::HeldSet>>, cap: usize) -> FloorReports {
        FloorReports {
            pending: Mutex::default(),
            recent: Mutex::default(),
            held,
            cap,
            wake: tokio::sync::Notify::new(),
            reported: AtomicU64::new(0),
            written: AtomicU64::new(0),
            write_failures: AtomicU64::new(0),
            over_budget: AtomicU64::new(0),
            turned_away: AtomicU64::new(0),
        }
    }

    fn pending(&self) -> std::sync::MutexGuard<'_, BTreeMap<[u8; 20], Report>> {
        self.pending.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn recent(
        &self,
    ) -> std::sync::MutexGuard<'_, HashMap<u32, std::collections::VecDeque<Instant>>> {
        self.recent.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// A worker's floor drop at `now`. The first report of a key keeps its
    /// worker. The budget decides first, on its own: whether the map has
    /// room never changes whether a report is over budget, or the first
    /// one over it. (Locks: `recent`, then `pending`.)
    fn report(&self, worker: WorkerId, key: &InfoHash, at: i64, now: Instant) -> Reported {
        self.reported.fetch_add(1, Ordering::Relaxed);
        let mut recent = self.recent();
        let q = recent.entry(worker.0).or_default();
        while q
            .front()
            .is_some_and(|t| now.duration_since(*t) >= FLOOR_WINDOW)
        {
            q.pop_front();
        }
        if q.len() >= FLOOR_BUDGET {
            // The window holds the budget's reports, plus one mark for the
            // first report over it, so that one alone is `first`.
            let first = q.len() == FLOOR_BUDGET;
            if first {
                q.push_back(now);
            }
            self.over_budget.fetch_add(1, Ordering::Relaxed);
            return Reported::OverBudget { first };
        }
        {
            let mut pending = self.pending();
            if !pending.contains_key(&key.0) {
                if pending.len() >= self.cap {
                    self.turned_away.fetch_add(1, Ordering::Relaxed);
                    return Reported::TurnedAway;
                }
                pending.insert(
                    key.0,
                    Report {
                        worker: worker.0,
                        at,
                    },
                );
            }
        }
        q.push_back(now);
        drop(recent);
        if let Some(held) = &self.held {
            held.insert(key.0);
        }
        self.wake.notify_one();
        Reported::Taken
    }

    /// Forgets a revoked worker: its reports not written yet and its
    /// budget window (#607's review). Its written tombstones stay until
    /// WK-13's purge-by-worker; the built-in worker's held set keeps the
    /// dropped keys until the next start, refusing more, never less.
    /// Returns how many reports it dropped.
    pub(crate) fn forget_worker(&self, worker: WorkerId) -> usize {
        let mut recent = self.recent();
        recent.remove(&worker.0);
        let mut pending = self.pending();
        let before = pending.len();
        pending.retain(|_, r| r.worker != worker.0);
        before - pending.len()
    }

    fn holds(&self, key: &InfoHash) -> bool {
        self.pending().contains_key(&key.0)
    }

    pub(crate) fn len(&self) -> usize {
        self.pending().len()
    }

    /// Reports waiting to be written.
    fn unwritten(&self) -> usize {
        self.len()
    }

    /// Writes up to `FLOOR_BATCH` pending reports, removing each once its
    /// row is in. Blocking. `Err` keeps them all for the next try.
    fn flush(
        &self,
        db: &std::path::Path,
        config: StoreConfig,
    ) -> Result<usize, lodestar_store::Error> {
        let batch: Vec<([u8; 20], Report)> = self
            .pending()
            .iter()
            .take(FLOOR_BATCH)
            .map(|(k, v)| (*k, *v))
            .collect();
        if batch.is_empty() {
            return Ok(0);
        }
        let mut store = Store::open(db, config)?;
        let mut by_worker: BTreeMap<(u32, i64), Vec<[u8; 20]>> = BTreeMap::new();
        for (k, r) in &batch {
            by_worker.entry((r.worker, r.at)).or_default().push(*k);
        }
        for ((w, at), keys) in &by_worker {
            // Nothing stays stored under a held hash (as `hold_claims`): a
            // key stored after this worker's grant (another worker's
            // result, or the built-in worker's) goes, and the indexer drops
            // it. First, so a crash between the two leaves the report to
            // come again (the crash window), never a torrent stored under
            // a hold.
            store.delete_torrents(keys)?;
            // A key already tombstoned (by its rule or an earlier report)
            // keeps its row; either way it's held now.
            store.tombstone_from_worker(keys, *w, *at)?;
        }
        let mut pending = self.pending();
        for (k, v) in &batch {
            if pending.get(k) == Some(v) {
                pending.remove(k);
            }
        }
        self.written
            .fetch_add(batch.len() as u64, Ordering::Relaxed);
        Ok(batch.len())
    }
}

/// Floor reports written per store transaction.
const FLOOR_BATCH: usize = 1024;
/// How long the floor writer waits before trying a failed write again.
const FLOOR_RETRY: Duration = Duration::from_secs(5);

/// Writes floor reports as they come, until `stopped`; then once more,
/// so a report taken before a shutdown is in the store when it ends.
async fn write_floor(
    floor: Arc<FloorReports>,
    db: PathBuf,
    config: StoreConfig,
    mut stopped: watch::Receiver<bool>,
) {
    let mut failing = false;
    loop {
        let stop = tokio::select! {
            () = floor.wake.notified() => false,
            () = tokio::time::sleep(FLOOR_RETRY), if failing || floor.unwritten() > 0 => false,
            _ = stopped.changed() => true,
        };
        loop {
            let (f, db, config) = (floor.clone(), db.clone(), config.clone());
            match tokio::task::spawn_blocking(move || f.flush(&db, config)).await {
                Ok(Ok(0)) => {
                    failing = false;
                    break;
                }
                Ok(Ok(_)) => failing = false,
                Ok(Err(e)) => {
                    floor.write_failures.fetch_add(1, Ordering::Relaxed);
                    if !std::mem::replace(&mut failing, true) {
                        lodestar_log::log_event!(
                            events::CLUSTER_FLOOR_REPORT_FAILED,
                            error = lodestar_log::Error::new(&e)
                        );
                    }
                    break;
                }
                Err(_) => break,
            }
        }
        if stop {
            return;
        }
    }
}

/// Whether claimed keys are stored (§19.3's authoritative check before a
/// grant, and before a re-grant), on a read-only connection of its own. A
/// key that's tombstoned or quarantined, reported by a worker, or on the
/// floor's hash lists is held: it's never granted, so a held key (a floor
/// drop above all) is never fetched again by a worker, as the built-in
/// worker's intake refuses it (WK-10d). A read that fails leaves the
/// answer unknown, which refuses a claim and makes a re-grant wait: the
/// check fails closed (#607's reviews).
struct StoreCheck {
    reader: Mutex<Store>,
    errors: Arc<AtomicU64>,
    /// Floor reports not written yet: refused as held.
    floor: Arc<FloorReports>,
    /// The floor's hash lists (CS-1a/1b and floor-marked lists), as the
    /// built-in intake checks them (`None` without a fleet).
    filter: Option<Arc<Handle>>,
}

impl StoreCheck {
    /// One answer per key, with `between` run after the floor map is read
    /// and before the store is (a seam for the ordering test).
    fn check_with(&self, keys: &[InfoHash], between: impl FnOnce()) -> Vec<KeyCheck> {
        // The map first: a report leaves it only after its row is
        // committed, so a key read missing here is in the store by the
        // time the store is read (#607's review).
        let reported: Vec<bool> = keys.iter().map(|k| self.floor.holds(k)).collect();
        between();
        let reader = self.reader.lock().unwrap_or_else(PoisonError::into_inner);
        let hashes: Vec<[u8; 20]> = keys.iter().map(|k| k.0).collect();
        // `None`: the read failed, so any key might have been held.
        let held = reader.held_among(&hashes).map_err(|_| {
            self.errors.fetch_add(1, Ordering::Relaxed);
        });
        let engine = self.filter.as_ref().map(|f| f.current());
        keys.iter()
            .enumerate()
            .map(|(i, k)| {
                let listed = engine
                    .as_ref()
                    .is_some_and(|e| e.blocked_hash(&[k.0]).is_some());
                let held = held
                    .as_ref()
                    .ok()
                    .map(|h| h.get(i).copied().unwrap_or(true));
                if reported[i] || listed || held == Some(true) {
                    return KeyCheck::Held;
                }
                match (reader.find(k), held) {
                    (Ok(Some(_)), _) => KeyCheck::Stored,
                    (Ok(None), Some(false)) => KeyCheck::Free,
                    (Ok(None), _) => KeyCheck::Unknown,
                    (Err(_), _) => {
                        self.errors.fetch_add(1, Ordering::Relaxed);
                        KeyCheck::Unknown
                    }
                }
            })
            .collect()
    }
}

impl StoredKeys for StoreCheck {
    /// A claim's check: anything but a free key is refused, an unknown
    /// one included.
    fn stored(&self, keys: &[InfoHash]) -> Vec<bool> {
        self.check_with(keys, || {})
            .into_iter()
            .map(|c| c != KeyCheck::Free)
            .collect()
    }

    fn regrant_check(&self, keys: &[InfoHash]) -> Vec<KeyCheck> {
        self.check_with(keys, || {})
    }
}

/// Opens the PKI (creating the CA on first use) and the store check's
/// connection. Blocking.
fn open_pki(
    w: &Wiring,
    errors: Arc<AtomicU64>,
    floor: Arc<FloorReports>,
) -> Result<(Pki, StoreCheck), StartError> {
    let store_err = |e: lodestar_store::Error| StartError::pki("store", &ClusterError::Store(e));
    let mut store = Store::open(&w.db, w.store.clone()).map_err(store_err)?;
    let secrets = SecretStore::open_or_create(&w.data_dir)
        .map_err(|e| StartError::pki("secrets", &ClusterError::Secret(e)))?;
    let coordinator = Coordinator::open(&mut store, &secrets, w.clock.now_secs())
        .map_err(|e| StartError::pki("pki", &e))?;
    let reader = Store::open_reader(&w.db, w.store.clone()).map_err(store_err)?;
    Ok((
        Pki {
            coordinator,
            store,
            secrets,
        },
        StoreCheck {
            reader: Mutex::new(reader),
            errors,
            floor,
            filter: w.fleet.as_ref().map(|f| f.filter.clone()),
        },
    ))
}

/// Seals the cluster's keys again with the key file's primary key, if a
/// cluster PKI exists; `Ok(false)` (and nothing written) if it never did.
/// Blocking; no server may be running on this database (an offline
/// rotation, with `serve` stopped, can call it directly).
pub(crate) fn reseal_pki(w: &Wiring) -> Result<bool, ClusterError> {
    let mut store = Store::open(&w.db, w.store.clone())?;
    if store.settings_with_prefix(PKI_PREFIX)?.is_empty() {
        return Ok(false);
    }
    let secrets = SecretStore::open_or_create(&w.data_dir)?;
    let mut coordinator = Coordinator::open(&mut store, &secrets, w.clock.now_secs())?;
    coordinator.reseal(&mut store, &secrets)?;
    Ok(true)
}

/// How often the cluster tick runs (WK-10): placement, the have-filter and
/// the filter and dictionary snapshots.
pub const TICK: Duration = Duration::from_secs(1);
/// Filter and dictionary acknowledgements one worker may have waiting for
/// the tick. An honest worker has at most a window of chunks in flight per
/// kind (`filter::WINDOW`, two kinds), so this is twice that; past it, that
/// worker's acknowledgements are dropped and counted, and no other
/// worker's are.
const ACKS_PER_WORKER: u32 = 4 * super::filter::WINDOW;
/// The tick's acknowledgement queue: room for every planned worker's share.
const ACK_QUEUE: usize = lodestar_cluster::placement::MAX_WORKERS * ACKS_PER_WORKER as usize;
/// Back-off between failed have-filter rebuilds (WK-3: a failing store
/// would otherwise be scanned again on every tick).
const REBUILD_RETRY_FIRST: Duration = Duration::from_secs(1);
const REBUILD_RETRY_MAX: Duration = Duration::from_secs(300);

/// A back-off after failures: 1 s, doubling to 5 minutes, cleared by a
/// success (WK-3's note for rebuilds; the review's N2 for snapshot reads).
#[derive(Debug)]
struct Retry {
    first: Duration,
    after: Mutex<Option<(Instant, Duration)>>,
}

impl Retry {
    fn new(first: Duration) -> Retry {
        Retry {
            first,
            after: Mutex::new(None),
        }
    }

    /// Whether a try is due.
    fn due(&self) -> bool {
        lock(&self.after).is_none_or(|(at, _)| Instant::now() >= at)
    }

    fn failed(&self) {
        let mut after = lock(&self.after);
        let wait = after.map_or(self.first, |(_, w)| (w * 2).min(REBUILD_RETRY_MAX));
        *after = Some((Instant::now() + wait, wait));
    }

    fn succeeded(&self) {
        *lock(&self.after) = None;
    }

    /// The current wait, if backing off.
    #[cfg(test)]
    fn wait(&self) -> Option<Duration> {
        lock(&self.after).map(|(_, w)| w)
    }
}

/// From this time of day (UTC), the next day's wire key and its snapshot
/// go out, so workers seal under it at 00:00Z with no gap (runner,
/// #592's B2).
pub const PRESEND_FROM: i64 = 23 * 3600;
const DAY: i64 = 86_400;

/// The time whose day the snapshot's wire key is for: `now`, or from
/// [`PRESEND_FROM`] the start of the next day.
fn key_time(now: i64) -> i64 {
    let into = now.rem_euclid(DAY);
    if into >= PRESEND_FROM {
        now - into + DAY
    } else {
        now
    }
}

/// The keys sealed records open with at `now`: yesterday's (a spool holds
/// a record for at most 24 h), today's, and from [`PRESEND_FROM`] the next
/// day's, which the snapshot sent then carries. A worker only ever holds a
/// key the coordinator built, and refuses to seal past that key's day by
/// its own clock (`Judge::judge_at`).
fn open_keys(seal_key: &[u8; 32], now: i64) -> Vec<WireKey> {
    let mut keys = wire_keys(seal_key, now);
    let next = day_of(key_time(now));
    if next > day_of(now) {
        keys.push(super::filter::wire_key(seal_key, next));
    }
    keys
}

/// Sets `keys` to the open keys at `now` when they've changed since
/// `seen` (the day, and the key day: a new day or the pre-send window).
/// Returns whether it set them.
fn follow_keys(
    keys: &WireKeys,
    seal_key: &[u8; 32],
    now: i64,
    seen: &mut Option<(u32, u32)>,
) -> bool {
    let mark = (day_of(now), day_of(key_time(now)));
    if *seen == Some(mark) {
        return false;
    }
    keys.set(open_keys(seal_key, now));
    *seen = Some(mark);
    true
}

/// The coordinator's per-process cluster state the tick drives: one
/// workers' have-filter (its observer can be set only once), one set of
/// snapshots, the counters.
struct Tick {
    placement: Arc<Placement>,
    fleet: Option<Fleet>,
    have: Arc<WorkerFilter>,
    /// The observer is set the first time a server is built.
    observing: AtomicBool,
    /// Taken out while an install runs on a blocking thread; the blocking
    /// side always puts it back, even if the tick is aborted meanwhile.
    snapshots: Mutex<Option<WorkerSnapshots>>,
    /// The intake's keys for the server being served (WK-9c).
    keys: Mutex<Option<Arc<WireKeys>>>,
    /// Filter and dictionary acknowledgements each worker has queued.
    queued_acks: Mutex<HashMap<WorkerId, u32>>,
    rebuilding: Arc<AtomicBool>,
    /// After a failed rebuild, and after a failed snapshot read.
    rebuild_retry: Arc<Retry>,
    read_retry: Retry,
    /// The have-filter is at its largest size and full; said once.
    at_cap: AtomicBool,
    /// The snapshots' store reads are failing; said once until they work.
    store_failing: AtomicBool,
    install_failures: AtomicU64,
    rebuild_failures: Arc<AtomicU64>,
    have_rebuilds: Arc<AtomicU64>,
    snapshot_installs: AtomicU64,
    acks_dropped: AtomicU64,
    /// Ticks run, for tests that wait on them.
    steps: AtomicU64,
    /// One read connection for the snapshots, opened on first use and
    /// dropped after an error.
    reader: Mutex<Option<Store>>,
    db: PathBuf,
    store: StoreConfig,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Tick {
    fn new(w: &Wiring) -> Tick {
        Tick {
            placement: w.placement.clone(),
            fleet: w.fleet.clone(),
            have: Arc::new(WorkerFilter::new()),
            observing: AtomicBool::new(false),
            snapshots: Mutex::new(Some(WorkerSnapshots::new())),
            keys: Mutex::new(None),
            queued_acks: Mutex::new(HashMap::new()),
            rebuilding: Arc::new(AtomicBool::new(false)),
            rebuild_retry: Arc::new(Retry::new(REBUILD_RETRY_FIRST)),
            read_retry: Retry::new(REBUILD_RETRY_FIRST),
            at_cap: AtomicBool::new(false),
            store_failing: AtomicBool::new(false),
            install_failures: AtomicU64::new(0),
            rebuild_failures: Arc::new(AtomicU64::new(0)),
            have_rebuilds: Arc::new(AtomicU64::new(0)),
            snapshot_installs: AtomicU64::new(0),
            acks_dropped: AtomicU64::new(0),
            steps: AtomicU64::new(0),
            reader: Mutex::new(None),
            db: w.db.clone(),
            store: w.store.clone(),
        }
    }

    fn snapshots(&self) -> std::sync::MutexGuard<'_, Option<WorkerSnapshots>> {
        lock(&self.snapshots)
    }

    /// Runs `f` on the snapshots' read connection (blocking). `None` if it
    /// can't open or `f` fails; the connection is opened again next time.
    fn with_reader<T>(
        &self,
        f: impl FnOnce(&Store) -> Result<T, lodestar_store::Error>,
    ) -> Option<T> {
        let mut reader = lock(&self.reader);
        if reader.is_none() {
            *reader = Store::open_reader(&self.db, self.store.clone()).ok();
        }
        let out = f(reader.as_ref()?);
        if out.is_err() {
            *reader = None;
        }
        out.ok()
    }

    /// A snapshot store read failed: said once, until one works again, and
    /// not tried again before its back-off.
    fn store_failed(&self) {
        self.read_retry.failed();
        self.install_failures.fetch_add(1, Ordering::Relaxed);
        if !self.store_failing.swap(true, Ordering::Relaxed) {
            lodestar_log::log_event!(
                events::CLUSTER_SNAPSHOT_FAILED,
                error = lodestar_log::Error::kind("store")
            );
        }
    }

    /// Once workers are allowed: every insert reaches the workers' filter
    /// from now on (until a worker connects that costs one atomic load).
    fn observe(&self) {
        if let Some(f) = &self.fleet
            && !self.observing.swap(true, Ordering::AcqRel)
        {
            f.stored.observe(self.have.clone());
        }
    }
}

/// What `Workers` does when a worker first goes over its floor budget:
/// saves its pause (WK-14's row), so Workers shows it, it survives a
/// restart, and only an admin's resume ends it. It must not block.
pub(crate) type OverBudgetHook = Arc<dyn Fn(WorkerId) + Send + Sync>;

/// Where `Workers` registers its [`OverBudgetHook`] (empty until then).
type HookSlot = Arc<Mutex<Option<OverBudgetHook>>>;

/// The server's inbound sink: the pipeline's intake (WK-9), with snapshot
/// acknowledgements routed to the tick (WK-10). Nothing here blocks: the
/// have-filter's `ack` takes only its own mutex, and the other kinds are
/// queued for the tick, at most [`ACKS_PER_WORKER`] per worker.
struct Sink {
    remote: Option<Arc<RemoteSink>>,
    floor: Arc<FloorReports>,
    clock: Arc<dyn Clock>,
    have: Arc<WorkerFilter>,
    acks: mpsc::Sender<(WorkerId, SnapshotAck)>,
    server: OnceLock<Weak<ClusterServer>>,
    tick: Weak<Tick>,
    over_budget: HookSlot,
}

impl Sink {
    /// A worker's first floor report over its budget: far more than an
    /// honest worker reports. In this order: it's paused and its leases
    /// are taken back (so its later reports are stale), it's logged, and
    /// then `Workers` saves the pause. The save never comes first, and the
    /// first two stand whether or not it happens (#607's review).
    fn over_budget(&self, worker: WorkerId) {
        if let Some(server) = self.server.get().and_then(Weak::upgrade) {
            server.pause(worker);
            server.authority().evict(worker);
        }
        lodestar_log::log_event!(
            events::CLUSTER_FLOOR_REPORTS_OVER_BUDGET,
            worker = lodestar_log::Count(u64::from(worker.0))
        );
        let hook = self
            .over_budget
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        if let Some(hook) = hook {
            hook(worker);
        }
    }
}

impl InboundSink for Sink {
    fn results(&self, worker: WorkerId, batch: Payload) -> bool {
        self.remote
            .as_ref()
            .is_some_and(|r| r.results(worker, batch))
    }

    fn counters(&self, worker: WorkerId, batch: Payload) -> bool {
        self.remote
            .as_ref()
            .is_some_and(|r| r.counters(worker, batch))
    }

    fn outcomes(&self, worker: WorkerId, accepted: Vec<(InfoHash, Lease, Outcome)>) {
        // A floor drop leaves a tombstone, as a local one does: the key is
        // never claimed, fetched or stored again (WK-10d).
        let blocked = lodestar_sources::Loss::Blocked as u8;
        let (now, mono) = (self.clock.now_secs(), Instant::now());
        for (key, _, outcome) in &accepted {
            if !matches!(outcome, Outcome::Lost { reason } if *reason == blocked) {
                continue;
            }
            if let Reported::OverBudget { first: true } = self.floor.report(worker, key, now, mono)
            {
                self.over_budget(worker);
            }
        }
        if let Some(r) = &self.remote {
            r.outcomes(worker, accepted);
        }
    }

    fn snapshot_ack(&self, worker: WorkerId, ack: SnapshotAck) {
        if ack.kind == SnapshotKind::HaveFilter {
            if let Some(server) = self.server.get().and_then(Weak::upgrade) {
                self.have.ack(worker, ack, &server, Instant::now());
            }
            return;
        }
        let Some(tick) = self.tick.upgrade() else {
            return;
        };
        let mut queued = lock(&tick.queued_acks);
        let n = queued.entry(worker).or_insert(0);
        if *n >= ACKS_PER_WORKER || self.acks.try_send((worker, ack)).is_err() {
            drop(queued);
            tick.acks_dropped.fetch_add(1, Ordering::Relaxed);
            return;
        }
        *n += 1;
    }

    fn reverified(&self, worker: WorkerId, reply: ReverifyReply) {
        if let Some(r) = &self.remote {
            r.reverified(worker, reply);
        }
    }

    fn local_hints(&self, hints: Vec<HintItem>) {
        if let Some(r) = &self.remote {
            r.local_hints(hints);
        }
    }
}

/// What one tick remembers for the next.
#[derive(Debug, Default)]
struct Seen {
    /// Each connected remote worker's session id at the last tick.
    sessions: BTreeMap<WorkerId, u64>,
    /// The day and key day the open keys were last set for.
    key_day: Option<(u32, u32)>,
    /// When the coordinator DHT's address vote was last read.
    address_at: Option<Instant>,
    /// The coordinator's own share as last applied, and when (WK-10e).
    share: Option<(Politeness, Instant)>,
}

/// How often the coordinator DHT's address vote is read.
const ADDRESS_EVERY: Duration = Duration::from_secs(60);

/// Hands the planner the coordinator's public address from its DHT's
/// vote, every [`ADDRESS_EVERY`] (WK-5: without it every plan treats the
/// fleet as one address). A missing vote, or one that isn't public,
/// changes nothing; the planner itself ignores an unchanged set.
async fn coordinator_address(tick: &Tick, wiring: &Wiring, seen: &mut Seen) {
    let due = seen
        .address_at
        .is_none_or(|at| at.elapsed() >= ADDRESS_EVERY);
    let Some(slot) = wiring.coordinator_dht.as_ref().filter(|_| due) else {
        return;
    };
    seen.address_at = Some(Instant::now());
    let handle = slot.lock().unwrap_or_else(PoisonError::into_inner).clone();
    let Some(handle) = handle else { return };
    let Ok(Some(snap)) = tokio::time::timeout(Duration::from_secs(2), handle.snapshot()).await
    else {
        return;
    };
    let ips = public_ips(snap.external_ip);
    if !ips.is_empty() {
        let _ = tick.placement.set_coordinator_ips(ips);
    }
}

/// Applies the coordinator's own share of the plan (WK-10e): at once when
/// it changes, and again every [`ADDRESS_EVERY`], so a DHT node started
/// meanwhile (a settings change restarts it) gets it within a minute. An
/// invalid share is refused and the last one stands; the coordinator's
/// DHT shard stays its own (`[dht]` settings), as before.
async fn coordinator_share(tick: &Tick, wiring: &Wiring, seen: &mut Seen) {
    let Some(apply) = &wiring.coordinator_share else {
        return;
    };
    let Ok(share) = checked_share(tick.placement.coordinator().politeness) else {
        return;
    };
    let fresh = seen
        .share
        .is_some_and(|(s, at)| s == share && at.elapsed() < ADDRESS_EVERY);
    if fresh {
        return;
    }
    seen.share = Some((share, Instant::now()));
    // Bounded: a stuck pool or node never holds the tick.
    let _ = tokio::time::timeout(Duration::from_secs(2), apply.apply(share)).await;
}

/// Only a public address enters the planner's set: never a loopback,
/// private, link-local, CGNAT or unspecified one (the same filter as a
/// worker's `Hello`). A v4-mapped v6 address counts as its v4.
fn public_ips(vote: Option<std::net::IpAddr>) -> Vec<std::net::IpAddr> {
    vote.map(|ip| ip.to_canonical())
        .filter(|ip| lodestar_network_policy::allows(SocketAddr::new(*ip, 1), false))
        .into_iter()
        .collect()
}

/// One server's tick: runs every [`TICK`] and applies acknowledgements as
/// they come, until it's aborted on close (before the server shuts down).
async fn run_tick(
    tick: Arc<Tick>,
    server: Arc<ClusterServer>,
    wiring: Wiring,
    mut acks: mpsc::Receiver<(WorkerId, SnapshotAck)>,
) {
    let mut every = tokio::time::interval(TICK);
    every.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut seen = Seen::default();
    loop {
        tokio::select! {
            _ = every.tick() => step(&tick, &server, &wiring, &mut seen).await,
            Some((worker, ack)) = acks.recv() => {
                if let Some(n) = lock(&tick.queued_acks).get_mut(&worker) {
                    *n = n.saturating_sub(1);
                }
                if let Some(s) = tick.snapshots().as_mut() {
                    s.ack(worker, ack, &server, Instant::now());
                }
            }
        }
    }
}

/// Aborts a task when dropped.
struct AbortOnDrop(tokio::task::AbortHandle);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Waits on the tick and says once, at error level, if it panicked (the
/// review's N7): placement, snapshots and the have-filter then stop until
/// the port reopens. Aborting the returned future's task aborts the tick,
/// even before it first runs: the guard is made here, not in the future
/// (#592's delta review).
fn watch_tick(tick: JoinHandle<()>) -> impl std::future::Future<Output = ()> + Send {
    let abort = AbortOnDrop(tick.abort_handle());
    async move {
        let _abort = abort;
        if let Err(e) = tick.await
            && e.is_panic()
        {
            lodestar_log::log_event!(
                events::CLUSTER_TICK_FAILED,
                error = lodestar_log::Error::kind("panic")
            );
        }
    }
}

/// One tick (the contracts are WK-3's, WK-5's and WK-8's for WK-10):
/// 1. a worker whose session is new starts both snapshots from chunk 0;
/// 2. placement brings membership in line and queues each assignment;
/// 3. the open keys follow the day;
/// 4. the have-filter syncs, and is rebuilt on a blocking thread when due;
/// 5. the filter and dictionary snapshots are read and installed on a
///    blocking thread when due, then synced.
async fn step(tick: &Arc<Tick>, server: &Arc<ClusterServer>, wiring: &Wiring, seen: &mut Seen) {
    tick.steps.fetch_add(1, Ordering::Relaxed);
    let fleet_snapshot = server.snapshot();
    new_sessions(tick, &fleet_snapshot, seen);
    // Members whose queue was full are retried by the next tick's sync.
    let _ = tick.placement.sync(&fleet_snapshot, server);
    coordinator_share(tick, wiring, seen).await;
    coordinator_address(tick, wiring, seen).await;
    let Some(fleet) = &tick.fleet else { return };
    let now = wiring.clock.now_secs();
    if let Some(k) = lock(&tick.keys).as_ref() {
        follow_keys(k, &fleet.seal_key, now, &mut seen.key_day);
    }
    have_step(tick, server, fleet, &fleet_snapshot, wiring);
    // From 23:00Z the snapshot is built for the next day's key.
    snapshot_step(tick, server, fleet, &fleet_snapshot, key_time(now)).await;
}

/// Starts both snapshots from chunk 0 for each worker whose session is new
/// (WK-8: a worker that restarted gets them again), and only for it. The
/// server's session ids (`WorkerStatus::session`) tell a reconnect apart
/// even when it happens between two ticks, as a certificate renewal's
/// does; another worker's transfer is never touched.
fn new_sessions(tick: &Tick, fleet: &FleetSnapshot, seen: &mut Seen) {
    let now: BTreeMap<WorkerId, u64> = fleet
        .workers
        .iter()
        .filter(|w| w.worker != WorkerId::LOCAL)
        .filter_map(|w| Some((w.worker, w.session?)))
        .collect();
    // While the snapshots are out of their slot (an install), nothing is
    // noted, so the next tick sees the new sessions.
    let mut slot = tick.snapshots();
    let Some(s) = slot.as_mut() else { return };
    for (w, id) in &now {
        if seen.sessions.get(w) != Some(id) {
            s.forget(*w);
        }
    }
    seen.sessions = now;
}

/// Clears a flag when dropped.
struct Clear<'a>(&'a AtomicBool);

impl Drop for Clear<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

fn have_step(
    tick: &Arc<Tick>,
    server: &ClusterServer,
    fleet: &Fleet,
    snapshot: &FleetSnapshot,
    wiring: &Wiring,
) {
    let synced = tick.have.sync(snapshot, server, Instant::now());
    // Said once each time it turns true (WK-3).
    if synced.at_cap && !tick.at_cap.swap(true, Ordering::Relaxed) {
        lodestar_log::log_event!(
            events::CLUSTER_HAVE_AT_CAP,
            stored = fleet.stored.swarm_hashes()
        );
    } else if !synced.at_cap {
        tick.at_cap.store(false, Ordering::Relaxed);
    }
    if !synced.rebuild || !tick.rebuild_retry.due() || tick.rebuilding.swap(true, Ordering::AcqRel)
    {
        return;
    }
    let (have, rebuilding, failures, rebuilds, retry) = (
        tick.have.clone(),
        tick.rebuilding.clone(),
        tick.rebuild_failures.clone(),
        tick.have_rebuilds.clone(),
        tick.rebuild_retry.clone(),
    );
    let stored = fleet.stored.swarm_hashes();
    let (db, config) = (wiring.db.clone(), wiring.store.clone());
    // Its own connection, opened in the scan: the scan is long, and the
    // snapshots' reads shouldn't wait for it.
    tokio::task::spawn_blocking(move || {
        let counts = RebuildCounts {
            rebuilding: &rebuilding,
            retry: &retry,
            failures: &failures,
            rebuilds: &rebuilds,
        };
        rebuild_have(&have, &counts, stored, |insert| {
            Store::open_reader(&db, config)?.for_each_swarm_hash(insert)
        });
    });
}

/// What a rebuild updates.
struct RebuildCounts<'a> {
    rebuilding: &'a AtomicBool,
    retry: &'a Retry,
    failures: &'a AtomicU64,
    rebuilds: &'a AtomicU64,
}

/// One rebuild of the workers' have-filter (blocking). The tick's flag is
/// cleared however it ends, a panic included. A panic in the scan becomes
/// an error inside `rebuild`, so the filter's own flag clears too and the
/// old filter stays (the review's N5); `WorkerFilter::rebuild` also clears
/// its flag if its own sizing or allocation unwinds.
fn rebuild_have(
    have: &WorkerFilter,
    c: &RebuildCounts<'_>,
    stored: u64,
    scan: impl FnOnce(&mut dyn FnMut(InfoHash)) -> Result<(), lodestar_store::Error>,
) {
    let _clear = Clear(c.rebuilding);
    let built = have.rebuild(stored, |insert| {
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| scan(insert)))
            .unwrap_or(Err(lodestar_store::Error::Corrupt("the scan panicked")))
    });
    match built {
        Ok(true) => {
            c.rebuilds.fetch_add(1, Ordering::Relaxed);
            c.retry.succeeded();
        }
        Ok(false) => {}
        Err(_) => {
            c.failures.fetch_add(1, Ordering::Relaxed);
            c.retry.failed();
        }
    }
}

/// The active file-tree dictionary as saved now. The store caches it at
/// open, so a long-lived reader's `active_file_dict` wouldn't see a newly
/// trained one: the id is read from `settings` and the bytes by that id.
/// The row is the store's own (`FILE_DICT_SETTING`), an integer.
fn active_dict(reader: &Store) -> Result<Option<i64>, lodestar_store::Error> {
    let mut stmt = reader
        .connection()
        .prepare_cached("SELECT value FROM settings WHERE key = 'file_dict'")?;
    let mut rows = stmt.query([])?;
    Ok(match rows.next()? {
        Some(row) => Some(row.get::<_, i64>(0)?),
        None => None,
    })
}

fn dict_by_id(reader: &Store, id: i64) -> Result<DictSnapshot, lodestar_store::Error> {
    let bytes: Vec<u8> =
        reader
            .connection()
            .query_row("SELECT data FROM zstd_dicts WHERE id = ?1", [id], |r| {
                r.get(0)
            })?;
    Ok(DictSnapshot { id, bytes })
}

async fn snapshot_step(
    tick: &Arc<Tick>,
    server: &ClusterServer,
    fleet: &Fleet,
    snapshot: &FleetSnapshot,
    now: i64,
) {
    let remote = snapshot
        .workers
        .iter()
        .any(|w| w.connected && w.worker != WorkerId::LOCAL);
    if remote && tick.read_retry.due() {
        let t = tick.clone();
        let read = tokio::task::spawn_blocking(move || t.with_reader(active_dict))
            .await
            .ok()
            .flatten();
        match read {
            None => tick.store_failed(),
            Some(dict_id) => {
                tick.store_failing.store(false, Ordering::Relaxed);
                tick.read_retry.succeeded();
                let engine = fleet.filter.current().version;
                let due = tick
                    .snapshots()
                    .as_ref()
                    .is_some_and(|s| s.due(&snapshot.workers, engine, now, dict_id));
                if due {
                    install(tick, fleet, engine, now, dict_id).await;
                }
            }
        }
    }
    let versions = tick.snapshots().as_mut().map(|s| {
        let _ = s.sync(snapshot, server, Instant::now());
        s.versions()
    });
    // Every tick (the review's N1): a change made while the snapshots were
    // out of their slot still reaches the plan. An unchanged pair is a
    // no-op. The coordinator's share of the replan is WK-10e's.
    if let Some((dict, filter)) = versions {
        let _ = tick.placement.set_versions(dict, filter);
    }
}

/// Reads both snapshots and installs them, on a blocking thread: at the
/// caps the read, the encode and the decode check are tens of MiB.
/// Acknowledgements that arrive meanwhile wait in the tick's queue and are
/// applied after it (stale ones are ignored by version and sequence). The
/// blocking side puts the snapshots back itself, so an abort of the tick
/// (Allow workers turned off, a reseal) can't lose them.
async fn install(tick: &Arc<Tick>, fleet: &Fleet, engine: u64, now: i64, dict_id: Option<i64>) {
    let Some(mut s) = tick.snapshots().take() else {
        return;
    };
    let (seal, t) = (fleet.seal_key, tick.clone());
    let joined = tokio::task::spawn_blocking(move || {
        let read = t.with_reader(|r| {
            let filter = read_snapshot(r, &seal, now)?;
            Ok((filter, dict_id.map(|id| dict_by_id(r, id)).transpose()?))
        });
        let installed = read.map(|(filter, dict)| {
            s.install(engine, &seal, &filter, dict.as_ref())
                .map_err(|e| wire_kind(&e))
        });
        *t.snapshots() = Some(s);
        installed
    })
    .await;
    match joined {
        Ok(Some(Ok(_))) => {
            tick.store_failing.store(false, Ordering::Relaxed);
            tick.read_retry.succeeded();
            tick.snapshot_installs.fetch_add(1, Ordering::Relaxed);
            // The new versions reach placement at the end of this tick.
        }
        // Remembered by `install`, so not read again until something
        // changes: said each time.
        Ok(Some(Err(kind))) => {
            tick.store_failing.store(false, Ordering::Relaxed);
            tick.read_retry.succeeded();
            tick.install_failures.fetch_add(1, Ordering::Relaxed);
            lodestar_log::log_event!(
                events::CLUSTER_SNAPSHOT_FAILED,
                error = lodestar_log::Error::kind(kind)
            );
        }
        Ok(None) => tick.store_failed(),
        // The read panicked: the snapshots went with it, so start again.
        Err(_) => {
            *tick.snapshots() = Some(WorkerSnapshots::new());
            tick.install_failures.fetch_add(1, Ordering::Relaxed);
            lodestar_log::log_event!(
                events::CLUSTER_SNAPSHOT_FAILED,
                error = lodestar_log::Error::kind("task_failed")
            );
        }
    }
}

/// A fixed kind for a snapshot's build error: the cap it passed, never
/// what's in it.
fn wire_kind(e: &lodestar_pipeline::wire::Error) -> &'static str {
    use lodestar_pipeline::wire::Error as E;
    match e {
        E::TooLarge(cap) | E::Invalid(cap) => cap,
        _ => "encode",
    }
}

/// The counters a server keeps, summed over every server this process
/// built (one per time "Allow workers" was turned on), so they never go
/// backwards.
#[derive(Debug, Default, Clone, Copy)]
struct Totals {
    sessions: u64,
    refused_capacity: u64,
    refused_timeout: u64,
    refused_handshake: u64,
    join_requests: u64,
    granted: u64,
    regrants: u64,
    busy: u64,
    stale_reports: u64,
    revoked: u64,
    protocol_errors: u64,
    version_refused: u64,
    skew_flags: u64,
    pki_errors: u64,
    check_failures: u64,
}

impl Totals {
    fn of(s: &FleetSnapshot) -> Totals {
        Totals {
            sessions: s.sessions,
            refused_capacity: s.refused,
            refused_timeout: s.handshake_timeouts,
            refused_handshake: s.handshake_failures,
            join_requests: s.join_requests,
            granted: s.harvested,
            regrants: s.regrants,
            busy: s.busy,
            stale_reports: s.stale_reports,
            revoked: s.revoked,
            protocol_errors: s.protocol_errors,
            version_refused: s.version_refused,
            skew_flags: s.skew_flags,
            pki_errors: s.pki_errors,
            check_failures: s.check_failures,
        }
    }

    fn plus(self, o: Totals) -> Totals {
        Totals {
            sessions: self.sessions + o.sessions,
            refused_capacity: self.refused_capacity + o.refused_capacity,
            refused_timeout: self.refused_timeout + o.refused_timeout,
            refused_handshake: self.refused_handshake + o.refused_handshake,
            join_requests: self.join_requests + o.join_requests,
            granted: self.granted + o.granted,
            regrants: self.regrants + o.regrants,
            busy: self.busy + o.busy,
            stale_reports: self.stale_reports + o.stale_reports,
            revoked: self.revoked + o.revoked,
            protocol_errors: self.protocol_errors + o.protocol_errors,
            version_refused: self.version_refused + o.version_refused,
            skew_flags: self.skew_flags + o.skew_flags,
            pki_errors: self.pki_errors + o.pki_errors,
            check_failures: self.check_failures + o.check_failures,
        }
    }
}

#[derive(Default)]
struct State {
    /// The server serving the port now, and where.
    current: Option<(Arc<ClusterServer>, SocketAddr)>,
    /// Counters of the servers already stopped.
    base: Totals,
}

type ResealReply = oneshot::Sender<Result<bool, ClusterError>>;

struct Inner {
    wiring: Wiring,
    allowed: watch::Sender<bool>,
    state: Mutex<State>,
    // Read by `ClusterHandle::reseal`, which the rotation flow calls.
    #[cfg_attr(not(test), allow(dead_code))]
    reseal: mpsc::Sender<ResealReply>,
    start_failures: AtomicU64,
    store_errors: Arc<AtomicU64>,
    tick: Arc<Tick>,
    /// Workers' floor reports, across servers (`write_floor` empties it).
    floor: Arc<FloorReports>,
    /// `Workers`' hook for a worker over its floor budget, across servers.
    over_budget: HookSlot,
}

/// The workers saved as paused (WK-14's rows), for a new server. A store
/// that won't open, or a row that won't read, pauses nobody here; `Workers`
/// applies its own copy once it follows the server.
fn saved_pauses(db: &std::path::Path, config: StoreConfig) -> Vec<WorkerId> {
    let Ok(store) = Store::open(db, config) else {
        return Vec::new();
    };
    let Ok(rows) = store.settings_with_prefix(super::ops::PREFS_PREFIX) else {
        return Vec::new();
    };
    rows.into_iter()
        .filter_map(|(key, text)| {
            let id = key
                .strip_prefix(super::ops::PREFS_PREFIX)?
                .parse::<u32>()
                .ok()
                .filter(|&id| id != 0)?;
            super::ops::Saved::parse(&text)
                .filter(super::ops::Saved::paused)
                .map(|_| WorkerId(id))
        })
        .collect()
}

/// How long `close` waits for an aborted tick to go.
const CLOSE_TICK: Duration = Duration::from_secs(10);

/// A running server, its `serve` task and its tick.
struct Running {
    server: Arc<ClusterServer>,
    task: JoinHandle<()>,
    ticker: JoinHandle<()>,
    /// Closes when the tick's future is gone (ended, or dropped by its
    /// abort), so `close` can wait for the tick, not only its watcher.
    tick_gone: watch::Receiver<()>,
}

impl Inner {
    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    async fn open(&self) -> Result<Running, StartError> {
        let listen = self.wiring.settings.listen;
        // Bound first: a port that's taken creates no CA.
        let listener = TcpListener::bind(listen)
            .await
            .map_err(|e| StartError::bind(listen, &e))?;
        let addr = listener
            .local_addr()
            .map_err(|e| StartError::bind(listen, &e))?;
        let (w, errors) = (self.wiring.clone(), self.store_errors.clone());
        let floor = self.floor.clone();
        let (pki, check) = tokio::task::spawn_blocking(move || open_pki(&w, errors, floor))
            .await
            .map_err(|_| StartError::task())??;
        let (db, store) = (self.wiring.db.clone(), self.wiring.store.clone());
        let paused = tokio::task::spawn_blocking(move || saved_pauses(&db, store))
            .await
            .unwrap_or_default();
        let preparer = check
            .reader
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .preparer();
        let check: Arc<dyn StoredKeys> = Arc::new(check);
        let config = server_config(&self.wiring.settings);
        // The intake (WK-9); the writer's backpressure is WK-9b's.
        let attached =
            self.wiring.remote.as_ref().map(|r| {
                super::intake::attach(r, preparer, check.clone(), config.hints_allow_private)
            });
        let (acks, ack_rx) = mpsc::channel(ACK_QUEUE);
        let sink = Arc::new(Sink {
            remote: attached.as_ref().map(|a| a.sink.clone()),
            floor: self.floor.clone(),
            clock: self.wiring.clock.clone(),
            have: self.tick.have.clone(),
            acks,
            server: OnceLock::new(),
            tick: Arc::downgrade(&self.tick),
            over_budget: self.over_budget.clone(),
        });
        let hooks = ServerHooks {
            authority: self.wiring.authority.clone(),
            stored: Some(check),
            sink: Some(sink.clone() as _),
            admission: attached.as_ref().map(|a| a.sink.clone() as _),
        };
        let server = ClusterServer::with_hooks(pki, self.wiring.clock.clone(), config, hooks)
            .map_err(|e| StartError::pki("server", &e))?;
        let _ = sink.server.set(Arc::downgrade(&server));
        if let (Some(a), Some(r)) = (&attached, &self.wiring.remote) {
            a.serving(r, &server);
        }
        // Sealed records open from the first result on, a spool replayed
        // after a restart included (WK-9c's note for WK-10).
        let keys = attached.as_ref().map(|a| a.keys.clone());
        if let (Some(k), Some(f)) = (&keys, &self.wiring.fleet) {
            k.set(open_keys(&f.seal_key, self.wiring.clock.now_secs()));
        }
        *lock(&self.tick.keys) = keys;
        // The last server's queue went with it.
        lock(&self.tick.queued_acks).clear();
        self.tick.observe();
        // The saved pauses are in before the first connection is served:
        // a paused worker that reconnects at once is never granted a key
        // (WK-14's window; `Workers` re-applies them too, later).
        for w in paused {
            server.pause(w);
        }
        let task = tokio::spawn(server.clone().serve(listener));
        let (gone, tick_gone) = watch::channel(());
        let tick = run_tick(
            self.tick.clone(),
            server.clone(),
            self.wiring.clone(),
            ack_rx,
        );
        let ticker = tokio::spawn(watch_tick(tokio::spawn(async move {
            let _gone = gone;
            tick.await;
        })));
        self.state().current = Some((server.clone(), addr));
        lodestar_log::log_event!(
            events::CLUSTER_LISTENING,
            address = lodestar_log::Listen(addr)
        );
        Ok(Running {
            server,
            task,
            ticker,
            tick_gone,
        })
    }

    async fn close(&self, running: Running, reason: &'static str) {
        if let Some(r) = &self.wiring.remote {
            r.serving(None, false);
        }
        running.ticker.abort();
        let _ = running.ticker.await;
        // Aborting the watcher aborts the tick, which goes at its next
        // await: waited for here (#592's review), so nothing of the old
        // tick runs on once the port is closed. Blocking work it started
        // (a rebuild, an install) finishes by itself and finds the server
        // gone.
        let mut gone = running.tick_gone;
        let _ =
            tokio::time::timeout(CLOSE_TICK, async { while gone.changed().await.is_ok() {} }).await;
        running.server.shutdown();
        let _ = running.task.await;
        {
            let mut state = self.state();
            state.base = state.base.plus(Totals::of(&running.server.snapshot()));
            state.current = None;
        }
        lodestar_log::log_event!(events::CLUSTER_CLOSED, reason = reason);
    }

    /// Opens the port, logging and counting a failure.
    async fn try_open(&self) -> Option<Running> {
        match self.open().await {
            Ok(r) => Some(r),
            Err(e) => {
                self.failed(&e);
                None
            }
        }
    }

    fn failed(&self, e: &StartError) {
        self.start_failures.fetch_add(1, Ordering::Relaxed);
        lodestar_log::log_event!(
            events::CLUSTER_START_FAILED,
            step = e.step,
            error = lodestar_log::Error::kind(e.kind)
        );
    }
}

/// The cluster port's controls and metrics, for `serve`'s other parts.
#[derive(Clone)]
pub struct ClusterHandle(Arc<Inner>);

impl std::fmt::Debug for ClusterHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClusterHandle")
            .field("allowed", &self.allowed())
            .field("listening", &self.local_addr())
            .finish()
    }
}

impl ClusterHandle {
    /// Registers what to do when a worker first goes over its floor
    /// budget (`Workers` saves the pause). One hook; a later call replaces
    /// it.
    pub(crate) fn on_floor_over_budget(&self, hook: OverBudgetHook) {
        *self
            .0
            .over_budget
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(hook);
    }

    /// A floor report from `worker` for a key made of `a` and `b`, as the
    /// sink takes one; `true` if it was taken (within the budget).
    #[cfg(test)]
    pub(crate) fn floor_report_for_test(&self, worker: WorkerId, a: u8, b: usize) -> bool {
        let mut key = [a; 20];
        key[1..9].copy_from_slice(&(b as u64).to_le_bytes());
        self.0
            .floor
            .report(worker, &InfoHash(key), 1, Instant::now())
            == Reported::Taken
    }

    /// How many of `worker`'s floor reports aren't written yet.
    #[cfg(test)]
    pub(crate) fn floor_pending_for_test(&self, worker: WorkerId) -> usize {
        self.0
            .floor
            .pending()
            .values()
            .filter(|r| r.worker == worker.0)
            .count()
    }

    /// Forgets a revoked worker's floor reports not written yet, and its
    /// budget window. Returns how many reports it dropped.
    pub(crate) fn forget_floor_reports(&self, worker: WorkerId) -> usize {
        self.0.floor.forget_worker(worker)
    }

    /// Turns "Allow workers" on or off; the port follows in the
    /// background. Not saved: the config (and, later, Settings →
    /// Workers) decides at the next start.
    // Settings → Workers calls it (WK-14); tests do today.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn set_allowed(&self, allowed: bool) {
        self.0.allowed.send_replace(allowed);
    }

    pub fn allowed(&self) -> bool {
        *self.0.allowed.borrow()
    }

    /// Where the cluster port is listening, while it is.
    pub fn local_addr(&self) -> Option<SocketAddr> {
        self.0.state().current.as_ref().map(|(_, a)| *a)
    }

    /// The server serving the port now (for the workers API, WK-14).
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn server(&self) -> Option<Arc<ClusterServer>> {
        self.0.state().current.as_ref().map(|(s, _)| s.clone())
    }

    /// Secrets-key rotation's cluster step, between `SecretStore::rotate`
    /// (which saved the new key to `secrets.key`) and
    /// `SecretStore::retire_old_keys`: stops the server, seals the
    /// cluster's keys again with the new primary key, and serves again if
    /// workers are allowed. `Ok(false)` if there's no cluster PKI to seal
    /// (nothing is created). Retire the old keys only after `Ok`.
    #[cfg_attr(not(test), allow(dead_code))]
    pub async fn reseal(&self) -> Result<bool, ClusterError> {
        let (reply, answer) = oneshot::channel();
        let stopped = || ClusterError::Corrupt("the cluster task has stopped");
        self.0.reseal.send(reply).await.map_err(|_| stopped())?;
        answer.await.map_err(|_| stopped())?
    }

    /// The `lodestar_cluster_*` series (docs/metrics.md, "Cluster port").
    pub fn render(&self, out: &mut String) {
        let inner = &self.0;
        let (snapshot, base) = {
            let state = inner.state();
            (
                state.current.as_ref().map(|(s, _)| s.snapshot()),
                state.base,
            )
        };
        let totals = snapshot.as_ref().map_or(base, |s| base.plus(Totals::of(s)));
        let mut o = Out(out);
        o.one(
            "cluster_allowed",
            "gauge",
            "1 when Allow workers is on.",
            u8::from(self.allowed()),
        );
        o.one(
            "cluster_listening",
            "gauge",
            "1 while the cluster port is bound and served.",
            u8::from(snapshot.is_some()),
        );
        let connected = snapshot
            .as_ref()
            .map_or(0, |s| s.workers.iter().filter(|w| w.connected).count());
        o.one(
            "cluster_workers_connected",
            "gauge",
            "Remote workers with a live session.",
            connected,
        );
        o.one(
            "cluster_handshaking",
            "gauge",
            "Connections still in their pre-authentication handshake.",
            snapshot.as_ref().map_or(0, |s| s.handshaking),
        );
        o.one(
            "cluster_leases_held",
            "gauge",
            "Leases held by remote workers.",
            snapshot
                .as_ref()
                .map_or(0, |s| s.workers.iter().map(|w| w.leases).sum::<usize>()),
        );
        o.one(
            "cluster_leases_waiting",
            "gauge",
            "Keys whose lease ended without an outcome, waiting to be granted again.",
            snapshot.as_ref().map_or(0, |s| s.pending),
        );
        o.one(
            "cluster_sessions_total",
            "counter",
            "Worker sessions started.",
            totals.sessions,
        );
        o.family(
            "cluster_preauth_refused_total",
            "counter",
            "Connections closed before authentication, by reason.",
        );
        for (reason, n) in [
            ("capacity", totals.refused_capacity),
            ("handshake_timeout", totals.refused_timeout),
            ("handshake_failed", totals.refused_handshake),
        ] {
            o.value(
                "cluster_preauth_refused_total",
                &format!("reason=\"{reason}\""),
                n,
            );
        }
        for (name, help, n) in [
            (
                "cluster_join_requests_total",
                "Join requests answered (granted or refused).",
                totals.join_requests,
            ),
            (
                "cluster_granted_total",
                "Keys granted to remote workers for the first time.",
                totals.granted,
            ),
            (
                "cluster_regrants_total",
                "Keys granted again after their lease ended without an outcome.",
                totals.regrants,
            ),
            (
                "cluster_busy_total",
                "Claimed keys denied because grants were held back.",
                totals.busy,
            ),
            (
                "cluster_stale_reports_total",
                "Reports with a stale fencing token, ignored.",
                totals.stale_reports,
            ),
            (
                "cluster_revoked_total",
                "Sessions closed because their certificate was revoked or expired.",
                totals.revoked,
            ),
            (
                "cluster_protocol_errors_total",
                "Sessions closed for a protocol error.",
                totals.protocol_errors,
            ),
            (
                "cluster_version_refused_total",
                "Workers refused for a protocol major version mismatch.",
                totals.version_refused,
            ),
            (
                "cluster_skew_flags_total",
                "Times a worker's clock was flagged as too far off.",
                totals.skew_flags,
            ),
            (
                "cluster_pki_errors_total",
                "Failed PKI reloads or upkeep on the heartbeat tick.",
                totals.pki_errors,
            ),
            (
                "cluster_start_failures_total",
                "Times turning the cluster port on failed (bind or PKI).",
                inner.start_failures.load(Ordering::Relaxed),
            ),
            (
                "cluster_store_check_errors_total",
                "Store reads that failed in the check before a grant (the key is refused, or a re-grant waits).",
                inner.store_errors.load(Ordering::Relaxed) + totals.check_failures,
            ),
            (
                "cluster_have_rebuilds_total",
                "Times the workers' have-filter was built or rebuilt from the store.",
                inner.tick.have_rebuilds.load(Ordering::Relaxed),
            ),
            (
                "cluster_have_rebuild_failures_total",
                "Workers' have-filter rebuilds that failed (the old filter stays).",
                inner.tick.rebuild_failures.load(Ordering::Relaxed),
            ),
            (
                "cluster_snapshot_installs_total",
                "Filter and dictionary snapshots built for remote workers.",
                inner.tick.snapshot_installs.load(Ordering::Relaxed),
            ),
            (
                "cluster_snapshot_install_failures_total",
                "Snapshot builds or store reads that failed (workers keep the ones they have).",
                inner.tick.install_failures.load(Ordering::Relaxed),
            ),
            (
                "cluster_snapshot_acks_dropped_total",
                "Snapshot acknowledgements dropped because that worker had too many waiting for the tick.",
                inner.tick.acks_dropped.load(Ordering::Relaxed),
            ),
        ] {
            o.one(name, "counter", help, n);
        }
        o.one(
            "cluster_have_filter_bytes",
            "gauge",
            "Memory the workers' have-filter holds (0 until a remote worker connects).",
            inner.tick.have.memory_bytes(),
        );
        o.one(
            "cluster_have_filter_at_cap",
            "gauge",
            "1 when the workers' have-filter is at its largest size and full.",
            u8::from(inner.tick.at_cap.load(Ordering::Relaxed)),
        );
        let floor = &inner.floor;
        o.one(
            "cluster_floor_reports_pending",
            "gauge",
            "Remote workers' floor drops not yet written as tombstones (refused to every claim meanwhile).",
            floor.len(),
        );
        for (name, help, n) in [
            (
                "cluster_floor_reports_total",
                "Floor drops remote workers reported (Lost as blocked).",
                &floor.reported,
            ),
            (
                "cluster_floor_reports_written_total",
                "Workers' floor drops written as worker-floor tombstones.",
                &floor.written,
            ),
            (
                "cluster_floor_report_write_failures_total",
                "Writes of workers' floor drops the store refused; the reports are kept and tried again.",
                &floor.write_failures,
            ),
            (
                "cluster_floor_reports_over_budget_total",
                "Floor drops reported past a worker's budget (100 an hour): dropped; the worker is paused and its leases taken back.",
                &floor.over_budget,
            ),
            (
                "cluster_floor_reports_turned_away_total",
                "Floor drops within a budget reported while 100,000 were already pending: counted only.",
                &floor.turned_away,
            ),
        ] {
            o.one(name, "counter", help, n.load(Ordering::Relaxed));
        }
    }
}

struct Out<'a>(&'a mut String);

impl Out<'_> {
    fn family(&mut self, name: &str, kind: &str, help: &str) {
        use std::fmt::Write;
        let _ = writeln!(self.0, "# HELP lodestar_{name} {help}");
        let _ = writeln!(self.0, "# TYPE lodestar_{name} {kind}");
    }

    fn value(&mut self, name: &str, labels: &str, v: impl std::fmt::Display) {
        use std::fmt::Write;
        let _ = writeln!(self.0, "lodestar_{name}{{{labels}}} {v}");
    }

    fn one(&mut self, name: &str, kind: &str, help: &str, v: impl std::fmt::Display) {
        use std::fmt::Write;
        self.family(name, kind, help);
        let _ = writeln!(self.0, "lodestar_{name} {v}");
    }
}

/// The cluster port, following "Allow workers" until [`shutdown`](Self::shutdown).
pub struct Cluster {
    handle: ClusterHandle,
    stop: watch::Sender<bool>,
    task: JoinHandle<()>,
    /// Writes workers' floor reports; it ends after a last flush, once
    /// `writer_stop` fires.
    writer_stop: watch::Sender<bool>,
    writer: JoinHandle<()>,
}

impl std::fmt::Debug for Cluster {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.handle.fmt(f)
    }
}

impl Cluster {
    /// Starts following `[cluster] enabled`. If it's on, the port is bound
    /// and the PKI opened before this returns, and a failure is returned.
    /// If it's off, nothing is bound, opened or created.
    pub async fn start(wiring: Wiring) -> Result<Cluster, StartError> {
        let enabled = wiring.settings.enabled;
        let (allowed, follow) = watch::channel(enabled);
        let (reseal, reseals) = mpsc::channel(1);
        let tick = Arc::new(Tick::new(&wiring));
        let wiring_held = wiring.held.clone();
        let inner = Arc::new(Inner {
            tick,
            wiring,
            allowed,
            state: Mutex::new(State::default()),
            reseal,
            start_failures: AtomicU64::new(0),
            store_errors: Arc::new(AtomicU64::new(0)),
            floor: Arc::new(FloorReports::new(wiring_held)),
            over_budget: HookSlot::default(),
        });
        let running = if enabled {
            match inner.open().await {
                Ok(r) => Some(r),
                Err(e) => {
                    inner.failed(&e);
                    return Err(e);
                }
            }
        } else {
            None
        };
        let (stop, stopped) = watch::channel(false);
        // The writer's own stop, fired only once the sessions are gone.
        let (writer_stop, writer_stopped) = watch::channel(false);
        let writer = tokio::spawn(write_floor(
            inner.floor.clone(),
            inner.wiring.db.clone(),
            inner.wiring.store.clone(),
            writer_stopped,
        ));
        let task = tokio::spawn(run(inner.clone(), follow, reseals, stopped, running));
        Ok(Cluster {
            handle: ClusterHandle(inner),
            stop,
            task,
            writer_stop,
            writer,
        })
    }

    pub fn handle(&self) -> ClusterHandle {
        self.handle.clone()
    }

    /// Stops every session and closes the port. Before the pipeline stops,
    /// so no remote work arrives while it saves.
    pub async fn shutdown(self) {
        let writer = self.stop_serving().await;
        writer.finish().await;
    }

    /// Stops every session and closes the port; the floor writer still
    /// runs (a session could deliver a report until the port closed).
    async fn stop_serving(self) -> FloorWriter {
        let _ = self.stop.send(true);
        let _ = self.task.await;
        FloorWriter {
            stop: self.writer_stop,
            task: self.writer,
        }
    }
}

/// The floor writer after the sessions are gone.
struct FloorWriter {
    stop: watch::Sender<bool>,
    task: JoinHandle<()>,
}

impl FloorWriter {
    /// Writes the last reports, then ends: before the pipeline stops.
    async fn finish(self) {
        let _ = self.stop.send(true);
        let _ = self.task.await;
    }
}

/// Follows "Allow workers" and reseal requests until `stopped`.
async fn run(
    inner: Arc<Inner>,
    mut allowed: watch::Receiver<bool>,
    mut reseals: mpsc::Receiver<ResealReply>,
    mut stopped: watch::Receiver<bool>,
    mut running: Option<Running>,
) {
    loop {
        tokio::select! {
            _ = stopped.changed() => break,
            changed = allowed.changed() => {
                if changed.is_err() {
                    break;
                }
                let want = *allowed.borrow_and_update();
                match (want, running.take()) {
                    (false, Some(r)) => inner.close(r, "workers_off").await,
                    (true, None) => running = inner.try_open().await,
                    (_, r) => running = r,
                }
            }
            Some(reply) = reseals.recv() => {
                if let Some(r) = running.take() {
                    inner.close(r, "reseal").await;
                }
                let w = inner.wiring.clone();
                let resealed = tokio::task::spawn_blocking(move || reseal_pki(&w))
                    .await
                    .unwrap_or(Err(ClusterError::Corrupt("the reseal task failed")));
                if *allowed.borrow_and_update() {
                    running = inner.try_open().await;
                }
                let _ = reply.send(resealed);
            }
        }
    }
    if let Some(r) = running {
        inner.close(r, "shutdown").await;
    }
}

#[cfg(test)]
mod tests {
    use lodestar_cluster::{Credentials, JoinToken, Joining, NoEvents, SystemClock};

    use super::*;

    const DEADLINE: Duration = Duration::from_secs(20);
    const WAIT: Duration = Duration::from_secs(15);

    fn wiring(dir: &std::path::Path, enabled: bool, listen: SocketAddr) -> Wiring {
        Wiring {
            db: dir.join("lodestar.db"),
            store: StoreConfig::default(),
            data_dir: dir.to_path_buf(),
            authority: Arc::new(LeaseAuthority::new(1, 600, Arc::new(NoEvents))),
            remote: None,
            settings: config::Cluster {
                enabled,
                listen,
                ..config::Cluster::default()
            },
            clock: Arc::new(SystemClock),
            placement: Arc::new(Placement::new(false)),
            fleet: None,
            coordinator_dht: None,
            coordinator_share: None,
            held: None,
        }
    }

    fn any_port() -> SocketAddr {
        SocketAddr::from(([127, 0, 0, 1], 0))
    }

    /// Polls `what` until it holds; fails with what it last saw.
    async fn until<T: std::fmt::Debug>(name: &str, mut what: impl FnMut() -> (bool, T)) -> T {
        let start = Instant::now();
        loop {
            let (done, seen) = what();
            if done {
                return seen;
            }
            assert!(start.elapsed() < DEADLINE, "{name}: still {seen:?}");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    fn pki_rows(dir: &std::path::Path) -> usize {
        let store = Store::open(dir.join("lodestar.db"), StoreConfig::default()).unwrap();
        store.settings_with_prefix(PKI_PREFIX).unwrap().len()
    }

    fn metrics(h: &ClusterHandle) -> String {
        let mut out = String::new();
        h.render(&mut out);
        out
    }

    fn series(text: &str, name: &str) -> u64 {
        text.lines()
            .find_map(|l| l.strip_prefix(name)?.strip_prefix(' ')?.parse().ok())
            .unwrap_or_else(|| panic!("{name} missing:\n{text}"))
    }

    async fn join(h: &ClusterHandle, name: &str) -> Credentials {
        let server = h.server().expect("serving");
        let token = server.create_join_token(name).await.unwrap();
        let joining = Joining::new(JoinToken::parse(&token.expose()).unwrap()).unwrap();
        let addr = h.local_addr().unwrap().to_string();
        lodestar_cluster::join(&addr, joining, Arc::new(SystemClock), WAIT)
            .await
            .unwrap()
    }

    /// A mutual-TLS link, upgraded, before `Hello`.
    async fn link(h: &ClusterHandle, creds: &Credentials) -> Option<lodestar_cluster::WorkerLink> {
        let addr = h.local_addr()?.to_string();
        lodestar_cluster::connect(&addr, creds, Arc::new(SystemClock), WAIT)
            .await
            .ok()
    }

    /// `Hello`, then `Welcome`, on `link`.
    async fn hello(
        link: lodestar_cluster::WorkerLink,
        creds: &Credentials,
    ) -> Result<lodestar_cluster::WorkerSession, lodestar_cluster::ClientError> {
        let hello = lodestar_cluster::proto::Hello::new(creds.worker().0, Default::default());
        let config = lodestar_cluster::WorkerConfig {
            auto_heartbeat: false,
            ..lodestar_cluster::WorkerConfig::default()
        };
        lodestar_cluster::WorkerSession::start(link, hello, Arc::new(SystemClock), config).await
    }

    /// Whether `creds` gets a session (TLS, the upgrade, then `Hello` and
    /// `Welcome`).
    async fn connects(h: &ClusterHandle, creds: &Credentials) -> bool {
        match link(h, creds).await {
            Some(l) => hello(l, creds).await.is_ok(),
            None => false,
        }
    }

    #[tokio::test]
    async fn off_binds_and_creates_nothing_until_allowed() {
        let dir = tempfile::tempdir().unwrap();
        let cluster = Cluster::start(wiring(dir.path(), false, any_port()))
            .await
            .unwrap();
        let h = cluster.handle();
        assert_eq!(h.local_addr(), None);
        assert!(h.server().is_none());
        // Not even the database or the secrets key is touched.
        assert!(!dir.path().join("lodestar.db").exists());
        assert!(!dir.path().join(SecretStore::KEY_FILE).exists());
        let off = metrics(&h);
        assert_eq!(series(&off, "lodestar_cluster_allowed"), 0);
        assert_eq!(series(&off, "lodestar_cluster_listening"), 0);
        assert_eq!(series(&off, "lodestar_cluster_sessions_total"), 0);

        h.set_allowed(true);
        let addr = until("listening", || (h.local_addr().is_some(), h.local_addr()))
            .await
            .unwrap();
        assert!(tokio::net::TcpStream::connect(addr).await.is_ok());
        assert!(pki_rows(dir.path()) >= 3, "the CA is created on first use");
        assert!(dir.path().join(SecretStore::KEY_FILE).exists());
        let on = metrics(&h);
        assert_eq!(series(&on, "lodestar_cluster_allowed"), 1);
        assert_eq!(series(&on, "lodestar_cluster_listening"), 1);

        // A worker's session is counted, and the count survives the port
        // closing.
        let creds = join(&h, "w1").await;
        assert!(connects(&h, &creds).await);
        until("a session", || {
            let n = series(&metrics(&h), "lodestar_cluster_sessions_total");
            (n >= 1, n)
        })
        .await;

        h.set_allowed(false);
        until("closed", || (h.local_addr().is_none(), h.local_addr())).await;
        until("port refused", || {
            let refused = std::net::TcpStream::connect(addr).is_err();
            (refused, refused)
        })
        .await;
        let closed = metrics(&h);
        assert_eq!(series(&closed, "lodestar_cluster_listening"), 0);
        assert!(series(&closed, "lodestar_cluster_sessions_total") >= 1);
        // The PKI is kept for the next time: the same worker comes back.
        let rows = pki_rows(dir.path());
        h.set_allowed(true);
        until("listening again", || {
            (h.local_addr().is_some(), h.local_addr())
        })
        .await;
        assert!(connects(&h, &creds).await);
        assert!(pki_rows(dir.path()) >= rows);
        cluster.shutdown().await;
    }

    #[tokio::test]
    async fn refuses_clients_without_this_clusters_certificate() {
        let (a_dir, b_dir) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let a = Cluster::start(wiring(a_dir.path(), true, any_port()))
            .await
            .unwrap();
        let b = Cluster::start(wiring(b_dir.path(), true, any_port()))
            .await
            .unwrap();
        let (ah, bh) = (a.handle(), b.handle());
        // Bytes that aren't TLS are dropped before anything is read.
        {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            let mut raw = tokio::net::TcpStream::connect(ah.local_addr().unwrap())
                .await
                .unwrap();
            raw.write_all(b"GET /cluster HTTP/1.1\r\nHost: x\r\n\r\n")
                .await
                .unwrap();
            let mut buf = Vec::new();
            let _ = tokio::time::timeout(WAIT, raw.read_to_end(&mut buf)).await;
            assert!(!String::from_utf8_lossy(&buf).contains("HTTP/1.1 101"));
        }
        until("handshake refused", || {
            let m = metrics(&ah);
            let n = series(
                &m,
                "lodestar_cluster_preauth_refused_total{reason=\"handshake_failed\"}",
            );
            (n >= 1, n)
        })
        .await;
        // A worker of another cluster has a certificate, but not one this
        // cluster's CA issued.
        let stranger = join(&bh, "stranger").await;
        assert!(connects(&bh, &stranger).await, "its own cluster takes it");
        assert!(!connects(&ah, &stranger).await);
        assert_eq!(
            series(&metrics(&ah), "lodestar_cluster_sessions_total"),
            0,
            "no session for an unauthenticated client"
        );
        a.shutdown().await;
        b.shutdown().await;
    }

    /// Turning workers off ends live sessions with `Stop(Shutdown)`, and a
    /// connection still before its `Hello` gets no session afterwards.
    #[tokio::test]
    async fn off_ends_every_session_and_every_handshake() {
        let dir = tempfile::tempdir().unwrap();
        let cluster = Cluster::start(wiring(dir.path(), true, any_port()))
            .await
            .unwrap();
        let h = cluster.handle();
        let (one, two) = (join(&h, "w1").await, join(&h, "w2").await);
        let held = link(&h, &one).await.expect("a link");
        let mut live = hello(link(&h, &two).await.unwrap(), &two).await.unwrap();
        until("connected", || {
            let n = series(&metrics(&h), "lodestar_cluster_workers_connected");
            (n == 1, n)
        })
        .await;

        h.set_allowed(false);
        let ended = tokio::time::timeout(DEADLINE, async {
            loop {
                match live.next_event().await {
                    Some(lodestar_cluster::WorkerEvent::Ended(reason)) => return reason,
                    Some(_) => continue,
                    None => return None,
                }
            }
        })
        .await
        .expect("the live session ended");
        assert_eq!(
            ended,
            Some(lodestar_cluster::proto::StopReason::Shutdown),
            "told why"
        );
        until("closed", || (h.local_addr().is_none(), h.local_addr())).await;
        // The link that was through TLS and the upgrade, but hadn't said
        // `Hello`, is refused now.
        assert!(hello(held, &one).await.is_err());
        let m = metrics(&h);
        assert_eq!(series(&m, "lodestar_cluster_workers_connected"), 0);
        assert_eq!(series(&m, "lodestar_cluster_sessions_total"), 1, "{m}");
        cluster.shutdown().await;
    }

    #[tokio::test]
    async fn a_taken_port_fails_the_start_and_creates_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let taken = std::net::TcpListener::bind(any_port()).unwrap();
        let addr = taken.local_addr().unwrap();
        let e = Cluster::start(wiring(dir.path(), true, addr))
            .await
            .unwrap_err();
        assert_eq!(e.step, "bind");
        assert!(e.to_string().contains("cluster.listen"), "{e}");
        assert!(!dir.path().join("lodestar.db").exists());
        assert!(!dir.path().join(SecretStore::KEY_FILE).exists());

        // Turned on later, a failure is counted and serving goes on.
        let cluster = Cluster::start(wiring(dir.path(), false, addr))
            .await
            .unwrap();
        let h = cluster.handle();
        h.set_allowed(true);
        until("a start failure", || {
            let n = series(&metrics(&h), "lodestar_cluster_start_failures_total");
            (n >= 1, n)
        })
        .await;
        assert_eq!(h.local_addr(), None);
        cluster.shutdown().await;
    }

    #[tokio::test]
    async fn reseal_between_rotate_and_retire_keeps_the_pki_open() {
        let dir = tempfile::tempdir().unwrap();
        // Never turned on: nothing to seal, and nothing is created.
        let off = Cluster::start(wiring(dir.path(), false, any_port()))
            .await
            .unwrap();
        // The database exists (another part of serve made it) but holds no
        // cluster rows.
        drop(Store::open(dir.path().join("lodestar.db"), StoreConfig::default()).unwrap());
        assert!(!off.handle().reseal().await.unwrap());
        assert_eq!(pki_rows(dir.path()), 0);
        off.shutdown().await;

        let cluster = Cluster::start(wiring(dir.path(), true, any_port()))
            .await
            .unwrap();
        let h = cluster.handle();
        let creds = join(&h, "w1").await;
        let fingerprint = |secrets: &SecretStore| {
            let mut store =
                Store::open(dir.path().join("lodestar.db"), StoreConfig::default()).unwrap();
            Coordinator::open(&mut store, secrets, SystemClock.now_secs())
                .map(|c| c.ca_fingerprint())
        };
        let mut secrets = SecretStore::open_or_create(dir.path()).unwrap();
        let before = fingerprint(&secrets).unwrap();
        let old_key = secrets.primary_key_id();

        secrets.rotate().unwrap();
        assert!(h.reseal().await.unwrap());
        secrets.retire_old_keys().unwrap();

        // Only the new key is left, and the PKI still opens with it: the
        // same CA, so joined workers keep working.
        let fresh = SecretStore::open_or_create(dir.path()).unwrap();
        assert_eq!(fresh.key_count(), 1);
        assert_ne!(fresh.primary_key_id(), old_key);
        assert_eq!(fingerprint(&fresh).unwrap(), before);
        until("serving again", || {
            (h.local_addr().is_some(), h.local_addr())
        })
        .await;
        assert!(connects(&h, &creds).await);

        // Off with a PKI: it's resealed, and the port stays closed.
        h.set_allowed(false);
        until("closed", || (h.local_addr().is_none(), h.local_addr())).await;
        let mut secrets = SecretStore::open_or_create(dir.path()).unwrap();
        secrets.rotate().unwrap();
        assert!(h.reseal().await.unwrap());
        secrets.retire_old_keys().unwrap();
        assert_eq!(h.local_addr(), None);
        let fresh = SecretStore::open_or_create(dir.path()).unwrap();
        assert_eq!(fingerprint(&fresh).unwrap(), before);
        cluster.shutdown().await;
    }

    /// "Off" right after "on" (before `serve` was first polled) still
    /// closes the port, and shutdown returns: the server notices a
    /// shutdown flag set before it subscribed.
    #[tokio::test]
    async fn off_right_after_on_closes_and_shuts_down() {
        for _ in 0..20 {
            let dir = tempfile::tempdir().unwrap();
            let cluster = Cluster::start(wiring(dir.path(), false, any_port()))
                .await
                .unwrap();
            let h = cluster.handle();
            h.set_allowed(true);
            until("the PKI opening", || {
                let made = dir.path().join("lodestar.db").exists();
                (made, made)
            })
            .await;
            h.set_allowed(false);
            tokio::time::timeout(DEADLINE, cluster.shutdown())
                .await
                .expect("shutdown returned");
            assert_eq!(h.local_addr(), None);
        }
    }

    /// A rotation that forgets the cluster's reseal doesn't pass quietly:
    /// once the old key is retired the PKI no longer opens, and turning
    /// workers on fails with the reason.
    #[tokio::test]
    async fn retire_without_reseal_fails_loudly() {
        let dir = tempfile::tempdir().unwrap();
        let cluster = Cluster::start(wiring(dir.path(), true, any_port()))
            .await
            .unwrap();
        cluster.shutdown().await;
        let mut secrets = SecretStore::open_or_create(dir.path()).unwrap();
        secrets.rotate().unwrap();
        secrets.retire_old_keys().unwrap();
        let e = Cluster::start(wiring(dir.path(), true, any_port()))
            .await
            .unwrap_err();
        assert_eq!(e.step, "pki", "{e}");
        assert!(e.to_string().starts_with("cluster port (pki): "), "{e}");
    }

    /// Every family rendered has its row in docs/metrics.md, and the text
    /// is valid exposition (one TYPE per family, every sample under one).
    #[tokio::test]
    async fn every_family_is_documented() {
        let dir = tempfile::tempdir().unwrap();
        let cluster = Cluster::start(wiring(dir.path(), false, any_port()))
            .await
            .unwrap();
        let text = metrics(&cluster.handle());
        let doc = include_str!("../../../../docs/metrics.md");
        let families: Vec<&str> = text
            .lines()
            .filter_map(|l| l.strip_prefix("# TYPE lodestar_"))
            .map(|l| l.split(' ').next().unwrap())
            .collect();
        assert_eq!(families.len(), 33, "{text}");
        for family in &families {
            assert!(
                doc.lines().any(|l| l.starts_with(&format!("| `{family}"))),
                "docs/metrics.md has no row for {family}"
            );
            assert_eq!(
                text.matches(&format!("# TYPE lodestar_{family} ")).count(),
                1
            );
        }
        for sample in text.lines().filter(|l| !l.starts_with('#')) {
            let name = sample.split(['{', ' ']).next().unwrap();
            assert!(
                families.iter().any(|f| name == format!("lodestar_{f}")),
                "{sample}"
            );
        }
        cluster.shutdown().await;
    }

    #[test]
    fn the_section_sets_the_server() {
        let c = config::Cluster {
            max_skew_ms: 5_000,
            handshake_timeout_secs: 7,
            max_pending: 32,
            max_pending_per_ip: 4,
            session_queue_mib: 2,
            ..config::Cluster::default()
        };
        let s = server_config(&c);
        assert_eq!(s.max_skew_ms, 5_000);
        assert_eq!(s.handshake_timeout, Duration::from_secs(7));
        assert_eq!((s.max_pending, s.max_pending_per_ip), (32, 4));
        assert_eq!(s.session_queue_bytes, 2 << 20);
        // The protocol's own defaults stay.
        let d = ServerConfig::default();
        assert_eq!(
            (s.heartbeat, s.lease_ttl_secs),
            (d.heartbeat, d.lease_ttl_secs)
        );
        assert!(s.auto_tick);
    }

    fn fleet() -> Fleet {
        Fleet {
            stored: Arc::new(Have::new(1 << 20)),
            filter: Arc::new(lodestar_pipeline::default_filter()),
            seal_key: [7; 32],
        }
    }

    /// The pipeline's intake, on `authority`, for a rig that needs one,
    /// with the writer's and the pool's ends (kept open by the caller).
    fn intake(authority: &Arc<LeaseAuthority>) -> (Arc<RemoteIntake>, Box<dyn std::any::Any>) {
        use lodestar_pipeline::remote::{Prior, RemoteParts};
        let (writer, w) = mpsc::channel(8);
        let (pool, p) = mpsc::channel(8);
        let intake = Arc::new(RemoteIntake::new(RemoteParts {
            authority: authority.clone(),
            writer,
            pool,
            rollups: Arc::new(lodestar_pipeline::Rollups::default()),
            epoch: 1,
            prior: Prior::default(),
            private_sources: 0,
            leases: Arc::new(lodestar_pipeline::stages::WorkerLeases::default()),
            refusals: None,
        }));
        (intake, Box::new((w, p)))
    }

    /// Makes a file-tree dictionary the active one, as training does.
    fn activate_dict(db: &std::path::Path, bytes: &[u8]) -> i64 {
        let store = Store::open(db, StoreConfig::default()).unwrap();
        let conn = store.connection();
        let id: i64 = conn
            .query_row(
                "INSERT INTO zstd_dicts (kind, created_at, samples, data)
                 VALUES ('file_tree', unixepoch(), 1, ?1) RETURNING id",
                [bytes],
                |r| r.get(0),
            )
            .unwrap();
        conn.execute(
            "INSERT INTO settings (key, value) VALUES ('file_dict', ?1)
             ON CONFLICT (key) DO UPDATE SET value = excluded.value",
            [id],
        )
        .unwrap();
        id
    }

    /// What one session was sent, acknowledging each snapshot chunk as it
    /// comes, until `done` says it has seen enough.
    #[derive(Debug, Default)]
    struct Got {
        assign: Option<lodestar_cluster::assign::Assignment>,
        have_headers: usize,
        /// (kind, version, seq) of every chunk.
        chunks: Vec<(SnapshotKind, u64, u32)>,
        /// (kind, version) of each transfer whose last chunk arrived.
        complete: Vec<(SnapshotKind, u64)>,
    }

    impl Got {
        fn done(&self, kind: SnapshotKind) -> bool {
            self.complete.iter().any(|c| c.0 == kind)
        }
    }

    async fn watch_session(
        link: &mut lodestar_cluster::WorkerLink,
        done: impl Fn(&Got) -> bool,
    ) -> Got {
        use lodestar_cluster::proto::{ToCoordinator, ToWorker};
        let mut got = Got::default();
        let watched = tokio::time::timeout(DEADLINE, async {
            while !done(&got) {
                let Ok(Some(msg)) = link.recv().await else {
                    panic!("the session ended: {got:?}");
                };
                match msg {
                    ToWorker::Assign(a) => {
                        got.assign = lodestar_cluster::assign::Assignment::from_message(&a)
                            .expect("a valid assignment");
                    }
                    ToWorker::HaveSnapshot(_) => got.have_headers += 1,
                    ToWorker::Snapshot(c) => {
                        got.chunks.push((c.kind, c.version, c.seq));
                        if c.seq + 1 == c.total {
                            got.complete.push((c.kind, c.version));
                        }
                        let ack = SnapshotAck {
                            kind: c.kind,
                            version: c.version,
                            seq: c.seq,
                        };
                        link.send(ToCoordinator::SnapshotAck(ack)).await.unwrap();
                    }
                    _ => {}
                }
            }
        })
        .await;
        assert!(watched.is_ok(), "not seen in time: {got:?}");
        got
    }

    /// A session on `addr`: the link, then `Hello` (the worker's side is
    /// read by hand).
    async fn session_at(addr: &str, creds: &Credentials) -> lodestar_cluster::WorkerLink {
        use lodestar_cluster::proto::{Hello, ToCoordinator};
        let mut l = lodestar_cluster::connect(addr, creds, Arc::new(SystemClock), WAIT)
            .await
            .expect("a link");
        l.send(ToCoordinator::Hello(Hello::new(
            creds.worker().0,
            Default::default(),
        )))
        .await
        .unwrap();
        l
    }

    fn ticks(h: &ClusterHandle) -> u64 {
        h.0.tick.steps.load(Ordering::Relaxed)
    }

    /// WK-14's window: a worker saved as paused is paused on a new server
    /// before it serves anyone, so one that reconnects at once is never
    /// granted a key. A row that doesn't read pauses nobody.
    #[tokio::test]
    async fn saved_pauses_are_in_before_the_server_serves() {
        let dir = tempfile::tempdir().unwrap();
        let w = wiring(dir.path(), true, any_port());
        {
            let mut store = Store::open(&w.db, w.store.clone()).unwrap();
            let paused = r#"{"v":1,"dht_off":false,"intensity":1,"cpu_permille":1000,"ram_mb":0,"up_kbps":0,"down_kbps":0,"pool_cap":50000,"paused":true}"#;
            let running = paused.replace("\"paused\":true", "\"paused\":false");
            store
                .set_settings_text(&[
                    ("workers.prefs.9", paused),
                    ("workers.prefs.10", &running),
                    ("workers.prefs.11", "not json"),
                ])
                .unwrap();
        }
        let saved = saved_pauses(&w.db, w.store.clone());
        assert_eq!(saved, vec![WorkerId(9)]);
        let cluster = Cluster::start(w).await.unwrap();
        let h = cluster.handle();
        // Paused already: in the server's fleet before any connection.
        let snap = h.server().unwrap().snapshot();
        let nine = snap.workers.iter().find(|s| s.worker == WorkerId(9));
        assert!(nine.is_some_and(|s| s.paused), "{:?}", snap.workers);
        assert!(!snap.workers.iter().any(|s| s.worker == WorkerId(10)));
        cluster.shutdown().await;
    }

    /// A held key (tombstoned here, as a floor drop leaves it) counts as
    /// stored for a claim, so no worker is granted it to fetch; a read
    /// error counts as not stored and is counted.
    #[test]
    fn a_held_key_is_never_granted_to_a_worker() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("lodestar.db");
        let mut store = Store::open(&db, StoreConfig::default()).unwrap();
        store
            .write(&lodestar_store::WriteBatch {
                filtered: &[lodestar_store::Filtered {
                    hashes: &[[3; 20]],
                    rule: "CS-8a",
                    action: 4,
                    held: Some(lodestar_store::Held::Tombstone),
                    sealed: None,
                    evidence: &[],
                    at: 1,
                    expires_at: 0,
                    adult_hold: false,
                }],
                ..Default::default()
            })
            .unwrap();
        let check = StoreCheck {
            reader: Mutex::new(Store::open_reader(&db, StoreConfig::default()).unwrap()),
            errors: Arc::default(),
            floor: Arc::default(),
            filter: None,
        };
        assert_eq!(
            check.stored(&[InfoHash([3; 20]), InfoHash([4; 20])]),
            vec![true, false],
            "the tombstoned key is refused; the other is free"
        );
        assert_eq!(check.errors.load(Ordering::Relaxed), 0);
    }

    /// WK-10d: a worker's floor report is refused to every claim as soon as the sink takes it,
    /// kept across a failed write, and written as a `worker-floor:<id>`
    /// tombstone on the next try; only a `Blocked` loss counts.
    #[test]
    fn a_floor_report_is_refused_at_once_and_never_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("lodestar.db");
        drop(Store::open(&db, StoreConfig::default()).unwrap());
        let floor = Arc::new(FloorReports::new(None));
        let tick = Arc::new(Tick::new(&wiring(dir.path(), false, any_port())));
        let sink = Sink {
            remote: None,
            floor: floor.clone(),
            clock: Arc::new(SystemClock),
            have: tick.have.clone(),
            acks: mpsc::channel(1).0,
            server: OnceLock::new(),
            tick: Arc::downgrade(&tick),
            over_budget: HookSlot::default(),
        };
        let lease = Lease {
            holder: WorkerId(7),
            token: lodestar_cluster::Token(1),
            cohort: 0,
            source: lodestar_store::SourceId::new(1).unwrap(),
            sources: 2,
            strategy: 0,
            discovered_at: 0,
            finder: None,
        };
        let lost = |reason: lodestar_sources::Loss| Outcome::Lost {
            reason: reason as u8,
        };
        sink.outcomes(
            WorkerId(7),
            vec![
                (
                    InfoHash([5; 20]),
                    lease,
                    lost(lodestar_sources::Loss::Blocked),
                ),
                (
                    InfoHash([6; 20]),
                    lease,
                    lost(lodestar_sources::Loss::GaveUp),
                ),
            ],
        );
        assert_eq!(floor.len(), 1, "only the floor drop");
        let check = StoreCheck {
            reader: Mutex::new(Store::open_reader(&db, StoreConfig::default()).unwrap()),
            errors: Arc::default(),
            floor: floor.clone(),
            filter: None,
        };
        let refused = || check.stored(&[InfoHash([5; 20]), InfoHash([6; 20])]);
        assert_eq!(refused(), vec![true, false], "refused before any write");
        // The store refuses the write: kept, still refused.
        let gone = dir.path().join("no-such-dir").join("lodestar.db");
        assert!(floor.flush(&gone, StoreConfig::default()).is_err());
        assert_eq!(floor.len(), 1);
        assert_eq!(refused(), vec![true, false]);
        // The next try lands it.
        assert_eq!(floor.flush(&db, StoreConfig::default()).unwrap(), 1);
        assert_eq!(floor.len(), 0);
        assert_eq!(refused(), vec![true, false], "now from the store");
        let rule = Store::open(&db, StoreConfig::default())
            .unwrap()
            .tombstones(10)
            .unwrap()
            .into_iter()
            .find(|t| t.hash == [5; 20])
            .map(|t| t.rule);
        assert_eq!(rule.as_deref(), Some("worker-floor:7"));
    }

    /// A key stored after its grant (another worker's result) and then
    /// reported as a floor drop doesn't stay stored: nothing is left under
    /// a held hash, as `hold_claims` keeps it (#604's review, N8).
    #[test]
    fn a_floor_report_deletes_what_was_stored_under_it() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("lodestar.db");
        let raw = b"d6:lengthi1048576e4:name9:floor.bin12:piece lengthi262144e6:pieces20:aaaaaaaaaaaaaaaaaaaae".to_vec();
        let info = lodestar_metainfo::Info::parse(&raw).unwrap();
        let key = info.v1.unwrap();
        let mut store = Store::open(&db, StoreConfig::default()).unwrap();
        store
            .insert(&[lodestar_store::NewTorrent {
                info: &info,
                raw_info: Some(&raw),
                discovered_at: 1,
                metadata_at: 1,
                extra_flags: 0,
                source: lodestar_store::SourceId::new(0).unwrap(),
            }])
            .unwrap();
        assert!(store.find(&key).unwrap().is_some());
        drop(store);
        let floor = FloorReports::new(None);
        floor.report(WorkerId(4), &key, 2, Instant::now());
        assert_eq!(floor.flush(&db, StoreConfig::default()).unwrap(), 1);
        let store = Store::open(&db, StoreConfig::default()).unwrap();
        assert!(store.find(&key).unwrap().is_none(), "deleted");
        assert_eq!(store.held_among(&[key.0]).unwrap(), vec![true]);
    }

    fn key_n(i: usize) -> InfoHash {
        let mut h = [0u8; 20];
        h[..8].copy_from_slice(&(i as u64).to_le_bytes());
        InfoHash(h)
    }

    /// #607's reviews: each worker may report `FLOOR_BUDGET` floor drops a
    /// rolling window; within it each one reaches the map and the built-in
    /// worker's held set. The first over it is `first` (once); it and the
    /// rest are dropped: neither in the map nor in the held set.
    #[test]
    fn floor_reports_have_a_per_worker_budget_and_reach_the_built_in_worker() {
        let held = Arc::new(lodestar_filter::HeldSet::default());
        let floor = FloorReports::new(Some(held.clone()));
        let now = Instant::now();
        for i in 0..FLOOR_BUDGET {
            assert_eq!(
                floor.report(WorkerId(2), &key_n(i), 1, now),
                Reported::Taken
            );
        }
        assert!(held.contains(&key_n(0).0), "the built-in worker refuses it");
        assert_eq!(
            floor.report(WorkerId(2), &key_n(1000), 1, now),
            Reported::OverBudget { first: true }
        );
        assert_eq!(
            floor.report(WorkerId(2), &key_n(1001), 1, now),
            Reported::OverBudget { first: false }
        );
        assert!(!floor.holds(&key_n(1001)) && !held.contains(&key_n(1001).0));
        assert_eq!(floor.unwritten(), FLOOR_BUDGET, "only those within it");
        assert_eq!(floor.over_budget.load(Ordering::Relaxed), 2);
        // Another worker has its own budget; and the window rolls.
        assert_eq!(
            floor.report(WorkerId(3), &key_n(2000), 1, now),
            Reported::Taken
        );
        let later = now + FLOOR_WINDOW;
        assert_eq!(
            floor.report(WorkerId(2), &key_n(3000), 1, later),
            Reported::Taken
        );
    }

    /// #607's delta review: the cap counts only reports within a budget; a
    /// turned-away report uses up no budget slot, and crossing the budget
    /// while the map is full is still `first`.
    #[test]
    fn a_full_map_turns_reports_away_but_never_hides_the_budget() {
        let held = Arc::new(lodestar_filter::HeldSet::default());
        let floor = FloorReports::with_cap(Some(held.clone()), 3);
        let now = Instant::now();
        for i in 0..3 {
            assert_eq!(
                floor.report(WorkerId(1), &key_n(i), 1, now),
                Reported::Taken
            );
        }
        // Full: worker 2's reports are turned away, counted, not held, and
        // its budget is untouched.
        for i in 0..FLOOR_BUDGET {
            assert_eq!(
                floor.report(WorkerId(2), &key_n(100 + i), 1, now),
                Reported::TurnedAway
            );
        }
        assert_eq!(
            floor.turned_away.load(Ordering::Relaxed),
            FLOOR_BUDGET as u64
        );
        assert!(!held.contains(&key_n(100).0));
        // A key already in the map is still taken (it uses no room).
        assert_eq!(
            floor.report(WorkerId(2), &key_n(0), 1, now),
            Reported::Taken
        );
        // Turned away, worker 2 used no slot: its whole budget is left.
        // Worker 1 uses up its budget on keys already in the map, and
        // crosses it while the map is full: still `first`, once.
        for _ in 3..FLOOR_BUDGET {
            assert_eq!(
                floor.report(WorkerId(1), &key_n(1), 1, now),
                Reported::Taken
            );
        }
        assert_eq!(
            floor.report(WorkerId(1), &key_n(600), 1, now),
            Reported::OverBudget { first: true }
        );
        assert_eq!(
            floor.report(WorkerId(1), &key_n(601), 1, now),
            Reported::OverBudget { first: false }
        );
        for _ in 1..FLOOR_BUDGET {
            assert_eq!(
                floor.report(WorkerId(2), &key_n(2), 1, now),
                Reported::Taken
            );
        }
        assert_eq!(
            floor.report(WorkerId(2), &key_n(2), 1, now),
            Reported::OverBudget { first: true }
        );
        assert_eq!(floor.len(), 3);
    }

    /// #607's delta review (M1): a worker over its budget can't fill the
    /// map for everyone: an honest worker's report is still taken.
    #[test]
    fn a_worker_over_budget_cant_crowd_out_an_honest_one() {
        let floor = FloorReports::with_cap(None, FLOOR_BUDGET + 10);
        let now = Instant::now();
        for i in 0..10_000 {
            let _ = floor.report(WorkerId(9), &key_n(i), 1, now);
        }
        assert_eq!(floor.len(), FLOOR_BUDGET);
        assert_eq!(
            floor.report(WorkerId(4), &key_n(50_000), 1, now),
            Reported::Taken
        );
        assert_eq!(floor.turned_away.load(Ordering::Relaxed), 0);
        // Revoked, it's forgotten: its unwritten reports and its window.
        assert_eq!(floor.forget_worker(WorkerId(9)), FLOOR_BUDGET);
        assert_eq!(floor.len(), 1);
        assert_eq!(
            floor.report(WorkerId(9), &key_n(1), 1, now),
            Reported::Taken
        );
    }

    /// #607's review: the claim check reads the floor map before the store,
    /// so a report written (and removed from the map) between the two reads
    /// is still refused; and the floor's hash list refuses too.
    #[test]
    fn a_report_written_between_the_checks_reads_is_still_refused() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("lodestar.db");
        drop(Store::open(&db, StoreConfig::default()).unwrap());
        let floor = Arc::new(FloorReports::new(None));
        floor.report(WorkerId(7), &InfoHash([5; 20]), 1, Instant::now());
        let filter = lodestar_pipeline::default_filter();
        filter.update(|c| {
            c.cs.add_hash([6; 20], lodestar_filter::Origin::Confirmed);
        });
        let check = StoreCheck {
            reader: Mutex::new(Store::open_reader(&db, StoreConfig::default()).unwrap()),
            errors: Arc::default(),
            floor: floor.clone(),
            filter: Some(Arc::new(filter)),
        };
        let keys = [InfoHash([5; 20]), InfoHash([6; 20]), InfoHash([7; 20])];
        let got = check.check_with(&keys, || {
            assert_eq!(floor.flush(&db, StoreConfig::default()).unwrap(), 1);
            assert_eq!(floor.len(), 0, "gone from the map before the store is read");
        });
        assert_eq!(got, vec![KeyCheck::Held, KeyCheck::Held, KeyCheck::Free]);
    }

    /// A report taken just before a shutdown is in the store once the
    /// cluster has shut down.
    #[tokio::test]
    async fn the_last_floor_reports_are_written_at_shutdown() {
        let dir = tempfile::tempdir().unwrap();
        let w = wiring(dir.path(), false, any_port());
        let db = w.db.clone();
        drop(Store::open(&db, StoreConfig::default()).unwrap());
        let cluster = Cluster::start(w).await.unwrap();
        let h = cluster.handle();
        let floor = h.0.floor.clone();
        floor.report(WorkerId(3), &InfoHash([9; 20]), 1, Instant::now());
        // Pending or already written: the metrics say which.
        let mut m = String::new();
        h.render(&mut m);
        let pending = series(&m, "lodestar_cluster_floor_reports_pending");
        let written = series(&m, "lodestar_cluster_floor_reports_written_total");
        assert_eq!(series(&m, "lodestar_cluster_floor_reports_total"), 1);
        assert_eq!(pending + written, 1, "{m}");
        // The sessions stop, and a report still in flight lands after: the
        // writer runs until they're gone (#607's review).
        let writer = cluster.stop_serving().await;
        floor.report(WorkerId(3), &InfoHash([10; 20]), 2, Instant::now());
        writer.finish().await;
        assert_eq!(floor.len(), 0);
        let held = Store::open(&db, StoreConfig::default())
            .unwrap()
            .held_among(&[[9; 20], [10; 20]])
            .unwrap();
        assert_eq!(held, vec![true, true]);
    }

    /// Only public addresses reach the planner from the DHT's vote.
    #[test]
    fn only_a_public_vote_is_the_coordinators_address() {
        let ip = |s: &str| Some(s.parse::<std::net::IpAddr>().unwrap());
        assert_eq!(public_ips(ip("81.2.69.7")), vec![ip("81.2.69.7").unwrap()]);
        assert_eq!(
            public_ips(ip("::ffff:81.2.69.7")),
            vec![ip("81.2.69.7").unwrap()]
        );
        assert_eq!(public_ips(ip("2a00:1450::1")).len(), 1);
        for not in [
            "127.0.0.1",
            "10.1.2.3",
            "192.168.0.9",
            "169.254.1.1",
            "100.64.0.1",
            "0.0.0.0",
            "::1",
            "::",
            "fe80::1",
            "fd00::1",
        ] {
            assert!(public_ips(ip(not)).is_empty(), "{not}");
        }
        assert!(public_ips(None).is_empty());
    }

    /// Once a shutdown returns, nothing of the old tick holds the server.
    /// (`close` also waits for the aborted tick to go, #592's review; this
    /// doesn't catch that wait's removal, since an aborted tick normally
    /// goes before `close` gets on.)
    #[tokio::test]
    async fn a_close_releases_the_server() {
        let dir = tempfile::tempdir().unwrap();
        let mut w = wiring(dir.path(), false, any_port());
        w.settings.enabled = true;
        w.fleet = Some(fleet());
        let (remote, _ends) = intake(&w.authority);
        w.remote = Some(remote);
        let cluster = Cluster::start(w).await.unwrap();
        let h = cluster.handle();
        until("ticking", || (ticks(&h) > 0, ticks(&h))).await;
        let server = Arc::downgrade(&h.server().unwrap());
        cluster.shutdown().await;
        assert!(server.upgrade().is_none(), "nothing still holds the server");
    }

    /// The tick's gate: a connected worker is planned and sent its
    /// assignment, the have-filter (built for it), the filter snapshot and
    /// the dictionary, paced by its acknowledgements; the assignment then
    /// names both versions, and the intake opens records under today's
    /// and yesterday's keys. A newly trained dictionary is built and sent
    /// once, not on every tick.
    #[tokio::test]
    async fn a_worker_gets_its_assignment_every_snapshot_and_a_new_dictionary() {
        let dir = tempfile::tempdir().unwrap();
        let mut w = wiring(dir.path(), false, any_port());
        let first_dict = activate_dict(&w.db, &[b'a'; 2048]);
        w.settings.enabled = true;
        w.fleet = Some(fleet());
        let (remote, _ends) = intake(&w.authority);
        w.remote = Some(remote);
        let (placement, now) = (w.placement.clone(), w.clock.now_secs());
        let w_clock = w.clock.clone();
        let cluster = Cluster::start(w).await.unwrap();
        let h = cluster.handle();
        // The keys are set when the server is built, before any tick.
        // Yesterday's and today's, and from 23:00Z tomorrow's too: whatever
        // `open_keys` says for now (the test may run either side of 23:00Z
        // or of midnight, so the expectation follows the clock).
        let keys = format!("{:?}", lock(&h.0.tick.keys));
        let later = w_clock.now_secs();
        // Midnight may pass between `now` and the build: either day's pair.
        assert!(
            [now, later].iter().any(|&t| {
                let today = day_of(t);
                keys.contains(&format!("{}, {today}", today - 1))
            }),
            "{keys}"
        );
        let want = |at: i64| {
            let ids: Vec<String> = open_keys(&[7; 32], at)
                .iter()
                .map(|k| k.id.to_string())
                .collect();
            format!("Some(WireKeys([{}]))", ids.join(", "))
        };
        assert!(keys == want(now) || keys == want(later), "{keys}");
        let creds = join(&h, "w1").await;
        let addr = h.local_addr().unwrap().to_string();

        let mut l = session_at(&addr, &creds).await;
        let got = watch_session(&mut l, |g| {
            g.have_headers >= 1
                && g.done(SnapshotKind::Filter)
                && g.done(SnapshotKind::Dictionary)
                && g.done(SnapshotKind::HaveFilter)
                && g.assign.is_some_and(|a| a.filter_version != 0)
        })
        .await;
        let a = got.assign.unwrap();
        let version = |kind| got.complete.iter().find(|c| c.0 == kind).unwrap().1;
        assert_eq!(a.filter_version, version(SnapshotKind::Filter), "{got:?}");
        assert_eq!(a.dict_version, u64::try_from(first_dict).unwrap());
        assert_eq!(version(SnapshotKind::Dictionary), a.dict_version);
        assert!(a.fetch && a.prepare);
        assert_eq!(placement.assignment(creds.worker()), Some(a));
        let m = metrics(&h);
        assert_eq!(series(&m, "lodestar_cluster_snapshot_installs_total"), 1);
        assert_eq!(series(&m, "lodestar_cluster_have_rebuilds_total"), 1);
        assert!(series(&m, "lodestar_cluster_have_filter_bytes") > 0);
        assert_eq!(
            series(&m, "lodestar_cluster_snapshot_install_failures_total"),
            0
        );

        // A new dictionary is trained: it's read by its id (the long-lived
        // reader's cached one is stale), sent, and built once.
        let second = activate_dict(&dir.path().join("lodestar.db"), &[b'b'; 2048]);
        let second = u64::try_from(second).unwrap();
        let got = watch_session(&mut l, |g| {
            g.complete.contains(&(SnapshotKind::Dictionary, second))
                && g.assign.is_some_and(|a| a.dict_version == second)
        })
        .await;
        assert!(got.assign.is_some());
        let settled = ticks(&h) + 3;
        until("three more ticks", || (ticks(&h) >= settled, ticks(&h))).await;
        assert_eq!(
            series(&metrics(&h), "lodestar_cluster_snapshot_installs_total"),
            2,
            "built once per change, not every tick"
        );
        cluster.shutdown().await;
    }

    /// A rig whose tick runs only when the test says: the server, its
    /// tick state, and a joined worker.
    struct Rig {
        _dir: tempfile::TempDir,
        tick: Arc<Tick>,
        server: Arc<ClusterServer>,
        wiring: Wiring,
        addr: String,
        creds: Credentials,
        acks: mpsc::Receiver<(WorkerId, SnapshotAck)>,
        seen: Seen,
    }

    async fn rig(db: Option<&str>) -> Rig {
        let dir = tempfile::tempdir().unwrap();
        let mut wiring = wiring(dir.path(), true, any_port());
        wiring.fleet = Some(fleet());
        let (pki, check) = open_pki(&wiring, Arc::new(AtomicU64::new(0)), Arc::default()).unwrap();
        let mut tick = Tick::new(&wiring);
        if let Some(db) = db {
            // The tick's reads go to a store that isn't there. Its reads are
            // retried at once and its rebuilds not for an hour, so the test
            // decides what runs, however slow the runner.
            wiring.db = dir.path().join(db);
            tick = Tick::new(&wiring);
            tick.rebuild_retry = Arc::new(Retry::new(Duration::from_secs(3600)));
            tick.read_retry = Retry::new(Duration::ZERO);
        }
        let tick = Arc::new(tick);
        let (acks, ack_rx) = mpsc::channel(ACK_QUEUE);
        let sink = Arc::new(Sink {
            remote: None,
            floor: Arc::default(),
            clock: wiring.clock.clone(),
            have: tick.have.clone(),
            acks,
            server: OnceLock::new(),
            tick: Arc::downgrade(&tick),
            over_budget: HookSlot::default(),
        });
        let hooks = ServerHooks {
            authority: wiring.authority.clone(),
            stored: Some(Arc::new(check)),
            sink: Some(sink.clone() as _),
            admission: None,
        };
        let server =
            ClusterServer::with_hooks(pki, wiring.clock.clone(), ServerConfig::default(), hooks)
                .unwrap();
        let _ = sink.server.set(Arc::downgrade(&server));
        let listener = TcpListener::bind(any_port()).await.unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        tokio::spawn(server.clone().serve(listener));
        let token = server.create_join_token("w1").await.unwrap();
        let joining = Joining::new(JoinToken::parse(&token.expose()).unwrap()).unwrap();
        let creds = lodestar_cluster::join(&addr, joining, Arc::new(SystemClock), WAIT)
            .await
            .unwrap();
        Rig {
            _dir: dir,
            tick,
            server,
            wiring,
            addr,
            creds,
            acks: ack_rx,
            seen: Seen::default(),
        }
    }

    impl Rig {
        async fn step(&mut self) {
            step(&self.tick, &self.server, &self.wiring, &mut self.seen).await;
        }

        fn sessions(&self) -> (u64, bool) {
            let s = self.server.snapshot();
            let on = s
                .workers
                .iter()
                .any(|w| w.worker == self.creds.worker() && w.connected);
            (s.sessions, on)
        }

        /// A second worker on the same server.
        async fn join(&self, name: &str) -> Credentials {
            let token = self.server.create_join_token(name).await.unwrap();
            let joining = Joining::new(JoinToken::parse(&token.expose()).unwrap()).unwrap();
            lodestar_cluster::join(&self.addr, joining, Arc::new(SystemClock), WAIT)
                .await
                .unwrap()
        }

        fn session_of(&self, w: WorkerId) -> Option<u64> {
            self.server
                .snapshot()
                .workers
                .iter()
                .find(|s| s.worker == w)
                .and_then(|s| s.session)
        }

        /// Applies the acknowledgements the sink queued, as the tick does,
        /// until one from each of `from` is in.
        async fn take_acks(&mut self, from: &[WorkerId]) {
            let mut seen: Vec<WorkerId> = Vec::new();
            while !from.iter().all(|w| seen.contains(w)) {
                let (w, ack) = tokio::time::timeout(DEADLINE, self.acks.recv())
                    .await
                    .expect("an acknowledgement in time")
                    .unwrap();
                if let Some(s) = self.tick.snapshots().as_mut() {
                    s.ack(w, ack, &self.server, Instant::now());
                }
                seen.push(w);
            }
        }
    }

    /// Reads `link` until a chunk of `until` arrives, and returns the
    /// filter-snapshot chunks seen before it, as (version, seq). Snapshot
    /// chunks of every kind share one queue per session, so they arrive in
    /// the order they were sent.
    async fn filter_chunks_before(
        link: &mut lodestar_cluster::WorkerLink,
        until: SnapshotKind,
    ) -> Vec<(u64, u32)> {
        use lodestar_cluster::proto::ToWorker;
        let mut chunks = Vec::new();
        tokio::time::timeout(DEADLINE, async {
            loop {
                match link.recv().await {
                    Ok(Some(ToWorker::Snapshot(c))) if c.kind == until => return,
                    Ok(Some(ToWorker::Snapshot(c))) if c.kind == SnapshotKind::Filter => {
                        chunks.push((c.version, c.seq));
                    }
                    Ok(Some(_)) => {}
                    other => panic!("the session ended: {other:?}"),
                }
            }
        })
        .await
        .expect("the marker chunk in time");
        chunks
    }

    /// The review's B1: one worker reconnecting (as a certificate renewal
    /// does, between two ticks) restarts its own snapshot transfer and
    /// leaves another worker's finished one alone.
    #[tokio::test]
    async fn a_reconnect_restarts_only_that_workers_transfer() {
        let mut r = rig(None).await;
        let b = r.join("w2").await;
        let a = r.creds.worker();
        let mut la = session_at(&r.addr, &r.creds).await;
        let mut lb = session_at(&r.addr, &b).await;
        until("both connected", || {
            let on = (r.session_of(a), r.session_of(b.worker()));
            (on.0.is_some() && on.1.is_some(), on)
        })
        .await;
        r.step().await;
        watch_session(&mut la, |g| g.done(SnapshotKind::Filter)).await;
        watch_session(&mut lb, |g| g.done(SnapshotKind::Filter)).await;
        // Both transfers finish: their acknowledgements are taken.
        r.take_acks(&[a, b.worker()]).await;

        // A reconnects with no tick in between.
        let first = r.session_of(a).unwrap();
        drop(la);
        until("A gone", || {
            let s = r.session_of(a);
            (s.is_none(), s)
        })
        .await;
        let mut la = session_at(&r.addr, &r.creds).await;
        until("A back on a new session", || {
            let s = r.session_of(a);
            (s.is_some_and(|s| s != first), s)
        })
        .await;
        r.step().await;
        let a_again = watch_session(&mut la, |g| g.done(SnapshotKind::Filter)).await;
        assert_eq!(a_again.chunks[0].2, 0, "A starts over: {a_again:?}");
        // A marker on B's snapshot queue: a dictionary, trained now, goes to
        // both. Any resend of B's finished filter transfer would be ahead
        // of it.
        activate_dict(&r.wiring.db, &[b'm'; 512]);
        r.step().await;
        let b_chunks = filter_chunks_before(&mut lb, SnapshotKind::Dictionary).await;
        assert!(
            b_chunks.is_empty(),
            "B's finished transfer was resent: {b_chunks:?}"
        );
    }

    /// #607's delta review: the first floor report over a worker's budget
    /// pauses it and takes its leases back in the sink itself, with no
    /// `Workers` hook registered (as at startup, or with `Workers` gone),
    /// so its later reports are stale.
    #[tokio::test]
    async fn a_worker_over_its_floor_budget_is_paused_and_loses_its_leases() {
        use lodestar_cluster::proto::{
            Capabilities, ClaimItem, GrantItem, Hello, Outcome, OutcomeItem,
        };
        let r = rig(None).await;
        let w = r.creds.worker();
        let link = lodestar_cluster::connect(&r.addr, &r.creds, Arc::new(SystemClock), WAIT)
            .await
            .unwrap();
        let config = lodestar_cluster::WorkerConfig {
            auto_heartbeat: false,
            ..lodestar_cluster::WorkerConfig::default()
        };
        let s = lodestar_cluster::WorkerSession::start(
            link,
            Hello::new(w.0, Capabilities::default()),
            Arc::new(SystemClock),
            config,
        )
        .await
        .unwrap();
        let items: Vec<ClaimItem> = (0..=FLOOR_BUDGET + 1)
            .map(|i| ClaimItem {
                key: key_n(i).0,
                source: 0,
                strategy: 0,
                discovered_ms: SystemClock.now_ms(),
                hints: vec![],
            })
            .collect();
        let grants = s.claim(items).await.unwrap();
        let mut reports: Vec<OutcomeItem> = grants
            .iter()
            .map(|g| match g {
                GrantItem::Granted { key, token, .. } => OutcomeItem {
                    key: *key,
                    token: *token,
                    outcome: Outcome::Lost {
                        reason: lodestar_sources::Loss::Blocked as u8,
                    },
                },
                GrantItem::Denied { .. } => panic!("denied: {g:?}"),
            })
            .collect();
        // One lease kept back: it's evicted with the rest.
        let kept = reports.pop().unwrap();
        s.report(reports).await.unwrap();
        s.heartbeat().await.unwrap();
        let status = |server: &ClusterServer| {
            server
                .snapshot()
                .workers
                .into_iter()
                .find(|x| x.worker == w)
                .map(|x| (x.paused, x.leases))
        };
        until("paused, leases gone", || {
            let st = status(&r.server);
            (st == Some((true, 0)), st)
        })
        .await;
        // Its report of the kept key is stale now.
        let stale = r.server.snapshot().stale_reports;
        s.report(vec![kept]).await.unwrap();
        s.heartbeat().await.unwrap();
        assert_eq!(r.server.snapshot().stale_reports, stale + 1);
    }

    /// The coordinator's shares as the tick applied them.
    #[derive(Default)]
    struct Shares(Mutex<Vec<Politeness>>);

    impl ApplyShare for Shares {
        fn apply(
            &self,
            share: Politeness,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'static>> {
            self.0.lock().unwrap().push(share);
            Box::pin(async {})
        }
    }

    /// WK-10e: the tick applies the coordinator's own share of the plan,
    /// at once when it changes and not again while it holds; a worker that
    /// may share its public address splits it.
    #[tokio::test]
    async fn the_coordinator_applies_its_own_share() {
        let mut r = rig(None).await;
        let shares = Arc::new(Shares::default());
        r.wiring.coordinator_share = Some(shares.clone());
        r.step().await;
        r.step().await;
        assert_eq!(
            *shares.0.lock().unwrap(),
            vec![Politeness::ALONE],
            "alone, applied once"
        );
        // A worker whose address is unknown might be behind the
        // coordinator's: the planner splits the coordinator's share.
        let _link = session_at(&r.addr, &r.creds).await;
        until("connected", || {
            let s = r.sessions();
            (s.1, s)
        })
        .await;
        r.step().await;
        let applied = shares.0.lock().unwrap().clone();
        let now = r.tick.placement.coordinator().politeness;
        assert_ne!(now, Politeness::ALONE, "the plan split it");
        assert_eq!(applied, vec![Politeness::ALONE, now]);
    }

    /// WK-10e: a share the protocol wouldn't accept is refused, so the
    /// last one stands; never "unlimited".
    #[test]
    fn an_invalid_share_is_refused() {
        assert_eq!(checked_share(Politeness::ALONE), Ok(Politeness::ALONE));
        for bad in [
            Politeness {
                peer_ip_groups: 0,
                ..Politeness::ALONE
            },
            Politeness {
                peer_ip_group: 1,
                ..Politeness::ALONE
            },
            Politeness {
                per_peer_ip: 0,
                ..Politeness::ALONE
            },
            Politeness {
                bep51_interval_scale: 0,
                ..Politeness::ALONE
            },
        ] {
            assert_eq!(
                checked_share(bad),
                Err(lodestar_cluster::assign::AssignError::Field("politeness"))
            );
        }
    }

    /// WK-8: a worker whose session restarted gets both snapshots again
    /// from chunk 0, even when it reconnected between two ticks, so the
    /// tick never saw it gone (here the first transfer is still waiting for
    /// the tick to take its acknowledgement, and would otherwise never be
    /// sent again).
    #[tokio::test]
    async fn a_session_that_restarted_between_ticks_gets_the_snapshot_again() {
        let mut r = rig(None).await;
        let mut first = session_at(&r.addr, &r.creds).await;
        until("connected", || {
            let s = r.sessions();
            (s.1, s)
        })
        .await;
        r.step().await;
        let got = watch_session(&mut first, |g| g.done(SnapshotKind::Filter)).await;
        let chunk = got
            .chunks
            .iter()
            .find(|c| c.0 == SnapshotKind::Filter)
            .copied()
            .unwrap();
        assert_eq!(chunk.2, 0);

        // Gone and back with no tick in between.
        drop(first);
        until("disconnected", || {
            let s = r.sessions();
            (!s.1, s)
        })
        .await;
        let mut second = session_at(&r.addr, &r.creds).await;
        until("connected again", || {
            let s = r.sessions();
            (s.1 && s.0 == 2, s)
        })
        .await;
        r.step().await;
        let again = watch_session(&mut second, |g| g.done(SnapshotKind::Filter)).await;
        assert!(again.chunks.contains(&chunk), "{again:?}");
        drop(r.acks);
    }

    /// Turning workers off (or a reseal) aborts the tick, maybe in the
    /// middle of an install: the snapshots still go back, so the next
    /// server's tick builds and sends them.
    #[tokio::test]
    // Held on purpose, to park the blocking half; nothing else here takes
    // the reader.
    #[allow(clippy::await_holding_lock)]
    async fn an_install_aborted_midway_keeps_the_snapshots() {
        let r = rig(None).await;
        let fleet = r.wiring.fleet.clone().unwrap();
        // Holding the read connection parks the install's blocking half
        // once it has taken the snapshots.
        let held = lock(&r.tick.reader);
        let tick = r.tick.clone();
        let task = tokio::spawn(async move { install(&tick, &fleet, 1, 1_000, None).await });
        until("taken", || {
            let taken = r.tick.snapshots().is_none();
            (taken, taken)
        })
        .await;
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        drop(held);
        until("put back by the blocking half", || {
            let back = r.tick.snapshots().is_some();
            (back, back)
        })
        .await;
        let version = until("installed", || {
            let v = r.tick.snapshots().as_ref().map(|s| s.versions().1);
            (v.is_some_and(|v| v != 0), v)
        })
        .await
        .unwrap();
        // The next tick hands the versions to placement (the review's N1),
        // though the install's own tick was aborted before it could.
        let _link = session_at(&r.addr, &r.creds).await;
        until("connected", || {
            let s = r.sessions();
            (s.1, s)
        })
        .await;
        let mut r = r;
        r.step().await;
        let planned = r
            .wiring
            .placement
            .assignment(r.creds.worker())
            .map(|a| a.filter_version);
        // That tick's own build (a real engine and day this time) may have
        // replaced it; whichever is installed is what's planned.
        let now = r.tick.snapshots().as_ref().map(|s| s.versions().1);
        assert!(now.is_some_and(|v| v != 0) && version != 0);
        assert_eq!(planned, now);
    }

    /// A store the snapshots can't read: said once, counted each time; a
    /// rebuild that fails backs off instead of scanning again every tick.
    #[tokio::test]
    async fn store_failures_are_said_once_and_rebuilds_back_off() {
        crate::test_log::install();
        let mut r = rig(Some("missing/lodestar.db")).await;
        let _link = session_at(&r.addr, &r.creds).await;
        until("connected", || {
            let s = r.sessions();
            (s.1, s)
        })
        .await;
        let said = || {
            crate::test_log::text()
                .matches("cluster.snapshot_failed")
                .count()
        };
        let before = said();
        r.step().await;
        until("the rebuild failed", || {
            let n = r.tick.rebuild_failures.load(Ordering::Relaxed);
            (n == 1 && !r.tick.rebuilding.load(Ordering::Acquire), n)
        })
        .await;
        assert_eq!(r.tick.rebuild_retry.wait(), Some(Duration::from_secs(3600)));
        r.step().await;
        assert_eq!(r.tick.install_failures.load(Ordering::Relaxed), 2);
        assert_eq!(said(), before + 1, "said once while it keeps failing");
        // Not tried again before its wait is up.
        assert!(!r.tick.rebuilding.load(Ordering::Acquire));
        assert_eq!(r.tick.rebuild_failures.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn retries_back_off_and_reset() {
        // Not due while waiting (an hour, so a slow runner can't pass it).
        let r = Retry::new(Duration::from_secs(3600));
        assert!(r.due());
        r.failed();
        assert!(!r.due(), "not before its wait");
        r.succeeded();
        assert!(r.due());
        assert_eq!(r.wait(), None);
        // The waits double from the first, up to five minutes.
        let r = Retry::new(Duration::from_secs(1));
        r.failed();
        assert_eq!(r.wait(), Some(Duration::from_secs(1)));
        for want in [2, 4, 8, 16, 32, 64, 128, 256, 300, 300] {
            r.failed();
            assert_eq!(r.wait(), Some(Duration::from_secs(want)));
        }
    }

    /// Sets a flag when dropped: a task holding it has ended.
    struct Ended(Arc<AtomicBool>);

    impl Drop for Ended {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    /// #592's delta review: aborting the watcher ends the tick, whether the
    /// watcher had run yet or not (a current-thread runtime, so the first
    /// abort lands before it runs).
    #[tokio::test]
    async fn aborting_the_watcher_ends_the_tick_before_and_after_it_runs() {
        for run_first in [false, true] {
            let (ended, started) = (
                Arc::new(AtomicBool::new(false)),
                Arc::new(AtomicBool::new(false)),
            );
            let (e, s) = (Ended(ended.clone()), started.clone());
            let tick = tokio::spawn(async move {
                let _e = e;
                s.store(true, Ordering::SeqCst);
                std::future::pending::<()>().await;
            });
            let watcher = tokio::spawn(watch_tick(tick));
            if run_first {
                until("the tick runs", || {
                    let on = started.load(Ordering::SeqCst);
                    (on, on)
                })
                .await;
            }
            watcher.abort();
            until("the tick ended", || {
                let done = ended.load(Ordering::SeqCst);
                (done, (run_first, done))
            })
            .await;
        }
    }

    /// A tick that panics is said once, at error level.
    #[tokio::test]
    async fn a_panicking_tick_is_said_once() {
        crate::test_log::install();
        let said = || {
            crate::test_log::text()
                .matches("cluster.tick_failed")
                .count()
        };
        let before = said();
        let tick = tokio::spawn(async { panic!("a test panic in the tick") });
        watch_tick(tick).await;
        assert_eq!(said(), before + 1);
        // A tick that's aborted, or ends, says nothing.
        let quiet = tokio::spawn(std::future::pending::<()>());
        quiet.abort();
        watch_tick(quiet).await;
        watch_tick(tokio::spawn(async {})).await;
        assert_eq!(said(), before + 1);
    }

    /// A panic in the rebuild's scan: counted as a failure, both flags
    /// clear, the old filter stays, and the next rebuild works.
    #[test]
    fn a_panicking_scan_keeps_the_old_filter() {
        let have = WorkerFilter::new();
        let (rebuilding, failures, rebuilds) =
            (AtomicBool::new(true), AtomicU64::new(0), AtomicU64::new(0));
        let retry = Retry::new(Duration::ZERO);
        let c = RebuildCounts {
            rebuilding: &rebuilding,
            retry: &retry,
            failures: &failures,
            rebuilds: &rebuilds,
        };
        rebuild_have(&have, &c, 10, |insert| {
            insert(InfoHash([1; 20]));
            Ok(())
        });
        let first = have.generation().expect("built");
        assert_eq!(rebuilds.load(Ordering::Relaxed), 1);
        rebuilding.store(true, Ordering::Relaxed);
        rebuild_have(&have, &c, 10, |_| panic!("a test panic in the scan"));
        assert_eq!(failures.load(Ordering::Relaxed), 1);
        assert!(
            !rebuilding.load(Ordering::Relaxed),
            "the tick's flag cleared"
        );
        assert_eq!(have.generation(), Some(first), "the old filter stays");
        // The filter's own flag cleared too: the next rebuild runs.
        rebuild_have(&have, &c, 10, |_| Ok(()));
        assert_eq!(rebuilds.load(Ordering::Relaxed), 2);
        assert!(have.generation() > Some(first));
    }

    /// Without a fleet (tests, or an install whose pipeline has no
    /// intake), the tick still plans, and builds and sends nothing else.
    #[tokio::test]
    async fn without_a_fleet_only_placement_runs() {
        let dir = tempfile::tempdir().unwrap();
        let cluster = Cluster::start(wiring(dir.path(), true, any_port()))
            .await
            .unwrap();
        let h = cluster.handle();
        let creds = join(&h, "w1").await;
        let mut l = session_at(&h.local_addr().unwrap().to_string(), &creds).await;
        let got = watch_session(&mut l, |g| g.assign.is_some()).await;
        assert_eq!(got.assign.unwrap().filter_version, 0);
        let m = metrics(&h);
        assert_eq!(series(&m, "lodestar_cluster_have_rebuilds_total"), 0);
        assert_eq!(series(&m, "lodestar_cluster_have_filter_bytes"), 0);
        assert!(got.chunks.is_empty() && got.have_headers == 0, "{got:?}");
        cluster.shutdown().await;
    }

    /// From 23:00Z the snapshot is built for the next day's wire key, and
    /// stays so across midnight (no second build at 00:00Z).
    #[test]
    fn the_next_days_key_goes_out_from_23z() {
        let day = 20_400_i64;
        let at = |d: i64, secs: i64| d * 86_400 + secs;
        assert_eq!(key_time(at(day, 12 * 3600)), at(day, 12 * 3600));
        assert_eq!(
            key_time(at(day, PRESEND_FROM - 1)),
            at(day, PRESEND_FROM - 1)
        );
        assert_eq!(
            day_of(key_time(at(day, PRESEND_FROM))),
            day_of(at(day + 1, 0))
        );
        assert_eq!(day_of(key_time(at(day, 86_399))), day_of(at(day + 1, 0)));
        assert_eq!(day_of(key_time(at(day + 1, 0))), day_of(at(day + 1, 0)));
        // The snapshot's due check, which compares the day it was built for
        // with the key time's: built at 23:00Z for tomorrow, it's not due
        // again at 00:00Z.
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("c.db"), StoreConfig::default()).unwrap();
        let seal = [8; 32];
        let mut s = WorkerSnapshots::new();
        let workers = vec![lodestar_cluster::WorkerStatus {
            worker: WorkerId(3),
            connected: true,
            session: Some(1),
            draining: false,
            paused: false,
            offset_ms: 0,
            skewed: false,
            leases: 0,
            last_seen_ms: 0,
            stats: Default::default(),
            external_ips: Vec::new(),
        }];
        let built = key_time(at(day, PRESEND_FROM));
        let snap = read_snapshot(&store, &seal, built).unwrap();
        assert_eq!(i64::from(snap.key.id), day + 1);
        s.install(1, &seal, &snap, None).unwrap();
        assert!(!s.due(&workers, 1, key_time(at(day, 86_399)), None));
        assert!(!s.due(&workers, 1, key_time(at(day + 1, 60)), None));
        assert!(s.due(&workers, 1, key_time(at(day + 1, PRESEND_FROM)), None));
    }

    #[test]
    fn the_open_keys_follow_the_day() {
        let seal = [3; 32];
        let day = 20_400_i64;
        let at = |d: i64, secs: i64| d * 86_400 + secs;
        let d = |n: i64| u32::try_from(n).unwrap();
        let ids = |keys: &WireKeys| format!("{keys:?}");
        let keys = WireKeys::default();
        let mut seen = None;
        assert!(follow_keys(&keys, &seal, at(day, 43_200), &mut seen));
        assert_eq!(
            ids(&keys),
            format!("WireKeys([{}, {}])", d(day - 1), d(day))
        );
        // The same day, before 23:00Z: nothing to set.
        assert!(!follow_keys(
            &keys,
            &seal,
            at(day, PRESEND_FROM - 1),
            &mut seen
        ));
        // From 23:00Z, the next day's too: the pre-sent snapshot's key.
        assert!(follow_keys(&keys, &seal, at(day, PRESEND_FROM), &mut seen));
        assert_eq!(
            ids(&keys),
            format!("WireKeys([{}, {}, {}])", d(day - 1), d(day), d(day + 1))
        );
        assert!(!follow_keys(&keys, &seal, at(day, 86_399), &mut seen));
        // Midnight: the new day's, and the one before.
        assert!(follow_keys(&keys, &seal, at(day + 1, 0), &mut seen));
        assert_eq!(
            ids(&keys),
            format!("WireKeys([{}, {}])", d(day), d(day + 1))
        );
        // Each is the day's own key, the one the snapshot carries.
        assert_eq!(
            open_keys(&seal, at(day + 1, 5))[1],
            super::super::filter::wire_key(&seal, d(day + 1))
        );
        // The first day has no yesterday.
        assert_eq!(open_keys(&seal, 100).len(), 1);
    }

    /// Acknowledgements of the filter and dictionary snapshots go to the
    /// tick's queue, at most `ACKS_PER_WORKER` waiting per worker: one
    /// worker past its share is dropped and counted, and never takes
    /// another's room or blocks.
    #[tokio::test]
    async fn the_sink_queues_snapshot_acks_per_worker_without_blocking() {
        let dir = tempfile::tempdir().unwrap();
        let tick = Arc::new(Tick::new(&wiring(dir.path(), false, any_port())));
        let (acks, mut rx) = mpsc::channel(ACK_QUEUE);
        let sink = Sink {
            remote: None,
            floor: Arc::default(),
            clock: Arc::new(SystemClock),
            have: tick.have.clone(),
            acks,
            server: OnceLock::new(),
            tick: Arc::downgrade(&tick),
            over_budget: HookSlot::default(),
        };
        let ack = |kind, seq| SnapshotAck {
            kind,
            version: 9,
            seq,
        };
        for seq in 0..=ACKS_PER_WORKER {
            sink.snapshot_ack(WorkerId(4), ack(SnapshotKind::Filter, seq));
        }
        sink.snapshot_ack(WorkerId(5), ack(SnapshotKind::Dictionary, 0));
        // The have-filter's go straight to it (here, with no server, nowhere).
        sink.snapshot_ack(WorkerId(5), ack(SnapshotKind::HaveFilter, 0));
        assert_eq!(tick.acks_dropped.load(Ordering::Relaxed), 1);
        let mut got = Vec::new();
        while let Ok(a) = rx.try_recv() {
            got.push(a);
        }
        assert_eq!(got.len(), ACKS_PER_WORKER as usize + 1);
        assert!(got.contains(&(WorkerId(5), ack(SnapshotKind::Dictionary, 0))));
        // Without an intake, results and counters are refused for now.
        let payload = Payload {
            batch: 1,
            bytes: vec![1],
        };
        assert!(!sink.results(WorkerId(4), payload.clone()));
        assert!(!sink.counters(WorkerId(4), payload));
    }
}
