//! The work a remote worker does (architecture §19.1, §19.4, §19.5;
//! workers plan WK-10b), behind [`session::Work`](super::session::Work).
//!
//! - **Judge first.** Nothing is fetched until the coordinator's filter
//!   snapshot is installed (and its dictionary, when `Assign` names one):
//!   a worker that restarted has no wire key, so it can't seal until a
//!   fresh snapshot arrives (WK-8). Snapshots are decoded and installed on
//!   a blocking thread, never on the async threads.
//! - **Claims:** keys found here pass the replicated have-filter, are
//!   batched (≤ 512 or 250 ms) and claimed over the session; each grant's
//!   token goes into the worker's leases and the key into the fetch pool.
//!   Re-grants and peer hints (filtered by the network policy before
//!   anything dials them) go to the pool too.
//! - **The stages** are WK-7's, store-free, with the worker's queues and
//!   the pool sized by `Assign`'s cap (plan Q5) and the byte budget of
//!   X-fetch-pool-bytes and X-fetch-meta-reserve. Every result is judged
//!   and sealed ([`Sealing`]); only `SealedRecord`s can reach the outbound
//!   queue. A result that can't be sealed yet (no snapshot, or past its
//!   wire key's day) waits for the fresh snapshot; it is never reported
//!   `Blocked`, which only the floor's drops are (runner, #592's B2).
//! - **Results** go out in `Results` batches (WK-6's codec,
//!   `encode_with(&[], &sealed)`), kept until the coordinator accepts them
//!   and sent again on a new session; an accepted batch's leases are
//!   committed. Losses are queued as outcomes and never dropped.
//! - **Have-filter messages** are handled in the session's one message
//!   loop, in order (WK-3).
//!
//! **Secrets:** the wire key and the have-filter key live inside the judge
//! and the filter, which print neither; nothing here formats a payload.

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use lodestar_cluster::assign::{Assignment, Politeness};
use lodestar_cluster::proto::{
    ClaimItem, DenyReason, GrantItem, HintItem, MAX_HINTS, MAX_OPAQUE, MAX_OUTCOME_BATCH, Outcome,
    OutcomeItem, Payload, SnapshotAck, SnapshotChunk, SnapshotKind, ToCoordinator,
};
use lodestar_cluster::{SessionHandle, Token, WorkerEvent, WorkerId};
use lodestar_metainfo::InfoHash;
use lodestar_pipeline::gate::GateStats;
use lodestar_pipeline::source_adult::AdultSources;
use lodestar_pipeline::stages::{
    ByteReceiver, ByteSender, Closed, Held, Inbox, OUTBOUND_BYTES, Outbox, Queues, Reports,
    Results, Weigh, WorkerLeases, WorkerStages, byte_queue, next_claims,
};
use lodestar_pipeline::wire::ResultsBatch;
use lodestar_pipeline::wire::filter_snapshot::{
    DictSnapshot, Incoming, Judge, Judged, SealedRecord, Step,
};
use lodestar_sources::{Discovery, Fetcher, Found, FunnelCounts, Loss, PoolConfig, Shutdown};
use lodestar_store::{Preparer, QueuedKey, SourceId, StoreConfig};
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;

use super::have::WorkerHave;
use super::session::Work;
use super::spool::{Origin, Replay, SpoolConfig, WorkerSpool};
use lodestar_pipeline::spool::Kind;

/// A worker's fetch-pool byte budget (X-fetch-pool-bytes' note for WK-10:
/// the same as WK-7's fetched queue).
pub const HELD_BYTES: usize = 4 << 20;
/// A worker's largest info dict: its bound must fit inside the byte budget
/// with room for the first window and the request (X-fetch-meta-reserve).
pub const MAX_METADATA: usize = HELD_BYTES - (32 << 10);
/// Sockets a worker opens at most (X-fetch-meta-reserve's proposal for a
/// 4 MiB budget over TCP). WK-10c sizes it with the measured RSS.
pub const MAX_SOCKETS: usize = 200;
/// How long a stop or a drain waits for the pool and the stages to hand
/// over what's in flight (the pool's own drain is 5 s,
/// X-fetch-meta-reserve). Past it, the stages are stopped and their keys'
/// leases go back to the coordinator.
pub const DRAIN_DEADLINE: Duration = Duration::from_secs(10);
/// How often the have-filter's resync request is retried and refused
/// result batches are sent again.
const POLL_EVERY: Duration = Duration::from_secs(5);
/// How often the heartbeat's stats are refreshed (the session sends the
/// latest with each heartbeat).
pub const STATS_EVERY: Duration = Duration::from_secs(5);
/// A fresh session is asked for at most once per this long, so an address
/// that flaps between votes can't cause a reconnect storm.
pub const RECONNECT_DEBOUNCE: Duration = Duration::from_secs(600);
/// Keys found here waiting to be checked and claimed. A full queue drops
/// the newest: the DHT finds a key again.
const FOUND_QUEUE: usize = 4096;
/// Snapshot acknowledgements waiting for the session: well above the
/// coordinator's window per transfer (four kinds at once).
const ACK_QUEUE: usize = 1024;
/// `Accepted` answers waiting for the session task: one per batch in
/// flight, which the outbound byte budget bounds far below this.
const ACCEPTED_QUEUE: usize = 4096;
/// Room left in a `Results` payload for the batch's own header.
const BATCH_SLACK: usize = 4 << 10;
/// The source a worker's own finds are counted under (the DHT's). The
/// coordinator merges the real source bits from its own record.
const DHT_SOURCE: u8 = 1;
/// One "maybe stored" sighting in this many is still claimed, chosen by a
/// random draw per sighting (not by the key), so a false positive of the
/// have-filter (~1 %, ~2 % above 30M stored) is recovered on a later
/// sighting instead of being lost for good. The coordinator answers those
/// claims from the store (runner, 2026-11-07T20:34Z, plan Q6; the WK-10b
/// decisions file).
pub const REDISCOVERY_SAMPLE: u64 = 16;
/// The longest sealing may be down (no snapshot whose key covers now)
/// before the results waiting on it are released: each is reported lost as
/// `GaveUp` (never `Blocked`), which ends its lease so the key can be
/// claimed again. The wait counts from when sealing went down, not from
/// each result (#599's review), so a backlog is let go all at once.
pub const SEAL_WAIT: Duration = Duration::from_secs(600);
const DAY: i64 = 86_400;

/// What the runtime is built from.
pub struct Config {
    pub worker: WorkerId,
    pub fetcher: Arc<dyn Fetcher>,
    /// The pool's settings; `max_pending` comes from `Assign`'s cap.
    pub pool: PoolConfig,
    /// The commit's compression level, for the preparer.
    pub compression_level: i32,
    /// Bytes of sealed results waiting for the session (WK-7's 8 MiB).
    pub outbound_bytes: usize,
    /// Dial peers in private ranges (tests and LAN-only setups).
    pub allow_private: bool,
    pub drain_deadline: Duration,
    /// The rediscovery sampler's seed; `None` draws one at start. Tests
    /// fix it.
    pub seed: Option<u64>,
    pub seal_wait: Duration,
    /// The DHT's settings (bind, bootstrap, state); the shard and
    /// intensity come from `Assign`. `None` runs no DHT (tests, or the
    /// DHT turned off in the worker's config).
    pub dht: Option<lodestar_dht::node::Config>,
    /// How long the DHT stays paused for a lost session before it stops.
    pub dht_linger: Duration,
    /// How often the heartbeat's stats are refreshed ([`STATS_EVERY`]).
    pub stats_every: Duration,
    /// At most one fresh session asked for per this long
    /// ([`RECONNECT_DEBOUNCE`]).
    pub reconnect_debounce: Duration,
    /// How often refused result batches are sent again (5 s).
    pub retry_every: Duration,
    /// How keys the coordinator answered `Busy` are claimed again
    /// (X-wk-busy-retry).
    pub busy_retry: BusyRetry,
    /// The on-disk spool while no session is up (WK-11); `None` keeps
    /// unaccepted batches in memory only, and pauses fetching at once.
    pub spool: Option<SpoolConfig>,
}

/// The retry of `Busy` claims: a capped exponential back-off with equal
/// jitter (the session's [`Backoff`](super::session::Backoff)), a limit on
/// attempts, and a limit on the keys waiting.
#[derive(Debug, Clone, Copy)]
pub struct BusyRetry {
    pub base: Duration,
    pub cap: Duration,
    /// Claims after the first, before the key is dropped.
    pub attempts: u8,
    /// Keys in a retry at once; a `Busy` key past it is dropped.
    pub capacity: usize,
}

impl Default for BusyRetry {
    /// 2 s doubling to a 60 s cap, 5 attempts (about a minute in all):
    /// `Busy` is backpressure (the writer's queue, or a store read) that
    /// clears in seconds, and past a minute the DHT finds the key again
    /// anyway. 1,024 keys: the claims channel's own size.
    fn default() -> BusyRetry {
        BusyRetry {
            base: Duration::from_secs(2),
            cap: Duration::from_secs(60),
            attempts: 5,
            capacity: 1024,
        }
    }
}

impl Config {
    /// A worker's defaults around `fetcher`.
    pub fn new(worker: WorkerId, fetcher: Arc<dyn Fetcher>) -> Config {
        Config {
            worker,
            fetcher,
            pool: PoolConfig {
                max_sockets: MAX_SOCKETS,
                max_held_bytes: HELD_BYTES,
                ..PoolConfig::default()
            },
            compression_level: StoreConfig::default().compression_level,
            outbound_bytes: OUTBOUND_BYTES,
            allow_private: false,
            drain_deadline: DRAIN_DEADLINE,
            seed: None,
            seal_wait: SEAL_WAIT,
            dht: None,
            dht_linger: super::dht::LINGER,
            stats_every: STATS_EVERY,
            reconnect_debounce: RECONNECT_DEBOUNCE,
            retry_every: POLL_EVERY,
            busy_retry: BusyRetry::default(),
            spool: None,
        }
    }
}

impl std::fmt::Debug for Config {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Config")
            .field("worker", &self.worker)
            .finish_non_exhaustive()
    }
}

/// What the runtime has done, for heartbeat stats (WK-10c) and tests.
#[derive(Debug, Default)]
pub struct Counts {
    /// Keys found here.
    pub found: AtomicU64,
    /// "Maybe stored" sightings dropped without a claim (`rediscovered`
    /// in the worker's counters; it includes the filter's false
    /// positives, about 1–2 %).
    pub skipped: AtomicU64,
    /// "Maybe stored" sightings claimed all the same (one in
    /// [`REDISCOVERY_SAMPLE`]).
    pub sampled: AtomicU64,
    /// Results that waited for a fresh snapshot, and those let go after
    /// [`SEAL_WAIT`].
    pub waited: AtomicU64,
    pub released: AtomicU64,
    /// Finds not claimed because sealing was down.
    pub held_back: AtomicU64,
    pub claimed: AtomicU64,
    /// `Busy` denials claimed again, keys dropped after the last attempt,
    /// and keys dropped because the retry was full (X-wk-busy-retry).
    pub busy_retried: AtomicU64,
    pub busy_gave_up: AtomicU64,
    pub busy_dropped: AtomicU64,
    pub granted: AtomicU64,
    /// Grants of a lease this worker already held under the same token
    /// (its own claim answered with it): not counted in `granted`, and
    /// never a new sighting (X-wk-own-lease-lost).
    pub held_again: AtomicU64,
    /// Results sent, and accepted by the coordinator.
    pub sent: AtomicU64,
    pub committed: AtomicU64,
    /// Losses queued as outcomes.
    pub lost: AtomicU64,
    /// Peer hints refused by the network policy.
    pub hints_refused: AtomicU64,
    /// Snapshots that didn't decode or install.
    pub snapshot_errors: AtomicU64,
    /// Fresh sessions asked for, and asks the debounce held back.
    pub reconnects: AtomicU64,
    pub reconnects_debounced: AtomicU64,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

fn bump(n: &AtomicU64) {
    n.fetch_add(1, Ordering::Relaxed);
}

/// Losses go out as outcomes. A remote worker can't tell whether its token
/// is still current, so it says yes and the coordinator fences (WK-7). The
/// queue is never dropped from: its length is bounded by the leases held.
struct Losses {
    tx: mpsc::UnboundedSender<OutcomeItem>,
    counts: Arc<Counts>,
}

impl Reports for Losses {
    fn lost(&self, key: &InfoHash, token: Token, loss: Loss) -> bool {
        bump(&self.counts.lost);
        let _ = self.tx.send(OutcomeItem {
            key: key.0,
            token: token.0,
            outcome: Outcome::Lost { reason: loss as u8 },
        });
        true
    }
}

/// The snapshots being received.
struct Snapshots {
    filter: Incoming,
    dict: Incoming,
    /// The dictionary received, if any.
    dictionary: Option<DictSnapshot>,
    /// Filter installs asked for, so a slow one never replaces a newer one.
    asked: u64,
}

/// The pool and the prepare stage, once running.
struct Stages {
    stop: watch::Sender<bool>,
    pool: JoinHandle<Vec<QueuedKey>>,
    prepare: JoinHandle<()>,
    /// The forwarder into the pool, and the task that tells it to pause.
    forward: JoinHandle<()>,
    pauser: JoinHandle<()>,
}

/// What the tasks and the message loop share.
struct Shared {
    config: Config,
    have: WorkerHave,
    judge: Arc<Judge>,
    leases: Arc<WorkerLeases>,
    counts: Arc<Counts>,
    session: watch::Sender<Option<SessionHandle>>,
    assignment: Mutex<Option<Assignment>>,
    snapshots: Mutex<Snapshots>,
    /// Serializes filter installs (blocking threads).
    install: Mutex<u64>,
    stages: Mutex<Option<Stages>>,
    /// The outbound queue's sending end, until the stages take it.
    out: Mutex<Option<ByteSender<SealedRecord>>>,
    /// Into the pool: grants, re-grants, hints. Bounded by the leases held
    /// (the pool's cap), and never dropped.
    into_pool: mpsc::UnboundedSender<Discovery>,
    pool_rx: Mutex<Option<mpsc::UnboundedReceiver<Discovery>>>,
    /// Bounded (#599's review): a lost acknowledgement stalls that
    /// transfer, which the coordinator starts over, and a lost `Accepted`
    /// leaves its batch to be sent again and answered again.
    acks: mpsc::Sender<SnapshotAck>,
    accepted: mpsc::Sender<(u64, bool)>,
    losses: Arc<Losses>,
    draining: AtomicBool,
    started: watch::Sender<bool>,
    /// The installed snapshot's wire-key day (`u32::MAX` before one), for
    /// "can a record be sealed now?"; it changes on every install.
    key_day: watch::Sender<u32>,
    sampler: Mutex<u64>,
    /// The pool's controls, once it runs: paused while a result waits for
    /// a key it can be sealed under, so no fetch is spent meanwhile, and
    /// while no session is up ([`Shared::refresh_pause`]).
    pool: Mutex<Option<lodestar_sources::PoolHandle>>,
    /// Whether the pool should pause ([`Shared::refresh_pause`]); one task
    /// per stage run applies the latest value to the pool, so no caller
    /// waits on the pool's command channel (#601's review).
    pool_paused: watch::Sender<bool>,
    /// This worker's per-peer-IP share for the pool (WK-10e), from the
    /// current assignment; the pause task applies each change.
    politeness: watch::Sender<Politeness>,
    /// What the pool was last told (`apply_pause`).
    pool_told: watch::Sender<bool>,
    /// The share the pool was last told (`apply_pause`).
    share_told: watch::Sender<Politeness>,
    /// Results waiting for a sealable key.
    waiting: std::sync::atomic::AtomicUsize,
    /// A new session's first `Assign` applies whatever its revision: a
    /// coordinator whose clock went back numbers its plans lower (WK-5).
    fresh_session: AtomicBool,
    /// When sealing went down (no installed key covers now): results wait
    /// until [`SEAL_WAIT`] after this, then go at once. Cleared once a key
    /// covers now again: by an install, by a result that seals, or by the
    /// next look ([`Shared::sealing_down`], from `sift` and the claims).
    unsealable_since: Mutex<Option<std::time::Instant>>,
    /// Set when a stage task ended by itself ([`supervise`]).
    failed: watch::Sender<bool>,
    /// Bumped to ask the session for a fresh one ([`Shared::ask_reconnect`]).
    reconnect: watch::Sender<u64>,
    last_reconnect: Mutex<Option<std::time::Instant>>,
    /// The public address the last `Hello` carried.
    hello_ip: Mutex<Option<std::net::IpAddr>>,
    /// Asks the session task to send everything it has now (a drain).
    flush: mpsc::UnboundedSender<tokio::sync::oneshot::Sender<()>>,
    /// What the DHT should run ([`want_dht`]).
    /// Taken (closed) at a stop, which ends the DHT's task once it has
    /// stopped its node: the task doesn't wait for this to be dropped.
    dht_want: Mutex<Option<watch::Sender<super::dht::Desired>>>,
    /// The pool's peer requests, to the DHT; `None` with no DHT.
    peer_requests: Option<mpsc::Sender<lodestar_sources::PeerRequest>>,
    dht_counts: Arc<super::dht::DhtCounts>,
    /// The spool (WK-11), if configured.
    spool: Option<Arc<WorkerSpool>>,
    /// Bumped just before each session change, so the session task
    /// notices one it missed while busy (a watch can change twice between
    /// looks).
    generation: AtomicU64,
}

/// The runtime (see the module docs).
pub struct Runtime {
    shared: Arc<Shared>,
    #[cfg_attr(not(test), allow(dead_code))]
    found: mpsc::Sender<Found>,
    tasks: Vec<JoinHandle<()>>,
    /// The session task, which owns the outbound queue's receiving end.
    results: Option<JoinHandle<()>>,
    /// The outcome task; it hands its queue back when stopped.
    reporter: Option<(watch::Sender<bool>, JoinHandle<Vec<OutcomeItem>>)>,
    /// The running DHT node's shard.
    dht_running: Option<super::dht::Running>,
}

impl std::fmt::Debug for Runtime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Runtime")
            .field("worker", &self.shared.config.worker)
            .field("started", &*self.shared.started.borrow())
            .finish_non_exhaustive()
    }
}

/// Where keys found on this worker go (its DHT, WK-10c; tests).
#[derive(Debug, Clone)]
pub struct Finds(mpsc::Sender<Found>);

// WK-10c's DHT sends its finds here and reads the counters for the
// heartbeat; tests do today.
#[cfg_attr(not(test), allow(dead_code))]
impl Finds {
    /// Queues a key to check and claim; `false` when the queue is full (the
    /// key is dropped: it will be found again).
    pub fn found(&self, found: Found) -> bool {
        self.0.try_send(found).is_ok()
    }
}

#[cfg_attr(not(test), allow(dead_code))]
impl Runtime {
    /// Starts the runtime's tasks; the stages start once the snapshots are
    /// in (see the module docs). Must be called in a Tokio runtime.
    pub fn start(config: Config) -> Runtime {
        let (into_pool, pool_rx) = mpsc::unbounded_channel();
        let (acks, ack_rx) = mpsc::channel(ACK_QUEUE);
        let (accepted, accepted_rx) = mpsc::channel(ACCEPTED_QUEUE);
        let (loss_tx, loss_rx) = mpsc::unbounded_channel();
        let (found, found_rx) = mpsc::channel(FOUND_QUEUE);
        let (flush, flush_rx) = mpsc::unbounded_channel();
        let (out, outbound) = byte_queue::<SealedRecord>(config.outbound_bytes, 4096);
        let counts = Arc::new(Counts::default());
        let dht_counts = Arc::new(super::dht::DhtCounts::default());
        let (dht_want, want_rx) = watch::channel(super::dht::Desired::Stop);
        let mut dht = None;
        let mut peer_requests = None;
        if let Some(base) = config.dht.clone() {
            let (tx, rx) = mpsc::channel(lodestar_dht::node::PEER_REQUEST_CAPACITY);
            peer_requests = Some(tx);
            dht = Some(super::dht::spawn(
                base,
                want_rx,
                found.clone(),
                rx,
                dht_counts.clone(),
                config.dht_linger,
            ));
        }
        // Opened by the session task, on a blocking thread.
        let spool = config
            .spool
            .clone()
            .map(|c| Arc::new(WorkerSpool::new(c, config.worker.0)));
        let shared = Arc::new(Shared {
            spool,
            generation: AtomicU64::new(0),
            have: WorkerHave::new(),
            judge: Arc::new(Judge::new(config.worker)),
            leases: Arc::new(WorkerLeases::tracking_out(out_horizon(&config))),
            counts: counts.clone(),
            session: watch::channel(None).0,
            assignment: Mutex::new(None),
            snapshots: Mutex::new(Snapshots {
                filter: Incoming::new(SnapshotKind::Filter),
                dict: Incoming::new(SnapshotKind::Dictionary),
                dictionary: None,
                asked: 0,
            }),
            install: Mutex::new(0),
            stages: Mutex::new(None),
            out: Mutex::new(Some(out)),
            into_pool,
            pool_rx: Mutex::new(Some(pool_rx)),
            acks,
            accepted,
            losses: Arc::new(Losses {
                tx: loss_tx,
                counts,
            }),
            draining: AtomicBool::new(false),
            started: watch::channel(false).0,
            key_day: watch::channel(u32::MAX).0,
            sampler: Mutex::new(config.seed.unwrap_or_else(super::session::random_jitter)),
            pool: Mutex::new(None),
            pool_paused: watch::channel(false).0,
            politeness: watch::channel(Politeness::ALONE).0,
            share_told: watch::channel(Politeness::ALONE).0,
            pool_told: watch::channel(false).0,
            failed: watch::channel(false).0,
            reconnect: watch::channel(0).0,
            last_reconnect: Mutex::new(None),
            hello_ip: Mutex::new(None),
            waiting: std::sync::atomic::AtomicUsize::new(0),
            fresh_session: AtomicBool::new(false),
            flush,
            unsealable_since: Mutex::new(None),
            dht_want: Mutex::new(Some(dht_want)),
            peer_requests,
            dht_counts,
            config,
        });
        let (stop_reports, reports_stopped) = watch::channel(false);
        let (dht_task, dht_running) = dht.unzip();
        let tasks = vec![
            tokio::spawn(send_acks(shared.clone(), ack_rx)),
            tokio::spawn(claim(shared.clone(), found_rx)),
            tokio::spawn(heartbeat_stats(shared.clone())),
            tokio::spawn(supervise(shared.clone())),
        ];
        // The DHT's task isn't aborted with the others: a stop (or a drop)
        // closes its `want`, and it ends after stopping its node, which
        // saves the node's state ([`super::dht::stopped`]).
        drop(dht_task);
        Runtime {
            tasks,
            dht_running,
            results: Some(tokio::spawn(send_results(
                shared.clone(),
                outbound,
                accepted_rx,
                flush_rx,
            ))),
            reporter: Some((
                stop_reports,
                tokio::spawn(report(shared.clone(), loss_rx, reports_stopped)),
            )),
            found,
            shared,
        }
    }

    pub fn finds(&self) -> Finds {
        Finds(self.found.clone())
    }

    /// The leases held and the results out under them (tests).
    #[cfg(test)]
    pub(crate) fn leases(&self) -> Arc<lodestar_pipeline::stages::WorkerLeases> {
        self.shared.leases.clone()
    }

    pub fn counts(&self) -> Arc<Counts> {
        self.shared.counts.clone()
    }

    /// The spool, if one is configured and opened (WK-11).
    pub fn spool(&self) -> Option<Arc<WorkerSpool>> {
        self.shared.spool.clone()
    }

    /// The DHT's counters.
    pub fn dht_counts(&self) -> Arc<super::dht::DhtCounts> {
        self.shared.dht_counts.clone()
    }

    /// The running DHT node's shard (`None` with no DHT configured).
    pub fn dht_running(&self) -> Option<super::dht::Running> {
        self.dht_running.clone()
    }

    /// Completes if the pool or the prepare stage ends by itself (a bug):
    /// the worker can't fetch or prepare any more, so `lodestar worker`
    /// stops with an error, and its service manager starts it again. The
    /// coordinator re-grants the keys it held when their leases end.
    pub fn failed(&self) -> impl std::future::Future<Output = ()> + Send + 'static {
        let mut rx = self.shared.failed.subscribe();
        async move {
            let _ = rx.wait_for(|f| *f).await;
        }
    }

    /// Whether the stages run: the snapshots are in.
    pub fn is_started(&self) -> bool {
        *self.shared.started.borrow()
    }

    /// The installed filter snapshot's version (0: none yet).
    pub fn filter_version(&self) -> u64 {
        self.shared.judge.version()
    }

    /// The assignment applied, if any.
    pub fn assignment(&self) -> Option<Assignment> {
        *lock(&self.shared.assignment)
    }

    /// Stops the stages within the drain deadline: the pool hands over
    /// what's finished (its own 5 s drain), then the stages are stopped
    /// whether or not the session took it all. Keys still held go back to
    /// the coordinator when their leases end.
    async fn stop_stages(&self) {
        self.shared.draining.store(true, Ordering::Release);
        want_dht(&self.shared);
        let Some(stages) = lock(&self.shared.stages).take() else {
            return;
        };
        let _ = stages.stop.send(true);
        let deadline = self.shared.config.drain_deadline;
        let Stages {
            pool,
            prepare,
            forward,
            pauser,
            ..
        } = stages;
        forward.abort();
        pauser.abort();
        let (pool_abort, prepare_abort) = (pool.abort_handle(), prepare.abort_handle());
        let both = async {
            let _ = pool.await;
            let _ = prepare.await;
        };
        if tokio::time::timeout(deadline, both).await.is_err() {
            // A stalled session: the prepare stage is waiting on the
            // outbound queue, or a fetch on its peer. Both stop here, and
            // their keys' leases go back to the coordinator when they end.
            pool_abort.abort();
            prepare_abort.abort();
        }
    }

    /// Stops everything (a local shutdown) within the drain deadline.
    pub async fn shutdown(mut self) {
        self.stop_stages().await;
        // The DHT saves its state as it stops (within its own deadline).
        close_dht(&self.shared);
        if let Some(running) = self.dht_running.clone() {
            super::dht::stopped(running).await;
        }
        self.abort_all();
    }

    fn abort_all(&mut self) {
        for t in self.tasks.drain(..) {
            t.abort();
        }
        if let Some(r) = self.results.take() {
            r.abort();
        }
        if let Some((stop, r)) = self.reporter.take() {
            let _ = stop.send(true);
            r.abort();
        }
    }

    /// The outcomes not reported yet, once the reporter is stopped.
    async fn take_outcomes(&mut self) -> Vec<OutcomeItem> {
        let Some((stop, task)) = self.reporter.take() else {
            return Vec::new();
        };
        let _ = stop.send(true);
        tokio::time::timeout(self.shared.config.drain_deadline, task)
            .await
            .ok()
            .and_then(Result::ok)
            .unwrap_or_default()
    }

    /// Applies one event from the session's message loop (in order).
    fn handle(&mut self, event: WorkerEvent) {
        let s = &self.shared;
        match event {
            WorkerEvent::Assign(a) => {
                // A payload that doesn't read changes nothing.
                if let Ok(Some(new)) = Assignment::from_message(&a) {
                    let mut current = lock(&s.assignment);
                    // Only a newer revision, except a new session's first
                    // (its coordinator's clock may have gone back, WK-5).
                    let fresh = s.fresh_session.swap(false, Ordering::AcqRel);
                    if fresh || current.is_none_or(|c| new.revision > c.revision) {
                        *current = Some(new);
                        // The pool's share follows at once (WK-10e); a
                        // payload with an invalid one never decoded.
                        s.politeness.send_if_modified(|p| {
                            std::mem::replace(p, new.politeness) != new.politeness
                        });
                    }
                }
                start_if_ready(s);
                want_dht(s);
                // The pool waits for a new session's first `Assign`, as the
                // DHT does.
                s.refresh_pause();
            }
            WorkerEvent::Regranted(items) => {
                for item in items {
                    if let GrantItem::Granted { key, token, .. } = item {
                        granted(s, InfoHash(key), Token(token), Vec::new(), 0);
                    }
                }
            }
            WorkerEvent::Hints(items) => hints(s, items),
            WorkerEvent::Accepted { batch, accepted } => {
                let _ = s.accepted.try_send((batch, accepted));
            }
            WorkerEvent::Snapshot(c) => snapshot_chunk(s, c),
            WorkerEvent::HaveSnapshot(b) => {
                if s.have.header(&b).is_err() {
                    bump(&s.counts.snapshot_errors);
                }
            }
            WorkerEvent::HaveDelta(b) => {
                if let (_, Some(ack)) = s.have.delta(&b) {
                    let _ = s.acks.try_send(ack);
                }
            }
            // Re-verification is WK-13's; the session handles the rest.
            WorkerEvent::Reverify(_)
            | WorkerEvent::Drain
            | WorkerEvent::ClockSkewed { .. }
            | WorkerEvent::Ended(_) => {}
        }
    }
}

impl Work for Runtime {
    /// What `Hello` reports (WK-10c): the memory this process may use and
    /// the public address the DHT last voted. The session adds the cores.
    fn capabilities(&self) -> lodestar_cluster::proto::Capabilities {
        let ip = self.shared.dht_counts.external_ip();
        *lock(&self.shared.hello_ip) = ip;
        lodestar_cluster::proto::Capabilities {
            ram_mb: super::budget::ram_mb(),
            external_ips: ip.into_iter().collect(),
            ..Default::default()
        }
    }

    /// A changed public address, or a snapshot that didn't decode, asks
    /// for a fresh session (debounced, [`RECONNECT_DEBOUNCE`]).
    fn reconnects(&self) -> Option<watch::Receiver<u64>> {
        Some(self.shared.reconnect.subscribe())
    }

    fn connected(&mut self, session: SessionHandle) {
        // Anything queued to the last session may be lost: ask for a fresh
        // have-filter snapshot (WK-3). The coordinator starts the filter
        // and dictionary snapshots over by itself (WK-10a).
        let _ = self.shared.acks.try_send(self.shared.have.reconnected());
        self.shared.fresh_session.store(true, Ordering::Release);
        // Before the session itself: the session task reads the session,
        // then the generation, so once it sees this session it sees this
        // bump too, and resets once for it, never again a moment later.
        self.shared.generation.fetch_add(1, Ordering::AcqRel);
        self.shared.session.send_replace(Some(session));
        want_dht(&self.shared);
        self.shared.refresh_pause();
    }

    fn disconnected(&mut self) {
        self.shared.generation.fetch_add(1, Ordering::AcqRel);
        self.shared.session.send_replace(None);

        // The pool opens no new connection, then the DHT pauses: the
        // coordinator re-plans without this worker and widens the others'
        // shares (WK-5); the DHT stops if no session is back within its
        // linger. The pool goes first so it isn't still asking a paused
        // DHT, whose queued lookups would finish empty and each cost a key
        // an attempt (#601's review).
        let s = self.shared.clone();
        s.refresh_pause();
        tokio::spawn(async move {
            if *s.started.borrow() {
                let mut told = s.pool_told.subscribe();
                // Bounded: a session back meanwhile leaves the pool running.
                let _ =
                    tokio::time::timeout(Duration::from_secs(1), told.wait_for(|paused| *paused))
                        .await;
            }
            want_dht(&s);
        });
    }

    fn event(&mut self, event: WorkerEvent) {
        self.handle(event);
    }

    /// Takes no new keys, lets the pool hand over what's finished (within
    /// the deadline), and returns the losses not reported yet. Results the
    /// session task sent are committed by the coordinator; everything else
    /// goes back with `Drained`.
    /// Once a spool replay has drained (WK-11): the leases this worker
    /// holds, to compare with the coordinator's count.
    fn reconcile(&mut self) -> Option<u32> {
        let sp = self.shared.spool.as_ref()?;
        sp.take_reconcile()
            .then(|| u32::try_from(self.shared.leases.len()).unwrap_or(u32::MAX))
    }

    fn reconciled(&mut self, worker: u32, coordinator: u32) {
        if let Some(sp) = &self.shared.spool {
            sp.reconcile(worker, coordinator);
        }
    }

    /// The leases still held, keys and results out (WK-11b). None while
    /// the spool isn't open yet or still has results to replay
    /// (CLUSTER-01): their leases
    /// aren't in `WorkerLeases` until their commit, so a list now would
    /// give them back and turn each replayed result into a late one. The
    /// replay draining asks for the comparison, and the new session's list
    /// goes out then.
    fn holding(&mut self) -> Option<Vec<lodestar_cluster::proto::HeldLease>> {
        if self
            .shared
            .spool
            .as_ref()
            .is_some_and(|sp| sp.replay_pending())
        {
            return None;
        }
        let list = self.shared.leases.tokens().into_iter();
        Some(
            list.map(|(key, token)| lodestar_cluster::proto::HeldLease {
                key: key.0,
                token: token.0,
            })
            .collect(),
        )
    }

    fn listed(&mut self, answer: lodestar_cluster::proto::ReconcileAnswer) {
        if let Some(sp) = &self.shared.spool {
            sp.listed(answer);
        }
    }

    /// A local stop with no session (CLUSTER-05): the stages stop as in a
    /// drain, and what's fetched and sealed goes to the spool, for the
    /// next start to send. Outcomes not reported can't be: their leases go
    /// back by the TTL, or at the next start's reconcile (CLUSTER-01).
    async fn stop(&mut self) {
        let started = std::time::Instant::now();
        self.stop_stages().await;
        self.flush(started).await;
    }

    async fn drain(&mut self) -> Vec<OutcomeItem> {
        let started = std::time::Instant::now();
        self.stop_stages().await;
        // What's fetched and sealed goes out before `Drained`, within what's
        // left of the deadline: results travel on the same command channel,
        // so the coordinator takes them first (no `Accepted` is awaited).
        self.flush(started).await;
        self.take_outcomes().await
    }
}

impl Runtime {
    /// Every result queued goes out on the session, or to the spool with
    /// none, within what's left of the drain deadline since `started`.
    async fn flush(&self, started: std::time::Instant) {
        let left = self
            .shared
            .config
            .drain_deadline
            .saturating_sub(started.elapsed());
        let (done, flushed) = tokio::sync::oneshot::channel();
        if self.shared.flush.send(done).is_ok() {
            let _ = tokio::time::timeout(left, flushed).await;
        }
    }
}

impl Drop for Runtime {
    fn drop(&mut self) {
        // A local stop drops the runtime (`Worker::run` owns it): everything
        // stops at once, and the keys' leases go back to the coordinator. A
        // late install on a blocking thread then can't start stages nobody
        // owns.
        self.shared.draining.store(true, Ordering::Release);
        // `Stop` first, then closed: a `Run` the DHT's task hadn't seen yet
        // can't start a node during the teardown (#601's delta review).
        want_dht(&self.shared);
        close_dht(&self.shared);
        if let Some(stages) = lock(&self.shared.stages).take() {
            let _ = stages.stop.send(true);
            stages.prepare.abort();
            stages.pool.abort();
            stages.forward.abort();
            stages.pauser.abort();
        }
        self.abort_all();
    }
}

/// A key granted to this worker: its token into the leases, the key into
/// the pool (queued until the stages run).
///
/// **The holder's own lease coming back** (X-wk-own-lease-lost): a claim
/// for a key this worker already holds is answered with its own lease, the
/// same token. That's no new grant: the key is in the pool already, or the
/// pool has fetched it and its result is on its way. Passed on as a new
/// sighting, a pool that fetched it lately would turn it away as a
/// duplicate, which ends the lease as lost, and another worker would
/// harvest and fetch the key again. So the lease stays as it is, the grant
/// isn't counted, and its peers go in as a reply, as hints do: they join a
/// key still waiting for peers, and a key the pool is done with ignores
/// them (a reply never ends a lease).
fn granted(s: &Shared, key: InfoHash, token: Token, peers: Vec<std::net::SocketAddr>, at: i64) {
    // Held already, or its result is on its way out under this token:
    // one answer from one lock (`WorkerLeases::insert`).
    let held_again = s.leases.insert(key, token) != lodestar_pipeline::stages::Insert::New;
    if held_again {
        bump(&s.counts.held_again);
    } else {
        bump(&s.counts.granted);
    }
    if held_again && peers.is_empty() {
        return;
    }
    let mut found = Found::key(key, 0);
    found.peers = peers;
    let _ = s.into_pool.send(Discovery {
        source: SourceId::new(DHT_SOURCE).expect("a source id"),
        found,
        at: if held_again { 0 } else { at },
        reply: held_again,
    });
}

/// Peer hints for keys this worker holds (the session passes only those
/// whose token is the lease it holds). Only peers the network policy
/// allows reach the pool (§18.7: in memory only).
fn hints(s: &Shared, items: Vec<HintItem>) {
    for item in items {
        let key = InfoHash(item.key);
        let total = item.hints.len();
        let peers: Vec<_> = item
            .hints
            .into_iter()
            .take(MAX_HINTS)
            .filter(|a| lodestar_network_policy::allows(*a, s.config.allow_private))
            .collect();
        s.counts
            .hints_refused
            .fetch_add((total - peers.len()) as u64, Ordering::Relaxed);
        if peers.is_empty() {
            continue;
        }
        let mut found = Found::key(key, 0);
        found.peers = peers;
        let _ = s.into_pool.send(Discovery {
            source: SourceId::new(DHT_SOURCE).expect("a source id"),
            found,
            at: 0,
            reply: true,
        });
    }
}

/// How long a result's lease is remembered once its result is out
/// (X-wk-own-lease-lost): a lease TTL, plus the longest a result may wait
/// to be sealed. A result still out past that (a batch the coordinator
/// keeps refusing, a spool replayed later) is forgotten, and so is every
/// entry at a worker restart; its own lease coming back then counts as new
/// again, as before the fix.
///
/// The TTL is the protocol's default, not the one the coordinator states.
/// `Welcome` carries the TTL it enforces (`lease_ttl_secs`), but only the
/// session sees it (`WorkerSession::welcome`): `Work::connected` hands the
/// runtime a `SessionHandle`, and the leases are built when the runtime
/// starts, before any session. The TTL enforced is the coordinator's lease
/// authority's (`ClusterServer::with_hooks` takes it from there, built from
/// the pipeline's `LEASE_TTL_SECS`); a test in `lodestar-pipeline`
/// (`the_protocols_lease_ttl_is_the_one_enforced`) fails if the two ever
/// differ.
fn out_horizon(config: &Config) -> Duration {
    let ttl = lodestar_cluster::ServerConfig::default().lease_ttl_secs;
    Duration::from_secs(ttl.unsigned_abs()) + config.seal_wait
}

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}

/// One filter or dictionary chunk; the have-filter's go to it.
fn snapshot_chunk(s: &Arc<Shared>, c: SnapshotChunk) {
    if c.kind == SnapshotKind::HaveFilter {
        if let Some(ack) = s.have.chunk(&c) {
            let _ = s.acks.try_send(ack);
        }
        return;
    }
    let kind = c.kind;
    let step = {
        let mut snaps = lock(&s.snapshots);
        let incoming = match kind {
            SnapshotKind::Filter => &mut snaps.filter,
            _ => &mut snaps.dict,
        };
        incoming.take(c)
    };
    let complete = match step {
        Ok(Step::Ack(ack)) => {
            let _ = s.acks.try_send(ack);
            return;
        }
        Ok(Step::Done(ack, complete)) => {
            let _ = s.acks.try_send(ack);
            complete
        }
        // Dropped; the coordinator starts it over after its stall.
        Err(_) => {
            bump(&s.counts.snapshot_errors);
            return;
        }
    };
    let asked = {
        let mut snaps = lock(&s.snapshots);
        if kind == SnapshotKind::Filter {
            snaps.asked += 1;
        }
        snaps.asked
    };
    let s = s.clone();
    // Decoding builds the engine: up to about 96 MiB at the caps. One at a
    // time, under the install lock, and a transfer a newer one has already
    // followed is skipped before it's built, so two never peak together.
    tokio::task::spawn_blocking(move || {
        let mut complete = complete;
        let stale = |asked: u64| asked < lock(&s.snapshots).asked;
        let ok = match kind {
            // A transfer a newer one has already followed is dropped before
            // it waits on the lock, so big payloads can't pile up there.
            SnapshotKind::Filter if stale(asked) => true,
            SnapshotKind::Filter => {
                let mut done = lock(&s.install);
                if *done >= asked || stale(asked) {
                    true
                } else if complete.version == s.judge.version() {
                    // The same snapshot again (a reconnect): nothing to build.
                    *done = asked;
                    true
                } else {
                    match complete.filter() {
                        Ok(snapshot) => {
                            *done = asked;
                            let _notes = s.judge.install(&snapshot, complete.version);
                            // Results waiting for a sealable key look again.
                            s.key_day.send_replace(snapshot.key.id);
                            // A key that covers now ends the outage at once,
                            // not at the next result or find (#599's review).
                            s.sealing_down(unix_now());
                            true
                        }
                        Err(_) => false,
                    }
                }
            }
            _ => match complete.dict() {
                Ok(d) => {
                    lock(&s.snapshots).dictionary = Some(d);
                    true
                }
                Err(_) => false,
            },
        };
        // The filter snapshot's bytes hold the wire key: overwritten once
        // used or skipped, as WK-8's `Transfer` and `WireKey` are.
        complete.bytes.fill(0);
        std::sync::atomic::compiler_fence(Ordering::SeqCst);
        if !ok {
            bump(&s.counts.snapshot_errors);
            // Its last chunk was acknowledged, so the coordinator counts it
            // delivered: said, so a stall until the next version is seen.
            // A fresh session starts every snapshot over (WK-10a).
            s.ask_reconnect();
            lodestar_log::log_event!(
                crate::logging::events::WORKER_SNAPSHOT_FAILED,
                kind = if kind == SnapshotKind::Filter {
                    "filter"
                } else {
                    "dictionary"
                }
            );
        }
        start_if_ready(&s);
    });
}

/// Starts the stages once a filter snapshot is installed, an assignment
/// with fetching is applied, and the dictionary it names is in. The
/// preparer is fixed then: a later dictionary takes effect at the next
/// start (the coordinator keeps the old ones, so its blobs still open).
fn start_if_ready(s: &Arc<Shared>) {
    if *s.started.borrow() || s.draining.load(Ordering::Acquire) {
        return;
    }
    // Subscribed before the assignment is read, so a share that changes
    // after the pool is built still wakes the pause task (#617's review).
    let share = s.politeness.subscribe();
    let Some(a) = *lock(&s.assignment) else {
        return;
    };
    if !a.fetch || !a.prepare || s.judge.version() == 0 {
        return;
    }
    let dict = {
        let snaps = lock(&s.snapshots);
        match (&snaps.dictionary, a.dict_version) {
            (_, 0) => None,
            (Some(d), v) if d.version() == v => Some((d.id, d.bytes.clone())),
            _ => return,
        }
    };
    let mut stages = lock(&s.stages);
    // Checked again under the lock: a drain that took the stages before
    // this ran must not find them started after it.
    if stages.is_some() || s.draining.load(Ordering::Acquire) {
        return;
    }
    let (Some(out), Some(pool_rx)) = (lock(&s.out).take(), lock(&s.pool_rx).take()) else {
        return;
    };
    // The pool's caps are fixed for the stages' run (the pool has no
    // setters); the DHT's intensity follows each `Assign`.
    let limits = super::budget::Limits::of(&a);
    let (discoveries, rx) = mpsc::channel(256);
    let forward = tokio::spawn(forward(pool_rx, discoveries));
    let (stop, shutdown): (watch::Sender<bool>, Shutdown) = Shutdown::new();
    let reports: Arc<dyn Reports> = s.losses.clone();
    let running = WorkerStages {
        worker: s.config.worker,
        pool: PoolConfig {
            max_pending: usize::try_from(limits.pool_cap).unwrap_or(usize::MAX),
            max_sockets: limits.max_sockets.min(s.config.pool.max_sockets),
            // From its first dial (WK-10e); later changes by the pause task.
            ip_cap: lodestar_sources::IpCap(Some(Arc::new(ShareCap(a.politeness)))),
            ..s.config.pool.clone()
        },
        fetcher: s.config.fetcher.clone(),
        peer_finders: s.peer_requests.iter().cloned().collect(),
        weights: Default::default(),
        restore: Vec::new(),
        preparer: Preparer::from_dict(dict, s.config.compression_level, false),
        filter: s.judge.filter(),
        reports: reports.clone(),
        leases: s.leases.clone(),
        funnel: Arc::new(FunnelCounts::default()),
        gate: Arc::new(GateStats::new(0)),
        stats: Arc::default(),
        adult: AdultSources::default(),
        queues: Queues::worker(),
    }
    .spawn(
        rx,
        shutdown,
        Sealing {
            shared: s.clone(),
            reports,
            out,
        },
    );
    let pauser = tokio::spawn(apply_pause(
        running.pool.clone(),
        s.pool_paused.subscribe(),
        s.pool_told.clone(),
        Share {
            built_with: a.politeness,
            latest: share,
            told: s.share_told.clone(),
        },
    ));
    *lock(&s.pool) = Some(running.pool);
    *stages = Some(Stages {
        stop,
        pool: running.pool_task,
        prepare: running.prepare,
        forward,
        pauser,
    });
    s.started.send_replace(true);
    want_dht(s);
    // A session may have gone while the stages were built.
    s.refresh_pause();
}

/// What the DHT should do now ([`super::dht`]): run on the assignment's
/// shard while a session is up and has sent its first `Assign`, the
/// stages run and nothing drains; pause while a session is missing or
/// its first `Assign` hasn't come; otherwise stop.
///
/// Worked out under the watch's write lock, so callers on different
/// threads (the message loop, an install's blocking thread) can't leave
/// an older answer last.
fn want_dht(s: &Shared) {
    use super::dht::{Desired, Want};
    let want = || {
        if s.draining.load(Ordering::Acquire) || !*s.started.borrow() {
            return Desired::Stop;
        }
        let Some(a) = *lock(&s.assignment) else {
            return Desired::Stop;
        };
        let Some(shard) = a.dht else {
            return Desired::Stop;
        };
        let up = s.session.borrow().as_ref().is_some_and(|h| !h.is_closed());
        // A full spool keeps only the routing too (WK-11).
        if !up || s.fresh_session.load(Ordering::Acquire) || s.spool_full() {
            return Desired::Pause;
        }
        Desired::Run(Want {
            shard,
            intensity: super::budget::Limits::of(&a).intensity,
            sample_scale: a.politeness.bep51_interval_scale,
        })
    };
    if let Some(tx) = lock(&s.dht_want).as_ref() {
        tx.send_if_modified(|w| {
            let now = want();
            let changed = *w != now;
            *w = now;
            changed
        });
    }
}

/// Ends the DHT's task: its node stops (and saves) as it ends.
fn close_dht(s: &Shared) {
    drop(lock(&s.dht_want).take());
}

/// What to do with a key found here (runner, 2026-11-07T20:34Z):
/// - before a have-filter is complete, claim it ("maybe": the coordinator
///   answers from the store);
/// - once one is, claim a "not stored";
/// - drop a "maybe stored", except one sighting in [`REDISCOVERY_SAMPLE`],
///   by the per-sighting `draw`, so a false positive isn't lost for good.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Sighting {
    Claim,
    Sampled,
    Drop,
}

fn rediscovery(ready: bool, maybe: bool, draw: u64) -> Sighting {
    match (ready, maybe) {
        (true, true) if draw.is_multiple_of(REDISCOVERY_SAMPLE) => Sighting::Sampled,
        (true, true) => Sighting::Drop,
        _ => Sighting::Claim,
    }
}

impl Shared {
    /// The next draw (SplitMix64): cheap, and seeded per runtime.
    fn draw(&self) -> u64 {
        let mut state = lock(&self.sampler);
        *state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = *state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Keys the pool can still take: its cap (the budget's, from the
    /// assignment) less the leases held.
    fn pool_room(&self) -> usize {
        let cap = lock(&self.assignment).map_or(0, |a| super::budget::Limits::of(&a).pool_cap);
        usize::try_from(cap)
            .unwrap_or(usize::MAX)
            .saturating_sub(self.leases.len())
    }

    /// Asks the session for a fresh one, at most once per
    /// `reconnect_debounce`; `false` (and counted) when it's too soon.
    fn ask_reconnect(&self) -> bool {
        let mut last = lock(&self.last_reconnect);
        if last.is_some_and(|at| at.elapsed() < self.config.reconnect_debounce) {
            bump(&self.counts.reconnects_debounced);
            return false;
        }
        *last = Some(std::time::Instant::now());
        bump(&self.counts.reconnects);
        self.reconnect.send_modify(|n| *n += 1);
        true
    }

    /// Whether the assignment lets this worker fetch: `fetch` and
    /// `prepare` both on (a worker can't send results it didn't prepare).
    /// Turned off after the stages started, the pool pauses and nothing is
    /// claimed; turned on again, both resume. The planner sets both on
    /// for every worker today (WK-5).
    fn fetching(&self) -> bool {
        lock(&self.assignment).is_some_and(|a| a.fetch && a.prepare)
    }

    /// Whether sealing is down at `now`: a result found it so, and no key
    /// covers `now` yet. A key that does (a fresh install, or the clock
    /// moving back onto the key's day) ends it.
    fn sealing_down(&self, now: i64) -> bool {
        let mut since = lock(&self.unsealable_since);
        if since.is_some() && self.sealable(now) {
            *since = None;
        }
        since.is_some()
    }

    /// Whether a record can be sealed at `now`: a snapshot is installed and
    /// `now` is on or before its wire key's day (`Judge::judge_at`'s rule).
    fn sealable(&self, now: i64) -> bool {
        let day = *self.key_day.borrow();
        // The judge drops an expired key (WK-10d): with none, nothing seals.
        day != u32::MAX
            && self.judge.version() != 0
            && self.judge.has_key()
            && now.div_euclid(DAY) <= i64::from(day)
    }
}

/// The judge around the outbound queue, as WK-8's `Judging`, except that a
/// result that can't be sealed yet waits for the next snapshot, until
/// [`SEAL_WAIT`] after sealing went down; the pool is paused meanwhile and
/// nothing new is claimed. Past that, every waiting result is released as
/// `GaveUp`, which ends its lease. `Blocked` is only ever the floor's
/// (CS-8a, hash lists), which WK-10c tombstones (runner, #592's B2).
struct Sealing<O> {
    shared: Arc<Shared>,
    reports: Arc<dyn Reports>,
    out: O,
}

impl<O: Outbox<SealedRecord>> Outbox<Results> for Sealing<O> {
    fn send(&self, item: Results) -> impl std::future::Future<Output = Result<(), Closed>> + Send {
        let s = self.shared.clone();
        let reports = self.reports.clone();
        async move {
            if !s.sealable(unix_now()) {
                bump(&s.counts.waited);
                // The wait runs from when sealing went down, once for all.
                let deadline = *lock(&s.unsealable_since)
                    .get_or_insert_with(std::time::Instant::now)
                    + s.config.seal_wait;
                let left = deadline.saturating_duration_since(std::time::Instant::now());
                // Past the wait already: released at once, and the pool isn't
                // paused for it (#599's review), so a backlog behind it goes
                // the same way instead of waiting on a pause.
                if left.is_zero() {
                    release(&s, &reports, item.key, item.token);
                    return Ok(());
                }
                // No fetch is started while a result waits: they'd only
                // spend their keys waiting on the pool's byte budget.
                if s.waiting.fetch_add(1, Ordering::AcqRel) == 0 {
                    s.refresh_pause();
                }
                let mut installs = s.key_day.subscribe();
                let ready = tokio::time::timeout(left, async {
                    while !s.sealable(unix_now()) {
                        // A new install, or the next minute (the clock may
                        // be what changed).
                        let _ =
                            tokio::time::timeout(Duration::from_secs(60), installs.changed()).await;
                    }
                })
                .await;
                if s.waiting.fetch_sub(1, Ordering::AcqRel) == 1 {
                    s.refresh_pause();
                }
                if ready.is_err() {
                    release(&s, &reports, item.key, item.token);
                    return Ok(());
                }
            }
            match s.judge.judge(item) {
                Judged::Send(o) => {
                    // Sealing works again.
                    *lock(&s.unsealable_since) = None;
                    self.out.send(o).await
                }
                // The floor: never sent or spooled (WK-8).
                Judged::Dropped { key, token, .. } => {
                    s.leases.done(&key, token);
                    reports.lost(&key, token, Loss::Blocked);
                    Ok(())
                }
                Judged::Invalid { key, token } => {
                    s.leases.done(&key, token);
                    reports.lost(&key, token, Loss::Invalid);
                    Ok(())
                }
                // The day turned between the check and the judge: released,
                // as one that waited too long.
                Judged::NotReady { key, token } => {
                    release(&s, &reports, key, token);
                    Ok(())
                }
            }
        }
    }
}

/// Lets a result go that can't be sealed: its lease ends with a loss that
/// isn't `Blocked` (`GaveUp`), so the key is never tombstoned and can be
/// claimed again. Unreported, the lease would last as long as this worker
/// heartbeats (runner, #592's B2: "released for a re-grant under a distinct
/// reason that doesn't tombstone").
fn release(s: &Shared, reports: &Arc<dyn Reports>, key: InfoHash, token: Token) {
    bump(&s.counts.released);
    s.leases.done(&key, token);
    reports.lost(&key, token, Loss::GaveUp);
}

impl Shared {
    /// Whether the spool is full (WK-11): fetching pauses, nothing new is
    /// claimed and the DHT keeps only its routing, session or not.
    fn spool_full(&self) -> bool {
        self.spool.as_ref().is_some_and(|sp| sp.is_full())
    }

    /// Pauses the pool, and the DHT with it while the spool is full.
    fn refresh_spool_pause(&self) {
        self.refresh_pause();
        want_dht(self);
    }

    /// Pauses the pool while a result waits for a sealable key or no
    /// session is up, and resumes it otherwise. Worked out from the state
    /// now, under the watch's write lock, so calls in any order leave it
    /// right. Paused, the pool opens no new connection; fetches under way
    /// finish (WK-5: a worker without a session stops opening connections,
    /// since the coordinator re-plans without it).
    fn refresh_pause(&self) {
        self.pool_paused.send_if_modified(|told| {
            let up = self
                .session
                .borrow()
                .as_ref()
                .is_some_and(|h| !h.is_closed());
            // A new session's first `Assign` may re-plan this worker: until
            // it comes, the pool waits, as the DHT does.
            let fresh = self.fresh_session.load(Ordering::Acquire);
            // A full spool pauses it too (WK-11), session or not, until a
            // replay makes room.
            let full = self.spool_full();
            let pause = self.waiting.load(Ordering::Acquire) > 0
                || !up
                || full
                || fresh
                || !self.fetching();
            std::mem::replace(told, pause) != pause
        });
    }
}

/// This worker's per-peer-IP share, as the pool asks for it (WK-10e): 0
/// for a peer IP in another same-IP member's bucket.
pub(crate) struct ShareCap(pub(crate) Politeness);

impl lodestar_sources::PeerIpCap for ShareCap {
    fn connections_to(&self, ip: std::net::IpAddr) -> usize {
        usize::from(self.0.connections_to(ip))
    }
}

/// The pool's per-peer-IP share, for [`apply_pause`]: the one it was
/// built with, the latest wanted, and where to say what it was told.
struct Share {
    built_with: Politeness,
    latest: watch::Receiver<Politeness>,
    told: watch::Sender<Politeness>,
}

/// Tells the pool the latest pause [`Shared::refresh_pause`] decided, and
/// the latest per-peer-IP share, in order, for as long as the stages run.
/// A changed share applies to the pool's next dial: connections already
/// open finish (WK-10e). The starting point is the share the pool was
/// built with, never the watch's value at the first poll: a replan that
/// lands in between is applied, not taken as already done (#617's review).
async fn apply_pause(
    pool: lodestar_sources::PoolHandle,
    mut want: watch::Receiver<bool>,
    told_tx: watch::Sender<bool>,
    share: Share,
) {
    let Share {
        built_with,
        latest: mut share,
        told: share_told,
    } = share;
    // The pool starts unpaused, with the share it was built with.
    let mut told = false;
    told_tx.send_replace(false);
    let mut shared = built_with;
    share_told.send_replace(shared);
    loop {
        let now = *share.borrow_and_update();
        if now != shared {
            pool.set_ip_cap(Some(Arc::new(ShareCap(now)))).await;
            shared = now;
            share_told.send_replace(now);
        }
        let pause = *want.borrow_and_update();
        if pause != told {
            pool.set_paused(pause).await;
            told = pause;
            told_tx.send_replace(pause);
        }
        let changed = tokio::select! {
            r = want.changed() => r,
            r = share.changed() => r,
        };
        if changed.is_err() {
            return;
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

/// Moves queued keys into the pool, waiting while it's full.
async fn forward(mut from: mpsc::UnboundedReceiver<Discovery>, to: mpsc::Sender<Discovery>) {
    while let Some(d) = from.recv().await {
        if to.send(d).await.is_err() {
            return;
        }
    }
}

/// The current session, waiting for one.
async fn current(s: &Shared) -> SessionHandle {
    let mut rx = s.session.subscribe();
    loop {
        if let Some(h) = rx.borrow_and_update().clone()
            && !h.is_closed()
        {
            return h;
        }
        if rx.changed().await.is_err() {
            std::future::pending::<()>().await;
        }
    }
}

/// How often [`supervise`] looks at the stage tasks.
const SUPERVISE_EVERY: Duration = Duration::from_secs(1);

/// Watches the pool and the prepare stage: one that ends while the
/// runtime isn't stopping them has failed (a panic, or a bug that ended
/// its loop). That's said once, and [`Runtime::failed`] completes.
async fn supervise(s: Arc<Shared>) {
    let mut every = tokio::time::interval(SUPERVISE_EVERY);
    every.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        every.tick().await;
        let dead = lock(&s.stages).as_ref().and_then(|st| {
            if st.pool.is_finished() {
                Some("pool")
            } else if st.prepare.is_finished() {
                Some("prepare")
            } else {
                None
            }
        });
        if let Some(stage) = dead
            && !s.draining.load(Ordering::Acquire)
        {
            lodestar_log::log_event!(crate::logging::events::WORKER_STAGE_FAILED, stage = stage);
            s.failed.send_replace(true);
            return;
        }
    }
}

/// Refreshes the heartbeat's stats every `stats_every` (WK-10c):
/// claimed, results sent ("fetched"), results the coordinator accepted
/// ("processed"), CPU and resident memory, and the spool's bytes (WK-11).
async fn heartbeat_stats(s: Arc<Shared>) {
    let mut process = super::budget::Process::default();
    let mut every = tokio::time::interval(s.config.stats_every);
    every.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        every.tick().await;
        let (cpu_permille, rss_mb) = process.sample();
        let get = |n: &AtomicU64| n.load(Ordering::Relaxed);
        let stats = lodestar_cluster::proto::Stats {
            claimed: get(&s.counts.claimed),
            fetched: get(&s.counts.sent),
            processed: get(&s.counts.committed),
            cpu_permille,
            rss_mb,
            spool_bytes: s.spool.as_ref().map_or(0, |sp| sp.bytes()),
        };
        let session = s.session.borrow().clone();
        if let Some(h) = session {
            let _ = h.set_stats(stats).await;
            // A new public address reaches the coordinator only in `Hello`
            // (WK-5): ask for a fresh session to carry it.
            let voted = s.dht_counts.external_ip();
            if voted.is_some() && voted != *lock(&s.hello_ip) {
                s.ask_reconnect();
            }
        }
    }
}

/// Sends snapshot acknowledgements, and the have-filter's resync request
/// again while no header comes (WK-3).
async fn send_acks(s: Arc<Shared>, mut acks: mpsc::Receiver<SnapshotAck>) {
    let mut poll = tokio::time::interval(POLL_EVERY);
    poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        let ack = tokio::select! {
            a = acks.recv() => match a { Some(a) => a, None => return },
            _ = poll.tick() => match s.have.poll(std::time::Instant::now()) {
                Some(a) => a,
                None => continue,
            },
        };
        // A lost acknowledgement stalls that transfer, which the
        // coordinator starts over; a new session asks again anyway.
        let h = current(&s).await;
        let _ = h.send(ToCoordinator::SnapshotAck(ack)).await;
    }
}

/// Keys the coordinator answered `Busy`, waiting to be claimed again
/// (X-wk-busy-retry). Bounded: at most `capacity` keys are in a retry at
/// once, each waiting at most once (`waiting.len() <= tries.len() <=
/// capacity`), and each claimed at most `attempts` more times.
struct BusyRetries {
    state: Mutex<RetryState>,
    wake: tokio::sync::Notify,
    policy: BusyRetry,
}

#[derive(Default)]
struct RetryState {
    tries: HashMap<[u8; 20], Try>,
    /// When each waiting key is due, with what to claim it with. A key is
    /// here at most once: its `Try::due` names its entry.
    waiting: BTreeMap<(tokio::time::Instant, [u8; 20]), (ClaimItem, Vec<std::net::SocketAddr>)>,
}

/// A key in a retry.
struct Try {
    /// Claims after the first so far.
    attempts: u8,
    backoff: super::session::Backoff,
    /// When it's due, while it waits; `None` while its claim is out.
    due: Option<tokio::time::Instant>,
}

/// What became of a `Busy` key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Busy {
    /// Claimed again after this wait.
    Again(Duration),
    /// Already waiting (found again and claimed meanwhile): it keeps its
    /// place, and the answer uses no attempt.
    Waiting,
    /// Its last attempt was spent: dropped.
    GaveUp,
    /// The retry holds `capacity` keys already: dropped.
    Full,
}

impl BusyRetries {
    fn new(policy: BusyRetry) -> BusyRetries {
        BusyRetries {
            state: Mutex::default(),
            wake: tokio::sync::Notify::new(),
            policy,
        }
    }

    fn busy(
        &self,
        item: ClaimItem,
        peers: Vec<std::net::SocketAddr>,
        now: tokio::time::Instant,
        jitter: u64,
    ) -> Busy {
        let key = item.key;
        let mut guard = lock(&self.state);
        let st = &mut *guard;
        match st.tries.get(&key) {
            Some(t) if t.due.is_some() => return Busy::Waiting,
            None if st.tries.len() >= self.policy.capacity => return Busy::Full,
            _ => {}
        }
        let (base, cap) = (self.policy.base, self.policy.cap);
        let t = st.tries.entry(key).or_insert_with(|| Try {
            attempts: 0,
            backoff: super::session::Backoff::new(base, cap),
            due: None,
        });
        t.attempts += 1;
        if t.attempts > self.policy.attempts {
            st.tries.remove(&key);
            return Busy::GaveUp;
        }
        let wait = t.backoff.next(jitter);
        let at = now + wait;
        t.due = Some(at);
        st.waiting.insert((at, key), (item, peers));
        drop(guard);
        self.wake.notify_one();
        Busy::Again(wait)
    }

    /// A key's retry ends: granted, denied for another reason, or not
    /// claimed after all. A wait it still had goes with it.
    fn done(&self, key: &[u8; 20]) {
        let mut st = lock(&self.state);
        if let Some(Try { due: Some(at), .. }) = st.tries.remove(key) {
            st.waiting.remove(&(at, *key));
        }
    }

    /// The keys due by `now` (their claims are out from here), and when
    /// the next one is.
    fn due(
        &self,
        now: tokio::time::Instant,
    ) -> (
        Vec<(ClaimItem, Vec<std::net::SocketAddr>)>,
        Option<tokio::time::Instant>,
    ) {
        let mut guard = lock(&self.state);
        let st = &mut *guard;
        let mut out = Vec::new();
        while let Some(entry) = st.waiting.first_entry() {
            if entry.key().0 > now {
                break;
            }
            let ((at, key), item) = entry.remove_entry();
            if let Some(t) = st.tries.get_mut(&key).filter(|t| t.due == Some(at)) {
                t.due = None;
                out.push(item);
            }
        }
        let next = st.waiting.keys().next().map(|(at, _)| *at);
        (out, next)
    }

    /// Keys in a retry now, and those waiting.
    #[cfg(test)]
    fn len(&self) -> (usize, usize) {
        let st = lock(&self.state);
        (st.tries.len(), st.waiting.len())
    }
}

/// Whether to log a give-up now: the first batch with any give-up after a
/// batch answered with no `Busy` at all, once per streak.
fn streak(giving_up: &mut bool, busy: u64, gave_up: u64) -> bool {
    if gave_up > 0 {
        return !std::mem::replace(giving_up, true);
    }
    if busy == 0 {
        *giving_up = false;
    }
    false
}

/// Feeds `Busy` keys back into the claims as each comes due.
async fn retry_busy(
    retries: Arc<BusyRetries>,
    items: mpsc::Sender<(ClaimItem, Vec<std::net::SocketAddr>)>,
) {
    loop {
        let (due, next) = retries.due(tokio::time::Instant::now());
        for item in due {
            if items.send(item).await.is_err() {
                return;
            }
        }
        match next {
            Some(at) => {
                tokio::select! {
                    () = tokio::time::sleep_until(at) => {}
                    () = retries.wake.notified() => {}
                }
            }
            None => retries.wake.notified().await,
        }
    }
}

/// Checks keys found here against the have-filter and claims them in
/// batches. Before a filter is complete every key is claimed ("maybe").
/// A key the coordinator answers `Busy` is claimed again later
/// ([`BusyRetries`]); any other denial drops it, as the DHT finds it again.
async fn claim(s: Arc<Shared>, found: mpsc::Receiver<Found>) {
    // Nothing is claimed before the stages can take it.
    let mut started = s.started.subscribe();
    if started.wait_for(|on| *on).await.is_err() {
        return;
    }
    let (items, mut batches) = mpsc::channel::<(ClaimItem, Vec<std::net::SocketAddr>)>(1024);
    let retries = Arc::new(BusyRetries::new(s.config.busy_retry));
    // Both end with this task (a stop aborts it).
    let _retry =
        AbortOnDrop(tokio::spawn(retry_busy(retries.clone(), items.clone())).abort_handle());
    let _check = AbortOnDrop(tokio::spawn(check(s.clone(), found, items)).abort_handle());
    // Once per streak of give-ups, not per key: a coordinator stuck on
    // `Busy` can't flood the log.
    let mut giving_up = false;
    // `next_claims` isn't cancel-safe: it's awaited to its end.
    while let Some(batch) = next_claims(&mut batches).await {
        let end = |batch: &[(ClaimItem, Vec<std::net::SocketAddr>)]| {
            for (c, _) in batch {
                retries.done(&c.key);
            }
        };
        if s.draining.load(Ordering::Acquire) {
            end(&batch);
            continue;
        }
        // Sifted before sealing went down, claimed after: dropped as `sift`
        // would drop them now (#599's review). The DHT finds them again.
        // Nor while the assignment turns fetching off, as `sift` holds
        // back new finds then: a retried key gets the same checks.
        if s.sealing_down(unix_now()) || !s.fetching() || s.spool_full() {
            s.counts
                .held_back
                .fetch_add(batch.len() as u64, Ordering::Relaxed);
            end(&batch);
            continue;
        }
        // Only as many as the pool has room for: the leases held never
        // pass the pool's cap, which bounds the queue into the pool and the
        // losses queued (#599's review). The rest are found again later.
        let mut batch = batch;
        let room = s.pool_room();
        if batch.len() > room {
            s.counts
                .held_back
                .fetch_add((batch.len() - room) as u64, Ordering::Relaxed);
            end(&batch[room..]);
            batch.truncate(room);
        }
        if batch.is_empty() {
            continue;
        }
        let mut sent: BTreeMap<[u8; 20], (ClaimItem, Vec<std::net::SocketAddr>)> = batch
            .iter()
            .map(|(c, p)| (c.key, (c.clone(), p.clone())))
            .collect();
        let items: Vec<ClaimItem> = batch.into_iter().map(|(c, _)| c).collect();
        s.counts
            .claimed
            .fetch_add(items.len() as u64, Ordering::Relaxed);
        let h = current(&s).await;
        // A claim that fails is dropped: the DHT finds the keys again.
        let Ok(grants) = h.claim(items).await else {
            for key in sent.keys() {
                retries.done(key);
            }
            continue;
        };
        let (mut busy, mut gave_up) = (0u64, 0u64);
        for g in grants {
            match g {
                GrantItem::Granted { key, token, .. } => {
                    retries.done(&key);
                    let peers = sent.remove(&key).map(|(_, p)| p).unwrap_or_default();
                    granted(&s, InfoHash(key), Token(token), peers, unix_now());
                }
                GrantItem::Denied {
                    key,
                    reason: DenyReason::Busy,
                } => {
                    busy += 1;
                    let Some((item, peers)) = sent.remove(&key) else {
                        continue;
                    };
                    let jitter = super::session::random_jitter();
                    match retries.busy(item, peers, tokio::time::Instant::now(), jitter) {
                        Busy::Again(_) => bump(&s.counts.busy_retried),
                        Busy::Waiting => {}
                        Busy::GaveUp => {
                            gave_up += 1;
                            bump(&s.counts.busy_gave_up);
                        }
                        // Dropped too, and said so in the same line.
                        Busy::Full => {
                            gave_up += 1;
                            bump(&s.counts.busy_dropped);
                        }
                    }
                }
                // Stored, held elsewhere, draining: dropped, as before.
                GrantItem::Denied { key, .. } => retries.done(&key),
            }
        }
        // A key the answer left out (the protocol promises one answer per
        // key, but nothing enforces it) ends its retry too, so it can't
        // hold a slot for ever. Busy keys are out of `sent` already, and
        // ending a denied key twice is a no-op.
        for key in sent.keys() {
            retries.done(key);
        }
        if streak(&mut giving_up, busy, gave_up) {
            // The first batch's count; the counters have the whole streak.
            lodestar_log::log_event!(
                crate::logging::events::WORKER_CLAIMS_GAVE_UP,
                keys = lodestar_log::Count(gave_up)
            );
        }
    }
}

/// The have-filter's check, in front of the claims.
async fn check(
    s: Arc<Shared>,
    mut found: mpsc::Receiver<Found>,
    items: mpsc::Sender<(ClaimItem, Vec<std::net::SocketAddr>)>,
) {
    while let Some(f) = found.recv().await {
        let (ready, maybe) = s.have.answer(&f.key);
        let Some(item) = sift(&s, f, ready, maybe) else {
            continue;
        };
        if items.send(item).await.is_err() {
            return;
        }
    }
}

/// One sighting through the have-filter's answer (`ready`: a filter is
/// complete; `maybe`: it says the key may be stored), counted; the claim
/// to make, if any.
fn sift(
    s: &Shared,
    f: Found,
    ready: bool,
    maybe: bool,
) -> Option<(ClaimItem, Vec<std::net::SocketAddr>)> {
    bump(&s.counts.found);
    // Nothing is claimed while sealing is down: it couldn't be sent, and
    // its lease would only wait (#599's review). Nor while the assignment
    // turns fetching off, or the spool is full (WK-11: the pool is paused,
    // so a lease would only wait).
    if s.sealing_down(unix_now()) || !s.fetching() || s.spool_full() {
        bump(&s.counts.held_back);
        return None;
    }
    match rediscovery(ready, maybe, s.draw()) {
        Sighting::Claim => {}
        Sighting::Sampled => bump(&s.counts.sampled),
        Sighting::Drop => {
            bump(&s.counts.skipped);
            return None;
        }
    }
    let peers: Vec<_> = f
        .peers
        .iter()
        .copied()
        .filter(|a| lodestar_network_policy::allows(*a, s.config.allow_private))
        .take(MAX_HINTS)
        .collect();
    let item = ClaimItem {
        key: f.key.0,
        source: DHT_SOURCE,
        strategy: f.strategy,
        discovered_ms: unix_now().saturating_mul(1000),
        hints: peers.clone(),
    };
    Some((item, peers))
}

/// Reports losses in batches, keeping each until a session takes it. On
/// a stop, hands back what's left.
async fn report(
    s: Arc<Shared>,
    mut losses: mpsc::UnboundedReceiver<OutcomeItem>,
    mut stopped: watch::Receiver<bool>,
) -> Vec<OutcomeItem> {
    let mut waiting: Vec<OutcomeItem> = Vec::new();
    loop {
        if waiting.is_empty() {
            tokio::select! {
                l = losses.recv() => match l {
                    Some(l) => waiting.push(l),
                    None => return waiting,
                },
                _ = stopped.wait_for(|s| *s) => break,
            }
        }
        while waiting.len() < MAX_OUTCOME_BATCH
            && let Ok(l) = losses.try_recv()
        {
            waiting.push(l);
        }
        let h = tokio::select! {
            h = current(&s) => h,
            _ = stopped.wait_for(|s| *s) => break,
        };
        let n = waiting.len().min(MAX_OUTCOME_BATCH);
        if h.report(waiting[..n].to_vec()).await.is_ok() {
            waiting.drain(..n);
        }
    }
    while let Ok(l) = losses.try_recv() {
        waiting.push(l);
    }
    waiting
}

/// A batch of results sent and not yet accepted. Its records keep their
/// bytes in the outbound queue's budget until then.
struct InFlight {
    payload: Vec<u8>,
    leases: Vec<([u8; 20], u64)>,
    _held: Vec<Held<SealedRecord>>,
    sent: bool,
    /// Refused for now; sent again on the next retry tick.
    refused: bool,
    /// Counted in `Counts::sent` already.
    counted: bool,
    kind: Kind,
    /// Where in the spool it was read from, for a replayed batch.
    from: Option<Origin>,
    /// When the batch was made, just after its records were sealed: a
    /// spooled one's age runs from this, not from when it was spooled, so
    /// the 24 h cap stays inside the wire key's two days (WK-11).
    at: i64,
}

/// Puts records into batches (`encode`) in flight. A record that can't be
/// encoded is released, so its lease ends (it can't happen for a record
/// the judge sealed).
fn batch_up(
    s: &Shared,
    held: Vec<Held<SealedRecord>>,
    next: &mut u64,
    flight: &mut BTreeMap<u64, InFlight>,
) {
    if held.is_empty() {
        return;
    }
    let (batches, refused) = encode(held, next);
    for h in refused {
        let r = h.get();
        bump(&s.counts.released);
        s.leases.done(&r.key, r.token);
        s.losses.lost(&r.key, r.token, Loss::GaveUp);
    }
    for (batch, payload, part) in batches {
        let leases = part
            .iter()
            .map(|h| (h.get().key.0, h.get().token.0))
            .collect();
        flight.insert(
            batch,
            InFlight {
                payload,
                leases,
                _held: part,
                sent: false,
                refused: false,
                counted: false,
                kind: Kind::Results,
                from: None,
                at: unix_now(),
            },
        );
    }
}

/// Sends every batch not on the wire on `session` (and, with
/// `refused_too`, those refused for now), counting each in
/// `Counts::sent` once, however often it's sent.
async fn send_due(
    s: &Shared,
    session: Option<SessionHandle>,
    flight: &mut BTreeMap<u64, InFlight>,
    refused_too: bool,
) {
    let Some(h) = session.filter(|h| !h.is_closed()) else {
        return;
    };
    for (&batch, f) in flight
        .iter_mut()
        .filter(|(_, f)| !f.sent || (refused_too && f.refused))
    {
        let payload = Payload {
            batch,
            bytes: f.payload.clone(),
        };
        let msg = match f.kind {
            Kind::Results => ToCoordinator::Results(payload),
            Kind::Counters => ToCoordinator::Counters(payload),
        };
        if h.send(msg).await.is_err() {
            break;
        }
        f.sent = true;
        f.refused = false;
        if !f.counted {
            f.counted = true;
            s.counts
                .sent
                .fetch_add(f.leases.len() as u64, Ordering::Relaxed);
        }
    }
}

/// How [`send_due_or_changed`] ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Sending {
    /// Every due batch went out, or the session refused one.
    Done,
    /// The session changed (a disconnect, or a new session) first.
    Changed,
    /// The runtime's session watch is gone: the task ends.
    Gone,
}

/// [`send_due`], given up as soon as the session changes. A batch whose
/// send was cut short isn't marked sent, so it goes again or to the spool.
async fn send_due_or_changed(
    s: &Shared,
    session: Option<SessionHandle>,
    flight: &mut BTreeMap<u64, InFlight>,
    refused_too: bool,
    sessions: &mut watch::Receiver<Option<SessionHandle>>,
) -> Sending {
    tokio::select! {
        () = send_due(s, session, flight, refused_too) => Sending::Done,
        changed = sessions.changed() => match changed {
            Ok(()) => Sending::Changed,
            Err(_) => Sending::Gone,
        },
    }
}

/// The current session, marked seen. On a change of generation (a
/// session came or went, even one missed while busy), whatever wasn't
/// accepted goes again (a batch id is idempotent, §19.2); replayed batches
/// are dropped and read again from the spool, past what was accepted.
///
/// The session first, then the generation: a connect bumps the generation
/// before it replaces the session, so a new session is never seen with
/// the old generation (and reset again after).
fn observe(
    s: &Shared,
    sessions: &mut watch::Receiver<Option<SessionHandle>>,
    seen: &mut u64,
    flight: &mut BTreeMap<u64, InFlight>,
    replay: &mut Replay,
) -> Option<SessionHandle> {
    let session = sessions.borrow_and_update().clone();
    let now_gen = s.generation.load(Ordering::Acquire);
    if now_gen != *seen {
        *seen = now_gen;
        flight.retain(|_, f| f.from.is_none());
        replay.restart();
        for f in flight.values_mut() {
            f.sent = false;
        }
    }
    session
}

/// The session task: sealed results into `Results` batches, kept until
/// accepted, sent again on a new session and after a refusal.
async fn send_results(
    s: Arc<Shared>,
    mut outbound: ByteReceiver<SealedRecord>,
    mut accepted: mpsc::Receiver<(u64, bool)>,
    mut flush: mpsc::UnboundedReceiver<tokio::sync::oneshot::Sender<()>>,
) {
    let mut next_batch: u64 = 1;
    let mut flight: BTreeMap<u64, InFlight> = BTreeMap::new();
    let mut sessions = s.session.subscribe();
    let mut retry = tokio::time::interval(s.config.retry_every);
    retry.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut replay = Replay::default();
    if let Some(sp) = &s.spool {
        sp.start().await;
    }
    let mut seen = s.generation.load(Ordering::Acquire);
    loop {
        let session = observe(&s, &mut sessions, &mut seen, &mut flight, &mut replay);
        if let Some(sp) = &s.spool {
            // No session: batches go to disk. A session: the spool is
            // replayed, a segment at a time (WK-11).
            if session.as_ref().is_none_or(|h| h.is_closed()) {
                spool_live(&s, sp, &mut flight).await;
            } else if replay.is_idle() {
                for r in sp.next(&mut replay).await {
                    flight.insert(
                        next_batch,
                        InFlight {
                            payload: r.payload,
                            leases: r.leases,
                            _held: Vec::new(),
                            sent: false,
                            refused: false,
                            counted: true,
                            kind: r.kind,
                            from: Some(r.from),
                            at: 0,
                        },
                    );
                    next_batch += 1;
                }
                // A segment removed on the way may have cleared "full".
                s.refresh_spool_pause();
            }
        }
        // Given up if the session changes meanwhile: a stalled coordinator
        // can hold a send for the whole reply timeout, and a stop's spool
        // fallback (CLUSTER-05) must not wait behind it.
        match send_due_or_changed(&s, session, &mut flight, false, &mut sessions).await {
            Sending::Done => {}
            Sending::Changed => continue,
            Sending::Gone => return,
        }
        tokio::select! {
            record = outbound.recv() => {
                let Some(first) = record else { return };
                let mut held = vec![first];
                let mut size = held[0].get().weigh();
                while size < MAX_OPAQUE - BATCH_SLACK
                    && let Ok(more) = Inbox::try_recv(&mut outbound)
                {
                    size += more.get().weigh();
                    held.push(more);
                }
                batch_up(&s, held, &mut next_batch, &mut flight);
            }
            Some(done) = flush.recv() => {
                // Everything queued now goes into batches, and every batch
                // not on the wire goes out on the current session.
                let mut held = Vec::new();
                while let Ok(more) = Inbox::try_recv(&mut outbound) {
                    held.push(more);
                }
                batch_up(&s, held, &mut next_batch, &mut flight);
                // A drain doesn't wait for the retry tick: refused batches
                // go again too.
                // Marked seen, so a session that came in the same wake
                // is sent on here, not taken for a change below.
                let session = observe(&s, &mut sessions, &mut seen, &mut flight, &mut replay);
                // With no session they go to the spool, durably, before
                // the flush is answered: a local stop exits right after
                // (CLUSTER-05).
                let offline = |h: &Option<SessionHandle>| h.as_ref().is_none_or(|h| h.is_closed());
                if let Some(sp) = &s.spool
                    && offline(&session)
                {
                    spool_live(&s, sp, &mut flight).await;
                }
                // The session changed while sending: gone, what's left goes
                // to the spool now, not at the next round; a new one, it's
                // sent there before the flush is answered. Each round needs
                // a real session change, and the drain's deadline bounds
                // the wait for the answer.
                let mut session = session;
                loop {
                    match send_due_or_changed(&s, session, &mut flight, true, &mut sessions).await {
                        Sending::Done => break,
                        Sending::Changed => {
                            session = observe(&s, &mut sessions, &mut seen, &mut flight, &mut replay);
                            if offline(&session) {
                                if let Some(sp) = &s.spool {
                                    spool_live(&s, sp, &mut flight).await;
                                }
                                break;
                            }
                        }
                        Sending::Gone => {
                            let _ = done.send(());
                            return;
                        }
                    }
                }
                let _ = done.send(());
            }
            Some((batch, ok)) = accepted.recv() => {
                if ok {
                    if let Some(f) = flight.remove(&batch) {
                        s.counts.committed.fetch_add(f.leases.len() as u64, Ordering::Relaxed);
                        // Accepted: their results are no longer out. Kept,
                        // finished, for the grace: the coordinator holds the
                        // lease until its writer commits. One that marks it
                        // (the `Fetched` below) denies this worker's claims
                        // meanwhile; an older one answers them with it.
                        for &(key, token) in &f.leases {
                            s.leases.committed(&InfoHash(key), Token(token));
                        }
                        // `Fetched` first, then forgotten: the coordinator
                        // keeps each lease until its writer commits, and
                        // with the mark answers this worker's own claims as
                        // committed meanwhile, never with the lease
                        // (X-wk-fetched-lease). One task, so in that order.
                        if let Some(h) = s.session.borrow().clone() {
                            let leases = f.leases;
                            tokio::spawn(async move {
                                for part in leases.chunks(MAX_OUTCOME_BATCH) {
                                    let fetched = part
                                        .iter()
                                        .map(|&(key, token)| OutcomeItem {
                                            key,
                                            token,
                                            outcome: Outcome::Fetched,
                                        })
                                        .collect();
                                    let _ = h.report(fetched).await;
                                }
                                let _ = h.committed(leases).await;
                            });
                        }
                        // A spooled segment goes once all of it is in.
                        if let (Some(from), Some(sp)) = (f.from, &s.spool)
                            && let Some(seq) = replay.accepted(from)
                        {
                            sp.remove(seq).await;
                            s.refresh_spool_pause();
                        }
                    }
                } else if let Some(f) = flight.get_mut(&batch) {
                    // Refused for now (backpressure): sent again on the
                    // next retry tick, not at once.
                    f.refused = true;
                }
            }
            changed = sessions.changed() => {
                if changed.is_err() {
                    return;
                }
                // The reset is at the top of the loop, by generation.
            }
            _ = retry.tick() => {
                for f in flight.values_mut().filter(|f| f.refused) {
                    f.refused = false;
                    f.sent = false;
                }
            }
        }
    }
}

/// With no session: every live batch not accepted goes to the spool and
/// leaves memory (its records' bytes leave the outbound budget, so the
/// prepare stage keeps going). One that won't fit stays in memory and
/// pauses fetching until the spool has room (WK-11).
async fn spool_live(s: &Shared, sp: &WorkerSpool, flight: &mut BTreeMap<u64, InFlight>) {
    let live: Vec<u64> = flight
        .iter()
        .filter(|(_, f)| f.from.is_none())
        .map(|(&b, _)| b)
        .collect();
    let mut wrote = false;
    for batch in live {
        let Some(f) = flight.get(&batch) else {
            continue;
        };
        if sp.is_full() {
            break;
        }
        if !sp
            .put(f.kind, batch, f.at, f.leases.clone(), f.payload.clone())
            .await
        {
            s.refresh_spool_pause();
            break;
        }
        wrote = true;
        if let Some(f) = flight.remove(&batch)
            && !f.counted
        {
            s.counts
                .sent
                .fetch_add(f.leases.len() as u64, Ordering::Relaxed);
        }
    }
    // Durable before anything else happens (appends between rounds are
    // synced at most a second apart by the spool itself).
    if wrote {
        sp.sync().await;
    }
}

/// Encodes records into batches no larger than `MAX_OPAQUE`, numbered
/// from `next` (never 0), splitting where needed: a sealed record always
/// fits a batch alone (WK-8). Every record is sealed (`encode_with(&[],
/// &sealed)`); nothing goes in clear.
type Encoded<T> = (u64, Vec<u8>, Vec<T>);

/// Returns the batches, and the records that won't encode even alone.
fn encode<T: std::borrow::Borrow<SealedRecord>>(
    held: Vec<T>,
    next: &mut u64,
) -> (Vec<Encoded<T>>, Vec<T>) {
    let sealed: Vec<SealedRecord> = held.iter().map(|h| h.borrow().clone()).collect();
    let batch = ResultsBatch {
        batch: *next,
        records: Vec::new(),
    };
    match batch.encode_with(&sealed) {
        Ok(bytes) if bytes.len() <= MAX_OPAQUE => {
            *next += 1;
            (vec![(batch.batch, bytes, held)], Vec::new())
        }
        _ if held.len() > 1 => {
            let mut held = held;
            let rest = held.split_off(held.len() / 2);
            let (mut out, mut refused) = encode(held, next);
            let (more, also) = encode(rest, next);
            out.extend(more);
            refused.extend(also);
            (out, refused)
        }
        // One record that won't encode can't be sent; that can't happen
        // for a record the judge sealed.
        _ => (Vec::new(), held),
    }
}

/// What the spool's tests (`worker/spool.rs`) reach inside the runtime.
#[cfg(test)]
impl Runtime {
    pub(crate) fn test_outbound(&self) -> ByteSender<SealedRecord> {
        lock(&self.shared.out).take().expect("the outbound queue")
    }
    pub(crate) fn test_pool_paused(&self) -> bool {
        self.shared.refresh_pause();
        *self.shared.pool_paused.borrow()
    }
    pub(crate) fn test_grant(&self, key: InfoHash, token: Token, peers: Vec<std::net::SocketAddr>) {
        granted(&self.shared, key, token, peers, unix_now());
    }
    /// Re-checks the pauses, then whether the DHT is told to run.
    pub(crate) fn test_dht_runs(&self) -> bool {
        self.shared.refresh_spool_pause();
        lock(&self.shared.dht_want)
            .as_ref()
            .is_some_and(|w| matches!(*w.borrow(), super::dht::Desired::Run(_)))
    }
    /// Whether the DHT is told to run, as it stands (no re-check).
    pub(crate) fn test_dht_told_to_run(&self) -> bool {
        lock(&self.shared.dht_want)
            .as_ref()
            .is_some_and(|w| matches!(*w.borrow(), super::dht::Desired::Run(_)))
    }
    /// Whether a fresh find would be claimed now.
    pub(crate) fn test_would_claim(&self, key: InfoHash) -> bool {
        sift(&self.shared, Found::key(key, 0), true, false).is_some()
    }
}

#[cfg(test)]
pub(crate) mod tests {
    //! The runtime on its own (events fed by hand), and end to end against
    //! an in-process coordinator: `serve`'s cluster port and tick (WK-10a)
    //! on a real pipeline, over TCP and mutual TLS.

    use std::collections::HashMap;
    use std::net::SocketAddr;
    use std::time::Instant;

    use lodestar_cluster::assign::{Budget, Intensity, Politeness, ShardSpec};
    use lodestar_cluster::proto::Assign;
    use lodestar_peer_wire::{FailureKind, Fetched};
    #[allow(unused_imports)]
    use lodestar_pipeline::wire::ResultsBatch;
    use lodestar_pipeline::wire::filter_snapshot::{
        FilterSnapshot, chunk, chunk_count, version_of,
    };
    use lodestar_sources::BoxFuture;
    use lodestar_store::{Store, StoreConfig};

    use super::*;

    pub(crate) const DEADLINE: Duration = Duration::from_secs(20);
    pub(crate) const W: WorkerId = WorkerId(5);

    pub(crate) async fn until<T: std::fmt::Debug>(
        name: &str,
        mut what: impl FnMut() -> (bool, T),
    ) -> T {
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

    /// A bencoded single-file info dict.
    pub(crate) fn info(name: &str) -> Vec<u8> {
        let mut v = b"d6:lengthi1048576e4:name".to_vec();
        v.extend(format!("{}:{name}", name.len()).bytes());
        v.extend(b"12:piece lengthi262144e6:pieces20:");
        v.extend([0u8; 20]);
        v.push(b'e');
        v
    }

    pub(crate) fn key_of(raw: &[u8]) -> InfoHash {
        lodestar_metainfo::Info::parse(raw).unwrap().v1.unwrap()
    }

    /// Serves the info dicts it's given; refuses every other key.
    #[derive(Default)]
    pub(crate) struct MapFetch(pub(crate) Mutex<HashMap<InfoHash, Vec<u8>>>);

    impl Fetcher for MapFetch {
        fn fetch(
            &self,
            _: SocketAddr,
            key: InfoHash,
        ) -> BoxFuture<'static, Result<Fetched, FailureKind>> {
            let info = lock(&self.0).get(&key).cloned();
            Box::pin(async move {
                info.map(|info| Fetched {
                    info,
                    v2: false,
                    encrypted: false,
                    pex: Vec::new(),
                    client: None,
                })
                .ok_or(FailureKind::Refused)
            })
        }
    }

    pub(crate) fn config(fetcher: Arc<dyn Fetcher>) -> Config {
        Config {
            allow_private: true,
            drain_deadline: Duration::from_secs(2),
            ..Config::new(W, fetcher)
        }
    }

    pub(crate) fn assignment(revision: u64, dict_version: u64, filter_version: u64) -> Assign {
        Assignment {
            revision,
            dht: None,
            fetch: true,
            prepare: true,
            intensity: Intensity::default(),
            budget: Budget::default(),
            pool_cap: 1000,
            dict_version,
            filter_version,
            politeness: Politeness::ALONE,
        }
        .to_message()
        .unwrap()
    }

    /// The coordinator's filter snapshot for today, from an empty store,
    /// as its chunks.
    ///
    /// Waits out the last minute before midnight UTC first: a test that
    /// seals under today's key would otherwise see the day turn under it
    /// (a condition with a deadline, not a fixed sleep).
    pub(crate) async fn filter_chunks() -> (u64, Vec<SnapshotChunk>) {
        let start = Instant::now();
        while unix_now().rem_euclid(DAY) >= DAY - 60 {
            assert!(
                start.elapsed() < Duration::from_secs(90),
                "midnight never came"
            );
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        filter_chunks_at(unix_now())
    }

    /// The same, with the wire key of the day `at` falls on.
    pub(crate) fn filter_chunks_at(at: i64) -> (u64, Vec<SnapshotChunk>) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("c.db"), StoreConfig::default()).unwrap();
        let snap: FilterSnapshot =
            crate::cluster::filter::read_snapshot(&store, &[4; 32], at).unwrap();
        let bytes = snap.encode().unwrap();
        let version = version_of(&bytes);
        let chunks = (0..chunk_count(bytes.len()))
            .map(|seq| chunk(SnapshotKind::Filter, version, &bytes, seq))
            .collect();
        (version, chunks)
    }

    /// WK-8's gate: a worker that starts (or restarts) can't seal, and so
    /// fetches nothing, until a filter snapshot arrives; then the stages
    /// start. An older assignment never replaces a newer one.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn nothing_runs_before_a_fresh_filter_snapshot() {
        let mut rt = Runtime::start(config(Arc::new(MapFetch::default())));
        rt.event(WorkerEvent::Assign(assignment(5, 0, 1)));
        rt.event(WorkerEvent::Assign(assignment(4, 0, 1)));
        assert_eq!(rt.assignment().map(|a| a.revision), Some(5));
        // A grant before then waits: nothing fetches, nothing is judged.
        rt.event(WorkerEvent::Regranted(vec![GrantItem::Granted {
            key: [1; 20],
            token: 7,
            ttl_secs: 600,
        }]));
        assert!(!rt.is_started());
        assert_eq!(rt.filter_version(), 0);

        let (version, chunks) = filter_chunks().await;
        for c in chunks {
            rt.event(WorkerEvent::Snapshot(c));
        }
        until("installed and started", || {
            (
                rt.is_started() && rt.filter_version() == version,
                rt.filter_version(),
            )
        })
        .await;
        rt.shutdown().await;
    }

    /// A DHT on loopback with no bootstrap: it binds, but finds nothing.
    fn dht_config() -> lodestar_dht::node::Config {
        lodestar_dht::node::Config {
            bind: vec!["127.0.0.1:0".parse().unwrap()],
            bootstrap: Vec::new(),
            state_path: None,
            allow_private: true,
            ..lodestar_dht::node::Config::default()
        }
    }

    pub(crate) fn assign_dht(
        revision: u64,
        shard: Option<ShardSpec>,
        intensity: Intensity,
    ) -> Assign {
        Assignment {
            revision,
            dht: shard,
            fetch: true,
            prepare: true,
            intensity,
            budget: Budget::default(),
            pool_cap: 1000,
            dict_version: 0,
            filter_version: 1,
            politeness: Politeness::ALONE,
        }
        .to_message()
        .unwrap()
    }

    fn prepare_task(rt: &Runtime) -> Option<tokio::task::Id> {
        lock(&rt.shared.stages).as_ref().map(|s| s.prepare.id())
    }

    /// A runtime with a DHT, connected, its stages and DHT running; the
    /// link and the session are kept by the test.
    async fn started_with_dht(
        linger: Duration,
    ) -> (
        Runtime,
        lodestar_cluster::CoordinatorLink,
        lodestar_cluster::WorkerSession,
    ) {
        started_with(Config {
            dht: Some(dht_config()),
            dht_linger: linger,
            ..config(Arc::new(MapFetch::default()))
        })
        .await
    }

    /// [`started_with_dht`] from a whole config (its `dht` set).
    async fn started_with(
        config: Config,
    ) -> (
        Runtime,
        lodestar_cluster::CoordinatorLink,
        lodestar_cluster::WorkerSession,
    ) {
        let mut rt = Runtime::start(config);
        let (coord, session) = session_pair().await;
        rt.connected(session.handle());
        let a = ShardSpec::new(64..128, 0xff).unwrap();
        rt.event(WorkerEvent::Assign(assign_dht(
            1,
            Some(a),
            Intensity::Balanced,
        )));
        let (_, chunks) = filter_chunks().await;
        for c in chunks {
            rt.event(WorkerEvent::Snapshot(c));
        }
        let running = rt.dht_running().expect("a DHT is configured");
        until("the DHT runs on its shard", || {
            let now = *running.borrow();
            (now == Some(a), now)
        })
        .await;
        (rt, coord, session)
    }

    /// WK-10c's gate: a live `Assign` with a new shard restarts the DHT
    /// node and nothing else: the stages and the session carry on. A new
    /// intensity alone restarts nothing, nor does a new BEP 51 scale.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_live_shard_change_restarts_only_the_dht() {
        let (mut rt, _coord, _session) = started_with_dht(super::super::dht::LINGER).await;
        let counts = rt.dht_counts();
        let running = rt.dht_running().unwrap();
        let stages = prepare_task(&rt).expect("the stages run");
        assert_eq!(counts.starts.load(Ordering::Relaxed), 1);

        let b = ShardSpec::new(128..256, 0x0f).unwrap();
        rt.event(WorkerEvent::Assign(assign_dht(
            2,
            Some(b),
            Intensity::Balanced,
        )));
        until("the DHT runs on the new shard", || {
            let now = *running.borrow();
            (now == Some(b), now)
        })
        .await;
        assert_eq!(counts.starts.load(Ordering::Relaxed), 2);
        assert_eq!(counts.stops.load(Ordering::Relaxed), 1);
        assert_eq!(
            prepare_task(&rt),
            Some(stages),
            "the stages weren't restarted"
        );
        assert!(rt.is_started());

        // Same shard, another intensity: applied to the running node.
        rt.event(WorkerEvent::Assign(assign_dht(3, Some(b), Intensity::Low)));
        until("the node runs at the new intensity", || {
            let n = counts.intensity_changes.load(Ordering::Relaxed);
            (n == 1, n)
        })
        .await;
        assert_eq!(counts.starts.load(Ordering::Relaxed), 2, "no restart");
        // Same shard, another BEP 51 share (WK-10e): applied, no restart.
        let mut a = Assignment::from_message(&assign_dht(4, Some(b), Intensity::Low))
            .unwrap()
            .unwrap();
        a.politeness.bep51_interval_scale = 3;
        rt.event(WorkerEvent::Assign(a.to_message().unwrap()));
        until("the node has the new scale", || {
            let n = counts.scale_changes.load(Ordering::Relaxed);
            (n == 1, n)
        })
        .await;
        assert_eq!(counts.starts.load(Ordering::Relaxed), 2, "no restart");
        // An assignment without a DHT stops it, and only it.
        rt.event(WorkerEvent::Assign(assign_dht(5, None, Intensity::Low)));
        until("the DHT stopped", || {
            let now = *running.borrow();
            (now.is_none(), now)
        })
        .await;
        assert_eq!(counts.starts.load(Ordering::Relaxed), 2);
        assert_eq!(prepare_task(&rt), Some(stages));
        rt.shutdown().await;
    }

    /// A lost session pauses the DHT at once; one back in time resumes
    /// it without a restart (its first `Assign` names the same shard).
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_lost_session_pauses_the_dht_and_a_quick_return_resumes_it() {
        let (mut rt, _coord, _session) = started_with_dht(super::super::dht::LINGER).await;
        let counts = rt.dht_counts();
        rt.disconnected();
        until("paused", || {
            let n = counts.pauses.load(Ordering::Relaxed);
            (n == 1, n)
        })
        .await;
        let (_coord2, session2) = session_pair().await;
        rt.connected(session2.handle());
        // Until the new session's first `Assign`, it stays paused.
        assert_eq!(
            *lock(&rt.shared.dht_want).as_ref().unwrap().borrow(),
            super::super::dht::Desired::Pause
        );
        let a = ShardSpec::new(64..128, 0xff).unwrap();
        rt.event(WorkerEvent::Assign(assign_dht(
            1,
            Some(a),
            Intensity::Balanced,
        )));
        until("resumed", || {
            let n = counts.resumes.load(Ordering::Relaxed);
            (n == 1, n)
        })
        .await;
        assert_eq!(counts.starts.load(Ordering::Relaxed), 1, "no restart");
        assert_eq!(counts.stops.load(Ordering::Relaxed), 0);
        rt.shutdown().await;
        assert_eq!(
            counts.stops.load(Ordering::Relaxed),
            1,
            "stopped (and saved) at shutdown"
        );
    }

    /// WK-5: a worker without a session opens no new connection. The pool
    /// pauses when the session goes and resumes when one is back.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_lost_session_pauses_the_pool() {
        let (mut rt, _coord, _session) = started_with_dht(super::super::dht::LINGER).await;
        // `Some(paused)` once no refresh holds the lock.
        let paused = |rt: &Runtime| Some(*rt.shared.pool_told.borrow());
        // A refresh spawned at start may still hold the lock.
        until("the pool runs", || {
            (paused(&rt) == Some(false), paused(&rt))
        })
        .await;
        rt.disconnected();
        until("the pool paused", || {
            (paused(&rt) == Some(true), paused(&rt))
        })
        .await;
        let (_coord2, session2) = session_pair().await;
        rt.connected(session2.handle());
        // Still paused until the new session's first `Assign`: wanted so,
        // and so told.
        assert!(*rt.shared.pool_paused.borrow(), "wanted paused");
        assert_eq!(paused(&rt), Some(true));
        let a = ShardSpec::new(64..128, 0xff).unwrap();
        rt.event(WorkerEvent::Assign(assign_dht(
            2,
            Some(a),
            Intensity::Balanced,
        )));
        until("the pool resumed", || {
            (paused(&rt) == Some(false), paused(&rt))
        })
        .await;
        rt.shutdown().await;
    }

    /// `Hello` carries the memory this process may use and the public
    /// address the DHT voted last run (from its saved state); a private
    /// address is never reported.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn hello_reports_the_saved_public_address_and_the_memory() {
        for (saved, reported) in [("81.2.69.7", true), ("192.168.1.2", false)] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("dht-state.bin");
            let ip: std::net::IpAddr = saved.parse().unwrap();
            lodestar_dht::state::State {
                external_ip: Some(ip),
                ..Default::default()
            }
            .save(&path)
            .unwrap();
            let rt = Runtime::start(Config {
                dht: Some(lodestar_dht::node::Config {
                    state_path: Some(path),
                    ..dht_config()
                }),
                ..config(Arc::new(MapFetch::default()))
            });
            let caps = rt.capabilities();
            let want = if reported { vec![ip] } else { Vec::new() };
            assert_eq!(caps.external_ips, want, "{saved}");
            if cfg!(target_os = "linux") {
                assert!(caps.ram_mb > 0);
            }
            rt.shutdown().await;
        }
    }

    /// A stop saves the DHT's state: `shutdown`, and a dropped runtime
    /// (as `lodestar worker` drops it) once `dht::stopped` returns.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_dht_state_is_saved_at_a_stop_and_a_drop() {
        for drop_it in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("dht-state.bin");
            let (rt, _coord, session) = started_with(Config {
                dht: Some(lodestar_dht::node::Config {
                    state_path: Some(path.clone()),
                    ..dht_config()
                }),
                ..config(Arc::new(MapFetch::default()))
            })
            .await;
            assert!(!path.exists(), "nothing saved while it runs");
            let started = Instant::now();
            if drop_it {
                let running = rt.dht_running().unwrap();
                // Something else still holds the shared state (an install
                // on a blocking thread, say): the drop must still end the
                // DHT's task, not wait for the last `Arc` to go.
                let held = rt.shared.clone();
                drop(rt);
                super::super::dht::stopped(running).await;
                drop(held);
            } else {
                rt.shutdown().await;
            }
            assert!(path.exists(), "saved (drop: {drop_it})");
            // Well inside `stopped`'s 11 s: the task ended because the stop
            // closed its channel, not at the timeout (#601's delta review).
            assert!(
                started.elapsed() < Duration::from_secs(5),
                "took {:?} (drop: {drop_it})",
                started.elapsed()
            );
            drop(session);
        }
    }

    /// The heartbeat carries the runtime's stats: keys claimed, results
    /// sent, and (on Linux) resident memory.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_heartbeat_carries_the_stats() {
        let fetch = Arc::new(MapFetch::default());
        let raw = info("Counted in the heartbeat");
        lock(&fetch.0).insert(key_of(&raw), raw.clone());
        let mut rt = Runtime::start(Config {
            stats_every: Duration::from_millis(50),
            ..config(fetch)
        });
        let (mut coord, session) = session_pair().await;
        rt.connected(session.handle());
        rt.event(WorkerEvent::Assign(assignment(1, 0, 1)));
        let (_, chunks) = filter_chunks().await;
        for c in chunks {
            rt.event(WorkerEvent::Snapshot(c));
        }
        until("started", || (rt.is_started(), ())).await;
        // A result to send, and a find to claim.
        granted(
            &rt.shared,
            key_of(&raw),
            Token(3),
            vec!["203.0.113.9:6881".parse().unwrap()],
            unix_now(),
        );
        assert!(rt.finds().found(found(42)));
        let start = Instant::now();
        loop {
            assert!(start.elapsed() < DEADLINE, "no heartbeat with the stats");
            // The heartbeat goes out at once; its acknowledgement never
            // comes here, so it isn't awaited.
            let _ = tokio::time::timeout(Duration::from_millis(50), session.heartbeat()).await;
            let mut seen = None;
            while let Ok(Ok(Some(m))) =
                tokio::time::timeout(Duration::from_millis(50), coord.recv()).await
            {
                if let ToCoordinator::Heartbeat(hb) = m {
                    seen = Some(hb.stats);
                }
            }
            if let Some(st) = seen
                && st.claimed > 0
                && st.fetched > 0
                && (st.rss_mb > 0 || !cfg!(target_os = "linux"))
            {
                break;
            }
        }
        rt.shutdown().await;
    }

    /// A new public address from the DHT asks for a fresh session, so the
    /// next `Hello` carries it; a flapping one asks at most once per
    /// debounce window.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_new_address_asks_for_a_fresh_session_debounced() {
        let mut rt = Runtime::start(Config {
            stats_every: Duration::from_millis(50),
            ..config(Arc::new(MapFetch::default()))
        });
        let asks = rt.reconnects().unwrap();
        // The Hello before connecting carried no address.
        assert!(rt.capabilities().external_ips.is_empty());
        let (_coord, session) = session_pair().await;
        rt.connected(session.handle());
        let c = rt.counts();
        rt.shared
            .dht_counts
            .set_external_ip(Some("81.2.69.7".parse().unwrap()));
        until("a fresh session asked for", || {
            let n = c.reconnects.load(Ordering::Relaxed);
            (n == 1, n)
        })
        .await;
        assert_eq!(*asks.borrow(), 1);
        // The address flaps: held back until the window passes.
        rt.shared
            .dht_counts
            .set_external_ip(Some("81.2.69.8".parse().unwrap()));
        until("the flap was debounced", || {
            let n = c.reconnects_debounced.load(Ordering::Relaxed);
            (n > 0, n)
        })
        .await;
        assert_eq!(c.reconnects.load(Ordering::Relaxed), 1);
        assert_eq!(*asks.borrow(), 1);
        // The next Hello carries the address; it then matches, and nothing
        // more is asked for.
        assert_eq!(
            rt.capabilities().external_ips,
            vec!["81.2.69.8".parse::<std::net::IpAddr>().unwrap()]
        );
        rt.shutdown().await;
    }

    /// A session that doesn't come back within the linger stops the DHT.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_session_gone_past_the_linger_stops_the_dht() {
        let (mut rt, _coord, _session) = started_with_dht(Duration::from_millis(200)).await;
        let counts = rt.dht_counts();
        let running = rt.dht_running().unwrap();
        rt.disconnected();
        until("stopped", || {
            let now = *running.borrow();
            (now.is_none(), now)
        })
        .await;
        assert_eq!(counts.pauses.load(Ordering::Relaxed), 1);
        assert_eq!(counts.stops.load(Ordering::Relaxed), 1);
        rt.shutdown().await;
    }

    /// Plan Q6's measured breakdown (WK-10c): resident memory as each part
    /// of a worker fills to its default size, with a have-filter for 10M
    /// stored swarm hashes. Linux only; run it alone, since RSS is the
    /// process's:
    /// `cargo test -p lodestar --bin lodestar -- --ignored --exact --nocapture worker::runtime::tests::rss_breakdown`.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    #[ignore = "a measurement, run on its own"]
    async fn rss_breakdown() {
        use lodestar_cluster::have::{HaveHeader, HaveKey, chunk_range, chunks_for};
        use lodestar_cluster::proto::SnapshotKind;
        const KEYS: u32 = 50_000;
        const STORED: u64 = 10_000_000;
        let mib = || super::super::budget::rss_kib().unwrap_or(0) as f64 / 1024.0;
        let mut rows: Vec<(&str, f64)> = Vec::new();
        let mut last = mib();
        let row = |rows: &mut Vec<(&str, f64)>, name, last: &mut f64| {
            let now = mib();
            rows.push((name, now - *last));
            *last = now;
        };
        rows.push(("process and Tokio before the runtime", last));

        let mut rt = Runtime::start(Config {
            dht: Some(dht_config()),
            ..config(Arc::new(Stalled))
        });
        let (_coord, session) = session_pair().await;
        rt.connected(session.handle());
        row(&mut rows, "runtime, session, idle", &mut last);

        let shard = ShardSpec::new(0..128, 0x0f).unwrap();
        let mut a = Assignment::from_message(&assign_dht(1, Some(shard), Intensity::Balanced))
            .unwrap()
            .unwrap();
        a.pool_cap = KEYS;
        rt.event(WorkerEvent::Assign(a.to_message().unwrap()));
        let (_, chunks) = filter_chunks().await;
        for c in chunks {
            rt.event(WorkerEvent::Snapshot(c));
        }
        let running = rt.dht_running().unwrap();
        until("started", || {
            (
                rt.is_started() && running.borrow().is_some(),
                rt.is_started(),
            )
        })
        .await;
        row(
            &mut rows,
            "filter snapshot, stages, DHT node (no peers)",
            &mut last,
        );

        // The have-filter for 10M stored, as the coordinator sizes it.
        let (capacity, per_key, hashes) = crate::cluster::have::worker_params(STORED);
        let bits = (capacity * per_key).div_ceil(64) * 64;
        let header = |generation, serial| HaveHeader {
            generation,
            serial,
            next_delta: 0,
            key: HaveKey::from_bytes([9; 16]),
            bits,
            hashes,
            capacity,
            count: STORED,
            chunks: chunks_for(bits),
        };
        let send = |rt: &mut Runtime, generation, serial, upto: u32| {
            rt.event(WorkerEvent::HaveSnapshot(
                header(generation, serial).encode(),
            ));
            for seq in 0..upto {
                let r = chunk_range(bits, seq);
                rt.event(WorkerEvent::Snapshot(SnapshotChunk {
                    kind: SnapshotKind::HaveFilter,
                    version: serial,
                    seq,
                    total: chunks_for(bits),
                    bytes: vec![0x55; r.len()],
                }));
            }
        };
        send(&mut rt, 1, 1, chunks_for(bits));
        until("the have-filter is in", || (rt.shared.have.is_ready(), ())).await;
        row(&mut rows, "have-filter, 10M stored", &mut last);
        // A swap: the next generation all but its last chunk, both held.
        send(&mut rt, 2, 2, chunks_for(bits) - 1);
        row(&mut rows, "have-filter swap (both held)", &mut last);

        // The pool at its cap: keys that never fetch.
        let grants = (0..KEYS)
            .map(|i| {
                let mut key = [0u8; 20];
                key[..4].copy_from_slice(&i.to_be_bytes());
                key[4] = 0xa5;
                GrantItem::Granted {
                    key,
                    token: u64::from(i) + 1,
                    ttl_secs: 600,
                }
            })
            .collect();
        rt.event(WorkerEvent::Regranted(grants));
        let pool = lock(&rt.shared.pool).clone().unwrap();
        until("the pool holds its cap", || {
            let st = pool.stats();
            let held = st.pending.load(Ordering::Relaxed)
                + st.active.load(Ordering::Relaxed)
                + st.waiting.load(Ordering::Relaxed);
            (held >= u64::from(KEYS) * 99 / 100, held)
        })
        .await;
        row(
            &mut rows,
            "pool at 50,000 keys (and their leases)",
            &mut last,
        );

        let total = mib();
        println!(
            "\nWorker RSS breakdown (MiB, deltas; {} build):",
            if cfg!(debug_assertions) {
                "debug"
            } else {
                "release"
            }
        );
        for (name, d) in &rows {
            println!("  {name:<48} {d:>7.1}");
        }
        println!("  {:<48} {total:>7.1}", "total");
        assert!(total > 0.0 || !cfg!(target_os = "linux"));
        drop(session);
        rt.shutdown().await;
    }

    /// WK-5: a new session's first `Assign` applies whatever its revision
    /// (a coordinator whose clock went back numbers its plans lower); after
    /// it, only a newer one does.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_new_sessions_first_assign_applies_a_lower_revision() {
        let mut rt = Runtime::start(config(Arc::new(MapFetch::default())));
        let (_coord, session) = session_pair().await;
        rt.connected(session.handle());
        rt.event(WorkerEvent::Assign(assignment(50, 0, 1)));
        assert_eq!(rt.assignment().map(|a| a.revision), Some(50));
        rt.disconnected();
        let (_coord2, session2) = session_pair().await;
        rt.connected(session2.handle());
        rt.event(WorkerEvent::Assign(assignment(7, 0, 1)));
        assert_eq!(
            rt.assignment().map(|a| a.revision),
            Some(7),
            "the first applies"
        );
        rt.event(WorkerEvent::Assign(assignment(6, 0, 1)));
        assert_eq!(
            rt.assignment().map(|a| a.revision),
            Some(7),
            "then only newer"
        );
        rt.event(WorkerEvent::Assign(assignment(8, 0, 1)));
        assert_eq!(rt.assignment().map(|a| a.revision), Some(8));
        rt.shutdown().await;
    }

    /// A later `Assign` with fetching off pauses the pool and claims
    /// nothing; turned on again, both resume.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_late_fetch_off_pauses_the_pool_and_the_claims() {
        let mut rt = Runtime::start(config(Arc::new(MapFetch::default())));
        let (_coord, session) = session_pair().await;
        rt.connected(session.handle());
        rt.event(WorkerEvent::Assign(assignment(1, 0, 1)));
        let (_, chunks) = filter_chunks().await;
        for c in chunks {
            rt.event(WorkerEvent::Snapshot(c));
        }
        until("started", || (rt.is_started(), ())).await;
        let paused = |rt: &Runtime| *rt.shared.pool_told.borrow();
        until("the pool runs", || (!paused(&rt), ())).await;
        let off = |revision, fetch| {
            let mut a = Assignment::from_message(&assignment(revision, 0, 1))
                .unwrap()
                .unwrap();
            a.fetch = fetch;
            a.to_message().unwrap()
        };
        rt.event(WorkerEvent::Assign(off(2, false)));
        until("the pool paused", || (paused(&rt), ())).await;
        let c = rt.counts();
        assert!(rt.finds().found(found(77)));
        until("the find was held back", || {
            let n = c.held_back.load(Ordering::Relaxed);
            (n == 1, n)
        })
        .await;
        assert_eq!(c.claimed.load(Ordering::Relaxed), 0);
        rt.event(WorkerEvent::Assign(off(3, true)));
        until("the pool resumed", || (!paused(&rt), ())).await;
        rt.shutdown().await;
    }

    /// A stage task that ends by itself is noticed: `failed` completes, so
    /// `lodestar worker` stops with an error and is started again. A stop
    /// the runtime asked for isn't a failure.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_stage_that_dies_is_noticed() {
        let mut rt = Runtime::start(config(Arc::new(MapFetch::default())));
        let (_coord, session) = session_pair().await;
        rt.connected(session.handle());
        rt.event(WorkerEvent::Assign(assignment(1, 0, 1)));
        let (_, chunks) = filter_chunks().await;
        for c in chunks {
            rt.event(WorkerEvent::Snapshot(c));
        }
        until("started", || (rt.is_started(), ())).await;
        let failed = rt.failed();
        // As a panic would end it.
        lock(&rt.shared.stages).as_ref().unwrap().pool.abort();
        tokio::time::timeout(DEADLINE, failed)
            .await
            .expect("the dead pool was noticed");
        rt.shutdown().await;

        // A drain stops the stages: not a failure.
        let mut rt = Runtime::start(config(Arc::new(MapFetch::default())));
        let (_coord2, session2) = session_pair().await;
        rt.connected(session2.handle());
        rt.event(WorkerEvent::Assign(assignment(1, 0, 1)));
        let (_, chunks) = filter_chunks().await;
        for c in chunks {
            rt.event(WorkerEvent::Snapshot(c));
        }
        until("started", || (rt.is_started(), ())).await;
        let _ = rt.drain().await;
        let failed = rt.failed();
        assert!(
            tokio::time::timeout(SUPERVISE_EVERY * 3, failed)
                .await
                .is_err(),
            "a drain isn't a failure"
        );
        rt.shutdown().await;
    }

    /// Claims stop at the pool's room: leases held never pass its cap.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn claims_stop_at_the_pools_room() {
        let mut rt = Runtime::start(config(Arc::new(MapFetch::default())));
        let (_coord, session) = session_pair().await;
        rt.connected(session.handle());
        let mut a = Assignment::from_message(&assignment(1, 0, 1))
            .unwrap()
            .unwrap();
        a.pool_cap = 2;
        rt.event(WorkerEvent::Assign(a.to_message().unwrap()));
        let (_, chunks) = filter_chunks().await;
        for c in chunks {
            rt.event(WorkerEvent::Snapshot(c));
        }
        until("started", || (rt.is_started(), ())).await;
        assert_eq!(rt.shared.pool_room(), 2);
        // The pool is full: two keys held.
        rt.shared.leases.insert(InfoHash([1; 20]), Token(1));
        rt.shared.leases.insert(InfoHash([2; 20]), Token(2));
        assert_eq!(rt.shared.pool_room(), 0);
        let c = rt.counts();
        assert!(rt.finds().found(found(9)));
        until("held back", || {
            let n = c.held_back.load(Ordering::Relaxed);
            (n == 1, n)
        })
        .await;
        assert_eq!(c.claimed.load(Ordering::Relaxed), 0);
        rt.shutdown().await;
    }

    fn claim_item(n: u8) -> ClaimItem {
        ClaimItem {
            key: [n; 20],
            source: 1,
            strategy: 0,
            discovered_ms: 0,
            hints: vec![],
        }
    }

    /// X-wk-busy-retry: the retry is bounded. At most `capacity` keys at
    /// once (one more is `Full`), each claimed at most `attempts` more
    /// times; and keys come due in order.
    #[test]
    fn busy_retries_are_bounded() {
        let r = BusyRetries::new(BusyRetry {
            base: Duration::from_millis(100),
            cap: Duration::from_millis(400),
            attempts: 2,
            capacity: 2,
        });
        let now = tokio::time::Instant::now();
        let later = now + Duration::from_secs(10);
        assert!(matches!(
            r.busy(claim_item(1), vec![], now, 0),
            Busy::Again(_)
        ));
        assert!(matches!(
            r.busy(claim_item(2), vec![], now, 0),
            Busy::Again(_)
        ));
        assert_eq!(r.busy(claim_item(3), vec![], now, 0), Busy::Full);
        assert_eq!(r.len(), (2, 2));
        // Both come due; key 1's second attempt, then its limit.
        assert_eq!(r.due(later).0.len(), 2);
        assert!(matches!(
            r.busy(claim_item(1), vec![], now, 0),
            Busy::Again(_)
        ));
        r.due(later);
        assert_eq!(r.busy(claim_item(1), vec![], now, 0), Busy::GaveUp);
        assert_eq!(r.len(), (1, 0));
        // Room again for a new key.
        assert!(matches!(
            r.busy(claim_item(3), vec![], now, 0),
            Busy::Again(_)
        ));
        // With jitter 0 the wait is the step's lower half.
        let r = BusyRetries::new(BusyRetry::default());
        assert_eq!(
            r.busy(claim_item(9), vec![], now, 0),
            Busy::Again(Duration::from_secs(1))
        );
    }

    /// #637's review: a key waits at most once. A second `Busy` while it
    /// waits (found again meanwhile) keeps its place and uses no attempt;
    /// a key that ends and comes back isn't fed back twice; and
    /// `waiting <= tries` holds throughout.
    #[test]
    fn a_busy_key_waits_at_most_once() {
        let r = BusyRetries::new(BusyRetry {
            base: Duration::from_millis(100),
            cap: Duration::from_millis(400),
            attempts: 2,
            capacity: 4,
        });
        let now = tokio::time::Instant::now();
        let later = now + Duration::from_secs(100);
        assert!(matches!(
            r.busy(claim_item(1), vec![], now, 0),
            Busy::Again(_)
        ));
        assert_eq!(r.busy(claim_item(1), vec![], now, 0), Busy::Waiting);
        assert_eq!(r.len(), (1, 1));
        let (due, _) = r.due(later);
        assert_eq!(due.len(), 1, "one copy");
        // Its claim is out: the next `Busy` is its second attempt, not a
        // third (the re-find's answer used none).
        assert!(matches!(
            r.busy(claim_item(1), vec![], now, 0),
            Busy::Again(_)
        ));
        // Ended while waiting: its wait goes too.
        r.done(&[1; 20]);
        assert_eq!(r.len(), (0, 0));
        assert!(matches!(
            r.busy(claim_item(1), vec![], now, 0),
            Busy::Again(_)
        ));
        assert_eq!(r.len(), (1, 1));
        let (due, next) = r.due(later);
        assert_eq!(due.len(), 1, "fed back once");
        assert_eq!(next, None);
    }

    /// #637's review: a give-up is logged once per streak. The streak ends
    /// with a batch answered with no `Busy` at all.
    #[test]
    fn give_ups_are_logged_once_per_streak() {
        let mut giving_up = false;
        assert!(streak(&mut giving_up, 3, 1), "the first give-up logs");
        assert!(!streak(&mut giving_up, 2, 2), "the next doesn't");
        assert!(!streak(&mut giving_up, 4, 0), "busy, no give-up: still on");
        assert!(!streak(&mut giving_up, 1, 1));
        assert!(!streak(&mut giving_up, 0, 0), "no busy: the streak ends");
        assert!(streak(&mut giving_up, 1, 1), "and the next logs again");
    }

    /// The next claim the coordinator gets, skipping everything else.
    async fn next_claim(
        coord: &mut lodestar_cluster::CoordinatorLink,
    ) -> lodestar_cluster::proto::ClaimBatch {
        use lodestar_cluster::proto::ToCoordinator;
        tokio::time::timeout(DEADLINE, async {
            loop {
                match coord.recv().await {
                    Ok(Some(ToCoordinator::Claim(b))) => return b,
                    Ok(Some(_)) => {}
                    other => panic!("the link ended: {other:?}"),
                }
            }
        })
        .await
        .expect("a claim in time")
    }

    /// Answers a claim: `busy` keys denied as busy (or `reason`), the rest
    /// granted.
    async fn answer(
        coord: &mut lodestar_cluster::CoordinatorLink,
        batch: &lodestar_cluster::proto::ClaimBatch,
        deny: impl Fn(&[u8; 20]) -> Option<DenyReason>,
    ) {
        use lodestar_cluster::proto::{GrantBatch, ToWorker};
        let items = batch
            .items
            .iter()
            .enumerate()
            .map(|(i, c)| match deny(&c.key) {
                Some(reason) => GrantItem::Denied { key: c.key, reason },
                None => GrantItem::Granted {
                    key: c.key,
                    token: 100 + i as u64,
                    ttl_secs: 600,
                },
            })
            .collect();
        coord
            .send(ToWorker::Grants(GrantBatch {
                batch: batch.batch,
                items,
            }))
            .await
            .unwrap();
    }

    /// A started runtime whose `Busy` retries wait milliseconds.
    async fn busy_rig(
        attempts: u8,
    ) -> (
        Runtime,
        lodestar_cluster::CoordinatorLink,
        lodestar_cluster::WorkerSession,
    ) {
        busy_rig_with(attempts, Duration::from_millis(20)).await
    }

    async fn busy_rig_with(
        attempts: u8,
        base: Duration,
    ) -> (
        Runtime,
        lodestar_cluster::CoordinatorLink,
        lodestar_cluster::WorkerSession,
    ) {
        busy_rig_full(BusyRetry {
            base,
            cap: base * 4,
            attempts,
            capacity: 16,
        })
        .await
    }

    async fn busy_rig_full(
        policy: BusyRetry,
    ) -> (
        Runtime,
        lodestar_cluster::CoordinatorLink,
        lodestar_cluster::WorkerSession,
    ) {
        let mut cfg = config(Arc::new(MapFetch::default()));
        cfg.busy_retry = policy;
        let mut rt = Runtime::start(cfg);
        let (coord, session) = session_pair().await;
        rt.connected(session.handle());
        rt.event(WorkerEvent::Assign(assignment(1, 0, 1)));
        let (_, chunks) = filter_chunks().await;
        for c in chunks {
            rt.event(WorkerEvent::Snapshot(c));
        }
        until("started", || (rt.is_started(), ())).await;
        (rt, coord, session)
    }

    /// X-wk-busy-retry: a key answered `Busy` is claimed again later, and
    /// granted then.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_busy_key_is_claimed_again_and_granted() {
        let (rt, mut coord, _session) = busy_rig(5).await;
        assert!(rt.finds().found(found(7)));
        let first = next_claim(&mut coord).await;
        assert_eq!(first.items[0].key, [7; 20]);
        answer(&mut coord, &first, |_| Some(DenyReason::Busy)).await;
        let again = next_claim(&mut coord).await;
        assert_eq!(again.items[0].key, [7; 20], "the same key, claimed again");
        assert_eq!(again.items[0].discovered_ms, first.items[0].discovered_ms);
        answer(&mut coord, &again, |_| None).await;
        until("granted", || {
            let held = rt.shared.leases.len();
            (held == 1, held)
        })
        .await;
        let c = rt.counts();
        assert_eq!(c.busy_retried.load(Ordering::Relaxed), 1);
        assert_eq!(c.busy_gave_up.load(Ordering::Relaxed), 0);
        rt.shutdown().await;
    }

    /// X-wk-own-lease-lost: the holder sights a key it has fetched (its
    /// result is out, not committed yet) and its claim is answered with its
    /// own lease. The lease stays, nothing is reported lost, the key isn't
    /// fetched again, and the grant isn't counted as a new one.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_holders_own_lease_coming_back_never_ends_it() {
        let raw = info("A key sighted by its holder");
        let key = key_of(&raw);
        let fetch = Arc::new(MapFetch::default());
        lock(&fetch.0).insert(key, raw);
        let mut rt = Runtime::start(config(fetch.clone()));
        let (mut coord, session) = session_pair().await;
        rt.connected(session.handle());
        rt.event(WorkerEvent::Assign(assignment(1, 0, 1)));
        let (_, chunks) = filter_chunks().await;
        for c in chunks {
            rt.event(WorkerEvent::Snapshot(c));
        }
        until("started", || (rt.is_started(), ())).await;
        let sighting = || {
            let mut f = Found::key(key, 0);
            f.peers = vec!["8.8.8.8:6881".parse().unwrap()];
            f
        };

        assert!(rt.finds().found(sighting()));
        let first = next_claim(&mut coord).await;
        answer(&mut coord, &first, |_| None).await;
        // Fetched and sent: the pool is done with it, the lease waits for
        // the commit.
        let _results = next_results(&mut coord).await;

        // Sighted again; the coordinator answers with the same lease.
        assert!(rt.finds().found(sighting()));
        let again = next_claim(&mut coord).await;
        assert_eq!(again.items[0].key, key.0);
        answer(&mut coord, &again, |_| None).await;
        let c = rt.counts();
        until("the lease came back", || {
            let n = c.held_again.load(Ordering::Relaxed);
            (n == 1, n)
        })
        .await;
        // The pool took the reply and turned it away as one for a key it's
        // done with (a duplicate it counts, and never a lost lease).
        let pool = lock(&rt.shared.pool).clone().expect("the stages run");
        until("the pool saw the reply", || {
            let n = pool.stats().duplicates.load(Ordering::Relaxed);
            (n >= 1, n)
        })
        .await;
        assert_eq!(c.lost.load(Ordering::Relaxed), 0, "its own lease ended");
        assert_eq!(c.granted.load(Ordering::Relaxed), 1, "one grant");
        // Its result carries it: out, not held twice.
        assert_eq!(
            (rt.shared.leases.len(), rt.shared.leases.pending_len()),
            (0, 1),
            "the lease is still the result's"
        );

        // Committed: finished, and the same lease coming back after the
        // commit (the two travel different paths) is still not a new one.
        rt.event(WorkerEvent::Accepted {
            batch: 1,
            accepted: true,
        });
        until("committed", || {
            let n = rt.shared.leases.pending_len();
            (n == 0, n)
        })
        .await;
        assert!(rt.finds().found(sighting()));
        let late = next_claim(&mut coord).await;
        answer(&mut coord, &late, |_| None).await;
        until("the lease came back after its commit", || {
            let n = c.held_again.load(Ordering::Relaxed);
            (n == 2, n)
        })
        .await;
        until("the pool saw that reply too", || {
            let n = pool.stats().duplicates.load(Ordering::Relaxed);
            (n >= 2, n)
        })
        .await;
        assert_eq!(c.lost.load(Ordering::Relaxed), 0);
        assert_eq!(rt.shared.leases.len(), 0);
        rt.shutdown().await;
    }

    /// X-wk-busy-retry: a key the coordinator keeps answering `Busy` is
    /// claimed `attempts` more times, then dropped and counted (and the
    /// streak logged once).
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_key_busy_past_its_limit_is_dropped() {
        let (rt, mut coord, _session) = busy_rig(2).await;
        assert!(rt.finds().found(found(8)));
        for _ in 0..3 {
            let b = next_claim(&mut coord).await;
            assert_eq!(b.items[0].key, [8; 20]);
            answer(&mut coord, &b, |_| Some(DenyReason::Busy)).await;
        }
        let c = rt.counts();
        until("given up", || {
            let n = c.busy_gave_up.load(Ordering::Relaxed);
            (n == 1, n)
        })
        .await;
        assert_eq!(c.busy_retried.load(Ordering::Relaxed), 2);
        assert_eq!(c.claimed.load(Ordering::Relaxed), 3);
        // Nothing more for it: the next claim is a new key's alone.
        assert!(rt.finds().found(found(9)));
        let next = next_claim(&mut coord).await;
        let keys: Vec<[u8; 20]> = next.items.iter().map(|i| i.key).collect();
        assert_eq!(keys, vec![[9; 20]]);
        rt.shutdown().await;
    }

    /// #637's review: a retried key gets the same checks as a new find. An
    /// `Assign` that turns fetching off ends its retry when it comes due,
    /// counted as held back; turned on again, it isn't claimed.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_retry_due_while_fetching_is_off_is_dropped() {
        let (mut rt, mut coord, _session) = busy_rig_with(5, Duration::from_secs(2)).await;
        assert!(rt.finds().found(found(7)));
        let first = next_claim(&mut coord).await;
        answer(&mut coord, &first, |_| Some(DenyReason::Busy)).await;
        let with_fetch = |revision, fetch| {
            let mut a = Assignment::from_message(&assignment(revision, 0, 1))
                .unwrap()
                .unwrap();
            a.fetch = fetch;
            a.to_message().unwrap()
        };
        // At least 1 s before it's due (equal jitter on a 2 s step), so a
        // stalled runner still turns fetching off first.
        rt.event(WorkerEvent::Assign(with_fetch(2, false)));
        let c = rt.counts();
        until("the retry held back", || {
            let n = c.held_back.load(Ordering::Relaxed);
            (n == 1, n)
        })
        .await;
        rt.event(WorkerEvent::Assign(with_fetch(3, true)));
        assert!(rt.finds().found(found(9)));
        let next = next_claim(&mut coord).await;
        let keys: Vec<[u8; 20]> = next.items.iter().map(|i| i.key).collect();
        assert_eq!(keys, vec![[9; 20]], "the retried key isn't claimed");
        rt.shutdown().await;
    }

    /// #637's delta review: a key the coordinator's answer leaves out ends
    /// its retry, so it doesn't hold a slot for ever. With room for one
    /// key, a second key's `Busy` is retried, not dropped as `Full`.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_unanswered_key_frees_its_retry_slot() {
        let (rt, mut coord, _session) = busy_rig_full(BusyRetry {
            base: Duration::from_millis(20),
            cap: Duration::from_millis(80),
            attempts: 5,
            capacity: 1,
        })
        .await;
        assert!(rt.finds().found(found(7)));
        let first = next_claim(&mut coord).await;
        answer(&mut coord, &first, |_| Some(DenyReason::Busy)).await;
        let again = next_claim(&mut coord).await;
        assert_eq!(again.items[0].key, [7; 20]);
        // Answered with the key left out.
        let empty = lodestar_cluster::proto::ClaimBatch {
            items: vec![],
            ..again.clone()
        };
        answer(&mut coord, &empty, |_| None).await;
        assert!(rt.finds().found(found(8)));
        let b = next_claim(&mut coord).await;
        assert_eq!(b.items[0].key, [8; 20]);
        answer(&mut coord, &b, |_| Some(DenyReason::Busy)).await;
        let c = rt.counts();
        until("the second key retried", || {
            let n = c.busy_retried.load(Ordering::Relaxed);
            (n == 2, n)
        })
        .await;
        assert_eq!(c.busy_dropped.load(Ordering::Relaxed), 0);
        rt.shutdown().await;
    }

    /// X-wk-busy-retry: any other denial drops the key at once, as before.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_key_denied_for_another_reason_is_dropped_at_once() {
        let (rt, mut coord, _session) = busy_rig(5).await;
        for (n, reason) in [
            (4, DenyReason::Stored),
            (5, DenyReason::HeldElsewhere),
            (6, DenyReason::Draining),
        ] {
            assert!(rt.finds().found(found(n)));
            let b = next_claim(&mut coord).await;
            assert_eq!(b.items[0].key, [n; 20]);
            answer(&mut coord, &b, |_| Some(reason)).await;
        }
        // The loop answers in order: by the time this key is claimed, the
        // three denials were handled, and none went to the retry.
        assert!(rt.finds().found(found(9)));
        let next = next_claim(&mut coord).await;
        let keys: Vec<[u8; 20]> = next.items.iter().map(|i| i.key).collect();
        assert_eq!(keys, vec![[9; 20]]);
        assert_eq!(rt.counts().busy_retried.load(Ordering::Relaxed), 0);
        rt.shutdown().await;
    }

    /// A dictionary named by `Assign` must arrive first too.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_named_dictionary_is_waited_for() {
        let mut rt = Runtime::start(config(Arc::new(MapFetch::default())));
        rt.event(WorkerEvent::Assign(assignment(1, 9, 1)));
        let (version, chunks) = filter_chunks().await;
        for c in chunks {
            rt.event(WorkerEvent::Snapshot(c));
        }
        until("installed", || (rt.filter_version() == version, ())).await;
        assert!(!rt.is_started(), "the dictionary isn't in");
        let dict = DictSnapshot {
            id: 9,
            bytes: vec![b'd'; 1024],
        }
        .encode()
        .unwrap();
        rt.event(WorkerEvent::Snapshot(chunk(
            SnapshotKind::Dictionary,
            9,
            &dict,
            0,
        )));
        until("started", || (rt.is_started(), ())).await;
        rt.shutdown().await;
    }

    /// Hints reach the pool only for peers the network policy allows.
    #[tokio::test]
    async fn hints_pass_the_network_policy() {
        let rt = Runtime::start(Config::new(W, Arc::new(MapFetch::default())));
        let item = HintItem {
            key: [2; 20],
            token: 3,
            hints: vec![
                "10.0.0.1:6881".parse().unwrap(),
                "127.0.0.1:6881".parse().unwrap(),
                "8.8.8.8:6881".parse().unwrap(),
                "8.8.4.4:0".parse().unwrap(),
            ],
        };
        hints(&rt.shared, vec![item]);
        assert_eq!(rt.counts().hints_refused.load(Ordering::Relaxed), 3);
        let mut rx = lock(&rt.shared.pool_rx).take().unwrap();
        let d = rx.try_recv().unwrap();
        assert_eq!(
            d.found.peers,
            vec!["8.8.8.8:6881".parse::<SocketAddr>().unwrap()]
        );
        assert!(d.reply);
        assert!(rx.try_recv().is_err());
        rt.shutdown().await;
    }

    /// Holds every fetch until `open`, and records what it dialled, what
    /// finished and what was dropped unfinished.
    #[derive(Default)]
    struct GatedState {
        open: Mutex<Option<watch::Sender<bool>>>,
        info: Mutex<HashMap<InfoHash, Vec<u8>>>,
        dials: Mutex<Vec<SocketAddr>>,
        finished: AtomicU64,
        dropped: Arc<AtomicU64>,
    }

    struct Gated(Arc<GatedState>);

    /// Counts a fetch dropped before it finished.
    struct DropFlag(Option<Arc<AtomicU64>>);

    impl Drop for DropFlag {
        fn drop(&mut self) {
            if let Some(n) = self.0.take() {
                n.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    impl Fetcher for Gated {
        fn fetch(
            &self,
            peer: SocketAddr,
            key: InfoHash,
        ) -> BoxFuture<'static, Result<Fetched, FailureKind>> {
            lock(&self.0.dials).push(peer);
            let s = self.0.clone();
            let mut open = lock(&s.open)
                .get_or_insert_with(|| watch::channel(false).0)
                .subscribe();
            Box::pin(async move {
                let mut flag = DropFlag(Some(s.dropped.clone()));
                let _ = open.wait_for(|o| *o).await;
                flag.0 = None;
                s.finished.fetch_add(1, Ordering::Relaxed);
                lock(&s.info)
                    .get(&key)
                    .cloned()
                    .map(|info| Fetched {
                        info,
                        v2: false,
                        encrypted: false,
                        pex: Vec::new(),
                        client: None,
                    })
                    .ok_or(FailureKind::Refused)
            })
        }
    }

    /// The brief's gate (WK-10e): an `Assign` that changes this worker's
    /// share applies to the running pool with no restart. A fetch already
    /// out to a peer IP that's no longer ours finishes; a new key's peer on
    /// that IP is never dialled, and its peer on an IP still ours is.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_live_share_change_applies_without_a_restart() {
        use lodestar_cluster::assign::peer_ip_bucket;
        let gated = Arc::new(GatedState::default());
        let mut rt = Runtime::start(config(Arc::new(Gated(gated.clone()))));
        let (_coord, session) = session_pair_with(1).await;
        rt.connected(session.handle());
        rt.event(WorkerEvent::Assign(assignment(1, 0, 1)));
        let (_, chunks) = filter_chunks().await;
        for c in chunks {
            rt.event(WorkerEvent::Snapshot(c));
        }
        until("started", || (rt.is_started(), ())).await;

        // Two public addresses in different buckets of two.
        let ours: SocketAddr = "203.0.113.9:6881".parse().unwrap();
        let theirs = (1..=250u8)
            .map(|i| SocketAddr::from(([198, 51, 100, i], 6881)))
            .find(|p| peer_ip_bucket(p.ip(), 2) != peer_ip_bucket(ours.ip(), 2))
            .unwrap();
        let first = info("in flight");
        lock(&gated.info).insert(key_of(&first), first.clone());
        granted(
            &rt.shared,
            key_of(&first),
            Token(1),
            vec![theirs],
            unix_now(),
        );
        until("dialled", || (lock(&gated.dials).contains(&theirs), ())).await;

        // The new share: two groups, ours the one `ours` is in.
        let share = Politeness {
            peer_ip_groups: 2,
            peer_ip_group: peer_ip_bucket(ours.ip(), 2),
            per_peer_ip: 1,
            bep51_interval_scale: 2,
        };
        let mut a = Assignment::from_message(&assignment(2, 0, 1))
            .unwrap()
            .unwrap();
        a.politeness = share;
        rt.event(WorkerEvent::Assign(a.to_message().unwrap()));
        until("the pool told", || {
            let told = *rt.shared.share_told.borrow();
            (told == share, told)
        })
        .await;
        // Ordered behind the pool's own commands, so it has the new cap.
        let pool = lock(&rt.shared.pool).clone().unwrap();
        pool.queued().await;

        let second = info("after the change");
        lock(&gated.info).insert(key_of(&second), second.clone());
        granted(
            &rt.shared,
            key_of(&second),
            Token(2),
            vec![
                SocketAddr::new(theirs.ip(), 6882),
                SocketAddr::new(ours.ip(), 6881),
            ],
            unix_now(),
        );
        until("dialled ours", || (lock(&gated.dials).contains(&ours), ())).await;
        lock(&gated.open)
            .get_or_insert_with(|| watch::channel(false).0)
            .send_replace(true);
        until("both fetches finished", || {
            let n = gated.finished.load(Ordering::Relaxed);
            (n == 2, n)
        })
        .await;
        let dials = lock(&gated.dials).clone();
        assert_eq!(
            dials.iter().filter(|p| p.ip() == theirs.ip()).count(),
            1,
            "only the fetch from before the change: {dials:?}"
        );
        assert_eq!(gated.dropped.load(Ordering::Relaxed), 0, "nothing dropped");
        rt.shutdown().await;
    }

    /// #617's review: a replan that changes the share after the pool is
    /// built but before the pause task first runs still reaches the pool.
    /// The task starts from the share the pool was built with, not from
    /// the watch, so it applies the new one and reports it only then.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_share_changed_before_the_pause_task_runs_reaches_the_pool() {
        use lodestar_cluster::assign::peer_ip_bucket;
        let gated = Arc::new(GatedState::default());
        let (disc_tx, discoveries) = mpsc::channel(16);
        let (out, _out_rx) = mpsc::channel(16);
        let (stop, shutdown) = Shutdown::new();
        // Built alone: up to two connections to any IP.
        let (pool, task) = lodestar_sources::pool::spawn(
            PoolConfig {
                ip_cap: lodestar_sources::IpCap(Some(Arc::new(ShareCap(Politeness::ALONE)))),
                ..PoolConfig::default()
            },
            Arc::new(Gated(gated.clone())),
            lodestar_sources::PoolIo {
                discoveries,
                peer_finders: Vec::new(),
                out,
                funnel: Arc::new(FunnelCounts::default()),
                shutdown,
            },
            HashMap::new(),
            Vec::new(),
        );
        let ours: SocketAddr = "203.0.113.9:6881".parse().unwrap();
        let theirs = (1..=250u8)
            .map(|i| SocketAddr::from(([198, 51, 100, i], 6881)))
            .find(|p| peer_ip_bucket(p.ip(), 2) != peer_ip_bucket(ours.ip(), 2))
            .unwrap();
        let split = Politeness {
            peer_ip_groups: 2,
            peer_ip_group: peer_ip_bucket(ours.ip(), 2),
            per_peer_ip: 1,
            bep51_interval_scale: 1,
        };
        // The replan lands before the pause task's first poll.
        let (share_tx, latest) = watch::channel(Politeness::ALONE);
        share_tx.send_replace(split);
        let told = watch::channel(Politeness::ALONE).0;
        let (_want_tx, want) = watch::channel(false);
        let pauser = tokio::spawn(apply_pause(
            pool.clone(),
            want,
            watch::channel(false).0,
            Share {
                built_with: Politeness::ALONE,
                latest,
                told: told.clone(),
            },
        ));
        until("the split told to the pool", || {
            let now = *told.borrow();
            (now == split, now)
        })
        .await;
        // Ordered behind the pool's own commands, so it has the split.
        pool.queued().await;
        let mut found = Found::key(InfoHash([7; 20]), 0);
        found.peers = vec![theirs, ours];
        disc_tx
            .send(Discovery {
                source: SourceId::new(DHT_SOURCE).unwrap(),
                found,
                at: 0,
                reply: false,
            })
            .await
            .unwrap();
        until("dialled ours", || (lock(&gated.dials).contains(&ours), ())).await;
        assert!(
            !lock(&gated.dials).contains(&theirs),
            "the pool kept the share it was built with"
        );
        pauser.abort();
        let _ = stop.send(true);
        let _ = task.await;
    }

    /// Never fetches: every key waits on a peer that doesn't answer, so
    /// the pool holds them while a shutdown asks it to stop.
    struct Stalled;

    impl Fetcher for Stalled {
        fn fetch(
            &self,
            _: SocketAddr,
            _: InfoHash,
        ) -> BoxFuture<'static, Result<Fetched, FailureKind>> {
            Box::pin(std::future::pending())
        }
    }

    /// The brief's gate: a shutdown whose session never takes anything
    /// (a coordinator that never reads, and the outbound queue full) finishes
    /// within its drain deadline. Fetched results wait on the outbound
    /// queue and fetches hang; neither holds the stop up.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_stop_with_a_stalled_session_finishes_within_its_deadline() {
        let fetch = Arc::new(MapFetch::default());
        let raws: Vec<Vec<u8>> = (0..40).map(|i| info(&format!("stalled {i}"))).collect();
        for r in &raws {
            lock(&fetch.0).insert(key_of(r), r.clone());
        }
        let mut cfg = config(fetch);
        // One record fills it.
        cfg.outbound_bytes = 1;
        let mut rt = Runtime::start(cfg);
        let (_coord, session) = session_pair_with(1).await;
        rt.connected(session.handle());
        rt.event(WorkerEvent::Assign(assignment(1, 0, 1)));
        let (_, chunks) = filter_chunks().await;
        for c in chunks {
            rt.event(WorkerEvent::Snapshot(c));
        }
        until("started", || (rt.is_started(), ())).await;
        for (n, r) in raws.iter().enumerate() {
            granted(
                &rt.shared,
                key_of(r),
                Token(100 + n as u64),
                vec!["203.0.113.9:6881".parse().unwrap()],
                unix_now(),
            );
        }
        // Results are waiting on the full outbound queue.
        until("the outbound queue is full", || {
            let n = rt.counts().granted.load(Ordering::Relaxed);
            (rt.shared.leases.len() < raws.len(), n)
        })
        .await;
        let deadline = rt.shared.config.drain_deadline;
        let started = Instant::now();
        tokio::time::timeout(DEADLINE, rt.shutdown())
            .await
            .expect("stopped");
        let took = started.elapsed();
        assert!(
            took < deadline + Duration::from_secs(5),
            "took {took:?} against a {deadline:?} deadline"
        );

        // And with fetches that never finish, the same.
        let mut rt = Runtime::start(config(Arc::new(Stalled)));
        let (_coord2, session2) = session_pair_with(1).await;
        rt.connected(session2.handle());
        rt.event(WorkerEvent::Assign(assignment(1, 0, 1)));
        let (_, chunks) = filter_chunks().await;
        for c in chunks {
            rt.event(WorkerEvent::Snapshot(c));
        }
        until("started", || (rt.is_started(), ())).await;
        granted(
            &rt.shared,
            InfoHash([3; 20]),
            Token(1),
            vec!["203.0.113.9:6881".parse().unwrap()],
            unix_now(),
        );
        let started = Instant::now();
        tokio::time::timeout(DEADLINE, rt.shutdown())
            .await
            .expect("stopped");
        assert!(started.elapsed() < deadline + Duration::from_secs(5));
    }

    fn found(n: u8) -> Found {
        let mut f = Found::key(InfoHash([n; 20]), 0);
        f.peers = vec!["8.8.8.8:6881".parse().unwrap()];
        f
    }

    /// The runner's rule (2026-11-07T20:34Z): before a have-filter is
    /// complete every sighting is claimed; once it is, a "not stored" is
    /// claimed, and a "maybe stored" is dropped and counted, except about
    /// one in 16, drawn per sighting.
    #[tokio::test]
    async fn maybes_are_dropped_once_a_filter_is_complete_but_sampled() {
        let mut rt = Runtime::start(Config {
            seed: Some(7),
            ..Config::new(W, Arc::new(MapFetch::default()))
        });
        rt.event(WorkerEvent::Assign(assignment(1, 0, 1)));
        let s = &rt.shared;
        // No filter: claimed, maybe or not.
        assert!(sift(s, found(1), false, true).is_some());
        assert!(sift(s, found(1), false, false).is_some());
        // Complete: a "no" is claimed.
        assert!(sift(s, found(2), true, false).is_some());
        let c = rt.counts();
        assert_eq!(c.skipped.load(Ordering::Relaxed), 0);
        // Complete, "maybe": dropped, about 1 in 16 claimed all the same.
        let n = 16_000;
        let claimed = (0..n)
            .filter(|_| sift(s, found(3), true, true).is_some())
            .count();
        assert!(
            (800..1200).contains(&claimed),
            "{claimed} of {n} maybes claimed"
        );
        assert_eq!(c.sampled.load(Ordering::Relaxed), claimed as u64);
        assert_eq!(
            c.skipped.load(Ordering::Relaxed),
            (n - claimed) as u64,
            "every dropped maybe counted as rediscovered"
        );
        assert_eq!(c.found.load(Ordering::Relaxed), 3 + n as u64);
        rt.shutdown().await;
    }

    /// A false positive (a new key the filter says may be stored) is
    /// claimed on a later sighting: the draw is per sighting, not per key.
    #[tokio::test]
    async fn a_false_positive_is_claimed_on_a_later_sighting() {
        // The production draw, over many seeds: the same key, sighted again
        // and again, is claimed within a few hundred sightings every time.
        for seed in 0..50u64 {
            let mut rt = Runtime::start(Config {
                seed: Some(seed),
                ..Config::new(W, Arc::new(MapFetch::default()))
            });
            rt.event(WorkerEvent::Assign(assignment(1, 0, 1)));
            let first = (1..=400)
                .find(|_| sift(&rt.shared, found(9), true, true).is_some())
                .unwrap_or_else(|| panic!("seed {seed}: never claimed in 400 sightings"));
            assert!(first <= 400);
            rt.shutdown().await;
        }
        // The same key, the same answer from the filter: only the draw
        // decides.
        assert_eq!(rediscovery(true, true, 16), Sighting::Sampled);
        assert_eq!(rediscovery(true, true, 17), Sighting::Drop);
        assert_eq!(rediscovery(false, true, 17), Sighting::Claim);
    }

    /// The runner's B2 on the worker: a snapshot pre-sent at 23:00Z for the
    /// next day seals across midnight; one only for today doesn't past it,
    /// and none before a snapshot.
    #[tokio::test]
    async fn a_pre_sent_snapshot_seals_across_midnight() {
        let mut rt = Runtime::start(config(Arc::new(MapFetch::default())));
        let now = unix_now();
        let midnight = now - now.rem_euclid(DAY) + DAY;
        assert!(!rt.shared.sealable(now), "no snapshot yet");
        let (today, chunks) = filter_chunks_at(now);
        for c in chunks {
            rt.event(WorkerEvent::Snapshot(c));
        }
        until("installed", || (rt.filter_version() == today, ())).await;
        assert!(rt.shared.sealable(now));
        assert!(
            !rt.shared.sealable(midnight + 30),
            "today's key ends at midnight"
        );
        // The next day's snapshot (sent from 23:00Z) covers both sides.
        let (next, chunks) = filter_chunks_at(midnight);
        assert_ne!(next, today);
        for c in chunks {
            rt.event(WorkerEvent::Snapshot(c));
        }
        until("the next day's installed", || {
            (rt.filter_version() == next, ())
        })
        .await;
        assert!(rt.shared.sealable(midnight - 3600 + 1));
        assert!(rt.shared.sealable(midnight + 30));
        rt.shutdown().await;
    }

    /// A result fetched while the installed snapshot's key has run out
    /// waits for the fresh one and is then sealed: never reported lost,
    /// never `Blocked` (runner, #592's B2).
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_late_snapshot_is_waited_for_not_dropped() {
        let fetch = Arc::new(MapFetch::default());
        let raw = info("Fetched before the new day's key");
        lock(&fetch.0).insert(key_of(&raw), raw.clone());
        let mut rt = Runtime::start(config(fetch));
        // Fetching needs a session up.
        let (_coord, session) = session_pair().await;
        rt.connected(session.handle());
        rt.event(WorkerEvent::Assign(assignment(1, 0, 1)));
        // Yesterday's snapshot: its key can't seal today.
        let (_, chunks) = filter_chunks_at(unix_now() - DAY);
        for c in chunks {
            rt.event(WorkerEvent::Snapshot(c));
        }
        until("started", || (rt.is_started(), ())).await;
        granted(
            &rt.shared,
            key_of(&raw),
            Token(9),
            vec!["203.0.113.9:6881".parse().unwrap()],
            unix_now(),
        );
        let c = rt.counts();
        until("waiting for a key", || {
            let n = c.waited.load(Ordering::Relaxed);
            (n == 1, n)
        })
        .await;
        let judge = rt.shared.judge.clone();
        let sealed = || {
            let j = &judge.counts;
            j.clear.load(Ordering::Relaxed) + j.marked.load(Ordering::Relaxed)
        };
        assert_eq!(sealed(), 0);
        let paused = |rt: &Runtime| Some(*rt.shared.pool_told.borrow());
        until("the pool paused while it waits", || {
            (paused(&rt) == Some(true), paused(&rt))
        })
        .await;
        // Today's arrives: the result is judged and sealed, the outage ends
        // with the install, and the pool resumes (#599's review).
        let (_, chunks) = filter_chunks().await;
        for c in chunks {
            rt.event(WorkerEvent::Snapshot(c));
        }
        until("sealed", || (sealed() == 1, sealed())).await;
        until("the pool resumed", || {
            (paused(&rt) == Some(false), paused(&rt))
        })
        .await;
        assert!(lock(&rt.shared.unsealable_since).is_none());
        assert_eq!(c.lost.load(Ordering::Relaxed), 0, "nothing reported lost");
        assert_eq!(c.released.load(Ordering::Relaxed), 0);
        rt.shutdown().await;
    }

    /// A sealed record for `n`, as the judge would make it.
    pub(crate) fn sealed(n: usize) -> SealedRecord {
        use lodestar_pipeline::wire::ResultRecord;
        use lodestar_pipeline::wire::filter_snapshot::WireKey;
        let raw = info(&format!("record {n}"));
        let parsed = lodestar_metainfo::Info::parse(&raw).unwrap();
        let torrent = Preparer::from_dict(None, 3, false)
            .prepare(&parsed, Some(&raw))
            .unwrap();
        let record = ResultRecord {
            key: torrent.info_hash,
            token: Token(n as u64 + 1),
            torrent,
            sources: 0,
            metadata_at: unix_now(),
            action: None,
            rule_version: 1,
            fix: None,
            private_source: false,
            source_adult: false,
        };
        let day = u32::try_from(unix_now().div_euclid(DAY)).unwrap();
        SealedRecord::seal(&record, &WireKey::new(day, [9; 32]), W).unwrap()
    }

    /// Batches stay within the wire's caps (`MAX_OPAQUE`, and the codec's
    /// 1,024 records): more records split into several, numbered on from
    /// `next`, every record in exactly one.
    #[test]
    fn batches_split_to_fit_and_count_up() {
        let records: Vec<SealedRecord> = (0..1500).map(sealed).collect();
        let mut next = 7;
        let (batches, refused) = encode(records.clone(), &mut next);
        assert!(refused.is_empty());
        assert!(batches.len() >= 2, "{} batches", batches.len());
        let ids: Vec<u64> = batches.iter().map(|b| b.0).collect();
        assert_eq!(ids, (7..7 + batches.len() as u64).collect::<Vec<_>>());
        assert_eq!(next, 7 + batches.len() as u64);
        let mut keys = Vec::new();
        for (id, bytes, part) in &batches {
            assert!(bytes.len() <= MAX_OPAQUE);
            let (clear, opened) = ResultsBatch::decode_with(bytes).unwrap();
            assert!(clear.records.is_empty(), "nothing in clear");
            assert_eq!(clear.batch, *id);
            assert_eq!(opened.len(), part.len());
            keys.extend(part.iter().map(|r| r.key));
        }
        assert_eq!(keys, records.iter().map(|r| r.key).collect::<Vec<_>>());
    }

    /// A result whose snapshot never comes is let go after the wait with a
    /// loss that isn't `Blocked` (so its lease ends, and the key isn't
    /// tombstoned), and a drain hands back the losses not reported yet.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_result_that_cant_be_sealed_is_released_not_blocked() {
        let fetch = Arc::new(MapFetch::default());
        let raw = info("Never sealable");
        lock(&fetch.0).insert(key_of(&raw), raw.clone());
        let mut rt = Runtime::start(Config {
            seal_wait: Duration::from_millis(300),
            ..config(fetch)
        });
        // Fetching needs a session up.
        let (mut coord, session) = session_pair().await;
        rt.connected(session.handle());
        rt.event(WorkerEvent::Assign(assignment(1, 0, 1)));
        let (_, chunks) = filter_chunks_at(unix_now() - DAY);
        for c in chunks {
            rt.event(WorkerEvent::Snapshot(c));
        }
        until("started", || (rt.is_started(), ())).await;
        granted(
            &rt.shared,
            key_of(&raw),
            Token(5),
            vec!["203.0.113.9:6881".parse().unwrap()],
            unix_now(),
        );
        let c = rt.counts();
        until("released", || {
            let n = c.released.load(Ordering::Relaxed);
            (n == 1, n)
        })
        .await;
        // Reported over the session (or, if it hadn't gone yet, handed
        // back by the drain).
        let mut outcomes = rt.drain().await;
        if outcomes.is_empty() {
            outcomes = tokio::time::timeout(DEADLINE, async {
                loop {
                    match coord.recv().await {
                        Ok(Some(ToCoordinator::Outcome(b))) => return b.items,
                        Ok(Some(_)) => {}
                        other => panic!("the link ended: {other:?}"),
                    }
                }
            })
            .await
            .expect("an outcome report");
        }
        assert_eq!(
            outcomes,
            vec![OutcomeItem {
                key: key_of(&raw).0,
                token: 5,
                outcome: Outcome::Lost {
                    reason: Loss::GaveUp as u8
                },
            }]
        );
        assert_ne!(Loss::GaveUp as u8, Loss::Blocked as u8);
    }

    /// #599's review: the wait runs from when sealing went down, not per
    /// result, so a backlog is released together (12 results and a 1 s wait
    /// take about 1 s, not 12), and nothing new is claimed meanwhile.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_backlog_is_released_together_and_claims_wait() {
        let fetch = Arc::new(MapFetch::default());
        let raws: Vec<Vec<u8>> = (0..12).map(|i| info(&format!("backlog {i}"))).collect();
        for r in &raws {
            lock(&fetch.0).insert(key_of(r), r.clone());
        }
        let mut rt = Runtime::start(Config {
            seal_wait: Duration::from_secs(1),
            ..config(fetch)
        });
        // Fetching needs a session up.
        let (_coord, session) = session_pair().await;
        rt.connected(session.handle());
        rt.event(WorkerEvent::Assign(assignment(1, 0, 1)));
        let (_, chunks) = filter_chunks_at(unix_now() - DAY);
        for c in chunks {
            rt.event(WorkerEvent::Snapshot(c));
        }
        until("started", || (rt.is_started(), ())).await;
        for (n, r) in raws.iter().enumerate() {
            granted(
                &rt.shared,
                key_of(r),
                Token(50 + n as u64),
                vec!["203.0.113.9:6881".parse().unwrap()],
                unix_now(),
            );
        }
        let c = rt.counts();
        until("waiting", || {
            let n = c.waited.load(Ordering::Relaxed);
            (n >= 1, n)
        })
        .await;
        let started = Instant::now();
        until("all released", || {
            let n = c.released.load(Ordering::Relaxed);
            (n == raws.len() as u64, n)
        })
        .await;
        let took = started.elapsed();
        assert!(
            took < Duration::from_secs(8),
            "released one wait after another: {took:?}"
        );
        assert_eq!(c.lost.load(Ordering::Relaxed), raws.len() as u64);
        // Sealing is still down: a find isn't claimed.
        assert!(sift(&rt.shared, found(4), false, false).is_none());
        assert_eq!(c.held_back.load(Ordering::Relaxed), 1);
        // A fresh snapshot ends it.
        let (_, chunks) = filter_chunks().await;
        for ch in chunks {
            rt.event(WorkerEvent::Snapshot(ch));
        }
        until("claims again", || {
            let ok = sift(&rt.shared, found(4), false, false).is_some();
            (ok, ok)
        })
        .await;
        rt.shutdown().await;
    }

    /// A worker session over the in-process link, with the coordinator's
    /// end returned for the test to drive.
    pub(crate) async fn session_pair() -> (
        lodestar_cluster::CoordinatorLink,
        lodestar_cluster::WorkerSession,
    ) {
        session_pair_with(64).await
    }

    /// X-worker-839-followups: a flush whose session changes to a new, live
    /// one while it sends answers only once the batch is on the new
    /// session. The send to the old session stalls (its link is full and
    /// never read). Whether the flush or the loop top has the batch when
    /// the session changes is up to the scheduler; either way it's out
    /// before the flush answers, and nothing goes to the spool.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_flush_that_sees_a_new_session_sends_there_before_answering() {
        let mut rt = Runtime::start(config(Arc::new(MapFetch::default())));
        let (_stalled, old) = session_pair_with(1).await;
        // The handshake leaves one slot: fill it, so every later frame
        // stalls.
        let filler = ToCoordinator::SnapshotAck(lodestar_cluster::proto::SnapshotAck {
            kind: lodestar_cluster::proto::SnapshotKind::Filter,
            version: 1,
            seq: 0,
        });
        old.handle().send(filler).await.unwrap();
        rt.connected(old.handle());
        let out = rt.test_outbound();
        out.send(sealed(3)).await.unwrap();

        let (mut coord, new) = session_pair().await;
        let shared = rt.shared.clone();
        let handle = new.handle();
        // As `connected` does: the generation, then the session.
        let reconnect = async move {
            shared.generation.fetch_add(1, Ordering::AcqRel);
            shared.session.send_replace(Some(handle));
        };
        let started = Instant::now();
        // The drain first: its flush is asked for before the session
        // changes.
        let (_, ()) = tokio::join!(rt.drain(), reconnect);
        // Answered before the drain deadline ran out, with the batch sent:
        // a send that stalled on the old session isn't counted.
        assert!(started.elapsed() < rt.shared.config.drain_deadline);
        assert_eq!(rt.counts().sent.load(Ordering::Relaxed), 1);
        let got = next_results(&mut coord).await;
        let (_, sealed) = ResultsBatch::decode_with(&got.bytes).unwrap();
        assert_eq!(sealed.len(), 1);
        drop((old, new, out));
    }

    /// A session over a link of `capacity` frames: at 1, a coordinator
    /// that never reads stalls it after the handshake.
    pub(crate) async fn session_pair_with(
        capacity: usize,
    ) -> (
        lodestar_cluster::CoordinatorLink,
        lodestar_cluster::WorkerSession,
    ) {
        use lodestar_cluster::proto::{
            Capabilities, Hello, PROTOCOL_MAJOR, PROTOCOL_MINOR, ToWorker, Welcome,
        };
        let (mut coord, worker) = lodestar_cluster::local_pair(capacity);
        let config = lodestar_cluster::WorkerConfig {
            auto_heartbeat: false,
            ..lodestar_cluster::WorkerConfig::default()
        };
        let clock = Arc::new(lodestar_cluster::ManualClock::new(1_000_000));
        let (session, ()) = tokio::join!(
            lodestar_cluster::WorkerSession::start(
                worker,
                Hello::new(W.0, Capabilities::default()),
                clock,
                config
            ),
            async {
                let _ = coord.recv().await;
                coord
                    .send(ToWorker::Welcome(Welcome {
                        major: PROTOCOL_MAJOR,
                        minor: PROTOCOL_MINOR,
                        worker: W.0,
                        heartbeat_ms: 60_000,
                        lease_ttl_secs: 600,
                        renew_now: false,
                    }))
                    .await
                    .unwrap();
            }
        );
        (coord, session.unwrap())
    }

    /// The next `Results` the coordinator gets, skipping anything else.
    pub(crate) async fn next_results(coord: &mut lodestar_cluster::CoordinatorLink) -> Payload {
        tokio::time::timeout(DEADLINE, async {
            loop {
                match coord.recv().await {
                    Ok(Some(ToCoordinator::Results(p))) => return p,
                    Ok(Some(_)) => {}
                    other => panic!("the link ended: {other:?}"),
                }
            }
        })
        .await
        .expect("a Results batch in time")
    }

    /// Results batches are kept until accepted: a refused one goes again on
    /// the next retry tick, a new session gets every one not accepted, and
    /// an accepted one is committed (and counted as sent once).
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn results_are_kept_until_accepted() {
        use lodestar_cluster::proto::ToWorker;
        let mut rt = Runtime::start(config(Arc::new(MapFetch::default())));
        let (mut coord, mut session) = session_pair().await;
        rt.connected(session.handle());
        // A record, as the stages would queue it.
        let out = lock(&rt.shared.out).take().unwrap();
        out.send(sealed(1)).await.unwrap();
        let first = next_results(&mut coord).await;
        let (_, opened) = ResultsBatch::decode_with(&first.bytes).unwrap();
        assert_eq!(opened.len(), 1);

        // Refused for now: it goes again, the same batch, on a later tick.
        coord
            .send(ToWorker::Accepted {
                batch: first.batch,
                accepted: false,
            })
            .await
            .unwrap();
        let e = tokio::time::timeout(DEADLINE, session.next_event())
            .await
            .unwrap()
            .unwrap();
        rt.event(e);
        let again = next_results(&mut coord).await;
        assert_eq!((again.batch, &again.bytes), (first.batch, &first.bytes));

        // A new session: what wasn't accepted goes on it.
        rt.disconnected();
        let (mut coord2, mut session2) = session_pair().await;
        rt.connected(session2.handle());
        let resent = next_results(&mut coord2).await;
        assert_eq!(resent.batch, first.batch);

        // Accepted: committed, and counted as sent once.
        coord2
            .send(ToWorker::Accepted {
                batch: first.batch,
                accepted: true,
            })
            .await
            .unwrap();
        let e = tokio::time::timeout(DEADLINE, session2.next_event())
            .await
            .unwrap()
            .unwrap();
        rt.event(e);
        let c = rt.counts();
        until("committed", || {
            let n = c.committed.load(Ordering::Relaxed);
            (n == 1, n)
        })
        .await;
        assert_eq!(c.sent.load(Ordering::Relaxed), 1, "counted once");
        drop(session);
        rt.shutdown().await;
    }

    /// An accepted batch's leases are reported `Fetched` (X-wk-fetched-lease):
    /// the coordinator then answers this worker's own claims for them as
    /// committed, never with the lease, however long its writer takes.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_accepted_batch_reports_its_leases_fetched() {
        use lodestar_cluster::proto::ToWorker;
        let mut rt = Runtime::start(config(Arc::new(MapFetch::default())));
        let (mut coord, mut session) = session_pair().await;
        rt.connected(session.handle());
        let out = lock(&rt.shared.out).take().unwrap();
        let record = sealed(1);
        let (key, token) = (record.key.0, record.token.0);
        out.send(record).await.unwrap();
        let first = next_results(&mut coord).await;
        coord
            .send(ToWorker::Accepted {
                batch: first.batch,
                accepted: true,
            })
            .await
            .unwrap();
        let e = tokio::time::timeout(DEADLINE, session.next_event())
            .await
            .unwrap()
            .unwrap();
        rt.event(e);
        let items = tokio::time::timeout(DEADLINE, async {
            loop {
                match coord.recv().await {
                    Ok(Some(ToCoordinator::Outcome(o))) => return o.items,
                    Ok(Some(_)) => {}
                    other => panic!("the link ended: {other:?}"),
                }
            }
        })
        .await
        .expect("an outcome in time");
        assert_eq!(
            items,
            [OutcomeItem {
                key,
                token,
                outcome: Outcome::Fetched,
            }]
        );
        rt.shutdown().await;
    }

    /// A drain sends a batch refused for now at once (not on the next
    /// retry tick), and counts it as sent once.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_drain_resends_a_refused_batch_and_counts_it_once() {
        use lodestar_cluster::proto::ToWorker;
        // The retry tick never fires here: only the drain can resend.
        let mut rt = Runtime::start(Config {
            retry_every: Duration::from_secs(3600),
            ..config(Arc::new(MapFetch::default()))
        });
        let (mut coord, mut session) = session_pair().await;
        rt.connected(session.handle());
        let out = lock(&rt.shared.out).take().unwrap();
        out.send(sealed(1)).await.unwrap();
        let first = next_results(&mut coord).await;
        coord
            .send(ToWorker::Accepted {
                batch: first.batch,
                accepted: false,
            })
            .await
            .unwrap();
        let e = tokio::time::timeout(DEADLINE, session.next_event())
            .await
            .unwrap()
            .unwrap();
        rt.event(e);
        // The refusal is in before the drain asks.
        until("refused", || {
            (rt.shared.accepted.capacity() == ACCEPTED_QUEUE, ())
        })
        .await;
        let (_, again) = tokio::join!(rt.drain(), next_results(&mut coord));
        assert_eq!(again.batch, first.batch, "sent again by the drain");
        assert_eq!(rt.counts().sent.load(Ordering::Relaxed), 1, "counted once");
        rt.shutdown().await;
    }

    mod end_to_end {
        use lodestar_cluster::{JoinToken, Joining, LeaseAuthority};
        use lodestar_pipeline::{Pipeline, PipelineConfig};

        use super::*;
        use crate::cluster::placement::Placement;
        use crate::cluster::serve::{Fleet, Wiring};
        use crate::cluster::{Cluster, ClusterHandle};
        use crate::worker::Layout;
        use crate::worker::session::{Settings, Tcp, Worker, sleep};

        struct Coordinator {
            _dir: tempfile::TempDir,
            pipeline: Pipeline,
            cluster: Cluster,
        }

        async fn coordinator() -> Coordinator {
            let dir = tempfile::tempdir().unwrap();
            let db = dir.path().join("lodestar.db");
            let store = Store::open(&db, StoreConfig::default()).unwrap();
            let config = PipelineConfig {
                remote_workers: true,
                flush_every: Duration::from_millis(100),
                batch_wait: Duration::from_millis(20),
                ..PipelineConfig::default()
            };
            let filter = config.filter.clone();
            let pipeline = Pipeline::start(config, store, vec![]).await.unwrap();
            let monitor = pipeline.monitor();
            let authority: Arc<LeaseAuthority> = monitor.lease_authority().clone();
            let cluster = Cluster::start(Wiring {
                db,
                store: StoreConfig::default(),
                data_dir: dir.path().to_path_buf(),
                authority,
                remote: Some(monitor.remote().clone()),
                settings: crate::config::Cluster {
                    enabled: true,
                    listen: "127.0.0.1:0".parse().unwrap(),
                    ..crate::config::Cluster::default()
                },
                clock: Arc::new(lodestar_cluster::SystemClock),
                placement: Arc::new(Placement::new(false)),
                coordinator_dht: None,
                coordinator_share: None,
                held: None,
                fleet: Some(Fleet {
                    stored: Arc::new(monitor),
                    filter,
                    seal_key: [6; 32],
                }),
            })
            .await
            .unwrap();
            Coordinator {
                _dir: dir,
                pipeline,
                cluster,
            }
        }

        async fn join(h: &ClusterHandle, layout: &Layout) -> lodestar_cluster::Credentials {
            let server = h.server().expect("serving");
            let token = server.create_join_token("w1").await.unwrap();
            let joining = Joining::new(JoinToken::parse(&token.expose()).unwrap()).unwrap();
            let addr = h.local_addr().unwrap().to_string();
            let creds = lodestar_cluster::join(
                &addr,
                joining,
                Arc::new(lodestar_cluster::SystemClock),
                DEADLINE,
            )
            .await
            .unwrap();
            layout.save(&creds).unwrap();
            creds
        }

        /// The brief's first gate: a worker against an in-process
        /// coordinator stores torrents end to end. It gets its assignment
        /// and the snapshots, claims what it finds, fetches, prepares,
        /// judges and seals, sends `Results`; the coordinator opens them
        /// under the day's wire key and commits them.
        #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn a_worker_stores_torrents_end_to_end() {
            let c = coordinator().await;
            let h = c.cluster.handle();
            let wdir = tempfile::tempdir().unwrap();
            let layout = Layout::new(wdir.path());
            let creds = join(&h, &layout).await;

            let fetch = Arc::new(MapFetch::default());
            let raws: Vec<Vec<u8>> = (0..5)
                .map(|i| info(&format!("A worker's find {i}")))
                .collect();
            for r in &raws {
                lock(&fetch.0).insert(key_of(r), r.clone());
            }
            let rt = Runtime::start(Config {
                allow_private: true,
                seed: Some(3),
                ..Config::new(creds.worker(), fetch)
            });
            let finds = rt.finds();
            let counts = rt.counts();
            let addr = h.local_addr().unwrap().to_string();
            let clock: Arc<dyn lodestar_cluster::Clock> = Arc::new(lodestar_cluster::SystemClock);
            let worker = Worker {
                layout,
                creds,
                connect: Tcp {
                    addr,
                    clock: clock.clone(),
                    within: DEADLINE,
                },
                clock,
                settings: Settings {
                    backoff_base: Duration::from_millis(10),
                    backoff_cap: Duration::from_millis(200),
                    ..Settings::default()
                },
                report: Arc::new(|_: &str| {}),
                jitter: || 0,
                wait: sleep(),
            };
            let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
            let running = tokio::spawn(worker.run(rt, async move {
                let _ = stopped.await;
            }));

            // The worker's DHT finds them (here, by hand) once it's started.
            let stored = || c.pipeline.writer_stats().stored.load(Ordering::Relaxed);
            for r in &raws {
                let mut f = Found::key(key_of(r), 0);
                f.peers = vec!["203.0.113.7:6881".parse().unwrap()];
                // Before the stages run, finds wait in the queue.
                assert!(finds.found(f));
            }
            until("every find stored", || {
                (
                    stored() == raws.len() as u64,
                    (
                        stored(),
                        counts.claimed.load(Ordering::Relaxed),
                        counts.granted.load(Ordering::Relaxed),
                        counts.sent.load(Ordering::Relaxed),
                        counts.committed.load(Ordering::Relaxed),
                        counts.lost.load(Ordering::Relaxed),
                    ),
                )
            })
            .await;
            until("committed on the worker", || {
                let n = counts.committed.load(Ordering::Relaxed);
                (n == raws.len() as u64, n)
            })
            .await;

            // Found again: the have-filter (its snapshot, then the deltas of
            // these inserts) says they're stored, so they aren't claimed.
            let refind = || {
                for r in &raws {
                    let mut f = Found::key(key_of(r), 0);
                    f.peers = vec!["203.0.113.7:6881".parse().unwrap()];
                    finds.found(f);
                }
            };
            // The deltas follow within a tick or two: find them again until
            // the filter has every one.
            until("rediscoveries skipped", || {
                let skipped = counts.skipped.load(Ordering::Relaxed);
                if skipped < raws.len() as u64 {
                    refind();
                }
                (skipped >= raws.len() as u64, skipped)
            })
            .await;
            // Each later sighting is dropped, or claimed as one of the
            // sampled one in 16; the coordinator answers those from the
            // store and grants none.
            let seen =
                |c: &Counts| c.skipped.load(Ordering::Relaxed) + c.sampled.load(Ordering::Relaxed);
            let (before, sampled, granted) = (
                seen(&counts),
                counts.sampled.load(Ordering::Relaxed),
                counts.granted.load(Ordering::Relaxed),
            );
            refind();
            until("checked", || {
                let n = seen(&counts);
                (n == before + raws.len() as u64, n)
            })
            .await;
            let newly_sampled = counts.sampled.load(Ordering::Relaxed) - sampled;
            until("the sampled ones claimed", || {
                let n = counts.claimed.load(Ordering::Relaxed);
                (
                    n >= raws.len() as u64 + counts.sampled.load(Ordering::Relaxed),
                    n,
                )
            })
            .await;
            assert!(newly_sampled <= raws.len() as u64);
            assert_eq!(
                counts.granted.load(Ordering::Relaxed),
                granted,
                "a stored key is never granted again"
            );

            // A drain finishes the work and stops the worker.
            let server = h.server().unwrap();
            let worker_id = server
                .snapshot()
                .workers
                .iter()
                .find(|w| w.connected)
                .unwrap()
                .worker;
            assert!(server.drain(worker_id));
            let exit = tokio::time::timeout(DEADLINE, running)
                .await
                .expect("stopped")
                .unwrap();
            assert_eq!(exit, crate::worker::session::Exit::Drained);
            drop(stop);
            c.cluster.shutdown().await;
            let store = c.pipeline.shutdown().await.unwrap();
            for r in &raws {
                assert!(store.find(&key_of(r)).unwrap().is_some());
            }
        }
    }
}
