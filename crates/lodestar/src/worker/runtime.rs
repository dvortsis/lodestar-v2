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

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use lodestar_cluster::assign::Assignment;
use lodestar_cluster::proto::{
    ClaimItem, GrantItem, HintItem, MAX_HINTS, MAX_OPAQUE, MAX_OUTCOME_BATCH, Outcome, OutcomeItem,
    Payload, SnapshotAck, SnapshotChunk, SnapshotKind, ToCoordinator,
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
const STATS_EVERY: Duration = Duration::from_secs(5);
/// Keys found here waiting to be checked and claimed. A full queue drops
/// the newest: the DHT finds a key again.
const FOUND_QUEUE: usize = 4096;
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
    pub granted: AtomicU64,
    /// Results sent, and accepted by the coordinator.
    pub sent: AtomicU64,
    pub committed: AtomicU64,
    /// Losses queued as outcomes.
    pub lost: AtomicU64,
    /// Peer hints refused by the network policy.
    pub hints_refused: AtomicU64,
    /// Snapshots that didn't decode or install.
    pub snapshot_errors: AtomicU64,
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
    /// The forwarder into the pool.
    forward: JoinHandle<()>,
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
    acks: mpsc::UnboundedSender<SnapshotAck>,
    accepted: mpsc::UnboundedSender<(u64, bool)>,
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
    /// Whether the pool was last told to pause; held across the telling,
    /// so pauses and resumes reach the pool in the order they're decided.
    pool_paused: tokio::sync::Mutex<bool>,
    /// Results waiting for a sealable key.
    waiting: std::sync::atomic::AtomicUsize,
    /// A new session's first `Assign` applies whatever its revision: a
    /// coordinator whose clock went back numbers its plans lower (WK-5).
    fresh_session: AtomicBool,
    /// When sealing went down (no installed key covers now): results wait
    /// until [`SEAL_WAIT`] after this, then go at once. Cleared by a
    /// result that seals.
    unsealable_since: Mutex<Option<std::time::Instant>>,
    /// Asks the session task to send everything it has now (a drain).
    flush: mpsc::UnboundedSender<tokio::sync::oneshot::Sender<()>>,
    /// What the DHT should run ([`want_dht`]).
    dht_want: watch::Sender<super::dht::Desired>,
    /// The pool's peer requests, to the DHT; `None` with no DHT.
    peer_requests: Option<mpsc::Sender<lodestar_sources::PeerRequest>>,
    dht_counts: Arc<super::dht::DhtCounts>,
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
        let (acks, ack_rx) = mpsc::unbounded_channel();
        let (accepted, accepted_rx) = mpsc::unbounded_channel();
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
        let shared = Arc::new(Shared {
            have: WorkerHave::new(),
            judge: Arc::new(Judge::new(config.worker)),
            leases: Arc::new(WorkerLeases::default()),
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
            pool_paused: tokio::sync::Mutex::new(false),
            waiting: std::sync::atomic::AtomicUsize::new(0),
            fresh_session: AtomicBool::new(false),
            flush,
            unsealable_since: Mutex::new(None),
            dht_want,
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
        ];
        // The DHT's task isn't aborted with the others: a stop (or a drop)
        // asks it to stop its node, which saves the node's state, and it
        // ends once the runtime is gone ([`super::dht::stopped`]).
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

    pub fn counts(&self) -> Arc<Counts> {
        self.shared.counts.clone()
    }

    /// The DHT's counters.
    pub fn dht_counts(&self) -> Arc<super::dht::DhtCounts> {
        self.shared.dht_counts.clone()
    }

    /// The running DHT node's shard (`None` with no DHT configured).
    pub fn dht_running(&self) -> Option<super::dht::Running> {
        self.dht_running.clone()
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
            ..
        } = stages;
        forward.abort();
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
                    }
                }
                start_if_ready(s);
                want_dht(s);
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
                let _ = s.accepted.send((batch, accepted));
            }
            WorkerEvent::Snapshot(c) => snapshot_chunk(s, c),
            WorkerEvent::HaveSnapshot(b) => {
                if s.have.header(&b).is_err() {
                    bump(&s.counts.snapshot_errors);
                }
            }
            WorkerEvent::HaveDelta(b) => {
                if let (_, Some(ack)) = s.have.delta(&b) {
                    let _ = s.acks.send(ack);
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
        lodestar_cluster::proto::Capabilities {
            ram_mb: super::budget::ram_mb(),
            external_ips: self.shared.dht_counts.external_ip().into_iter().collect(),
            ..Default::default()
        }
    }

    fn connected(&mut self, session: SessionHandle) {
        // Anything queued to the last session may be lost: ask for a fresh
        // have-filter snapshot (WK-3). The coordinator starts the filter
        // and dictionary snapshots over by itself (WK-10a).
        let _ = self.shared.acks.send(self.shared.have.reconnected());
        self.shared.fresh_session.store(true, Ordering::Release);
        self.shared.session.send_replace(Some(session));
        want_dht(&self.shared);
        refresh_pause_soon(&self.shared);
    }

    fn disconnected(&mut self) {
        self.shared.session.send_replace(None);
        // The DHT pauses at once: the coordinator re-plans without this
        // worker and widens the others' shares (WK-5); it stops if no
        // session is back within its linger.
        want_dht(&self.shared);
        // The pool opens no new connection meanwhile.
        refresh_pause_soon(&self.shared);
    }

    fn event(&mut self, event: WorkerEvent) {
        self.handle(event);
    }

    /// Takes no new keys, lets the pool hand over what's finished (within
    /// the deadline), and returns the losses not reported yet. Results the
    /// session task sent are committed by the coordinator; everything else
    /// goes back with `Drained`.
    async fn drain(&mut self) -> Vec<OutcomeItem> {
        let started = std::time::Instant::now();
        self.stop_stages().await;
        // What's fetched and sealed goes out before `Drained`, within what's
        // left of the deadline: results travel on the same command channel,
        // so the coordinator takes them first (no `Accepted` is awaited).
        let left = self
            .shared
            .config
            .drain_deadline
            .saturating_sub(started.elapsed());
        let (done, flushed) = tokio::sync::oneshot::channel();
        if self.shared.flush.send(done).is_ok() {
            let _ = tokio::time::timeout(left, flushed).await;
        }
        self.take_outcomes().await
    }
}

impl Drop for Runtime {
    fn drop(&mut self) {
        // A local stop drops the runtime (`Worker::run` owns it): everything
        // stops at once, and the keys' leases go back to the coordinator. A
        // late install on a blocking thread then can't start stages nobody
        // owns.
        self.shared.draining.store(true, Ordering::Release);
        want_dht(&self.shared);
        if let Some(stages) = lock(&self.shared.stages).take() {
            let _ = stages.stop.send(true);
            stages.prepare.abort();
            stages.pool.abort();
            stages.forward.abort();
        }
        self.abort_all();
    }
}

/// A key granted to this worker: its token into the leases, the key into
/// the pool (queued until the stages run).
fn granted(s: &Shared, key: InfoHash, token: Token, peers: Vec<std::net::SocketAddr>, at: i64) {
    bump(&s.counts.granted);
    s.leases.insert(key, token);
    let mut found = Found::key(key, 0);
    found.peers = peers;
    let _ = s.into_pool.send(Discovery {
        source: SourceId::new(DHT_SOURCE).expect("a source id"),
        found,
        at,
        reply: false,
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

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}

/// One filter or dictionary chunk; the have-filter's go to it.
fn snapshot_chunk(s: &Arc<Shared>, c: SnapshotChunk) {
    if c.kind == SnapshotKind::HaveFilter {
        if let Some(ack) = s.have.chunk(&c) {
            let _ = s.acks.send(ack);
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
            let _ = s.acks.send(ack);
            return;
        }
        Ok(Step::Done(ack, complete)) => {
            let _ = s.acks.send(ack);
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
    let Some(a) = *lock(&s.assignment) else {
        return;
    };
    if !a.fetch || s.judge.version() == 0 {
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
    *lock(&s.pool) = Some(running.pool);
    *stages = Some(Stages {
        stop,
        pool: running.pool_task,
        prepare: running.prepare,
        forward,
    });
    s.started.send_replace(true);
    want_dht(s);
    // A session may have gone while the stages were built.
    refresh_pause_soon(s);
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
        if !up || s.fresh_session.load(Ordering::Acquire) {
            return Desired::Pause;
        }
        Desired::Run(Want {
            shard,
            intensity: super::budget::Limits::of(&a).intensity,
        })
    };
    s.dht_want.send_if_modified(|w| {
        let now = want();
        let changed = *w != now;
        *w = now;
        changed
    });
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
        day != u32::MAX && self.judge.version() != 0 && now.div_euclid(DAY) <= i64::from(day)
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
                // No fetch is started while a result waits: they'd only
                // spend their keys waiting on the pool's byte budget.
                if s.waiting.fetch_add(1, Ordering::AcqRel) == 0 {
                    s.refresh_pause().await;
                }
                let mut installs = s.key_day.subscribe();
                let left = deadline.saturating_duration_since(std::time::Instant::now());
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
                    s.refresh_pause().await;
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
                    reports.lost(&key, token, Loss::Blocked);
                    Ok(())
                }
                Judged::Invalid { key, token } => {
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
    reports.lost(&key, token, Loss::GaveUp);
}

impl Shared {
    /// Pauses the pool while a result waits for a sealable key or no
    /// session is up, and resumes it otherwise. Worked out from the state
    /// now, under `pool_paused`, so calls in any order leave it right.
    /// Paused, the pool opens no new connection; fetches under way finish
    /// (WK-5: a worker without a session stops opening connections, since
    /// the coordinator re-plans without it).
    async fn refresh_pause(&self) {
        let mut told = self.pool_paused.lock().await;
        let up = self
            .session
            .borrow()
            .as_ref()
            .is_some_and(|h| !h.is_closed());
        let pause = self.waiting.load(Ordering::Acquire) > 0 || !up;
        let pool = lock(&self.pool).clone();
        if let Some(p) = pool
            && *told != pause
        {
            p.set_paused(pause).await;
            *told = pause;
        }
    }
}

/// [`Shared::refresh_pause`] from a caller that can't wait.
fn refresh_pause_soon(s: &Arc<Shared>) {
    let s = s.clone();
    tokio::spawn(async move { s.refresh_pause().await });
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

/// Refreshes the heartbeat's stats every [`STATS_EVERY`] (WK-10c):
/// claimed, results sent ("fetched"), results the coordinator accepted
/// ("processed"), CPU and resident memory. The spool is WK-11's.
async fn heartbeat_stats(s: Arc<Shared>) {
    let mut process = super::budget::Process::default();
    let mut every = tokio::time::interval(STATS_EVERY);
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
            spool_bytes: 0,
        };
        let session = s.session.borrow().clone();
        if let Some(h) = session {
            let _ = h.set_stats(stats).await;
        }
    }
}

/// Sends snapshot acknowledgements, and the have-filter's resync request
/// again while no header comes (WK-3).
async fn send_acks(s: Arc<Shared>, mut acks: mpsc::UnboundedReceiver<SnapshotAck>) {
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

/// Checks keys found here against the have-filter and claims them in
/// batches. Before a filter is complete every key is claimed ("maybe").
async fn claim(s: Arc<Shared>, found: mpsc::Receiver<Found>) {
    // Nothing is claimed before the stages can take it.
    let mut started = s.started.subscribe();
    if started.wait_for(|on| *on).await.is_err() {
        return;
    }
    let (items, mut batches) = mpsc::channel::<(ClaimItem, Vec<std::net::SocketAddr>)>(1024);
    // Ends with this task (a stop aborts it).
    let _check = AbortOnDrop(tokio::spawn(check(s.clone(), found, items)).abort_handle());
    // `next_claims` isn't cancel-safe: it's awaited to its end.
    while let Some(batch) = next_claims(&mut batches).await {
        if s.draining.load(Ordering::Acquire) {
            continue;
        }
        let peers: BTreeMap<[u8; 20], Vec<_>> =
            batch.iter().map(|(c, p)| (c.key, p.clone())).collect();
        let items: Vec<ClaimItem> = batch.into_iter().map(|(c, _)| c).collect();
        s.counts
            .claimed
            .fetch_add(items.len() as u64, Ordering::Relaxed);
        let h = current(&s).await;
        // A claim that fails is dropped: the DHT finds the keys again.
        let Ok(grants) = h.claim(items).await else {
            continue;
        };
        for g in grants {
            if let GrantItem::Granted { key, token, .. } = g {
                let peers = peers.get(&key).cloned().unwrap_or_default();
                granted(&s, InfoHash(key), Token(token), peers, unix_now());
            }
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
    // its lease would only wait (#599's review).
    if s.sealing_down(unix_now()) {
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
            },
        );
    }
}

/// The session task: sealed results into `Results` batches, kept until
/// accepted, sent again on a new session and after a refusal.
async fn send_results(
    s: Arc<Shared>,
    mut outbound: ByteReceiver<SealedRecord>,
    mut accepted: mpsc::UnboundedReceiver<(u64, bool)>,
    mut flush: mpsc::UnboundedReceiver<tokio::sync::oneshot::Sender<()>>,
) {
    let mut next_batch: u64 = 1;
    let mut flight: BTreeMap<u64, InFlight> = BTreeMap::new();
    let mut sessions = s.session.subscribe();
    let mut retry = tokio::time::interval(POLL_EVERY);
    retry.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        // Send whatever is due on the current session.
        let session = sessions.borrow_and_update().clone();
        if let Some(h) = session.filter(|h| !h.is_closed()) {
            for (&batch, f) in flight.iter_mut().filter(|(_, f)| !f.sent) {
                let msg = ToCoordinator::Results(Payload {
                    batch,
                    bytes: f.payload.clone(),
                });
                if h.send(msg).await.is_err() {
                    break;
                }
                f.sent = true;
                // Counted once, not on every resend.
                if !f.counted {
                    f.counted = true;
                    s.counts
                        .sent
                        .fetch_add(f.leases.len() as u64, Ordering::Relaxed);
                }
            }
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
                let session = s.session.borrow().clone();
                if let Some(h) = session.filter(|h| !h.is_closed()) {
                    for (&batch, f) in flight.iter_mut().filter(|(_, f)| !f.sent) {
                        let msg = ToCoordinator::Results(Payload { batch, bytes: f.payload.clone() });
                        if h.send(msg).await.is_err() {
                            break;
                        }
                        f.sent = true;
                    }
                }
                let _ = done.send(());
            }
            Some((batch, ok)) = accepted.recv() => {
                if ok {
                    if let Some(f) = flight.remove(&batch) {
                        s.counts.committed.fetch_add(f.leases.len() as u64, Ordering::Relaxed);
                        if let Some(h) = s.session.borrow().clone() {
                            let leases = f.leases;
                            tokio::spawn(async move { let _ = h.committed(leases).await; });
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
                // A new session: whatever wasn't accepted goes again (a
                // batch id is idempotent, §19.2).
                for f in flight.values_mut() {
                    f.sent = false;
                }
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

#[cfg(test)]
mod tests {
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

    const DEADLINE: Duration = Duration::from_secs(20);
    const W: WorkerId = WorkerId(5);

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

    /// A bencoded single-file info dict.
    fn info(name: &str) -> Vec<u8> {
        let mut v = b"d6:lengthi1048576e4:name".to_vec();
        v.extend(format!("{}:{name}", name.len()).bytes());
        v.extend(b"12:piece lengthi262144e6:pieces20:");
        v.extend([0u8; 20]);
        v.push(b'e');
        v
    }

    fn key_of(raw: &[u8]) -> InfoHash {
        lodestar_metainfo::Info::parse(raw).unwrap().v1.unwrap()
    }

    /// Serves the info dicts it's given; refuses every other key.
    #[derive(Default)]
    struct MapFetch(Mutex<HashMap<InfoHash, Vec<u8>>>);

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

    fn config(fetcher: Arc<dyn Fetcher>) -> Config {
        Config {
            allow_private: true,
            drain_deadline: Duration::from_secs(2),
            ..Config::new(W, fetcher)
        }
    }

    fn assignment(revision: u64, dict_version: u64, filter_version: u64) -> Assign {
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
    fn filter_chunks() -> (u64, Vec<SnapshotChunk>) {
        filter_chunks_at(unix_now())
    }

    /// The same, with the wire key of the day `at` falls on.
    fn filter_chunks_at(at: i64) -> (u64, Vec<SnapshotChunk>) {
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

        let (version, chunks) = filter_chunks();
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

    fn assign_dht(revision: u64, shard: Option<ShardSpec>, intensity: Intensity) -> Assign {
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
        let mut rt = Runtime::start(Config {
            dht: Some(dht_config()),
            dht_linger: linger,
            ..config(Arc::new(MapFetch::default()))
        });
        let (coord, session) = session_pair().await;
        rt.connected(session.handle());
        let a = ShardSpec::new(64..128, 0xff).unwrap();
        rt.event(WorkerEvent::Assign(assign_dht(
            1,
            Some(a),
            Intensity::Balanced,
        )));
        let (_, chunks) = filter_chunks();
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
    /// intensity alone restarts nothing.
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
        // An assignment without a DHT stops it, and only it.
        rt.event(WorkerEvent::Assign(assign_dht(4, None, Intensity::Low)));
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
            *rt.shared.dht_want.borrow(),
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
        let paused = |rt: &Runtime| rt.shared.pool_paused.try_lock().ok().map(|g| *g);
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
        let (_, chunks) = filter_chunks();
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

    /// A dictionary named by `Assign` must arrive first too.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_named_dictionary_is_waited_for() {
        let mut rt = Runtime::start(config(Arc::new(MapFetch::default())));
        rt.event(WorkerEvent::Assign(assignment(1, 9, 1)));
        let (version, chunks) = filter_chunks();
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
        let (_, chunks) = filter_chunks();
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
        let (_, chunks) = filter_chunks();
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
        let rt = Runtime::start(Config {
            seed: Some(7),
            ..Config::new(W, Arc::new(MapFetch::default()))
        });
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
            let rt = Runtime::start(Config {
                seed: Some(seed),
                ..Config::new(W, Arc::new(MapFetch::default()))
            });
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
        // Today's arrives: the result is judged and sealed.
        let (_, chunks) = filter_chunks();
        for c in chunks {
            rt.event(WorkerEvent::Snapshot(c));
        }
        until("sealed", || (sealed() == 1, sealed())).await;
        assert_eq!(c.lost.load(Ordering::Relaxed), 0, "nothing reported lost");
        assert_eq!(c.released.load(Ordering::Relaxed), 0);
        rt.shutdown().await;
    }

    /// A sealed record for `n`, as the judge would make it.
    fn sealed(n: usize) -> SealedRecord {
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
        let (_, chunks) = filter_chunks();
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
    async fn session_pair() -> (
        lodestar_cluster::CoordinatorLink,
        lodestar_cluster::WorkerSession,
    ) {
        session_pair_with(64).await
    }

    /// A session over a link of `capacity` frames: at 1, a coordinator
    /// that never reads stalls it after the handshake.
    async fn session_pair_with(
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
    async fn next_results(coord: &mut lodestar_cluster::CoordinatorLink) -> Payload {
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
