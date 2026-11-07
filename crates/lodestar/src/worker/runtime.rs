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
//!   and sealed (`Judging`); only `SealedRecord`s can reach the outbound
//!   queue.
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
    ByteReceiver, ByteSender, Held, Inbox, OUTBOUND_BYTES, Queues, Reports, Weigh, WorkerLeases,
    WorkerStages, byte_queue, next_claims,
};
use lodestar_pipeline::wire::ResultsBatch;
use lodestar_pipeline::wire::filter_snapshot::{
    DictSnapshot, Incoming, Judge, Judging, SealedRecord, Step,
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
/// Keys found here waiting to be checked and claimed. A full queue drops
/// the newest: the DHT finds a key again.
const FOUND_QUEUE: usize = 4096;
/// Room left in a `Results` payload for the batch's own header.
const BATCH_SLACK: usize = 4 << 10;
/// The source a worker's own finds are counted under (the DHT's). The
/// coordinator merges the real source bits from its own record.
const DHT_SOURCE: u8 = 1;

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
    /// Keys the have-filter said were stored, not claimed.
    pub skipped: AtomicU64,
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
}

/// The runtime (see the module docs).
pub struct Runtime {
    shared: Arc<Shared>,
    found: mpsc::Sender<Found>,
    tasks: Vec<JoinHandle<()>>,
    /// The session task, which owns the outbound queue's receiving end.
    results: Option<JoinHandle<()>>,
    /// The outcome task; it hands its queue back when stopped.
    reporter: Option<(watch::Sender<bool>, JoinHandle<Vec<OutcomeItem>>)>,
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

impl Finds {
    /// Queues a key to check and claim; `false` when the queue is full (the
    /// key is dropped: it will be found again).
    pub fn found(&self, found: Found) -> bool {
        self.0.try_send(found).is_ok()
    }
}

impl Runtime {
    /// Starts the runtime's tasks; the stages start once the snapshots are
    /// in (see the module docs). Must be called in a Tokio runtime.
    pub fn start(config: Config) -> Runtime {
        let (into_pool, pool_rx) = mpsc::unbounded_channel();
        let (acks, ack_rx) = mpsc::unbounded_channel();
        let (accepted, accepted_rx) = mpsc::unbounded_channel();
        let (loss_tx, loss_rx) = mpsc::unbounded_channel();
        let (found, found_rx) = mpsc::channel(FOUND_QUEUE);
        let (out, outbound) = byte_queue::<SealedRecord>(config.outbound_bytes, 4096);
        let counts = Arc::new(Counts::default());
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
            config,
        });
        let (stop_reports, reports_stopped) = watch::channel(false);
        Runtime {
            tasks: vec![
                tokio::spawn(send_acks(shared.clone(), ack_rx)),
                tokio::spawn(claim(shared.clone(), found_rx)),
            ],
            results: Some(tokio::spawn(send_results(
                shared.clone(),
                outbound,
                accepted_rx,
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
        let Some(stages) = lock(&self.shared.stages).take() else {
            return;
        };
        let _ = stages.stop.send(true);
        let deadline = self.shared.config.drain_deadline;
        let Stages { pool, prepare, .. } = stages;
        let both = async {
            let _ = pool.await;
            let _ = prepare.await;
        };
        let mut both = std::pin::pin!(both);
        if tokio::time::timeout(deadline, &mut both).await.is_err() {
            // A stalled session: the prepare stage is waiting on the
            // outbound queue. Its keys' leases go back to the coordinator.
            drop(both);
        }
    }

    /// Stops everything (a local shutdown) within the drain deadline.
    pub async fn shutdown(mut self) {
        self.stop_stages().await;
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
                    if current.is_none_or(|c| new.revision > c.revision) {
                        *current = Some(new);
                    }
                }
                start_if_ready(s);
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
    fn connected(&mut self, session: SessionHandle) {
        // Anything queued to the last session may be lost: ask for a fresh
        // have-filter snapshot (WK-3). The coordinator starts the filter
        // and dictionary snapshots over by itself (WK-10a).
        let _ = self.shared.acks.send(self.shared.have.reconnected());
        self.shared.session.send_replace(Some(session));
    }

    fn disconnected(&mut self) {
        self.shared.session.send_replace(None);
    }

    fn event(&mut self, event: WorkerEvent) {
        self.handle(event);
    }

    /// Takes no new keys, lets the pool hand over what's finished (within
    /// the deadline), and returns the losses not reported yet. Results the
    /// session task sent are committed by the coordinator; everything else
    /// goes back with `Drained`.
    async fn drain(&mut self) -> Vec<OutcomeItem> {
        self.stop_stages().await;
        self.take_outcomes().await
    }
}

impl Drop for Runtime {
    fn drop(&mut self) {
        if let Some(stages) = lock(&self.shared.stages).take() {
            let _ = stages.stop.send(true);
            stages.prepare.abort();
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
        snaps.asked += 1;
        snaps.asked
    };
    let s = s.clone();
    // Decoding builds the engine: up to about 96 MiB at the caps.
    tokio::task::spawn_blocking(move || {
        let ok = match kind {
            SnapshotKind::Filter => match complete.filter() {
                Ok(snapshot) => {
                    let mut done = lock(&s.install);
                    // A later transfer was installed first: keep it.
                    if *done < asked {
                        *done = asked;
                        let _notes = s.judge.install(&snapshot, complete.version);
                    }
                    true
                }
                Err(_) => false,
            },
            _ => match complete.dict() {
                Ok(d) => {
                    lock(&s.snapshots).dictionary = Some(d);
                    true
                }
                Err(_) => false,
            },
        };
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
    if stages.is_some() {
        return;
    }
    let (Some(out), Some(pool_rx)) = (lock(&s.out).take(), lock(&s.pool_rx).take()) else {
        return;
    };
    let (discoveries, rx) = mpsc::channel(256);
    tokio::spawn(forward(pool_rx, discoveries));
    let (stop, shutdown): (watch::Sender<bool>, Shutdown) = Shutdown::new();
    let reports: Arc<dyn Reports> = s.losses.clone();
    let running = WorkerStages {
        worker: s.config.worker,
        pool: PoolConfig {
            max_pending: usize::try_from(a.pool_cap).unwrap_or(usize::MAX),
            ..s.config.pool.clone()
        },
        fetcher: s.config.fetcher.clone(),
        peer_finders: Vec::new(),
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
        Judging {
            judge: s.judge.clone(),
            reports,
            out,
        },
    );
    *stages = Some(Stages {
        stop,
        pool: running.pool_task,
        prepare: running.prepare,
    });
    s.started.send_replace(true);
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
    tokio::spawn(check(s.clone(), found, items));
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
        bump(&s.counts.found);
        if s.have.is_ready() && s.have.might_be_stored(&f.key) {
            bump(&s.counts.skipped);
            continue;
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
        if items.send((item, peers)).await.is_err() {
            return;
        }
    }
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
}

/// The session task: sealed results into `Results` batches, kept until
/// accepted, sent again on a new session and after a refusal.
async fn send_results(
    s: Arc<Shared>,
    mut outbound: ByteReceiver<SealedRecord>,
    mut accepted: mpsc::UnboundedReceiver<(u64, bool)>,
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
                s.counts
                    .sent
                    .fetch_add(f.leases.len() as u64, Ordering::Relaxed);
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
                for (batch, payload, part) in encode(held, &mut next_batch) {
                    let leases = part.iter().map(|h| (h.get().key.0, h.get().token.0)).collect();
                    flight.insert(batch, InFlight { payload, leases, _held: part, sent: false, refused: false });
                }
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
type Encoded = (u64, Vec<u8>, Vec<Held<SealedRecord>>);

fn encode(held: Vec<Held<SealedRecord>>, next: &mut u64) -> Vec<Encoded> {
    let sealed: Vec<SealedRecord> = held.iter().map(|h| h.get().clone()).collect();
    let batch = ResultsBatch {
        batch: *next,
        records: Vec::new(),
    };
    match batch.encode_with(&sealed) {
        Ok(bytes) if bytes.len() <= MAX_OPAQUE => {
            *next += 1;
            vec![(batch.batch, bytes, held)]
        }
        _ if held.len() > 1 => {
            let mut held = held;
            let rest = held.split_off(held.len() / 2);
            let mut out = encode(held, next);
            out.extend(encode(rest, next));
            out
        }
        // One record that won't encode can't be sent; that can't happen
        // for a record the judge sealed. Its lease ends when it expires.
        _ => Vec::new(),
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

    use lodestar_cluster::assign::{Budget, Intensity, Politeness};
    use lodestar_cluster::proto::Assign;
    use lodestar_peer_wire::{FailureKind, Fetched};
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
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("c.db"), StoreConfig::default()).unwrap();
        let snap: FilterSnapshot =
            crate::cluster::filter::read_snapshot(&store, &[4; 32], unix_now()).unwrap();
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
    /// (here no session at all, and the outbound queue full) finishes
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
            let claimed = counts.claimed.load(Ordering::Relaxed);
            let skipped = counts.skipped.load(Ordering::Relaxed);
            refind();
            until("checked", || {
                let n = counts.skipped.load(Ordering::Relaxed);
                (n == skipped + raws.len() as u64, n)
            })
            .await;
            assert_eq!(
                counts.claimed.load(Ordering::Relaxed),
                claimed,
                "none claimed again"
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
