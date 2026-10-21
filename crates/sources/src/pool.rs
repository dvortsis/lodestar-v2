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
//! and waits for their answer; when every known peer has failed it asks
//! again, with exponential back-off, up to a retry limit. Each key leaves
//! the pool exactly once: fetched, or lost with a reason recorded in the
//! funnel.
//!
//! Harvest outruns peer finding by orders of magnitude (§3), so the order
//! of lookups decides the yield. Each key has a *rank*: the strongest
//! evidence of a live swarm any sighting carried, plus one per extra
//! sighting. Lookups go to user-interest keys first, then by rank, newest
//! first within a rank, with retries behind first attempts. A full pool
//! evicts the oldest key of the lowest rank, never one ranked above the
//! newcomer.
//!
//! One task owns all the state. Fetches run as child tasks and report back.

use std::cmp::Reverse;
use std::collections::{BTreeMap, BinaryHeap, HashMap, HashSet, VecDeque};
use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use lodestar_metainfo::InfoHash;
use lodestar_peer_wire::{FailureKind, FetchOptions, Fetched};
use lodestar_store::{QueuedKey, SourceId};
use tokio::sync::{mpsc, oneshot};
use tokio::task::{Id, JoinHandle, JoinSet};
use tokio::time::{Instant, sleep_until};

use crate::funnel::{Funnel, Loss, Stage};
use crate::plugin::{BoxFuture, Discovery, Shutdown};

/// The highest rank a key can reach.
pub const MAX_RANK: u8 = 7;
const RANKS: usize = MAX_RANK as usize + 1;
/// Extra sightings that count towards a key's rank.
const MAX_SIGHTING_BONUS: u8 = 3;

/// Fetches one key from one peer.
pub trait Fetcher: Send + Sync + 'static {
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
    /// Peer lookups in flight. Match it to what the peer finders run at
    /// once (the DHT's `Intensity::max_get_peers`): asks beyond that queue
    /// inside the finder, outside this pool's ordering, and can run out
    /// their lookup wait before they even start.
    pub max_lookups: usize,
    /// How long a lookup may take before the pool stops waiting for its
    /// answer. Finders answer empty lookups, so this is a safety net.
    pub lookup_wait: Duration,
    /// Wait before the second lookup; quadrupled for each later one.
    pub retry_backoff: Duration,
    /// Lookups per key before it's given up as lost.
    pub max_attempts: u16,
    /// Recently fetched keys remembered, so a rediscovery racing the writer
    /// isn't fetched twice.
    pub recent: usize,
}

impl Default for PoolConfig {
    fn default() -> Self {
        PoolConfig {
            max_sockets: 400,
            per_ip: 2,
            max_pending: 200_000,
            max_peers_per_key: 32,
            max_lookups: 64,
            lookup_wait: Duration::from_secs(60),
            retry_backoff: Duration::from_secs(120),
            max_attempts: 4,
            recent: 50_000,
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
}

/// What happened to the keys of one (source, strategy), by the strategy of
/// the sighting that first queued them.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct StrategyCounts {
    /// New keys, including any dropped on arrival by a full pool.
    pub harvested: u64,
    /// Peer lookups started.
    pub lookups: u64,
    /// Keys that got their first peer.
    pub peer_found: u64,
    /// Fetch attempts (one per peer tried).
    pub attempts: u64,
    pub fetched: u64,
    pub lost: u64,
}

#[derive(Clone, Copy)]
enum Count {
    Harvested,
    Lookup,
    PeerFound,
    Attempt,
    Fetched,
    Lost,
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
    pub failures: [AtomicU64; 6],
    /// Keys lost, by [`Loss`] code (index = code).
    pub lost: [AtomicU64; 11],
    pub duplicates: AtomicU64,
    /// Keys leaving the pool, by their rank at the time: fetched, and lost
    /// for any reason (eviction included).
    pub fetched_by_rank: [AtomicU64; RANKS],
    pub lost_by_rank: [AtomicU64; RANKS],
    strategies: Mutex<BTreeMap<(SourceId, u8), StrategyCounts>>,
}

impl PoolStats {
    /// Per-(source, strategy) counts, sorted.
    pub fn strategies(&self) -> Vec<((SourceId, u8), StrategyCounts)> {
        let map = self.strategies.lock().expect("strategy stats lock");
        map.iter().map(|(k, v)| (*k, *v)).collect()
    }

    fn count(&self, source: SourceId, strategy: u8, what: Count) {
        let mut map = self.strategies.lock().expect("strategy stats lock");
        let c = map.entry((source, strategy)).or_default();
        let n = match what {
            Count::Harvested => &mut c.harvested,
            Count::Lookup => &mut c.lookups,
            Count::PeerFound => &mut c.peer_found,
            Count::Attempt => &mut c.attempts,
            Count::Fetched => &mut c.fetched,
            Count::Lost => &mut c.lost,
        };
        *n += 1;
    }

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
    }
}

enum Cmd {
    SetWeight(SourceId, u32),
    Interest(InfoHash, oneshot::Sender<bool>),
    Snapshot(oneshot::Sender<Vec<QueuedKey>>),
    Queued(oneshot::Sender<Vec<(SourceId, usize)>>),
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
    pub peer_finders: Vec<mpsc::Sender<InfoHash>>,
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
        states: [0; STATES],
        admitted: Default::default(),
        lookup_interest: VecDeque::new(),
        lookup_ranks: Default::default(),
        per_ip: HashMap::new(),
        tasks: JoinSet::new(),
        task_keys: HashMap::new(),
        recent: HashSet::new(),
        recent_order: VecDeque::new(),
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
    Ready = 0,
    Fetching = 1,
    /// Waiting for a lookup slot.
    LookupQueued = 2,
    /// A lookup is out; peers may arrive until the finders have answered
    /// or the timer fires.
    Lookup = 3,
    /// Every peer failed or none turned up; the next lookup is scheduled.
    Backoff = 4,
}
const STATES: usize = 5;

/// Moves an entry to a new state, keeping the per-state counts.
fn set_state(states: &mut [usize; STATES], e: &mut Entry, state: State) {
    states[e.state as usize] -= 1;
    states[state as usize] += 1;
    e.state = state;
}

fn rank_of(evidence: u8, sightings: u8) -> u8 {
    let bonus = sightings.saturating_sub(1).min(MAX_SIGHTING_BONUS);
    evidence.saturating_add(bonus).min(MAX_RANK)
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
    /// Peer finders still to answer the lookup in flight.
    asked: u8,
    /// The strongest evidence any sighting carried.
    evidence: u8,
    sightings: u8,
    rank: u8,
    had_peers: bool,
    last_failure: Option<FailureKind>,
    state: State,
    /// Bumped on every state change, to invalidate old timers.
    generation: u32,
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

struct Pool {
    cfg: PoolConfig,
    fetcher: Arc<dyn Fetcher>,
    funnel: Arc<dyn Funnel>,
    finders: Vec<mpsc::Sender<InfoHash>>,
    out: mpsc::Sender<FetchedKey>,
    stats: Arc<PoolStats>,
    entries: HashMap<InfoHash, Entry>,
    sources: HashMap<SourceId, SourceQueue>,
    /// Sources with ready keys, in DRR order.
    rotation: VecDeque<SourceId>,
    interest: VecDeque<InfoHash>,
    timers: BinaryHeap<Reverse<(Instant, u64, InfoHash, u32)>>,
    timer_seq: u64,
    /// Entries per [`State`].
    states: [usize; STATES],
    /// Admission order per rank, for eviction. A key moves to the back of
    /// its new rank when it rises; stale copies are skipped and cleaned
    /// lazily.
    admitted: [VecDeque<InfoHash>; RANKS],
    /// Keys waiting for a lookup slot: user interest first (in order),
    /// then by rank, newest first (the back), retries at the front.
    lookup_interest: VecDeque<InfoHash>,
    lookup_ranks: [VecDeque<InfoHash>; RANKS],
    per_ip: HashMap<IpAddr, usize>,
    tasks: JoinSet<Result<Fetched, FailureKind>>,
    /// What each fetch task is fetching, so even a panicked task is
    /// accounted for.
    task_keys: HashMap<Id, (InfoHash, SocketAddr)>,
    recent: HashSet<InfoHash>,
    recent_order: VecDeque<InfoHash>,
}

impl Pool {
    async fn run(
        mut self,
        mut discoveries: mpsc::Receiver<Discovery>,
        mut cmds: mpsc::Receiver<Cmd>,
        mut shutdown: Shutdown,
    ) -> Vec<QueuedKey> {
        loop {
            self.dispatch();
            self.publish_stats();
            let wake = self.timers.peek().map(|Reverse((at, ..))| *at);
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
                Some(cmd) = cmds.recv() => self.on_cmd(cmd),
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
                _ = sleep_until(wake.unwrap_or_else(Instant::now)), if wake.is_some() => {
                    self.on_timers();
                }
            }
        }
        self.tasks.abort_all();
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
        let key = d.found.key;
        if self.recent.contains(&key) {
            self.stats.duplicates.fetch_add(1, Ordering::Relaxed);
            return;
        }
        if let Some(e) = self.entries.get_mut(&key) {
            self.stats.duplicates.fetch_add(1, Ordering::Relaxed);
            e.sources |= 1u64 << d.source.get();
            // A private source can't make a public key private, but a
            // public sighting does make a private key public.
            e.private &= d.found.private_source;
            if d.found.answers_lookup {
                if d.found.peers.is_empty() {
                    self.on_empty_answer(key);
                }
            } else {
                e.sightings = e.sightings.saturating_add(1);
                e.evidence = e.evidence.max(d.found.evidence);
                self.rerank(key);
            }
            if d.found.interest {
                self.raise_interest(key);
            }
            self.add_peers(key, &d.found.peers);
            return;
        }
        if d.found.answers_lookup && d.found.peers.is_empty() {
            // An empty answer for a key that has left the pool meanwhile.
            return;
        }
        let cohort = d.at.div_euclid(3600);
        let rank = rank_of(d.found.evidence, 1);
        self.funnel.record(cohort, d.source, Stage::Harvested, None);
        self.stats
            .count(d.source, d.found.strategy, Count::Harvested);
        if self.entries.len() >= self.cfg.max_pending && !self.evict_one(rank) {
            self.lose_untracked(cohort, d.source, d.found.strategy, rank, Loss::Evicted);
            return;
        }
        self.insert(
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
                asked: 0,
                evidence: d.found.evidence,
                sightings: 1,
                rank,
                had_peers: false,
                last_failure: None,
                state: State::Backoff,
                generation: 0,
            },
        );
        if d.found.peers.is_empty() {
            self.request_peers(key);
        } else {
            self.add_peers(key, &d.found.peers);
        }
    }

    fn restore(&mut self, q: QueuedKey) {
        if self.entries.contains_key(&q.info_hash) || self.entries.len() >= self.cfg.max_pending {
            return;
        }
        // The rank isn't persisted: restored keys start at the bottom.
        self.insert(
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
                asked: 0,
                evidence: 0,
                sightings: 1,
                rank: 0,
                had_peers: false,
                last_failure: None,
                state: State::Backoff,
                generation: 0,
            },
        );
        self.request_peers(q.info_hash);
    }

    /// Adds a new entry (in state `Backoff`) and records its admission.
    fn insert(&mut self, key: InfoHash, e: Entry) {
        debug_assert_eq!(e.state, State::Backoff);
        let rank = e.rank;
        self.states[State::Backoff as usize] += 1;
        self.entries.insert(key, e);
        self.admit(key, rank);
    }

    fn admit(&mut self, key: InfoHash, rank: u8) {
        self.admitted[usize::from(rank)].push_back(key);
        let queued: usize = self.admitted.iter().map(VecDeque::len).sum();
        if queued > 2 * self.entries.len() + 1024 {
            let entries = &self.entries;
            for (r, q) in self.admitted.iter_mut().enumerate() {
                q.retain(|k| entries.get(k).is_some_and(|e| usize::from(e.rank) == r));
            }
        }
    }

    /// Recomputes a key's rank after a new sighting.
    fn rerank(&mut self, key: InfoHash) {
        let e = self.entries.get_mut(&key).expect("present");
        let rank = rank_of(e.evidence, e.sightings);
        if rank <= e.rank {
            return;
        }
        e.rank = rank;
        let waiting = e.state == State::LookupQueued && !e.interest;
        self.admit(key, rank);
        if waiting {
            // Its copy at the old rank is now stale.
            self.lookup_ranks[usize::from(rank)].push_back(key);
        }
    }

    /// A peer finder found nothing. When none is left to answer, the
    /// lookup is over: back off without waiting out the timer.
    fn on_empty_answer(&mut self, key: InfoHash) {
        let e = self.entries.get_mut(&key).expect("present");
        // An answer to an earlier lookup (or one that came after the timer)
        // says nothing about this one.
        if e.state != State::Lookup || e.asked == 0 {
            return;
        }
        e.asked -= 1;
        if e.asked == 0 {
            self.back_off(key);
        }
    }

    /// Merges peers into a key; a key that was waiting becomes ready.
    fn add_peers(&mut self, key: InfoHash, peers: &[SocketAddr]) {
        let cap = self.cfg.max_peers_per_key;
        let Some(e) = self.entries.get_mut(&key) else {
            return;
        };
        let before = e.peers.len();
        for p in peers {
            if e.peers.len() >= cap {
                break;
            }
            if !e.peers.contains(p) {
                e.peers.push(*p);
            }
        }
        if e.peers.len() == before {
            return;
        }
        if !e.had_peers {
            e.had_peers = true;
            self.funnel
                .record(e.cohort, e.source, Stage::PeerFound, None);
            self.stats.count(e.source, e.strategy, Count::PeerFound);
        }
        match e.state {
            State::Ready | State::Fetching => {}
            State::Lookup | State::LookupQueued | State::Backoff => self.make_ready(key),
        }
    }

    fn make_ready(&mut self, key: InfoHash) {
        let e = self.entries.get_mut(&key).expect("present");
        set_state(&mut self.states, e, State::Ready);
        e.generation = e.generation.wrapping_add(1);
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
            State::Backoff => self.request_peers(key),
            // Its copy in the rank queue is skipped once it has run.
            State::LookupQueued => self.lookup_interest.push_back(key),
            State::Fetching | State::Lookup => {}
        }
    }

    /// Asks the peer finders for peers, or queues the ask if too many
    /// lookups are out. Gives the key up once it has used its attempts.
    fn request_peers(&mut self, key: InfoHash) {
        let max_attempts = self.cfg.max_attempts;
        let lookups_full = self.states[State::Lookup as usize] >= self.cfg.max_lookups;
        let e = self.entries.get_mut(&key).expect("present");
        if e.attempts >= max_attempts {
            let loss = e.last_failure.map_or(Loss::NoPeers, Loss::from);
            self.lose(key, loss);
            return;
        }
        e.generation = e.generation.wrapping_add(1);
        if lookups_full {
            set_state(&mut self.states, e, State::LookupQueued);
            if e.interest {
                self.lookup_interest.push_back(key);
            } else {
                let q = &mut self.lookup_ranks[usize::from(e.rank)];
                // First lookups beat retries: a key whose last lookup came
                // up empty is less likely to find peers than a fresh one.
                if e.attempts == 0 {
                    q.push_back(key);
                } else {
                    q.push_front(key);
                }
                self.compact_lookup_queue();
            }
            return;
        }
        e.attempts += 1;
        set_state(&mut self.states, e, State::Lookup);
        let generation = e.generation;
        // Private keys never go to public peer finding (§18.7, BEP 27);
        // they wait for peers from their own source instead.
        let mut asked = 0u8;
        if !e.private {
            for f in &self.finders {
                if f.try_send(key).is_ok() {
                    asked = asked.saturating_add(1);
                }
            }
        }
        e.asked = asked;
        let (source, strategy) = (e.source, e.strategy);
        self.stats.count(source, strategy, Count::Lookup);
        let at = Instant::now() + self.cfg.lookup_wait;
        self.schedule(at, key, generation);
    }

    /// Drops stale copies from the rank queues once they outnumber the
    /// keys actually waiting.
    fn compact_lookup_queue(&mut self) {
        let queued: usize = self.lookup_ranks.iter().map(VecDeque::len).sum();
        if queued <= 2 * self.states[State::LookupQueued as usize] + 1024 {
            return;
        }
        let entries = &self.entries;
        for (r, q) in self.lookup_ranks.iter_mut().enumerate() {
            let mut seen = HashSet::new();
            q.retain(|k| {
                entries
                    .get(k)
                    .is_some_and(|e| e.state == State::LookupQueued && usize::from(e.rank) == r)
                    && seen.insert(*k)
            });
        }
    }

    fn schedule(&mut self, at: Instant, key: InfoHash, generation: u32) {
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
            let Some(e) = self.entries.get(&key) else {
                continue;
            };
            if e.generation != generation {
                continue;
            }
            match e.state {
                // No answer with peers in time.
                State::Lookup => self.back_off(key),
                State::Backoff => self.request_peers(key),
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
        set_state(&mut self.states, e, State::Backoff);
        e.generation = e.generation.wrapping_add(1);
        let generation = e.generation;
        let delay =
            base.saturating_mul(4u32.saturating_pow(u32::from(e.attempts.saturating_sub(1))));
        self.schedule(Instant::now() + delay, key, generation);
    }

    /// Starts fetches while sockets are free.
    fn dispatch(&mut self) {
        self.drain_lookup_queue();
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
            set_state(&mut self.states, e, State::Fetching);
            e.generation = e.generation.wrapping_add(1);
            self.stats.count(e.source, e.strategy, Count::Attempt);
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

    /// Starts queued lookups while slots are free: interest, then the
    /// highest rank, newest first.
    fn drain_lookup_queue(&mut self) {
        while self.states[State::Lookup as usize] < self.cfg.max_lookups {
            let Some(key) = self.next_lookup() else {
                break;
            };
            self.request_peers(key);
        }
    }

    fn next_lookup(&mut self) -> Option<InfoHash> {
        let waiting = |entries: &HashMap<InfoHash, Entry>, k: &InfoHash, rank: Option<usize>| {
            entries.get(k).is_some_and(|e| {
                e.state == State::LookupQueued && rank.is_none_or(|r| usize::from(e.rank) == r)
            })
        };
        while let Some(key) = self.lookup_interest.pop_front() {
            if waiting(&self.entries, &key, None) {
                return Some(key);
            }
        }
        for r in (0..RANKS).rev() {
            while let Some(key) = self.lookup_ranks[r].pop_back() {
                if waiting(&self.entries, &key, Some(r)) {
                    return Some(key);
                }
            }
        }
        None
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
                let e = self.remove(&key);
                self.stats.fetched.fetch_add(1, Ordering::Relaxed);
                self.stats.fetched_by_rank[usize::from(e.rank)].fetch_add(1, Ordering::Relaxed);
                self.stats.count(e.source, e.strategy, Count::Fetched);
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
        if self.recent_order.len() >= self.cfg.recent {
            if let Some(old) = self.recent_order.pop_front() {
                self.recent.remove(&old);
            }
        }
        self.recent.insert(key);
        self.recent_order.push_back(key);
    }

    /// Drops the oldest key of the lowest rank, up to `max_rank`, that's
    /// still looking for peers and isn't wanted by a user. Keys with peers
    /// are the likeliest to succeed, so they stay.
    fn evict_one(&mut self, max_rank: u8) -> bool {
        for r in 0..=usize::from(max_rank) {
            let q = &mut self.admitted[r];
            for _ in 0..64.min(q.len()) {
                let Some(key) = q.pop_front() else {
                    break;
                };
                match self.entries.get(&key) {
                    // Gone, or moved up to another rank.
                    None => continue,
                    Some(e) if usize::from(e.rank) != r => continue,
                    Some(e) if matches!(e.state, State::Ready | State::Fetching) || e.interest => {
                        q.push_back(key);
                    }
                    Some(_) => {
                        self.lose(key, Loss::Evicted);
                        return true;
                    }
                }
            }
        }
        false
    }

    fn remove(&mut self, key: &InfoHash) -> Entry {
        let e = self.entries.remove(key).expect("present");
        self.states[e.state as usize] -= 1;
        e
    }

    fn lose(&mut self, key: InfoHash, loss: Loss) {
        let e = self.remove(&key);
        self.lose_untracked(e.cohort, e.source, e.strategy, e.rank, loss);
    }

    fn lose_untracked(
        &mut self,
        cohort: i64,
        source: SourceId,
        strategy: u8,
        rank: u8,
        loss: Loss,
    ) {
        self.stats.lost[loss as usize].fetch_add(1, Ordering::Relaxed);
        self.stats.lost_by_rank[usize::from(rank)].fetch_add(1, Ordering::Relaxed);
        self.stats.count(source, strategy, Count::Lost);
        self.funnel.record(cohort, source, Stage::Lost, Some(loss));
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
                attempts: if e.state == State::Lookup {
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
        s.lookups.store(
            self.states[State::Lookup as usize] as u64,
            Ordering::Relaxed,
        );
        s.lookup_backlog.store(
            self.states[State::LookupQueued as usize] as u64,
            Ordering::Relaxed,
        );
    }
}
