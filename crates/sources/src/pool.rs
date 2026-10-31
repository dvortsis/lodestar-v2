//! Crawl scheduler v1: the shared metadata-fetch pool (architecture §18.4).
//!
//! Keys from every source queue here. Keys with peers wait in one queue
//! per source; a deficit round robin over the sources, weighted by
//! priority, decides whose key gets the next socket, so a flood from one
//! source can't starve the others. User-interest keys jump the queue. The
//! global hard caps (sockets, connections per IP, queued keys, outstanding
//! peer lookups) always win.
//!
//! A key without peers asks the peer-finder plugins (the DHT's `get_peers`)
//! and waits; when every known peer has failed it asks again, with
//! exponential back-off, up to a retry limit. Each key leaves the pool
//! exactly once: fetched, or lost with a reason recorded in the funnel.
//!
//! One task owns all the state. Fetches run as child tasks and report back.

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap, HashSet, VecDeque};
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use lodestar_metainfo::InfoHash;
use lodestar_peer_wire::{FailureKind, FetchOptions, Fetched};
use lodestar_store::{QueuedKey, SourceId};
use tokio::sync::{mpsc, oneshot};
use tokio::task::{Id, JoinHandle, JoinSet};
use tokio::time::{Instant, sleep_until};

use crate::funnel::{Funnel, Loss, Stage};
use crate::plugin::{BoxFuture, Discovery, LookupSession, PeerRequest, Shutdown, Terminal};

mod admission;
use admission::{Admission, Class, Grant, WaitKind, Waiter};

/// The owned-request map, the one map whose iteration order reaches
/// scheduling (`poll_replies`). Production keeps the randomly keyed std hash
/// table; the `testing` feature swaps in a B-tree so the crawler simulator
/// (`crates/crawler-sim`) repeats across runs, platforms and toolchains.
#[cfg(not(feature = "testing"))]
type RequestMap = HashMap<InfoHash, Request>;
#[cfg(feature = "testing")]
type RequestMap = std::collections::BTreeMap<InfoHash, Request>;

/// Fetches one key from one peer.
pub trait Fetcher: Send + Sync + 'static {
    fn allows(&self, _peer: SocketAddr) -> bool {
        true
    }

    fn fetch(
        &self,
        peer: SocketAddr,
        key: InfoHash,
    ) -> BoxFuture<'static, Result<Fetched, FailureKind>>;
}

/// The real fetcher: TCP with MSE (§6).
#[derive(Debug, Clone, Default)]
pub struct PeerWireFetcher(pub FetchOptions);

impl Fetcher for PeerWireFetcher {
    fn allows(&self, peer: SocketAddr) -> bool {
        self.0.allows(peer)
    }

    fn fetch(
        &self,
        peer: SocketAddr,
        key: InfoHash,
    ) -> BoxFuture<'static, Result<Fetched, FailureKind>> {
        let opts = self.0.clone();
        Box::pin(async move {
            lodestar_peer_wire::fetch(peer, key, &opts)
                .await
                .map_err(|e| e.kind())
        })
    }
}

#[derive(Debug, Clone)]
pub struct PoolConfig {
    /// Concurrent fetches (§6 default 400; set by the intensity preset).
    pub max_sockets: usize,
    /// Concurrent connections to one IP.
    pub per_ip: usize,
    /// Keys queued or in flight; beyond this the oldest idle key is evicted.
    pub max_pending: usize,
    pub max_peers_per_key: usize,
    /// Peer lookups in flight across all peer finders.
    pub max_lookups: usize,
    /// How long a lookup may take to produce peers.
    pub lookup_wait: Duration,
    /// Wait before the second lookup; quadrupled for each later one.
    /// A minimum 1 ms delay prevents zero-duration rejection retry loops.
    pub retry_backoff: Duration,
    /// Lookups per key before it's given up as lost.
    pub max_attempts: u16,
    /// Recently fetched keys remembered, so a rediscovery racing the writer
    /// isn't fetched twice.
    pub recent: usize,
    /// Start paused (the crawler's pause): see [`PoolHandle::set_paused`].
    pub paused: bool,
    /// Coordinated admission (CR-ADM, `dht.coordinated_admission`): ask a
    /// finder only with one of its permits, and queue for one otherwise.
    /// Off, the pool behaves exactly as before the switch existed.
    pub coordinated_admission: bool,
    /// Permits per finder under coordinated admission: the finder's own
    /// cap on owned requests (the DHT's `PEER_REQUEST_CAPACITY`).
    pub finder_capacity: usize,
    /// Streaming peers (CR-1a-S, `dht.streaming_peers`): ask finders for a
    /// [`LookupSession`], so a fetch can start on the first batch while the
    /// lookup keeps its slot until it ends. Off, the pool behaves exactly
    /// as before the switch existed.
    pub streaming_peers: bool,
}

impl Default for PoolConfig {
    fn default() -> Self {
        PoolConfig {
            max_sockets: 400,
            per_ip: 2,
            max_pending: 200_000,
            max_peers_per_key: 32,
            max_lookups: 256,
            lookup_wait: Duration::from_secs(30),
            retry_backoff: Duration::from_secs(120),
            max_attempts: 4,
            recent: 50_000,
            paused: false,
            coordinated_admission: false,
            finder_capacity: 32,
            streaming_peers: false,
        }
    }
}

/// A verified info dictionary on its way to the writer.
#[derive(Debug, Clone)]
pub struct FetchedKey {
    pub key: InfoHash,
    pub fetched: Fetched,
    /// The source that found it first.
    pub source: SourceId,
    /// Every source that reported it while it was queued (a bitset).
    pub sources: u64,
    pub cohort: i64,
    pub strategy: u8,
    /// Unix seconds of the first discovery.
    pub discovered_at: i64,
    /// Only private sources reported it (§18.7); the torrent is stored
    /// flagged so it's never scraped publicly.
    pub private_source: bool,
}

/// Counters for metrics and the status page.
#[derive(Debug, Default)]
pub struct PoolStats {
    pub pending: AtomicU64,
    pub active: AtomicU64,
    pub waiting: AtomicU64,
    pub lookups: AtomicU64,
    pub lookup_backlog: AtomicU64,
    pub fetched: AtomicU64,
    /// Failed fetch attempts by [`FailureKind`], in declaration order.
    pub failures: [AtomicU64; 7],
    /// Keys lost, by [`Loss`] code (index = code).
    pub lost: [AtomicU64; 11],
    pub duplicates: AtomicU64,
    pub finder_full: AtomicU64,
    pub finder_closed: AtomicU64,
    /// Requests completed or timed out without observed accepted ownership.
    pub lookup_unaccepted: AtomicU64,
    pub retry_unaccepted: AtomicU64,
    /// Eviction service reached this process: unaccepted, lookup, peers, fetch.
    pub evicted_service: [AtomicU64; 4],
    /// Coordinated admission (CR-ADM); all zero with the switch off.
    /// Keys that queued for a permit, by kind: arrival, retry, requeue.
    pub admission_waits: [AtomicU64; 3],
    /// Waiters that left without a request: evicted, peers arrived.
    pub admission_wait_cancelled: [AtomicU64; 2],
    pub admission_wakeups: AtomicU64,
    /// Requests granted permits, by kind, then class (interactive,
    /// background).
    pub admission_grants: [[AtomicU64; 2]; 3],
    /// Permitted requests not accepted: refused, unacknowledged.
    pub admission_refusals: [AtomicU64; 2],
    pub admission_holds: AtomicU64,
    /// Permits returned in the pool: finder full, finder closed, unused.
    pub admission_permit_returns: [AtomicU64; 3],
    /// Gauges: keys waiting for a lookup, and permits requests hold.
    pub admission_waiters: AtomicU64,
    pub admission_permits_in_use: AtomicU64,
    /// Streaming peers (CR-1a-S); all zero with the switch off. Batches
    /// read from sessions, and their peers an entry turned away because it
    /// already held `max_peers_per_key` (not producer backpressure).
    pub stream_batches: AtomicU64,
    pub stream_peers_capped: AtomicU64,
    /// Session terminals seen, by [`Terminal::ALL`]; a dropped terminal
    /// sender counts as `source_failed`.
    pub stream_terminals: [AtomicU64; 4],
    /// Time from a key's first batch to its first fetch start: the sum in
    /// milliseconds, and its count.
    pub stream_first_fetch_millis: AtomicU64,
    pub stream_first_fetches: AtomicU64,
}

impl PoolStats {
    pub fn failure_count(&self, kind: FailureKind) -> u64 {
        self.failures[failure_index(kind)].load(Ordering::Relaxed)
    }

    pub fn lost_count(&self, loss: Loss) -> u64 {
        self.lost[loss as usize].load(Ordering::Relaxed)
    }
}

fn failure_index(kind: FailureKind) -> usize {
    match kind {
        FailureKind::Unreachable => 0,
        FailureKind::TimedOut => 1,
        FailureKind::Refused => 2,
        FailureKind::EncryptionFailed => 3,
        FailureKind::Invalid => 4,
        FailureKind::Oversize => 5,
        FailureKind::HashMismatch => 6,
    }
}

enum Cmd {
    SetWeight(SourceId, u32),
    Interest(InfoHash, oneshot::Sender<bool>),
    Snapshot(oneshot::Sender<Vec<QueuedKey>>),
    Queued(oneshot::Sender<Vec<(SourceId, usize)>>),
    SetPaused(bool),
}

/// Talks to a running pool.
#[derive(Clone)]
pub struct PoolHandle {
    cmds: mpsc::Sender<Cmd>,
    stats: Arc<PoolStats>,
}

impl PoolHandle {
    pub fn stats(&self) -> &PoolStats {
        &self.stats
    }

    pub async fn set_weight(&self, source: SourceId, weight: u32) {
        let _ = self.cmds.send(Cmd::SetWeight(source, weight)).await;
    }

    /// Moves a queued key to the front. Returns false if it isn't queued.
    pub async fn interest(&self, key: InfoHash) -> bool {
        let (tx, rx) = oneshot::channel();
        let _ = self.cmds.send(Cmd::Interest(key, tx)).await;
        rx.await.unwrap_or(false)
    }

    /// Everything queued or in flight, for periodic persistence.
    pub async fn snapshot(&self) -> Vec<QueuedKey> {
        let (tx, rx) = oneshot::channel();
        let _ = self.cmds.send(Cmd::Snapshot(tx)).await;
        rx.await.unwrap_or_default()
    }

    /// Pauses or resumes the pool (the crawler's pause). Paused, it starts
    /// no fetches and asks for no peers: keys keep queueing, and a key due
    /// a lookup waits for one without using up an attempt. Fetches and
    /// lookups already out finish or time out as usual.
    pub async fn set_paused(&self, paused: bool) {
        let _ = self.cmds.send(Cmd::SetPaused(paused)).await;
    }

    /// Keys ready to fetch, per source.
    pub async fn queued(&self) -> Vec<(SourceId, usize)> {
        let (tx, rx) = oneshot::channel();
        let _ = self.cmds.send(Cmd::Queued(tx)).await;
        rx.await.unwrap_or_default()
    }
}

impl std::fmt::Debug for PoolHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PoolHandle")
            .field("stats", &self.stats)
            .finish()
    }
}

/// What the pool is wired to.
pub struct PoolIo {
    pub discoveries: mpsc::Receiver<Discovery>,
    /// Peer-finder plugins' request channels.
    pub peer_finders: Vec<mpsc::Sender<PeerRequest>>,
    pub out: mpsc::Sender<FetchedKey>,
    pub funnel: Arc<dyn Funnel>,
    pub shutdown: Shutdown,
}

impl std::fmt::Debug for PoolIo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PoolIo")
            .field("peer_finders", &self.peer_finders.len())
            .finish_non_exhaustive()
    }
}

/// Starts the pool. `weights` are the sources' DRR weights (default 3);
/// `restore` is the queue saved by the previous run. The task returns the
/// queue at shutdown, for persisting.
pub fn spawn(
    config: PoolConfig,
    fetcher: Arc<dyn Fetcher>,
    io: PoolIo,
    weights: HashMap<SourceId, u32>,
    restore: Vec<QueuedKey>,
) -> (PoolHandle, JoinHandle<Vec<QueuedKey>>) {
    let (cmd_tx, cmd_rx) = mpsc::channel(64);
    let stats = Arc::new(PoolStats::default());
    let admission = config
        .coordinated_admission
        .then(|| Admission::new(io.peer_finders.len(), config.finder_capacity.max(1)));
    let mut pool = Pool {
        cfg: config,
        fetcher,
        funnel: io.funnel,
        finders: io.peer_finders,
        out: io.out,
        stats: stats.clone(),
        entries: HashMap::new(),
        sources: HashMap::new(),
        rotation: VecDeque::new(),
        interest: VecDeque::new(),
        timers: BinaryHeap::new(),
        timer_seq: 0,
        generations: 0,
        admitted: VecDeque::new(),
        lookup_backlog: VecDeque::new(),
        admission,
        waiting: 0,
        lookups: 0,
        requests: RequestMap::default(),
        per_ip: HashMap::new(),
        tasks: JoinSet::new(),
        task_keys: HashMap::new(),
        recent: HashSet::new(),
        recent_order: VecDeque::new(),
        first_batch: HashMap::new(),
    };
    for (source, weight) in weights {
        pool.source(source).weight = weight.max(1);
    }
    for q in restore {
        pool.restore(q);
    }
    let task = tokio::spawn(pool.run(io.discoveries, cmd_rx, io.shutdown));
    (
        PoolHandle {
            cmds: cmd_tx,
            stats,
        },
        task,
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    /// In a ready queue (it has untried peers).
    Ready,
    Fetching,
    /// Waiting for a lookup slot.
    LookupQueued,
    /// A lookup is out; peers may arrive until the timer fires.
    Lookup,
    /// Every peer failed or none turned up; the next lookup is scheduled.
    Backoff,
}

struct Entry {
    source: SourceId,
    sources: u64,
    cohort: i64,
    strategy: u8,
    discovered_at: i64,
    private: bool,
    interest: bool,
    peers: Vec<SocketAddr>,
    /// `peers[..tried]` have been tried.
    tried: usize,
    attempts: u16,
    had_peers: bool,
    /// Transient diagnostics only; never affects scheduling or persistence.
    lookup_requested: bool,
    fetch_started: bool,
    last_failure: Option<FailureKind>,
    state: State,
    /// Set from the pool's counter on every state change, to invalidate
    /// old timers and requests. Pool-wide, so a key evicted and found again
    /// never meets a timer left by its earlier lifetime.
    generation: u64,
}

impl Entry {
    fn has_untried(&self) -> bool {
        self.tried < self.peers.len()
    }
}

struct SourceQueue {
    weight: u32,
    deficit: u32,
    ready: VecDeque<InfoHash>,
    in_rotation: bool,
}

enum FinderReply {
    /// The finder's index and its admission answer.
    Admission(
        usize,
        oneshot::Receiver<Option<oneshot::Receiver<Vec<SocketAddr>>>>,
    ),
    Completion(oneshot::Receiver<Vec<SocketAddr>>),
    /// A streaming request's admission (CR-1a-S): the finder answers the
    /// session, or, if it doesn't stream, the ordinary admission.
    StreamAdmission(
        usize,
        oneshot::Receiver<Option<oneshot::Receiver<Vec<SocketAddr>>>>,
        oneshot::Receiver<Option<LookupSession>>,
    ),
    Session(LookupSession),
}

struct Request {
    generation: u64,
    /// Sent with admission permits (CR-ADM).
    coordinated: bool,
    /// A finder refused it, in this poll or an earlier one; it classifies
    /// the request's refusal if no finder accepts.
    refused: bool,
    accepted: bool,
    /// An accepted finder answered. Accepted with none answering means the
    /// finders dropped the work (a pause, a shutdown): cancelled, and the
    /// attempt isn't used.
    answered: bool,
    replies: Vec<FinderReply>,
    /// Sent as a streaming request (CR-1a-S). Accepted, it owns its lookup
    /// apart from the entry's state, until every finder's terminal or the
    /// deadline; its `generation` then fences its own deadline timer.
    streaming: bool,
    /// A batch has brought peers.
    batched: bool,
}

struct Pool {
    cfg: PoolConfig,
    fetcher: Arc<dyn Fetcher>,
    funnel: Arc<dyn Funnel>,
    finders: Vec<mpsc::Sender<PeerRequest>>,
    out: mpsc::Sender<FetchedKey>,
    stats: Arc<PoolStats>,
    entries: HashMap<InfoHash, Entry>,
    sources: HashMap<SourceId, SourceQueue>,
    /// Sources with ready keys, in DRR order.
    rotation: VecDeque<SourceId>,
    interest: VecDeque<InfoHash>,
    timers: BinaryHeap<Reverse<(Instant, u64, InfoHash, u64)>>,
    timer_seq: u64,
    /// The last entry generation handed out ([`next_generation`]).
    generations: u64,
    /// Admission order, for eviction (lazily cleaned).
    admitted: VecDeque<InfoHash>,
    lookup_backlog: VecDeque<InfoHash>,
    /// Coordinated admission's permits and waiters, with the switch on;
    /// they replace `lookup_backlog` then.
    admission: Option<Admission>,
    /// Entries in `LookupQueued`.
    waiting: usize,
    lookups: usize,
    requests: RequestMap,
    per_ip: HashMap<IpAddr, usize>,
    tasks: JoinSet<Result<Fetched, FailureKind>>,
    /// What each fetch task is fetching, so even a panicked task is
    /// accounted for.
    task_keys: HashMap<Id, (InfoHash, SocketAddr)>,
    recent: HashSet<InfoHash>,
    recent_order: VecDeque<InfoHash>,
    /// Streaming peers: when keys not yet fetched got their first batch,
    /// for the first-batch-to-fetch time. At most one per entry.
    first_batch: HashMap<InfoHash, Instant>,
}

/// A queued copy still stands for its entry: the entry waits for a
/// lookup at the generation it was queued with.
fn is_waiting(entries: &HashMap<InfoHash, Entry>, w: &Waiter) -> bool {
    entries
        .get(&w.key)
        .is_some_and(|e| e.state == State::LookupQueued && e.generation == w.generation)
}

/// The next entry generation. Never 0, which a new entry starts at, and
/// never repeated in a pool's life.
fn next_generation(last: &mut u64) -> u64 {
    *last += 1;
    *last
}

impl Pool {
    async fn run(
        mut self,
        mut discoveries: mpsc::Receiver<Discovery>,
        mut cmds: mpsc::Receiver<Cmd>,
        mut shutdown: Shutdown,
    ) -> Vec<QueuedKey> {
        let mut replies = tokio::time::interval(Duration::from_millis(25));
        replies.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            self.dispatch();
            self.publish_stats();
            let wake = self.timers.peek().map(|Reverse((at, ..))| *at);
            let credit = self.credit_target();
            tokio::select! {
                biased;
                _ = shutdown.requested() => break,
                Some(joined) = self.tasks.join_next_with_id(), if !self.tasks.is_empty() => {
                    let (id, result) = match joined {
                        Ok((id, result)) => (id, result),
                        // Only a panic ends a fetch task early: a bug, but
                        // one peer's bytes mustn't take the pool down.
                        Err(e) => (e.id(), Err(FailureKind::Invalid)),
                    };
                    if let Some((key, peer)) = self.task_keys.remove(&id) {
                        self.on_result(key, peer, result).await;
                    }
                }
                _ = replies.tick(), if !self.requests.is_empty() => self.poll_replies(),
                Some(cmd) = cmds.recv() => self.on_cmd(cmd),
                (finder, permit) = admission::acquire(credit.clone()), if credit.is_some() => {
                    self.stats.admission_wakeups.fetch_add(1, Ordering::Relaxed);
                    if let Some(a) = &mut self.admission {
                        a.on_woken(finder, permit);
                    }
                    // A finder answers a refusal before it returns the
                    // permit, so reading replies now sets the refusal's
                    // hold before the permit can go to another waiter.
                    self.poll_replies();
                }
                _ = sleep_until(wake.unwrap_or_else(Instant::now)), if wake.is_some() => {
                    self.poll_replies();
                    self.on_timers();
                }
                Some(d) = discoveries.recv() => {
                    self.on_discovery(d);
                    // Take what's already waiting before dispatching again.
                    for _ in 0..255 {
                        match discoveries.try_recv() {
                            Ok(d) => self.on_discovery(d),
                            Err(_) => break,
                        }
                    }
                }

            }
        }
        self.tasks.abort_all();
        if let Some(a) = &mut self.admission {
            let unused = a.drop_held();
            self.stats.admission_permit_returns[2].fetch_add(unused, Ordering::Relaxed);
        }
        self.snapshot()
    }

    fn source(&mut self, id: SourceId) -> &mut SourceQueue {
        self.sources.entry(id).or_insert(SourceQueue {
            weight: crate::Priority::Normal.weight(),
            deficit: 0,
            ready: VecDeque::new(),
            in_rotation: false,
        })
    }

    fn on_cmd(&mut self, cmd: Cmd) {
        match cmd {
            Cmd::SetWeight(source, weight) => self.source(source).weight = weight.max(1),
            Cmd::Interest(key, reply) => {
                let found = self.entries.contains_key(&key);
                if found {
                    self.raise_interest(key);
                }
                let _ = reply.send(found);
            }
            Cmd::Snapshot(reply) => {
                let _ = reply.send(self.snapshot());
            }
            Cmd::SetPaused(paused) => {
                self.cfg.paused = paused;
                // A paused pool grants nothing, so it keeps no permit back.
                if paused && let Some(a) = &mut self.admission {
                    let unused = a.drop_held();
                    self.stats.admission_permit_returns[2].fetch_add(unused, Ordering::Relaxed);
                }
            }
            Cmd::Queued(reply) => {
                let mut q: Vec<(SourceId, usize)> = self
                    .sources
                    .iter()
                    .map(|(id, s)| (*id, s.ready.len()))
                    .collect();
                q.sort();
                let _ = reply.send(q);
            }
        }
    }

    fn on_discovery(&mut self, d: Discovery) {
        self.poll_request(d.found.key);
        let key = d.found.key;
        // Peers for a key the pool has finished with (fetched, lost) are
        // late; they don't make it new again.
        if d.reply && !self.entries.contains_key(&key) {
            self.stats.duplicates.fetch_add(1, Ordering::Relaxed);
            return;
        }
        if self.recent.contains(&key) {
            self.stats.duplicates.fetch_add(1, Ordering::Relaxed);
            return;
        }
        if self.entries.contains_key(&key) {
            self.stats.duplicates.fetch_add(1, Ordering::Relaxed);
            let e = self.entries.get_mut(&key).expect("present");
            e.sources |= 1u64 << d.source.get();
            // A private source can't make a public key private, but a
            // public sighting does make a private key public.
            e.private &= d.found.private_source;
            if d.found.interest {
                self.raise_interest(key);
            }
            self.add_peers(key, &d.found.peers);
            return;
        }
        if self.entries.len() >= self.cfg.max_pending && !self.evict_one() {
            let cohort = d.at.div_euclid(3600);
            self.funnel.admitted(&key, cohort, d.source);
            self.stats.evicted_service[0].fetch_add(1, Ordering::Relaxed);
            self.lose_untracked(&key, cohort, d.source, Loss::Evicted, None);
            return;
        }
        let cohort = d.at.div_euclid(3600);
        self.funnel.admitted(&key, cohort, d.source);
        self.entries.insert(
            key,
            Entry {
                source: d.source,
                sources: 1u64 << d.source.get(),
                cohort,
                strategy: d.found.strategy,
                discovered_at: d.at,
                private: d.found.private_source,
                interest: d.found.interest,
                peers: Vec::new(),
                tried: 0,
                attempts: 0,
                had_peers: false,
                lookup_requested: false,
                fetch_started: false,
                last_failure: None,
                state: State::Backoff,
                generation: 0,
            },
        );
        self.admitted.push_back(key);
        if self.admitted.len() > 2 * self.entries.len() + 1024 {
            let entries = &self.entries;
            self.admitted.retain(|k| entries.contains_key(k));
        }
        if d.found.peers.is_empty() {
            self.request_peers(key, WaitKind::Arrival);
        } else {
            self.add_peers(key, &d.found.peers);
            if self
                .entries
                .get(&key)
                .is_some_and(|e| e.state == State::Backoff)
            {
                self.request_peers(key, WaitKind::Arrival);
            }
        }
    }

    fn restore(&mut self, q: QueuedKey) {
        if self.entries.contains_key(&q.info_hash) || self.entries.len() >= self.cfg.max_pending {
            return;
        }
        self.entries.insert(
            q.info_hash,
            Entry {
                source: q.source,
                sources: 1u64 << q.source.get(),
                cohort: q.cohort,
                strategy: q.strategy,
                discovered_at: q.cohort * 3600,
                private: q.private_source,
                interest: q.priority > 0,
                peers: Vec::new(),
                tried: 0,
                attempts: q.attempts,
                had_peers: false,
                lookup_requested: false,
                fetch_started: false,
                last_failure: None,
                state: State::Backoff,
                generation: 0,
            },
        );
        self.admitted.push_back(q.info_hash);
        self.request_peers(q.info_hash, WaitKind::Arrival);
    }

    /// Merges peers into a key; a key that was waiting becomes ready.
    /// Returns how many new peers the entry's cap turned away.
    fn add_peers(&mut self, key: InfoHash, peers: &[SocketAddr]) -> usize {
        let cap = self.cfg.max_peers_per_key;
        let Some(e) = self.entries.get_mut(&key) else {
            return 0;
        };
        let before = e.peers.len();
        let mut capped = 0;
        for p in peers
            .iter()
            .copied()
            .map(lodestar_network_policy::normalize)
        {
            if !self.fetcher.allows(p) {
                continue;
            }
            if e.peers.contains(&p) {
                continue;
            }
            if e.peers.len() >= cap {
                capped += 1;
                continue;
            }
            e.peers.push(p);
        }
        if e.peers.len() == before {
            return capped;
        }
        if !e.had_peers {
            e.had_peers = true;
            self.funnel
                .record(e.cohort, e.source, Stage::PeerFound, None);
        }
        let state = e.state;
        match state {
            State::Ready | State::Fetching => {}
            // CR-1a-S: peers make the key socket-eligible, but an accepted
            // stream keeps its lookup slot until its terminal.
            State::Lookup if self.owns_stream(&key) => self.make_ready(key),
            State::Lookup => {
                self.lookups -= 1;
                self.requests.remove(&key);
                self.make_ready(key);
            }
            State::LookupQueued | State::Backoff => self.make_ready(key),
        }
        capped
    }

    /// An accepted streaming request holds this key's lookup (CR-1a-S).
    fn owns_stream(&self, key: &InfoHash) -> bool {
        self.requests
            .get(key)
            .is_some_and(|r| r.streaming && r.accepted)
    }

    fn make_ready(&mut self, key: InfoHash) {
        let e = self.entries.get_mut(&key).expect("present");
        if e.state == State::LookupQueued {
            self.waiting -= 1;
            if self.admission.is_some() {
                self.stats.admission_wait_cancelled[1].fetch_add(1, Ordering::Relaxed);
            }
        }
        e.state = State::Ready;
        e.generation = next_generation(&mut self.generations);
        let (source, interest) = (e.source, e.interest);
        if interest {
            self.interest.push_back(key);
        } else {
            let q = self.source(source);
            q.ready.push_back(key);
            if !q.in_rotation {
                q.in_rotation = true;
                self.rotation.push_back(source);
            }
        }
    }

    fn raise_interest(&mut self, key: InfoHash) {
        let Some(e) = self.entries.get_mut(&key) else {
            return;
        };
        if e.interest {
            return;
        }
        e.interest = true;
        match e.state {
            // The stale copy in its source queue is skipped when popped.
            State::Ready => self.interest.push_back(key),
            State::Backoff => self.request_peers(key, WaitKind::Retry),
            // A copy joins the interactive queue; whichever is granted
            // first makes the other stale.
            // Private keys never take permits.
            State::LookupQueued if self.admission.is_some() && !e.private => {
                let kind = if e.lookup_requested {
                    WaitKind::Retry
                } else {
                    WaitKind::Arrival
                };
                let waiter = Waiter {
                    key,
                    generation: e.generation,
                    kind,
                };
                if let Some(a) = &mut self.admission {
                    a.push(Class::Interactive, waiter, false);
                }
            }
            State::Fetching | State::LookupQueued | State::Lookup => {}
        }
    }

    /// Asks the peer finders for peers, or queues the ask if too many
    /// lookups are out. Gives the key up once it has used its attempts.
    /// Under coordinated admission it also queues without a permit from
    /// every open finder, or behind keys already waiting.
    fn request_peers(&mut self, key: InfoHash, kind: WaitKind) {
        self.request_peers_as(key, kind, None);
    }

    /// [`request_peers`](Self::request_peers), or with `grant` the send a
    /// waiter's permits were taken for (from [`drain_admission`]).
    ///
    /// [`drain_admission`]: Self::drain_admission
    fn request_peers_as(&mut self, key: InfoHash, kind: WaitKind, grant: Option<Grant>) {
        let max_attempts = self.cfg.max_attempts;
        let e = self.entries.get_mut(&key).expect("present");
        if e.attempts >= max_attempts {
            let loss = e.last_failure.map_or(Loss::NoPeers, Loss::from);
            self.lose(key, loss);
            if grant.is_some() {
                self.stats.admission_permit_returns[2].fetch_add(1, Ordering::Relaxed);
            }
            return;
        }
        if e.state == State::LookupQueued {
            self.waiting -= 1;
        }
        e.generation = next_generation(&mut self.generations);
        let coordinated = !e.private && self.admission.as_ref().is_some_and(Admission::active);
        if self.cfg.paused || self.lookups >= self.cfg.max_lookups {
            // `drain_admission` only grants while neither holds.
            debug_assert!(grant.is_none(), "a grant while paused or at the cap");
            if coordinated {
                self.wait(key, kind, false);
                return;
            }
            e.state = State::LookupQueued;
            self.waiting += 1;
            if e.interest {
                self.lookup_backlog.push_front(key);
            } else {
                self.lookup_backlog.push_back(key);
            }
            return;
        }
        let grant = match grant {
            Some(grant) => Some(grant),
            None if coordinated => {
                self.poll_unaccepted();
                let entries = &self.entries;
                let a = self.admission.as_mut().expect("coordinated");
                // A key already waiting goes first: no arrival or retry
                // overtakes one.
                let permits = if a.has_live_waiter(|w| is_waiting(entries, w)) {
                    None
                } else {
                    a.take_permits(Instant::now())
                };
                let Some(permits) = permits else {
                    self.wait(key, kind, false);
                    return;
                };
                let class = if entries[&key].interest {
                    Class::Interactive
                } else {
                    Class::Background
                };
                Some(Grant { permits, class })
            }
            None => None,
        };
        let e = self.entries.get_mut(&key).expect("present");
        if e.lookup_requested && e.attempts == 0 {
            self.stats.retry_unaccepted.fetch_add(1, Ordering::Relaxed);
        }
        e.lookup_requested = true;
        e.state = State::Lookup;
        let generation = e.generation;
        let coordinated = grant.is_some();
        // Private keys wait for their own source; no public request is made.
        let mut replies = Vec::new();
        if !e.private {
            let mut permits = grant.map(|g| {
                self.stats.admission_grants[kind as usize][g.class as usize]
                    .fetch_add(1, Ordering::Relaxed);
                g.permits.into_iter()
            });
            for (i, finder) in self.finders.iter().enumerate() {
                let permit = match &mut permits {
                    // A closed finder has no permit and isn't asked again.
                    Some(p) => match p.next().flatten() {
                        Some(permit) => Some(permit),
                        None => continue,
                    },
                    None => None,
                };
                let (admit, rx) = oneshot::channel();
                let (session, session_rx) = if self.cfg.streaming_peers {
                    let (tx, rx) = oneshot::channel();
                    (Some(tx), Some(rx))
                } else {
                    (None, None)
                };
                let request = PeerRequest {
                    key,
                    admit,
                    session,
                    permit,
                };
                match finder.try_send(request) {
                    Ok(()) => replies.push(match session_rx {
                        Some(session) => FinderReply::StreamAdmission(i, rx, session),
                        None => FinderReply::Admission(i, rx),
                    }),
                    Err(mpsc::error::TrySendError::Full(r)) => {
                        self.stats.finder_full.fetch_add(1, Ordering::Relaxed);
                        if r.permit.is_some() {
                            self.stats.admission_permit_returns[0].fetch_add(1, Ordering::Relaxed);
                        }
                    }
                    Err(mpsc::error::TrySendError::Closed(r)) => {
                        self.stats.finder_closed.fetch_add(1, Ordering::Relaxed);
                        if r.permit.is_some() {
                            self.stats.admission_permit_returns[1].fetch_add(1, Ordering::Relaxed);
                            if let Some(a) = &mut self.admission {
                                a.close(i);
                            }
                        }
                    }
                }
            }
        }
        if replies.is_empty() {
            self.stats.lookup_unaccepted.fetch_add(1, Ordering::Relaxed);
            e.state = State::Backoff;
            self.schedule(
                Instant::now() + self.cfg.retry_backoff.max(Duration::from_secs(1)),
                key,
                generation,
            );
            return;
        }
        self.requests.insert(
            key,
            Request {
                generation,
                coordinated,
                refused: false,
                accepted: false,
                answered: false,
                replies,
                streaming: self.cfg.streaming_peers,
                batched: false,
            },
        );
        self.lookups += 1;
        let at = Instant::now() + self.cfg.lookup_wait;
        self.schedule(at, key, generation);
    }

    /// Queues a key for a permit (coordinated admission). Requeued work
    /// has waited its turn already and goes to the front.
    fn wait(&mut self, key: InfoHash, kind: WaitKind, front: bool) {
        let e = self.entries.get_mut(&key).expect("present");
        e.state = State::LookupQueued;
        self.waiting += 1;
        let class = if e.interest {
            Class::Interactive
        } else {
            Class::Background
        };
        let waiter = Waiter {
            key,
            generation: e.generation,
            kind,
        };
        self.stats.admission_waits[kind as usize].fetch_add(1, Ordering::Relaxed);
        let entries = &self.entries;
        let a = self.admission.as_mut().expect("coordinated");
        a.push(class, waiter, front);
        a.compact(self.waiting, |w| is_waiting(entries, w));
    }

    /// Grants waiting keys the permits available now, oldest first, and
    /// gives back a woken permit nobody is left to use.
    fn drain_admission(&mut self) {
        loop {
            if self.lookups >= self.cfg.max_lookups {
                return;
            }
            self.poll_unaccepted();
            let entries = &self.entries;
            let Some(a) = self.admission.as_mut() else {
                return;
            };
            let live = |w: &Waiter| is_waiting(entries, w);
            if !a.has_live_waiter(live) {
                break;
            }
            if !a.active() {
                // Every finder closed: waiters take the ordinary no-finder
                // path (unaccepted, back-off), with no permit to grant.
                let (waiter, _) = a.pop(live).expect("a live waiter");
                self.request_peers(waiter.key, waiter.kind);
                continue;
            }
            let Some(permits) = a.take_permits(Instant::now()) else {
                return;
            };
            let (waiter, class) = a.pop(live).expect("a live waiter");
            self.request_peers_as(waiter.key, waiter.kind, Some(Grant { permits, class }));
        }
        if let Some(a) = &mut self.admission {
            let unused = a.drop_held();
            self.stats.admission_permit_returns[2].fetch_add(unused, Ordering::Relaxed);
        }
    }

    /// Reads the answers of permitted requests not yet accepted. A finder
    /// answers a refusal before it returns the permit, so reading first
    /// sets the refusal's hold before that permit can be taken again,
    /// whichever path takes it.
    fn poll_unaccepted(&mut self) {
        let keys: Vec<_> = self
            .requests
            .iter()
            .filter(|(_, r)| r.coordinated && !r.accepted)
            .map(|(key, _)| *key)
            .collect();
        for key in keys {
            self.poll_request(key);
        }
    }

    /// What the pool's wakeup waits on: a finder's next permit, while keys
    /// wait for one and the pool could use it.
    fn credit_target(&self) -> Option<admission::WaitTarget> {
        let a = self.admission.as_ref()?;
        if self.cfg.paused || self.lookups >= self.cfg.max_lookups || !a.has_waiters() {
            return None;
        }
        a.wait_target()
    }

    /// Poll only the bounded set of owned requests; no task per discovery.
    fn poll_replies(&mut self) {
        let keys: Vec<_> = self.requests.keys().copied().collect();
        for key in keys {
            self.poll_request(key);
        }
    }

    fn poll_request(&mut self, key: InfoHash) {
        use tokio::sync::oneshot::error::TryRecvError;
        let Some(mut request) = self.requests.remove(&key) else {
            return;
        };
        if request.streaming {
            self.poll_stream(key, request);
            return;
        }
        if !self
            .entries
            .get(&key)
            .is_some_and(|e| e.state == State::Lookup && e.generation == request.generation)
        {
            return;
        }
        let mut waiting = Vec::new();
        let mut peers = Vec::new();
        let mut unique = HashSet::new();
        // Finders that refused, or never acknowledged, this request: each
        // rests from the poll that sees it, whatever the other finders do.
        let mut said_no = Vec::new();
        for mut reply in request.replies {
            if let FinderReply::Admission(finder, rx) = &mut reply {
                let finder = *finder;
                match rx.try_recv() {
                    Ok(Some(done)) => {
                        if !request.accepted {
                            self.entries.get_mut(&key).expect("present").attempts += 1;
                            request.accepted = true;
                        }
                        if request.coordinated
                            && let Some(a) = &mut self.admission
                        {
                            a.clear_hold(finder);
                        }
                        reply = FinderReply::Completion(done);
                    }
                    Err(TryRecvError::Empty) => {
                        waiting.push(reply);
                        continue;
                    }
                    Ok(None) => {
                        request.refused = true;
                        said_no.push(finder);
                        continue;
                    }
                    Err(TryRecvError::Closed) => {
                        said_no.push(finder);
                        continue;
                    }
                }
            }
            if let FinderReply::Completion(rx) = &mut reply {
                match rx.try_recv() {
                    Ok(found) => {
                        request.answered = true;
                        for p in found.into_iter().map(lodestar_network_policy::normalize) {
                            if peers.len() >= self.cfg.max_peers_per_key {
                                break;
                            }
                            if self.fetcher.allows(p) && unique.insert(p) {
                                peers.push(p);
                            }
                        }
                    }
                    Err(TryRecvError::Empty) => waiting.push(reply),
                    Err(TryRecvError::Closed) => {}
                }
            }
        }
        request.replies = waiting;
        let finished = request.replies.is_empty();
        if finished && !request.accepted {
            self.stats.lookup_unaccepted.fetch_add(1, Ordering::Relaxed);
        }
        if request.coordinated {
            self.hold_finders(said_no.iter());
        }
        if finished && !request.accepted && request.coordinated {
            self.requeue_refused(key, request.refused, &[]);
            return;
        }
        if finished && request.accepted && !request.answered {
            self.requeue_cancelled(key, request.coordinated);
            return;
        }
        self.requests.insert(key, request);
        self.add_peers(key, &peers);
        if finished
            && self
                .entries
                .get(&key)
                .is_some_and(|e| e.state == State::Lookup)
        {
            self.requests.remove(&key);
            self.lookups -= 1;
            self.entries.get_mut(&key).expect("present").state = State::Backoff;
            self.back_off(key);
        }
    }

    /// [`poll_request`](Self::poll_request) for a streaming request
    /// (CR-1a-S). Batches make the key socket-eligible without releasing
    /// the lookup; a finder's terminal releases only that finder, after its
    /// queued batches are drained; the last one releases the logical lookup
    /// once. The request is the receivers' fence: it leaves the map with
    /// its entry, so nothing stale can reach a recreated one.
    fn poll_stream(&mut self, key: InfoHash, mut request: Request) {
        use tokio::sync::oneshot::error::TryRecvError;
        let Some(e) = self.entries.get(&key) else {
            debug_assert!(false, "a request outlived its entry");
            return;
        };
        let untouched = e.state == State::Lookup && e.generation == request.generation;
        let mut waiting = Vec::new();
        let mut peers = Vec::new();
        let mut said_no = Vec::new();
        for reply in std::mem::take(&mut request.replies) {
            let reply = match reply {
                FinderReply::StreamAdmission(finder, mut admit, mut session) => {
                    // A finder that streams answers the session; one that
                    // doesn't answers the ordinary admission.
                    let answer = match session.try_recv() {
                        Ok(Some(s)) => Ok(FinderReply::Session(s)),
                        Ok(None) => Err(true),
                        Err(closed) => match admit.try_recv() {
                            Ok(Some(done)) => Ok(FinderReply::Completion(done)),
                            Ok(None) => Err(true),
                            Err(TryRecvError::Closed) if closed == TryRecvError::Closed => {
                                Err(false)
                            }
                            Err(_) => {
                                waiting.push(FinderReply::StreamAdmission(finder, admit, session));
                                continue;
                            }
                        },
                    };
                    match answer {
                        Ok(reply) => {
                            if !request.accepted {
                                self.entries.get_mut(&key).expect("present").attempts += 1;
                                request.accepted = true;
                            }
                            if request.coordinated
                                && let Some(a) = &mut self.admission
                            {
                                a.clear_hold(finder);
                            }
                            reply
                        }
                        Err(refused) => {
                            request.refused |= refused;
                            said_no.push(finder);
                            continue;
                        }
                    }
                }
                other => other,
            };
            match reply {
                FinderReply::Session(mut session) => {
                    self.drain_batches(&mut session, &mut peers);
                    let end = match session.terminal.try_recv() {
                        Ok(end) => end,
                        Err(TryRecvError::Empty) => {
                            waiting.push(FinderReply::Session(session));
                            continue;
                        }
                        // Never success: the finder went without saying.
                        Err(TryRecvError::Closed) => Terminal::SourceFailed,
                    };
                    // The finder closed its batches before the terminal, so
                    // whatever it queued is there now.
                    self.drain_batches(&mut session, &mut peers);
                    self.stats.stream_terminals[end as usize].fetch_add(1, Ordering::Relaxed);
                    if end != Terminal::SourceFailed {
                        request.answered = true;
                    }
                }
                FinderReply::Completion(mut rx) => match rx.try_recv() {
                    Ok(found) => {
                        request.answered = true;
                        peers.extend(found);
                    }
                    Err(TryRecvError::Empty) => waiting.push(FinderReply::Completion(rx)),
                    Err(TryRecvError::Closed) => {}
                },
                FinderReply::Admission(..) | FinderReply::StreamAdmission(..) => {
                    unreachable!("answered above")
                }
            }
        }
        request.replies = waiting;
        let finished = request.replies.is_empty();
        // Several finders may report the same endpoint in one poll.
        let mut unique = HashSet::new();
        peers.retain(|p| unique.insert(lodestar_network_policy::normalize(*p)));
        if !peers.is_empty() {
            request.batched = true;
        }
        if finished && !request.accepted {
            self.stats.lookup_unaccepted.fetch_add(1, Ordering::Relaxed);
        }
        if request.coordinated {
            self.hold_finders(said_no.iter());
        }
        if finished && !request.accepted && request.coordinated {
            self.requeue_refused(key, request.refused, &[]);
            return;
        }
        // Accepted and dropped before anything came (a pause, a shutdown):
        // cancelled, so the attempt isn't used.
        if finished && request.accepted && !request.answered && !request.batched && untouched {
            self.requeue_cancelled(key, request.coordinated);
            return;
        }
        self.requests.insert(key, request);
        let before = self.entries[&key].peers.len();
        let capped = self.add_peers(key, &peers);
        // The first-batch clock starts only on peers the entry kept.
        let e = &self.entries[&key];
        if e.peers.len() > before && !e.fetch_started && !self.first_batch.contains_key(&key) {
            self.first_batch.insert(key, Instant::now());
        }
        self.stats
            .stream_peers_capped
            .fetch_add(capped as u64, Ordering::Relaxed);
        if finished {
            // Every finder is done: the logical lookup ends, once.
            self.requests.remove(&key);
            self.lookups -= 1;
            if self.entries[&key].state == State::Lookup {
                self.back_off(key);
            }
        }
    }

    /// Reads a session's queued batches: at most two, so bounded.
    fn drain_batches(&self, session: &mut LookupSession, peers: &mut Vec<SocketAddr>) {
        while let Ok(batch) = session.batches.try_recv() {
            self.stats.stream_batches.fetch_add(1, Ordering::Relaxed);
            peers.extend(batch);
        }
    }

    /// Ends a key's streaming lookup at its deadline (CR-1a-S): every
    /// finder is cancelled, and a key with no peer left to try backs off.
    fn stream_deadline(&mut self, key: InfoHash) {
        self.requests.remove(&key);
        self.lookups -= 1;
        if self.entries[&key].state == State::Lookup {
            self.back_off(key);
        }
    }

    /// Every accepting finder dropped the request unanswered: give the
    /// attempt back and queue the key for the next lookup slot, with no
    /// back-off. A paused pool holds it until it resumes.
    fn requeue_cancelled(&mut self, key: InfoHash, coordinated: bool) {
        self.lookups -= 1;
        let e = self.entries.get_mut(&key).expect("present");
        e.attempts = e.attempts.saturating_sub(1);
        // It was accepted, so its retry isn't a retry of unaccepted work.
        e.lookup_requested = false;
        if coordinated {
            self.wait(key, WaitKind::Requeue, true);
            return;
        }
        e.state = State::LookupQueued;
        self.waiting += 1;
        if e.interest {
            self.lookup_backlog.push_front(key);
        } else {
            self.lookup_backlog.push_back(key);
        }
    }

    /// Coordinated admission: no finder accepted the permitted request.
    /// No attempt was used and none is charged as back-off; the key goes
    /// back to the front of its queue. Finders that said no rested when
    /// that was seen ([`admission::REFUSAL_HOLD`]), so the permits they gave
    /// back don't come straight back to them; `silent` ones, which never
    /// answered, rest now. `refused` says whether any finder refused, so
    /// the refusal counts as refused rather than unacknowledged.
    fn requeue_refused(&mut self, key: InfoHash, refused: bool, silent: &[usize]) {
        self.lookups -= 1;
        let reason = usize::from(!refused);
        self.stats.admission_refusals[reason].fetch_add(1, Ordering::Relaxed);
        self.hold_finders(silent.iter());
        self.wait(key, WaitKind::Requeue, true);
    }

    /// Rests each finder for [`admission::REFUSAL_HOLD`]; a permit kept
    /// back from one goes back now.
    fn hold_finders<'a>(&mut self, finders: impl Iterator<Item = &'a usize>) {
        let Some(a) = &mut self.admission else {
            return;
        };
        let until = Instant::now() + admission::REFUSAL_HOLD;
        for &finder in finders {
            let unused = a.hold(finder, until);
            self.stats.admission_permit_returns[2].fetch_add(unused, Ordering::Relaxed);
            self.stats.admission_holds.fetch_add(1, Ordering::Relaxed);
        }
    }

    fn schedule(&mut self, at: Instant, key: InfoHash, generation: u64) {
        self.timer_seq += 1;
        self.timers
            .push(Reverse((at, self.timer_seq, key, generation)));
    }

    fn on_timers(&mut self) {
        let now = Instant::now();
        while let Some(Reverse((at, _, key, generation))) = self.timers.peek().copied() {
            if at > now {
                break;
            }
            self.timers.pop();
            let Some(e) = self.entries.get_mut(&key) else {
                continue;
            };
            if e.generation != generation {
                // A stream's deadline outlives its entry's state changes.
                if self
                    .requests
                    .get(&key)
                    .is_some_and(|r| r.streaming && r.generation == generation)
                {
                    self.stream_deadline(key);
                }
                continue;
            }
            match e.state {
                State::Lookup => {
                    // No peers turned up in time.
                    let request = self.requests.remove(&key);
                    if request.as_ref().is_some_and(|r| !r.accepted) {
                        self.stats.lookup_unaccepted.fetch_add(1, Ordering::Relaxed);
                    }
                    // A permitted request no finder ever acknowledged is an
                    // unacknowledged one, as if its admission had closed: no
                    // back-off, and the silent finders rest. The key waits
                    // for a permit, which only returns once the finder has
                    // dealt with the request.
                    if let Some(r) = request.filter(|r| r.coordinated && !r.accepted) {
                        let silent: Vec<usize> = r
                            .replies
                            .iter()
                            .filter_map(|reply| match reply {
                                FinderReply::Admission(finder, _)
                                | FinderReply::StreamAdmission(finder, ..) => Some(*finder),
                                FinderReply::Completion(_) | FinderReply::Session(_) => None,
                            })
                            .collect();
                        self.requeue_refused(key, r.refused, &silent);
                        continue;
                    }
                    self.lookups -= 1;
                    let e = self.entries.get_mut(&key).expect("present");
                    e.state = State::Backoff;
                    self.back_off(key);
                }
                State::Backoff => self.request_peers(key, WaitKind::Retry),
                State::Ready | State::Fetching | State::LookupQueued => {}
            }
        }
    }

    /// Schedules the next lookup, or gives up if there won't be one.
    fn back_off(&mut self, key: InfoHash) {
        let (base, max_attempts) = (self.cfg.retry_backoff, self.cfg.max_attempts);
        let e = self.entries.get_mut(&key).expect("present");
        if e.attempts >= max_attempts {
            let loss = e.last_failure.map_or(Loss::NoPeers, Loss::from);
            self.lose(key, loss);
            return;
        }
        e.state = State::Backoff;
        e.generation = next_generation(&mut self.generations);
        let generation = e.generation;
        let delay = base
            .max(Duration::from_millis(1))
            .saturating_mul(4u32.saturating_pow(u32::from(e.attempts.saturating_sub(1))));
        self.schedule(Instant::now() + delay, key, generation);
    }

    /// Starts fetches while sockets are free.
    fn dispatch(&mut self) {
        if self.cfg.paused {
            return;
        }
        if self.admission.is_some() {
            self.drain_admission();
        }
        // Under coordinated admission this still serves keys that don't
        // take permits: private ones, and all of them once no finder is
        // open.
        self.drain_lookup_backlog();
        let mut deferred = Vec::new();
        while self.tasks.len() < self.cfg.max_sockets {
            let Some(key) = self.next_ready() else {
                break;
            };
            let per_ip = self.cfg.per_ip;
            let e = self.entries.get_mut(&key).expect("validated");
            let pick = (e.tried..e.peers.len())
                .find(|&i| self.per_ip.get(&e.peers[i].ip()).copied().unwrap_or(0) < per_ip);
            let Some(i) = pick else {
                // Every untried peer's IP is busy; try again next round.
                deferred.push(key);
                continue;
            };
            e.peers.swap(e.tried, i);
            let peer = e.peers[e.tried];
            e.tried += 1;
            e.state = State::Fetching;
            e.fetch_started = true;
            e.generation = next_generation(&mut self.generations);
            if let Some(at) = self.first_batch.remove(&key) {
                let ms = Instant::now().duration_since(at).as_millis();
                self.stats
                    .stream_first_fetch_millis
                    .fetch_add(u64::try_from(ms).unwrap_or(u64::MAX), Ordering::Relaxed);
                self.stats
                    .stream_first_fetches
                    .fetch_add(1, Ordering::Relaxed);
            }
            *self.per_ip.entry(peer.ip()).or_insert(0) += 1;
            let handle = self.tasks.spawn(self.fetcher.fetch(peer, key));
            self.task_keys.insert(handle.id(), (key, peer));
        }
        for key in deferred {
            // Still `Ready`; back into the queue it came from.
            let e = &self.entries[&key];
            if e.interest {
                self.interest.push_back(key);
            } else {
                let source = e.source;
                let q = self.source(source);
                q.ready.push_back(key);
                if !q.in_rotation {
                    q.in_rotation = true;
                    self.rotation.push_back(source);
                }
            }
        }
    }

    fn drain_lookup_backlog(&mut self) {
        while self.lookups < self.cfg.max_lookups {
            let Some(key) = self.lookup_backlog.pop_front() else {
                break;
            };
            if self
                .entries
                .get(&key)
                .is_some_and(|e| e.state == State::LookupQueued)
            {
                self.request_peers(key, WaitKind::Retry);
            }
        }
    }

    fn is_ready(&self, key: &InfoHash) -> bool {
        self.entries
            .get(key)
            .is_some_and(|e| e.state == State::Ready)
    }

    /// Interest first, then deficit round robin over the sources.
    fn next_ready(&mut self) -> Option<InfoHash> {
        while let Some(key) = self.interest.pop_front() {
            if self.is_ready(&key) {
                return Some(key);
            }
        }
        while let Some(&source) = self.rotation.front() {
            let q = self.sources.get_mut(&source).expect("rotating");
            if q.deficit == 0 {
                q.deficit = q.weight;
            }
            let key = loop {
                match q.ready.pop_front() {
                    Some(k)
                        if self
                            .entries
                            .get(&k)
                            .is_some_and(|e| e.state == State::Ready) =>
                    {
                        break Some(k);
                    }
                    Some(_) => continue,
                    None => break None,
                }
            };
            match key {
                Some(k) => {
                    q.deficit -= 1;
                    if q.deficit == 0 {
                        self.rotation.rotate_left(1);
                    }
                    return Some(k);
                }
                None => {
                    q.deficit = 0;
                    q.in_rotation = false;
                    self.rotation.pop_front();
                }
            }
        }
        None
    }

    async fn on_result(
        &mut self,
        key: InfoHash,
        peer: SocketAddr,
        result: Result<Fetched, FailureKind>,
    ) {
        if let Some(n) = self.per_ip.get_mut(&peer.ip()) {
            *n -= 1;
            if *n == 0 {
                self.per_ip.remove(&peer.ip());
            }
        }
        let Some(e) = self.entries.get_mut(&key) else {
            return;
        };
        match result {
            Ok(fetched) => {
                let e = self.entries.remove(&key).expect("present");
                // A verified result cancels every finder still streaming.
                if self.requests.remove(&key).is_some() {
                    self.lookups -= 1;
                }
                self.first_batch.remove(&key);
                self.stats.fetched.fetch_add(1, Ordering::Relaxed);
                self.funnel.record(e.cohort, e.source, Stage::Fetched, None);
                self.remember(key);
                let out = FetchedKey {
                    key,
                    fetched,
                    source: e.source,
                    sources: e.sources,
                    cohort: e.cohort,
                    strategy: e.strategy,
                    discovered_at: e.discovered_at,
                    private_source: e.private,
                };
                // Waiting here is the backpressure from a slow writer.
                let _ = self.out.send(out).await;
            }
            Err(kind) => {
                self.stats.failures[failure_index(kind)].fetch_add(1, Ordering::Relaxed);
                e.last_failure = Some(kind);
                if kind == FailureKind::Oversize {
                    // A property of the torrent, not the peer.
                    self.lose(key, Loss::Oversize);
                } else if e.has_untried() {
                    self.make_ready(key);
                } else if self.requests.get(&key).is_some_and(|r| r.streaming) {
                    // CR-1a-S: the lookup still runs; wait for its next
                    // batch or its end, with no second lookup.
                    e.state = State::Lookup;
                    e.generation = next_generation(&mut self.generations);
                } else {
                    self.back_off(key);
                }
            }
        }
    }

    fn remember(&mut self, key: InfoHash) {
        if self.cfg.recent == 0 {
            return;
        }
        if self.recent_order.len() >= self.cfg.recent
            && let Some(old) = self.recent_order.pop_front()
        {
            self.recent.remove(&old);
        }
        self.recent.insert(key);
        self.recent_order.push_back(key);
    }

    /// Drops the oldest key that's still looking for peers and isn't wanted
    /// by a user. Keys with peers are the likeliest to succeed, so they stay.
    /// Under coordinated admission a key whose lookup holds a permit stays
    /// too: the oldest waiter is the next one granted, so evicting granted
    /// work would cancel nearly every lookup just after admission.
    fn evict_one(&mut self) -> bool {
        for _ in 0..64.min(self.admitted.len()) {
            let Some(key) = self.admitted.pop_front() else {
                break;
            };
            let permitted = self.requests.get(&key).is_some_and(|r| r.coordinated);
            match self.entries.get(&key) {
                None => continue,
                Some(e)
                    if matches!(e.state, State::Ready | State::Fetching)
                        || e.interest
                        || permitted =>
                {
                    self.admitted.push_back(key);
                }
                Some(_) => {
                    self.lose(key, Loss::Evicted);
                    return true;
                }
            }
        }
        false
    }

    fn lose(&mut self, key: InfoHash, loss: Loss) {
        let e = self.entries.remove(&key).expect("present");
        if loss == Loss::Evicted {
            let stage = if e.fetch_started {
                3
            } else if e.had_peers {
                2
            } else if e.attempts > 0 {
                1
            } else {
                0
            };
            self.stats.evicted_service[stage].fetch_add(1, Ordering::Relaxed);
        }
        if e.state == State::Lookup {
            self.requests.remove(&key);
            self.lookups -= 1;
        } else if self.requests.remove(&key).is_some() {
            // A stream's lookup, while its key is ready or fetching.
            self.lookups -= 1;
        }
        self.first_batch.remove(&key);
        if e.state == State::LookupQueued {
            self.waiting -= 1;
            if self.admission.is_some() && loss == Loss::Evicted {
                self.stats.admission_wait_cancelled[0].fetch_add(1, Ordering::Relaxed);
            }
        }
        // The last failure goes with the loss only when it's the cause: a
        // key evicted after a hash mismatch didn't fail verification.
        let cause = e.last_failure.filter(|&k| Loss::from(k) == loss);
        self.lose_untracked(&key, e.cohort, e.source, loss, cause);
    }

    fn lose_untracked(
        &mut self,
        key: &InfoHash,
        cohort: i64,
        source: SourceId,
        loss: Loss,
        cause: Option<FailureKind>,
    ) {
        self.stats.lost[loss as usize].fetch_add(1, Ordering::Relaxed);
        self.funnel.lost(key, cohort, source, loss, cause);
    }

    fn snapshot(&self) -> Vec<QueuedKey> {
        let mut out: Vec<QueuedKey> = self
            .entries
            .iter()
            .map(|(key, e)| QueuedKey {
                info_hash: *key,
                source: e.source,
                cohort: e.cohort,
                strategy: e.strategy,
                priority: u8::from(e.interest),
                // The attempt in progress doesn't count as used.
                attempts: if self.requests.get(key).is_some_and(|r| r.accepted) {
                    e.attempts.saturating_sub(1)
                } else {
                    e.attempts
                },
                private_source: e.private,
            })
            .collect();
        out.sort_by_key(|q| (Reverse(q.priority), q.cohort, q.info_hash.0));
        out
    }

    fn publish_stats(&self) {
        let s = &self.stats;
        let waiting = self.entries.len().saturating_sub(self.tasks.len());
        s.pending
            .store(self.entries.len() as u64, Ordering::Relaxed);
        s.active.store(self.tasks.len() as u64, Ordering::Relaxed);
        s.waiting.store(waiting as u64, Ordering::Relaxed);
        s.lookups.store(self.lookups as u64, Ordering::Relaxed);
        s.lookup_backlog
            .store(self.lookup_backlog.len() as u64, Ordering::Relaxed);
        if let Some(a) = &self.admission {
            s.admission_waiters
                .store(self.waiting as u64, Ordering::Relaxed);
            s.admission_permits_in_use
                .store(a.in_use() as u64, Ordering::Relaxed);
        }
    }
}

#[cfg(test)]
mod diagnostic_cost {
    use super::*;
    use std::mem::{align_of, size_of};

    // Exact entry field types at the prepared-fix base 5e1a53c0, before the two
    // diagnostic flags. Measure compiler padding on the target, not just payload.
    #[allow(dead_code)]
    struct PreviousEntry {
        source: SourceId,
        sources: u64,
        cohort: i64,
        strategy: u8,
        discovered_at: i64,
        private: bool,
        interest: bool,
        peers: Vec<SocketAddr>,
        tried: usize,
        attempts: u16,
        had_peers: bool,
        last_failure: Option<FailureKind>,
        state: State,
        generation: u32,
    }

    #[test]
    fn diagnostic_entry_storage_is_bounded() {
        let delta = size_of::<Entry>().saturating_sub(size_of::<PreviousEntry>());
        // Two inline bools may consume one alignment unit; no heap owner is new.
        assert!(delta <= align_of::<Entry>());
        println!(
            "entry_before={} entry_after={} added_at_200000={} pool_stats={} pool_counter_payload_added={}",
            size_of::<PreviousEntry>(),
            size_of::<Entry>(),
            delta * 200_000,
            size_of::<PoolStats>(),
            8 * size_of::<AtomicU64>()
        );
    }

    #[test]
    #[cfg(not(debug_assertions))]
    #[ignore = "isolated release microcheck; run after other validation stops"]
    fn diagnostic_atomic_cost_report() {
        use std::hint::black_box;
        const N: u64 = 1_000_000;
        let old = AtomicU64::new(0);
        let category = AtomicU64::new(0);
        let mut baseline = Vec::new();
        let mut diagnostic = Vec::new();
        for round in 0..9 {
            // Alternate order to reduce consistent warm-cache/order bias.
            for added in [round % 2 == 0, round % 2 != 0] {
                let start = std::time::Instant::now();
                for _ in 0..N {
                    black_box(&old).fetch_add(1, Ordering::Relaxed);
                    if added {
                        black_box(&category).fetch_add(1, Ordering::Relaxed);
                    }
                }
                let ns = start.elapsed().as_secs_f64() * 1e9 / N as f64;
                if added {
                    diagnostic.push(ns);
                } else {
                    baseline.push(ns);
                }
            }
        }
        assert_eq!(old.load(Ordering::Relaxed), 18 * N);
        assert_eq!(category.load(Ordering::Relaxed), 9 * N);
        baseline.sort_by(f64::total_cmp);
        diagnostic.sort_by(f64::total_cmp);
        println!(
            "atomic_ns_per_event baseline_median={:.2} classified_median={:.2} added_median={:.2} classified_range={:.2}..{:.2}",
            baseline[4],
            diagnostic[4],
            diagnostic[4] - baseline[4],
            diagnostic[0],
            diagnostic[8]
        );
        // Preselected conservative microcheck ceiling, not an application SLA.
        // Measures uncontended atomic overhead only; no network/throughput claim.
        assert!(diagnostic[4] - baseline[4] < 100.0);
    }
}

#[cfg(test)]
mod pause_cancel {
    use super::*;
    use crate::funnel::FunnelCounts;
    use crate::plugin::Found;

    struct NoFetch;

    impl Fetcher for NoFetch {
        fn fetch(
            &self,
            _: SocketAddr,
            _: InfoHash,
        ) -> BoxFuture<'static, Result<Fetched, FailureKind>> {
            Box::pin(async { Err(FailureKind::Refused) })
        }
    }

    /// Waits for `done`, polling, for at most 10 s.
    async fn until(what: &str, mut done: impl AsyncFnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !done().await {
            assert!(Instant::now() < deadline, "timed out waiting: {what}");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    #[tokio::test]
    async fn a_finder_dropping_accepted_work_on_pause_costs_no_attempt_or_back_off() {
        let (disc, discoveries) = mpsc::channel(4);
        let (finder, mut requests) = mpsc::channel::<PeerRequest>(4);
        let (out, _results) = mpsc::channel(4);
        let (stop, shutdown) = Shutdown::new();
        let config = PoolConfig {
            lookup_wait: Duration::from_secs(600),
            // Far beyond the 10 s deadline: a back-off would fail the test.
            retry_backoff: Duration::from_secs(3600),
            max_attempts: 3,
            ..PoolConfig::default()
        };
        let (handle, task) = spawn(
            config,
            Arc::new(NoFetch),
            PoolIo {
                discoveries,
                peer_finders: vec![finder],
                out,
                funnel: Arc::new(FunnelCounts::default()),
                shutdown,
            },
            HashMap::new(),
            vec![],
        );
        let source = SourceId::new(0).unwrap();
        for n in [1u8, 2] {
            disc.send(Discovery {
                source,
                found: Found::key(InfoHash([n; 20]), 0),
                at: 1_700_000_000,
                reply: false,
            })
            .await
            .unwrap();
        }
        let mut held = Vec::new();
        for _ in 0..2 {
            let request = tokio::time::timeout(Duration::from_secs(10), requests.recv())
                .await
                .unwrap()
                .unwrap();
            let (done, rx) = oneshot::channel::<Vec<SocketAddr>>();
            request.admit.send(Some(rx)).unwrap();
            held.push(done);
        }
        until("both accepted", async || {
            handle.snapshot().await.len() == 2
                && handle.stats().lookups.load(Ordering::Relaxed) == 2
        })
        .await;
        // The crawler's pause: the pool pauses, the actor drops its queue.
        handle.set_paused(true).await;
        drop(held);
        until("both cancelled", async || {
            handle.stats().lookups.load(Ordering::Relaxed) == 0
        })
        .await;
        assert!(handle.snapshot().await.iter().all(|q| q.attempts == 0));
        assert_eq!(handle.stats().lookup_unaccepted.load(Ordering::Relaxed), 0);
        assert!(requests.try_recv().is_err(), "a paused pool asks nothing");

        handle.set_paused(false).await;
        for _ in 0..2 {
            let request = tokio::time::timeout(Duration::from_secs(10), requests.recv())
                .await
                .expect("asked again at once, not after a back-off")
                .unwrap();
            let (done, rx) = oneshot::channel();
            request.admit.send(Some(rx)).unwrap();
            done.send(Vec::new()).unwrap();
        }
        until("the retries finished", async || {
            handle.stats().lookups.load(Ordering::Relaxed) == 0
        })
        .await;
        // Answered work still counts its attempt.
        assert!(handle.snapshot().await.iter().all(|q| q.attempts == 1));
        assert_eq!(handle.stats().retry_unaccepted.load(Ordering::Relaxed), 0);
        stop.send(true).unwrap();
        task.await.unwrap();
    }
}
