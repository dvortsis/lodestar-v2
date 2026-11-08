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
//!
//! Info dicts are held to a byte budget (`max_held_bytes`). A fetch
//! reserves its result's bytes once the peer has announced the size, before
//! it downloads them (X-fetch-meta-reserve), and waits while the budget is
//! full. The pool counts a finished result from then until the consumer
//! takes it from the output channel. It starts no fetch while the budget
//! is full, while a whole reservation or a growth waits, or while a result
//! waits for a channel slot.

use std::cmp::Reverse;
use std::collections::{BTreeMap, BinaryHeap, HashMap, HashSet, VecDeque};
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use lodestar_metainfo::InfoHash;
use lodestar_peer_wire::{FailureKind, FetchOptions, Fetched, Reserve};
use lodestar_store::{QueuedKey, SourceId};
use tokio::sync::mpsc::error::TrySendError;
use tokio::sync::{Notify, mpsc, oneshot};
use tokio::task::{Id, JoinHandle, JoinSet};
use tokio::time::{Instant, sleep_until, timeout_at};

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

    /// [`fetch`](Self::fetch), reserving the result's bytes from the pool's
    /// budget before downloading them ([`Reservation::reserve`]). The pool
    /// always calls this one. By default it reserves nothing, and the pool
    /// counts the result's bytes once it's finished, so the budget then
    /// holds only after the fact.
    fn fetch_reserving(
        &self,
        peer: SocketAddr,
        key: InfoHash,
        reservation: Reservation,
    ) -> BoxFuture<'static, Result<Fetched, FailureKind>> {
        drop(reservation);
        self.fetch(peer, key)
    }
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

    /// Peer-wire reserves at the extended handshake, with the bound of
    /// what its result can hold (`lodestar_peer_wire::Reserve`).
    fn fetch_reserving(
        &self,
        peer: SocketAddr,
        key: InfoHash,
        reservation: Reservation,
    ) -> BoxFuture<'static, Result<Fetched, FailureKind>> {
        let mut opts = self.0.clone();
        opts.reserve = Some(Reserve(Arc::new(move |now, total| {
            let r = reservation.clone();
            Box::pin(async move { r.reserve(now, total).await })
        })));
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
    /// A fleet member's per-peer-IP share from the start (WK-10e), so it
    /// never dials before it has it; `None` (the default) is `per_ip`
    /// alone. [`PoolHandle::set_ip_cap`] changes it.
    pub ip_cap: IpCap,
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
    /// The byte budget for info dicts (X-fetch-pool-bytes,
    /// X-fetch-meta-reserve): what fetches in flight have reserved for
    /// their results, plus finished results not yet joined, waiting for a
    /// slot in the output channel, or in the channel and not yet taken.
    /// Each is weighed by [`weigh`]. A fetch reserves once its peer has
    /// announced the size and before it downloads, and waits while the
    /// budget is full, so with [`PeerWireFetcher`] the total never passes
    /// the budget, except by one dict larger than the whole budget, which
    /// is fetched alone. A [`Fetcher`] that doesn't reserve is counted once
    /// its result is in, so then the total can pass the budget by what its
    /// fetches in flight bring. See
    /// `docs/decisions/2026-11-06-X-fetch-meta-reserve.md`.
    pub max_held_bytes: usize,
}

/// [`PoolConfig::max_held_bytes`]'s default: 64 MiB.
pub const MAX_HELD_BYTES: usize = 64 << 20;

/// The bytes a fetched key holds: an upper estimate, with its heap.
pub fn weigh(fetched: &Fetched) -> u64 {
    let heap = fetched.info.capacity()
        + fetched.pex.capacity() * std::mem::size_of::<SocketAddr>()
        + fetched.client.as_ref().map_or(0, String::capacity);
    (std::mem::size_of::<FetchedKey>() + heap) as u64
}

/// How long a stopping pool waits for its consumer to take the results it
/// still holds. After that the rest go back into the saved queue, as they
/// do when the consumer is gone.
pub const SHUTDOWN_DRAIN: Duration = Duration::from_secs(5);

/// Reserve timeouts a key may have without one being charged as
/// a failed fetch (runner, 2026-11-07): enough to ride out a burst, but a
/// key that keeps waiting out (a large dict behind steady small ones) uses
/// up its peers and attempts in the end, like any timeout.
pub const RESERVE_WAITS_FREE: u8 = 3;

/// The pool's byte budget (X-fetch-meta-reserve): what fetches in flight
/// have reserved for their results, plus the bytes of finished results the
/// pool holds, against [`PoolConfig::max_held_bytes`]. Both are in
/// [`weigh`]'s units.
///
/// A fetch of a dict over 64 KiB reserves in two steps: its first request
/// window at the handshake (a *partial* claim), the rest once its peer has
/// sent that whole window (*growth*). A claim that's granted never waits
/// again, except a partial one for its growth. Two rules keep that from
/// deadlocking:
/// - **Growth goes first.** While a growth waits, nothing else is granted
///   and the pool starts no fetch, so the bytes it needs can only come
///   free: fetches that hold their whole reservation finish, and the
///   consumer takes held results.
/// - **Partial claims leave room for any one of them to grow.** A partial
///   claim is admitted only if the partial claims' bytes plus the largest
///   growth any of them still needs fit the budget. So once the rest has
///   drained, the oldest growth always fits beside the other partial
///   claims, which wait behind it holding only their first windows.
///
/// Partial claims wait in their own queue. One held back by its own rule
/// holds up nothing else; the oldest, when it passes its rule and only
/// lacks room, goes before new whole claims and stops dispatch, so steady
/// small traffic can't keep a large dict out.
#[derive(Debug)]
struct Budget {
    limit: u64,
    state: std::sync::Mutex<BudgetState>,
    /// Woken whenever bytes are released, for reservations waiting.
    freed: Notify,
    /// Woken when a reservation stops waiting, so the pool can dispatch.
    settled: Notify,
}

#[derive(Debug, Default)]
struct BudgetState {
    reserved: u64,
    held: u64,
    /// Waiting first reservations of whole results, oldest first, by claim
    /// id. Only the oldest may take room, so a large one isn't starved by
    /// small ones.
    queue: VecDeque<u64>,
    /// Waiting first reservations of partial claims, oldest first. Apart
    /// from `queue`, so a partial claim held back by its own rule (a liar's
    /// claim, perhaps) never holds up a whole one that fits.
    partials: VecDeque<u64>,
    /// The partial claim at the head of `partials` if it passes its own
    /// rule and only lacks room: it goes before whole first claims, and the
    /// pool starts no fetch, until it's granted or leaves.
    starved: Option<u64>,
    /// Waiting growths, oldest first; they go before any first reservation.
    growth: VecDeque<u64>,
    /// Bytes held by partial claims, and the growth each still needs.
    partial: u64,
    needs: BTreeMap<u64, usize>,
    next_id: u64,
}

impl BudgetState {
    fn add_need(&mut self, bytes: u64, need: u64) {
        self.partial += bytes;
        *self.needs.entry(need).or_insert(0) += 1;
    }

    fn remove_need(&mut self, bytes: u64, need: u64) {
        if need == 0 {
            return;
        }
        self.partial = self.partial.saturating_sub(bytes);
        if let Some(n) = self.needs.get_mut(&need) {
            *n -= 1;
            if *n == 0 {
                self.needs.remove(&need);
            }
        }
    }

    fn largest_need(&self) -> u64 {
        self.needs.last_key_value().map_or(0, |(need, _)| *need)
    }
}

impl Budget {
    fn new(limit: usize) -> Budget {
        Budget {
            limit: limit as u64,
            state: std::sync::Mutex::new(BudgetState::default()),
            freed: Notify::new(),
            settled: Notify::new(),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, BudgetState> {
        // Nothing under it panics (the counts saturate), so a poisoned
        // lock would still hold the best counts known.
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Held and reserved bytes, read together.
    fn usage(&self) -> (u64, u64) {
        let s = self.lock();
        (s.held, s.reserved)
    }

    /// Room for another fetch: under the budget, and no whole reservation,
    /// growth or room-starved partial claim waiting. A partial claim held
    /// back by its own rule doesn't stop dispatch: it may be a liar's.
    fn has_room(&self) -> bool {
        let s = self.lock();
        s.queue.is_empty()
            && s.growth.is_empty()
            && s.starved.is_none()
            && s.reserved.saturating_add(s.held) < self.limit
    }

    /// Bytes the pool no longer holds.
    fn release_held(&self, bytes: u64) {
        if bytes == 0 {
            return;
        }
        let mut s = self.lock();
        s.held = s.held.saturating_sub(bytes);
        drop(s);
        self.freed.notify_waiters();
    }
}

/// One fetch's claim on the pool's byte budget, handed to
/// [`Fetcher::fetch_reserving`]. Clones share one claim, released when the
/// last of them is dropped: whether the fetch succeeded, failed, timed
/// out, was cancelled or panicked. A success turns the claim into the
/// held bytes of its result instead.
#[derive(Debug, Clone)]
pub struct Reservation(Arc<Claim>);

#[derive(Debug)]
struct Claim {
    budget: Arc<Budget>,
    id: u64,
    /// Bytes reserved; changed only under the budget's lock.
    bytes: AtomicU64,
    /// For a partial claim, the growth it still needs (0 otherwise);
    /// changed only under the budget's lock.
    need: AtomicU64,
    /// Set while waiting for room and left set if the wait never ended:
    /// the fetch ran out of time waiting, not because of its peer.
    waited_out: AtomicBool,
}

impl Reservation {
    fn new(budget: Arc<Budget>) -> Reservation {
        let id = {
            let mut s = budget.lock();
            s.next_id += 1;
            s.next_id
        };
        Reservation(Arc::new(Claim {
            budget,
            id,
            bytes: AtomicU64::new(0),
            need: AtomicU64::new(0),
            waited_out: AtomicBool::new(false),
        }))
    }

    /// Reserves room for `now` heap bytes of a result whose heap holds at
    /// most `total` (each with the [`FetchedKey`] itself, [`weigh`]'s
    /// units), waiting while the budget has no room. `now < total` makes a
    /// partial claim, which a later call grows (peer-wire's two steps,
    /// `lodestar_peer_wire::Reserve`). Called again with less, it keeps
    /// what it has.
    ///
    /// A first reservation that finds nothing else reserved or held always
    /// gets its room, so even a dict larger than the whole budget is
    /// fetched, alone. Waiting reservations are served oldest first,
    /// growths before first reservations. Cancel-safe: dropped while
    /// waiting, it leaves its queue and keeps only what it had.
    pub async fn reserve(&self, now: usize, total: usize) {
        let key = std::mem::size_of::<FetchedKey>() as u64;
        let want_total = key.saturating_add(total as u64);
        let want = key.saturating_add(now as u64).min(want_total);
        let claim = &*self.0;
        let budget = &*claim.budget;
        let mut queued: Option<Queued<'_>> = None;
        loop {
            let freed = budget.freed.notified();
            tokio::pin!(freed);
            freed.as_mut().enable();
            // The first failure joins its queue under the same lock as the
            // check, so no other claim can slip in between.
            if self.try_grow(want, want_total, queued.is_none()) {
                if let Some(q) = queued.take() {
                    claim.waited_out.store(false, Ordering::Relaxed);
                    drop(q);
                }
                return;
            }
            if queued.is_none() {
                claim.waited_out.store(true, Ordering::Relaxed);
                queued = Some(Queued(claim));
            }
            freed.await;
        }
    }

    /// Takes the room for `want` bytes (of `want_total`) if the rules in
    /// [`Budget`] allow it now.
    fn try_grow(&self, want: u64, want_total: u64, enqueue: bool) -> bool {
        let claim = &*self.0;
        let mut s = claim.budget.lock();
        if self.grant(&mut s, want, want_total) {
            return true;
        }
        if enqueue {
            if claim.bytes.load(Ordering::Relaxed) > 0 {
                s.growth.push_back(claim.id);
            } else if want < want_total {
                s.partials.push_back(claim.id);
            } else {
                s.queue.push_back(claim.id);
            }
        }
        false
    }

    /// [`try_grow`](Self::try_grow)'s rules, under the budget's lock.
    fn grant(&self, s: &mut BudgetState, want: u64, want_total: u64) -> bool {
        let claim = &*self.0;
        let limit = claim.budget.limit;
        let mine = claim.bytes.load(Ordering::Relaxed);
        if mine >= want {
            return true;
        }
        let growing = mine > 0;
        let partial = want < want_total;
        let first_in = |q: &VecDeque<u64>| q.front().is_none_or(|&id| id == claim.id);
        let in_turn = if growing {
            first_in(&s.growth)
        } else if partial {
            s.growth.is_empty() && first_in(&s.partials)
        } else {
            s.growth.is_empty() && s.starved.is_none() && first_in(&s.queue)
        };
        if !in_turn {
            return false;
        }
        let used = s.reserved.saturating_add(s.held);
        let others = used.saturating_sub(mine);
        let extra = want - mine;
        let first_partial = partial && !growing;
        if first_partial && others > 0 {
            let need = s.largest_need().max(want_total - want);
            if s.partial.saturating_add(want).saturating_add(need) > limit {
                // Held back by its own rule (perhaps a liar's claim): it
                // holds up nothing else.
                if s.starved == Some(claim.id) {
                    s.starved = None;
                    claim.budget.settled.notify_one();
                    claim.budget.freed.notify_waiters();
                }
                return false;
            }
        }
        if others > 0 && used.saturating_add(extra) > limit {
            if first_partial {
                // An honest-looking large dict that only lacks room: it
                // goes before new whole claims and stops dispatch, or
                // steady small traffic could keep it out for ever.
                s.starved = Some(claim.id);
            }
            return false;
        }
        if s.starved == Some(claim.id) {
            s.starved = None;
        }
        s.reserved += extra;
        let old_need = claim.need.load(Ordering::Relaxed);
        s.remove_need(mine, old_need);
        let need = if partial { want_total - want } else { 0 };
        if partial {
            s.add_need(want, need);
        }
        claim.need.store(need, Ordering::Relaxed);
        claim.bytes.store(want, Ordering::Relaxed);
        true
    }

    /// The bytes reserved.
    pub fn bytes(&self) -> u64 {
        self.0.bytes.load(Ordering::Relaxed)
    }

    /// The fetch's result weighs `weight`: its reservation becomes held
    /// bytes. A fetcher that reserved less (or nothing) adds the rest here,
    /// after the fact.
    fn settle(&self, weight: u64) {
        let claim = &*self.0;
        let mut s = claim.budget.lock();
        let mine = claim.bytes.swap(0, Ordering::Relaxed);
        let need = claim.need.swap(0, Ordering::Relaxed);
        s.remove_need(mine, need);
        s.reserved = s.reserved.saturating_sub(mine);
        s.held = s.held.saturating_add(weight);
        drop(s);
        if weight < mine {
            claim.budget.freed.notify_waiters();
        }
    }

    fn waited_out(&self) -> bool {
        self.0.waited_out.load(Ordering::Relaxed)
    }
}

impl Drop for Claim {
    fn drop(&mut self) {
        let mine = *self.bytes.get_mut();
        if mine > 0 {
            let need = *self.need.get_mut();
            let mut s = self.budget.lock();
            s.remove_need(mine, need);
            s.reserved = s.reserved.saturating_sub(mine);
            drop(s);
            self.budget.freed.notify_waiters();
        }
    }
}

/// A claim's place in a queue of the budget's; leaving it (or being
/// dropped with a cancelled wait) wakes the next waiter and the pool.
struct Queued<'a>(&'a Claim);

impl Drop for Queued<'_> {
    fn drop(&mut self) {
        let budget = &*self.0.budget;
        let mut s = budget.lock();
        let id = self.0.id;
        s.queue.retain(|&q| q != id);
        s.partials.retain(|&q| q != id);
        s.growth.retain(|&q| q != id);
        if s.starved == Some(id) {
            s.starved = None;
        }
        drop(s);
        budget.freed.notify_waiters();
        budget.settled.notify_one();
    }
}

impl Default for PoolConfig {
    fn default() -> Self {
        PoolConfig {
            max_sockets: 400,
            per_ip: 2,
            ip_cap: IpCap::default(),
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
            max_held_bytes: MAX_HELD_BYTES,
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
    /// Gauge: the bytes of finished fetches the pool holds (see
    /// [`PoolConfig::max_held_bytes`]).
    pub held_bytes: AtomicU64,
    /// Times the budget (held plus reserved bytes) or a waiting
    /// reservation stopped the pool starting fetches. A result waiting for
    /// an output slot stops it too, but isn't counted here.
    pub held_full: AtomicU64,
    /// Gauge: the bytes fetches in flight have reserved for their results.
    pub reserved_bytes: AtomicU64,
    /// Fetches whose deadline ran out while they waited for a reservation.
    /// The peer isn't charged: it's tried again later.
    pub reserve_timeouts: AtomicU64,
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

/// How many connections this install may open to one peer IP (WK-10e,
/// plan Q9): a fleet member's share of one install's limit, so the members
/// behind one public address together never open more than one install
/// would. 0 means never dial that IP (another member's bucket). The pool
/// never goes above its own `per_ip` whatever this says.
pub trait PeerIpCap: Send + Sync + 'static {
    fn connections_to(&self, ip: IpAddr) -> usize;
}

/// A [`PeerIpCap`] in a [`PoolConfig`]: `None` is `per_ip` alone.
#[derive(Clone, Default)]
pub struct IpCap(pub Option<Arc<dyn PeerIpCap>>);

impl std::fmt::Debug for IpCap {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(if self.0.is_some() {
            "IpCap(policy)"
        } else {
            "IpCap(None)"
        })
    }
}

/// `per_ip`, or the policy's cap for `ip` if one is set, whichever is lower.
fn cap_for(policy: Option<&Arc<dyn PeerIpCap>>, per_ip: usize, ip: IpAddr) -> usize {
    policy.map_or(per_ip, |p| p.connections_to(ip).min(per_ip))
}

enum Cmd {
    SetWeight(SourceId, u32),
    SetIpCap(Option<Arc<dyn PeerIpCap>>),
    Interest(InfoHash, oneshot::Sender<bool>),
    Snapshot(oneshot::Sender<Vec<QueuedKey>>),
    Queued(oneshot::Sender<Vec<(SourceId, usize)>>),
    SetPaused(bool),
    DropSource(SourceId, oneshot::Sender<usize>),
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

    /// Sets the per-peer-IP policy (WK-10e); `None` is `per_ip` alone, as
    /// a single install. It applies to the next dial: a connection already
    /// open finishes, and a peer the policy no longer allows is skipped
    /// from then on.
    pub async fn set_ip_cap(&self, cap: Option<Arc<dyn PeerIpCap>>) {
        let _ = self.cmds.send(Cmd::SetIpCap(cap)).await;
    }

    /// Drops every key only `source` found, in whatever state (a fetch in
    /// flight is abandoned when it ends), for a purge (SRC-14b). A key
    /// other sources found too stays without `source`'s bit, and passes to
    /// the lowest of them when `source` found it first. A dropped key ends
    /// as evicted, which releases its lease. Returns the keys dropped; 0
    /// when the pool has stopped.
    pub async fn drop_source(&self, source: SourceId) -> usize {
        let (tx, rx) = oneshot::channel();
        let _ = self.cmds.send(Cmd::DropSource(source, tx)).await;
        rx.await.unwrap_or(0)
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
    /// Where fetched keys go. The pool must be this channel's only sender:
    /// it reads the channel's length to tell which of its results the
    /// consumer still hasn't taken, and another sender's items would be
    /// counted as the pool's (an overcount, which only slows fetching).
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
    let budget = Arc::new(Budget::new(config.max_held_bytes));
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
        outbox: VecDeque::new(),
        sent: VecDeque::new(),
        permits: Vec::new(),
        out_closed: false,
        full: false,
        budget,
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
    /// Fetches of this key that ran out of time waiting for a reservation
    /// (X-fetch-meta-reserve); the `RESERVE_WAITS_FREE`th is charged.
    reserve_waits: u8,
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
    /// Each fetch's result, and whether it ran out of time waiting for a
    /// reservation.
    tasks: JoinSet<(Result<Fetched, FailureKind>, bool)>,
    /// What each fetch task is fetching, so even a panicked task is
    /// accounted for.
    task_keys: HashMap<Id, (InfoHash, SocketAddr)>,
    recent: HashSet<InfoHash>,
    recent_order: VecDeque<InfoHash>,
    /// Streaming peers: when keys not yet fetched got their first batch,
    /// for the first-batch-to-fetch time. At most one per entry.
    first_batch: HashMap<InfoHash, Instant>,
    /// Joined results waiting for a slot in the output channel, oldest
    /// first, with their weights.
    outbox: VecDeque<Waiting>,
    /// The weights of results sent into the output channel, oldest first,
    /// back to the oldest the consumer may not have taken yet. The pool is
    /// the channel's only sender, so the channel's length says how many of
    /// these it still holds ([`settle`](Self::settle)).
    sent: VecDeque<u64>,
    /// Output slots reserved and not yet used. Each reservation completes
    /// when the consumer has taken an item, which is what wakes the pool to
    /// release that item's bytes.
    permits: Vec<mpsc::OwnedPermit<FetchedKey>>,
    /// The output channel's receiver is gone; results wait in the outbox
    /// and go back to the queue at shutdown.
    out_closed: bool,
    /// The budget stopped fetching at the last dispatch.
    full: bool,
    /// Bytes reserved by fetches in flight and held in finished results,
    /// from the moment a fetch finishes (its reservation becomes the
    /// result's weight) until the consumer takes it or the pool drops it.
    budget: Arc<Budget>,
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
        // A second handle on the output, for reserving slots while the
        // pool's own state is borrowed by the other branches.
        let reserver = self.out.clone();
        let budget = self.budget.clone();
        loop {
            self.flush();
            self.dispatch();
            self.publish_stats();
            let wake = self.timers.peek().map(|Reverse((at, ..))| *at);
            let credit = self.credit_target();
            // Wait for an output slot while a result waits for one, or
            // while results of the pool's are still in the channel: a slot
            // freeing up means the consumer took one, which releases its
            // bytes (and may let a stopped budget start fetching again).
            // The slots taken are kept for the next results, at most the
            // channel's capacity.
            let reserve = !self.out_closed && (!self.outbox.is_empty() || !self.sent.is_empty());
            tokio::select! {
                biased;
                _ = shutdown.requested() => break,
                Some(joined) = self.tasks.join_next_with_id(), if !self.tasks.is_empty() => {
                    match joined {
                        Ok((id, (result, waited_out))) => self.joined(id, result, waited_out),
                        // Only a panic ends a fetch task early: a bug, but
                        // one peer's bytes mustn't take the pool down.
                        Err(e) => self.joined(e.id(), Err(FailureKind::Invalid), false),
                    }
                }
                // A reservation stopped waiting: dispatch may start again.
                () = budget.settled.notified(), if self.full => {}
                permit = async { reserver.clone().reserve_owned().await }, if reserve => {
                    match permit {
                        Ok(permit) => self.permits.push(permit),
                        Err(_) => self.out_closed = true,
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
        // A fetch that finished before the abort is delivered like any
        // other; one the abort cancelled keeps its entry, so it's saved
        // with the queue.
        while let Some(joined) = self.tasks.join_next_with_id().await {
            match joined {
                Ok((id, (result, waited_out))) => self.joined(id, result, waited_out),
                // Its reservation went with the task.
                Err(e) if e.is_cancelled() => {
                    self.task_keys.remove(&e.id());
                }
                Err(e) => self.joined(e.id(), Err(FailureKind::Invalid), false),
            }
        }
        if let Some(a) = &mut self.admission {
            let unused = a.drop_held();
            self.stats.admission_permit_returns[2].fetch_add(unused, Ordering::Relaxed);
        }
        // Drain what's waiting for the consumer, which runs until the pool
        // ends (the pipeline stops the pool first). Without a consumer, or
        // with one that hasn't taken them by the deadline, a result goes
        // back to the queue, to be fetched again next run.
        self.flush();
        let drain_by = Instant::now() + SHUTDOWN_DRAIN;
        let mut requeued = Vec::new();
        while let Some(w) = self.outbox.pop_front() {
            self.budget.release_held(w.weight);
            if self.out_closed {
                requeued.push(w.queued());
                continue;
            }
            match timeout_at(drain_by, self.out.reserve()).await {
                Ok(Ok(permit)) => permit.send(w.out),
                Ok(Err(_)) => {
                    self.out_closed = true;
                    requeued.push(w.queued());
                }
                Err(_) => {
                    // The consumer is stalled: the rest go back too.
                    self.out_closed = true;
                    requeued.push(w.queued());
                }
            }
        }
        self.permits.clear();
        let mut queue = self.snapshot();
        if !requeued.is_empty() {
            queue.extend(requeued);
            queue.sort_by_key(queue_order);
        }
        queue
    }

    /// Handles a joined fetch's result. Its bytes are already held in the
    /// budget (the task settled its reservation as it finished).
    fn joined(&mut self, id: Id, result: Result<Fetched, FailureKind>, waited_out: bool) {
        match self.task_keys.remove(&id) {
            Some((key, peer)) => self.on_result(key, peer, result, waited_out),
            None => {
                if let Ok(fetched) = &result {
                    self.budget.release_held(weigh(fetched));
                }
            }
        }
    }

    /// Moves the oldest results waiting into the output channel, as far
    /// as it has room.
    fn flush(&mut self) {
        while !self.outbox.is_empty() && !self.out_closed {
            let permit = match self.permits.pop() {
                Some(permit) => permit,
                None => match self.out.clone().try_reserve_owned() {
                    Ok(permit) => permit,
                    Err(TrySendError::Full(_)) => break,
                    Err(TrySendError::Closed(_)) => {
                        self.out_closed = true;
                        break;
                    }
                },
            };
            let w = self.outbox.pop_front().expect("not empty");
            permit.send(w.out);
            self.sent.push_back(w.weight);
        }
        self.settle();
    }

    /// Forgets the weights of results the consumer has taken from the
    /// channel. Items in it are the last ones sent: its length is its
    /// capacity less the free slots and the slots the pool has reserved.
    fn settle(&mut self) {
        let free = self.out.capacity() + self.permits.len();
        let queued = if self.out_closed {
            0
        } else {
            self.out.max_capacity().saturating_sub(free)
        };
        let mut taken = 0;
        while self.sent.len() > queued {
            taken += self.sent.pop_front().expect("longer than queued");
        }
        self.budget.release_held(taken);
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
                // Results not yet handed over are saved too: a crash
                // before they're sent would otherwise lose them silently.
                // Nothing filters one stored after this save: restore skips
                // only keys already queued, so it's fetched again next run,
                // as a key fetching when the queue was saved is.
                let mut queue = self.snapshot();
                if !self.outbox.is_empty() {
                    queue.extend(self.outbox.iter().map(Waiting::queued));
                    queue.sort_by_key(queue_order);
                }
                let _ = reply.send(queue);
            }
            Cmd::SetIpCap(cap) => self.cfg.ip_cap = IpCap(cap),
            Cmd::SetPaused(paused) => {
                self.cfg.paused = paused;
                // A paused pool grants nothing, so it keeps no permit back.
                if paused && let Some(a) = &mut self.admission {
                    let unused = a.drop_held();
                    self.stats.admission_permit_returns[2].fetch_add(unused, Ordering::Relaxed);
                }
            }
            Cmd::DropSource(source, reply) => {
                let _ = reply.send(self.drop_source(source));
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
            // Not a reply: a sighting of its own, which a worker may hold a
            // new lease for; the funnel ends it (R-mem-triage).
            if !d.reply {
                self.funnel.duplicate(&key, d.at.div_euclid(3600), d.source);
            }
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
                reserve_waits: 0,
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
        if self.entries.contains_key(&q.info_hash) {
            return;
        }
        // A saved queue larger than the cap: the key's restored lease ends
        // as an eviction, or nothing ever ends it (R-mem-triage).
        if self.entries.len() >= self.cfg.max_pending {
            self.lose_untracked(&q.info_hash, q.cohort, q.source, Loss::Evicted, None);
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
                reserve_waits: 0,
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
            // Another member's bucket: never dialled from here.
            if cap_for(self.cfg.ip_cap.0.as_ref(), self.cfg.per_ip, p.ip()) == 0 {
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
            // Backing off after waiting out a reservation, it still has its
            // peer: ready now rather than a second lookup.
            State::Backoff if e.has_untried() => self.make_ready(key),
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
            self.stream_ended(key);
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
        self.stream_ended(key);
    }

    /// A stream's lookup has ended (its slot already given back). A key
    /// still waiting on it is ready if it has a peer left to try (one that
    /// waited out a reservation, X-fetch-meta-reserve), and backs off
    /// otherwise.
    fn stream_ended(&mut self, key: InfoHash) {
        let e = &self.entries[&key];
        if e.state != State::Lookup {
            return;
        }
        if e.has_untried() {
            self.make_ready(key);
        } else {
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
                // A key that backed off after waiting out a reservation
                // still has its peer to try; otherwise it's due a lookup.
                State::Backoff if e.has_untried() => self.make_ready(key),
                State::Backoff => self.request_peers(key, WaitKind::Retry),
                State::Ready | State::Fetching | State::LookupQueued => {}
            }
        }
    }

    /// A key with no peer left that this install may dial: a stream keeps
    /// its lookup (its next batch or its end brings the key back), and any
    /// other key backs off for its next lookup, as after a failure.
    fn out_of_peers(&mut self, key: InfoHash) {
        if self.requests.get(&key).is_some_and(|r| r.streaming) {
            let e = self.entries.get_mut(&key).expect("present");
            e.state = State::Lookup;
            e.generation = next_generation(&mut self.generations);
        } else {
            self.back_off(key);
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
        // A result still waiting for a channel slot means the consumer is
        // behind: start nothing more (the backpressure from a slow writer).
        if !self.outbox.is_empty() {
            return;
        }
        self.settle();
        let mut full = false;
        let mut deferred = Vec::new();
        while self.tasks.len() < self.cfg.max_sockets {
            // The budget is checked before each start: a fetch reserves
            // only at its handshake, so this can't foresee its bytes, but a
            // full budget or a reservation waiting stops more starting.
            if !self.budget.has_room() {
                // Counted only when a key may be waiting for a socket.
                full = !self.interest.is_empty() || !self.rotation.is_empty();
                break;
            }
            let Some(key) = self.next_ready() else {
                break;
            };
            let (per_ip, policy) = (self.cfg.per_ip, self.cfg.ip_cap.0.as_ref());
            let e = self.entries.get_mut(&key).expect("validated");
            let pick = (e.tried..e.peers.len()).find(|&i| {
                let ip = e.peers[i].ip();
                self.per_ip.get(&ip).copied().unwrap_or(0) < cap_for(policy, per_ip, ip)
            });
            let Some(i) = pick else {
                // Peers the policy no longer lets this install dial (it
                // changed since they were added) are dropped, so the key
                // isn't put off for ever (WK-10e).
                let tried = e.tried;
                let mut at = 0;
                e.peers.retain(|p| {
                    let keep = at < tried || cap_for(policy, per_ip, p.ip()) > 0;
                    at += 1;
                    keep
                });
                if e.has_untried() {
                    // Every untried peer's IP is busy; try again next round.
                    deferred.push(key);
                } else {
                    self.out_of_peers(key);
                }
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
            let reservation = Reservation::new(self.budget.clone());
            let fetch = self.fetcher.fetch_reserving(peer, key, reservation.clone());
            let handle = self.tasks.spawn(async move {
                let result = fetch.await;
                // Held from the moment it finishes, joined or not; on any
                // other ending the reservation is released as it drops.
                if let Ok(fetched) = &result {
                    reservation.settle(weigh(fetched));
                }
                let waited_out = result.is_err() && reservation.waited_out();
                (result, waited_out)
            });
            self.task_keys.insert(handle.id(), (key, peer));
        }
        if full && !self.full {
            self.stats.held_full.fetch_add(1, Ordering::Relaxed);
        }
        self.full = full;
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

    fn on_result(
        &mut self,
        key: InfoHash,
        peer: SocketAddr,
        result: Result<Fetched, FailureKind>,
        waited_out: bool,
    ) {
        if let Some(n) = self.per_ip.get_mut(&peer.ip()) {
            *n -= 1;
            if *n == 0 {
                self.per_ip.remove(&peer.ip());
            }
        }
        let weight = result.as_ref().map_or(0, weigh);
        let Some(e) = self.entries.get_mut(&key) else {
            // Abandoned (a purge): its bytes go with it.
            self.budget.release_held(weight);
            return;
        };
        // The deadline ran out while it waited for room in the budget: not
        // the peer's failure, up to `RESERVE_WAITS_FREE` times for
        // a key. The next one is charged as the timeout it is, so a key
        // can't cycle for ever on waits alone.
        let waited_out = waited_out && {
            self.stats.reserve_timeouts.fetch_add(1, Ordering::Relaxed);
            e.reserve_waits += 1;
            if e.reserve_waits >= RESERVE_WAITS_FREE {
                e.reserve_waits = 0;
                false
            } else {
                true
            }
        };
        if waited_out {
            // The peer goes last among its untried peers, so the others go
            // first, and no attempt is used.
            let others = e.has_untried();
            if let Some(i) = e.peers[..e.tried].iter().position(|p| *p == peer) {
                e.peers.remove(i);
                e.peers.push(peer);
                e.tried -= 1;
            }
            if others {
                self.make_ready(key);
            } else if self.requests.get(&key).is_some_and(|r| r.streaming) {
                // CR-1a-S, as after a failure: the stream keeps its lookup
                // slot; its next batch or its end brings the key back, the
                // peer still untried.
                e.state = State::Lookup;
                e.generation = next_generation(&mut self.generations);
            } else {
                // A timer only, no lookup and no loss: the key waits
                // `retry_backoff`, then redials the same peer.
                e.state = State::Backoff;
                e.generation = next_generation(&mut self.generations);
                let generation = e.generation;
                let at = Instant::now() + self.cfg.retry_backoff.max(Duration::from_millis(1));
                self.schedule(at, key, generation);
            }
            return;
        }
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
                let interest = e.interest;
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
                // Sent at the loop's next flush; until it's sent, no
                // fetch starts (the backpressure from a slow writer).
                self.outbox.push_back(Waiting {
                    out,
                    weight,
                    interest,
                });
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

    /// See [`PoolHandle::drop_source`].
    fn drop_source(&mut self, source: SourceId) -> usize {
        let bit = 1u64 << source.get();
        let mut only = Vec::new();
        for (key, e) in &mut self.entries {
            if e.sources & bit == 0 && e.source != source {
                continue;
            }
            let rest = e.sources & !bit;
            if rest == 0 {
                only.push(*key);
                continue;
            }
            e.sources = rest;
            // Whether it's public was the AND of every finder's sightings,
            // the dropped one's included: which of the rest were private
            // isn't known, so it's private from here (SRC-14b). A live
            // public sighting makes it public again.
            e.private = true;
            if e.source == source {
                // The lowest remaining finder; its bit is under 64.
                e.source = SourceId::new(rest.trailing_zeros() as u8).expect("bit under 64");
            }
        }
        for key in &only {
            self.lose(*key, Loss::Evicted);
        }
        // Results waiting for an output slot are still the pool's: the
        // same rule applies to them, as to the fetch they came from.
        let mut dropped = HashSet::new();
        let mut kept = VecDeque::with_capacity(self.outbox.len());
        for mut w in std::mem::take(&mut self.outbox) {
            let out = &mut w.out;
            if out.sources & bit == 0 && out.source != source {
                kept.push_back(w);
                continue;
            }
            let rest = out.sources & !bit;
            if rest == 0 {
                self.budget.release_held(w.weight);
                dropped.insert(out.key);
                // Found again by another source, it's new, not a duplicate.
                self.recent.remove(&out.key);
                self.stats.evicted_service[3].fetch_add(1, Ordering::Relaxed);
                let (key, cohort, source) = (out.key, out.cohort, out.source);
                self.lose_untracked(&key, cohort, source, Loss::Evicted, None);
                continue;
            }
            out.sources = rest;
            out.private_source = true;
            if out.source == source {
                out.source = SourceId::new(rest.trailing_zeros() as u8).expect("bit under 64");
            }
            kept.push_back(w);
        }
        self.outbox = kept;
        if !dropped.is_empty() {
            // Its place in the recent order goes too, or ageing it out
            // later would forget the key if it's fetched again.
            self.recent_order.retain(|k| !dropped.contains(k));
        }
        only.len() + dropped.len()
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
        // Every lookup counted holds a request, so the request decides: a
        // stream that has already ended took its count with it, though the
        // key may still read `Lookup`.
        if self.requests.remove(&key).is_some() {
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
        out.sort_by_key(queue_order);
        out
    }

    fn publish_stats(&mut self) {
        self.settle();
        let s = &self.stats;
        let (held, reserved) = self.budget.usage();
        s.held_bytes.store(held, Ordering::Relaxed);
        s.reserved_bytes.store(reserved, Ordering::Relaxed);
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

/// The saved queue's order: interest first, then the oldest cohort.
fn queue_order(q: &QueuedKey) -> (Reverse<u8>, i64, [u8; 20]) {
    (Reverse(q.priority), q.cohort, q.info_hash.0)
}

/// A fetched result waiting for a slot in the output channel.
struct Waiting {
    out: FetchedKey,
    /// Its [`weigh`]t, held in the budget.
    weight: u64,
    /// A user asked for it; kept if it goes back to the queue.
    interest: bool,
}

impl Waiting {
    fn queued(&self) -> QueuedKey {
        queued(&self.out, self.interest)
    }
}

/// A fetched key not handed over, saved to be fetched again.
fn queued(k: &FetchedKey, interest: bool) -> QueuedKey {
    QueuedKey {
        info_hash: k.key,
        source: k.source,
        cohort: k.cohort,
        strategy: k.strategy,
        priority: u8::from(interest),
        attempts: 0,
        private_source: k.private_source,
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

#[cfg(test)]
mod budget {
    //! The byte budget's claims (X-fetch-meta-reserve): each is released
    //! exactly once, whichever way its fetch ends.
    use super::*;

    const KEY: u64 = std::mem::size_of::<FetchedKey>() as u64;

    /// Polls `done` until it holds, for at most 10 s (paused or real time).
    async fn until(what: &str, mut done: impl AsyncFnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !done().await {
            assert!(Instant::now() < deadline, "timed out waiting: {what}");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    /// Waits until the budget's queues satisfy `done` (a waiter spawned
    /// has queued), for at most 10 s.
    async fn queued(b: &Budget, done: impl Fn(&BudgetState) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !done(&b.lock()) {
            assert!(Instant::now() < deadline, "the waiter never queued");
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    }

    fn used(b: &Budget) -> u64 {
        let (held, reserved) = b.usage();
        held + reserved
    }

    #[tokio::test]
    async fn a_claim_is_released_once_when_its_last_clone_drops() {
        let b = Arc::new(Budget::new(1000));
        let r = Reservation::new(b.clone());
        let copy = r.clone();
        r.reserve(100, 100).await;
        // Again, smaller: it keeps the larger reservation.
        copy.reserve(50, 50).await;
        assert_eq!(r.bytes(), KEY + 100);
        assert_eq!(b.usage(), (0, KEY + 100));
        drop(r);
        assert_eq!(b.usage(), (0, KEY + 100), "a clone still holds it");
        drop(copy);
        assert_eq!(b.usage(), (0, 0));
    }

    #[tokio::test]
    async fn a_settled_claim_becomes_held_bytes_and_releases_the_rest() {
        let b = Arc::new(Budget::new(1000));
        let r = Reservation::new(b.clone());
        r.reserve(200, 200).await;
        r.settle(KEY + 150);
        assert_eq!(b.usage(), (KEY + 150, 0));
        drop(r);
        assert_eq!(b.usage(), (KEY + 150, 0), "nothing released twice");
        b.release_held(KEY + 150);
        assert_eq!(used(&b), 0);
        // A fetcher that reserved nothing is counted after the fact.
        let r = Reservation::new(b.clone());
        r.settle(500);
        drop(r);
        assert_eq!(b.usage(), (500, 0));
    }

    #[tokio::test(start_paused = true)]
    async fn a_full_budget_makes_reservations_wait_oldest_first() {
        let b = Arc::new(Budget::new(4 * KEY as usize + 300));
        let first = Reservation::new(b.clone());
        first
            .reserve(2 * KEY as usize + 250, 2 * KEY as usize + 250)
            .await;
        assert!(b.has_room());
        // Too large to fit beside the first: it waits, and so does a small
        // one after it that would fit beside the first.
        let big = Reservation::new(b.clone());
        let small = Reservation::new(b.clone());
        let mut big_wait = tokio::spawn({
            let big = big.clone();
            async move { big.reserve(KEY as usize + 40, KEY as usize + 40).await }
        });
        queued(&b, |s| s.queue.len() == 1).await;
        let mut small_wait = tokio::spawn({
            let small = small.clone();
            async move { small.reserve(10, 10).await }
        });
        tokio::time::sleep(Duration::from_secs(10)).await;
        assert!(!big_wait.is_finished() && !small_wait.is_finished());
        assert!(!b.has_room(), "no fetch starts while one waits");
        assert!(big.0.waited_out.load(Ordering::Relaxed));
        // The first ends: the big one goes first, then the small one fits.
        drop(first);
        (&mut big_wait).await.unwrap();
        assert!(!big.0.waited_out.load(Ordering::Relaxed));
        (&mut small_wait).await.unwrap();
        assert_eq!(used(&b), big.bytes() + small.bytes());
        assert!(used(&b) <= 4 * KEY + 300);
        drop((big, small));
        assert_eq!(used(&b), 0);
        assert!(b.has_room());
    }

    #[tokio::test(start_paused = true)]
    async fn a_cancelled_wait_leaves_the_queue_and_takes_nothing() {
        let b = Arc::new(Budget::new(KEY as usize + 100));
        let first = Reservation::new(b.clone());
        first.reserve(100, 100).await;
        let waiting = Reservation::new(b.clone());
        let timed_out =
            tokio::time::timeout(Duration::from_secs(8), waiting.reserve(100, 100)).await;
        assert!(timed_out.is_err());
        assert!(waiting.waited_out(), "ran out of time waiting");
        assert_eq!(waiting.bytes(), 0);
        assert!(b.lock().queue.is_empty(), "left the queue");
        drop(waiting);
        assert_eq!(used(&b), KEY + 100);
        // Another can still go once the first ends.
        drop(first);
        let next = Reservation::new(b.clone());
        next.reserve(100, 100).await;
        assert_eq!(used(&b), KEY + 100);
    }

    /// A dict larger than the whole budget is reserved once nothing else
    /// is reserved or held, so it's fetched alone rather than never.
    #[tokio::test(start_paused = true)]
    async fn a_claim_over_the_budget_passes_alone() {
        let b = Arc::new(Budget::new(100));
        let r = Reservation::new(b.clone());
        r.reserve(10_000, 10_000).await;
        assert_eq!(used(&b), KEY + 10_000);
        r.settle(KEY + 10_000);
        drop(r);
        let next = Reservation::new(b.clone());
        let wait = tokio::time::timeout(Duration::from_secs(8), next.reserve(1, 1)).await;
        assert!(wait.is_err(), "waits while the big one is held");
        b.release_held(KEY + 10_000);
        next.reserve(1, 1).await;
        assert_eq!(used(&b), KEY + 1);
    }

    /// Peers that announce a large dict and never send hold only their
    /// first window: small dicts still get room, and partial claims stop
    /// once one more couldn't be sure to grow.
    #[tokio::test(start_paused = true)]
    async fn partial_claims_that_never_grow_leave_room_for_small_dicts() {
        let limit = 10_000u64;
        let b = Arc::new(Budget::new(limit as usize));
        let mut liars = Vec::new();
        // Each holds KEY + 100 and could need 4,900 more.
        loop {
            let r = Reservation::new(b.clone());
            let got = tokio::time::timeout(Duration::from_secs(1), r.reserve(100, 5_000)).await;
            if got.is_err() {
                break;
            }
            liars.push(r);
        }
        let n = liars.len() as u64;
        assert!(n >= 2, "{n} admitted");
        let (_, reserved) = b.usage();
        assert_eq!(reserved, n * (KEY + 100));
        assert!(reserved + 4_900 <= limit, "room left for any one to grow");
        assert!(reserved + (KEY + 100) + 4_900 > limit, "and no more");
        // One more keeps waiting, held back by its own rule...
        let blocked = Reservation::new(b.clone());
        let blocked_wait = tokio::spawn({
            let blocked = blocked.clone();
            async move { blocked.reserve(100, 5_000).await }
        });
        queued(&b, |s| s.partials.len() == 1).await;
        // ...but holds up neither a small dict, which reserves whole beside
        // them, nor dispatch.
        assert!(
            b.has_room(),
            "a waiting partial claim doesn't stop dispatch"
        );
        let small = Reservation::new(b.clone());
        tokio::time::timeout(Duration::from_secs(1), small.reserve(50, 50))
            .await
            .expect("a small dict isn't held up by the liars");
        assert!(!blocked_wait.is_finished());
        // A liar's deadline ends: the blocked one gets its window.
        drop(liars.pop());
        blocked_wait.await.unwrap();
        drop((liars, small, blocked));
        assert_eq!(used(&b), 0);
    }

    /// No deadlock: partial claims all asking to grow are served one at a
    /// time, oldest first, as the ones ahead finish. While a growth waits,
    /// no first reservation is granted and no fetch starts.
    #[tokio::test(start_paused = true)]
    async fn growths_go_first_and_always_complete() {
        let (now, total) = (500usize, 2_000usize);
        // Room for exactly one small claim beside three grown windows' worth,
        // so only growth's priority holds the small one back.
        let limit = 3 * (KEY + now as u64) + (total - now) as u64 + KEY + 1;
        let b = Arc::new(Budget::new(limit as usize));
        let claims: Vec<Reservation> = (0..3).map(|_| Reservation::new(b.clone())).collect();
        for c in &claims {
            tokio::time::timeout(Duration::from_secs(1), c.reserve(now, total))
                .await
                .expect("three partial claims fit");
        }
        let fourth = Reservation::new(b.clone());
        assert!(
            tokio::time::timeout(Duration::from_secs(1), fourth.reserve(now, total))
                .await
                .is_err(),
            "a fourth couldn't be sure to grow"
        );
        // All three grow: the first fits at once, the others wait for it.
        claims[0].reserve(total, total).await;
        let mut waits: Vec<_> = claims[1..]
            .iter()
            .map(|c| {
                let c = c.clone();
                tokio::spawn(async move { c.reserve(total, total).await })
            })
            .collect();
        let small = Reservation::new(b.clone());
        let mut small_wait = tokio::spawn({
            let small = small.clone();
            async move { small.reserve(1, 1).await }
        });
        tokio::time::sleep(Duration::from_secs(10)).await;
        assert!(waits.iter().all(|w| !w.is_finished()));
        assert!(
            !small_wait.is_finished(),
            "growth goes first, though it fits"
        );
        assert!(!b.has_room());
        let mut claims = claims.into_iter();
        drop(claims.next());
        (&mut waits[0]).await.unwrap();
        drop(claims.next());
        (&mut waits[1]).await.unwrap();
        (&mut small_wait).await.unwrap();
        assert!(used(&b) <= limit);
        drop((claims, small, fourth));
        assert_eq!(used(&b), 0);
        assert_eq!(b.lock().partial, 0);
        assert!(b.lock().needs.is_empty());
    }

    /// A raw plaintext peer for one fetch: it announces `info`'s size and
    /// serves it, or (with `stall`) announces it and never sends a piece.
    async fn raw_peer(info: Vec<u8>, info_hash: [u8; 20], stall: bool) -> SocketAddr {
        use lodestar_peer_wire::message;
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut s, _) = listener.accept().await.unwrap();
            let frame = |ext_id: u8, body: Vec<u8>| {
                let mut f = ((body.len() + 2) as u32).to_be_bytes().to_vec();
                f.extend([20, ext_id]);
                f.extend(body);
                f
            };
            let mut theirs = [0u8; message::HANDSHAKE_LEN];
            s.read_exact(&mut theirs).await.unwrap();
            s.write_all(&message::handshake(
                &info_hash,
                b"-RW0001-rawrawrawraw",
                true,
            ))
            .await
            .unwrap();
            let hello = message::our_extended_handshake(Some(info.len()));
            s.write_all(&frame(0, hello)).await.unwrap();
            loop {
                let mut len = [0u8; 4];
                if s.read_exact(&mut len).await.is_err() {
                    return;
                }
                let mut body = vec![0u8; u32::from_be_bytes(len) as usize];
                if s.read_exact(&mut body).await.is_err() {
                    return;
                }
                if stall || body.len() < 2 || body[0] != 20 || body[1] != message::OUR_UT_METADATA {
                    continue;
                }
                if let Ok(message::MetadataMessage::Request { piece }) =
                    message::parse_metadata_message(&body[2..])
                {
                    let start = piece as usize * message::METADATA_PIECE;
                    let end = (start + message::METADATA_PIECE).min(info.len());
                    let reply =
                        message::metadata_data(piece as usize, info.len(), &info[start..end]);
                    // The fetcher told us to use its `ut_metadata` id, 1.
                    s.write_all(&frame(1, reply)).await.unwrap();
                }
            }
        });
        addr
    }

    /// The real fetcher, through the pool: a fetch reserves from the
    /// pool's budget at the handshake, its success becomes held bytes the
    /// consumer's take releases, and a stalled peer's reservation is
    /// released at its deadline. Real time and TCP, with condition waits.
    #[tokio::test]
    async fn the_peer_wire_fetcher_reserves_from_the_pool_budget() {
        let info =
            b"d6:lengthi1e4:name4:test12:piece lengthi16384e6:pieces20:AAAAAAAAAAAAAAAAAAAAe"
                .to_vec();
        let key = lodestar_metainfo::Info::parse(&info)
            .unwrap()
            .primary_hash();
        let honest = raw_peer(info.clone(), key.0, false).await;
        let stalled_key = InfoHash([3; 20]);
        let staller = raw_peer(vec![b'x'; 300_000], stalled_key.0, true).await;

        let (disc_tx, discoveries) = mpsc::channel(16);
        let (out_tx, mut out) = mpsc::channel(16);
        let (stop, shutdown) = Shutdown::new();
        let fetcher = PeerWireFetcher(FetchOptions {
            allow_private: true,
            encryption: lodestar_peer_wire::Encryption::Disable,
            timeout: Duration::from_secs(3),
            ..FetchOptions::default()
        });
        let (handle, task) = spawn(
            PoolConfig::default(),
            Arc::new(fetcher),
            PoolIo {
                discoveries,
                peer_finders: Vec::new(),
                out: out_tx,
                funnel: Arc::new(crate::FunnelCounts::default()),
                shutdown,
            },
            HashMap::new(),
            Vec::new(),
        );
        let source = SourceId::new(0).unwrap();
        for (k, peer) in [(key, honest), (stalled_key, staller)] {
            disc_tx
                .send(Discovery {
                    source,
                    found: crate::Found::key(k, 0).with_peers(vec![peer]),
                    at: 1_700_000_000,
                    reply: false,
                })
                .await
                .unwrap();
        }
        let stats = handle.stats();
        let gauge = |g: &AtomicU64| g.load(Ordering::Relaxed);
        // The staller announced 300 KB but sent nothing: only its first
        // window (64 KiB, with the bound) shows.
        until("the stalled fetch's reservation", async || {
            handle.queued().await; // a round of the pool publishes its stats
            gauge(&stats.reserved_bytes) >= 64 << 10
        })
        .await;
        assert!(gauge(&stats.reserved_bytes) < 100_000, "not its claim");
        let got = tokio::time::timeout(Duration::from_secs(10), out.recv())
            .await
            .expect("the honest dict")
            .unwrap();
        assert_eq!(got.key, key);
        assert_eq!(got.fetched.info, info);
        // Taken by the consumer, and the staller ended at its deadline:
        // nothing is held or reserved.
        until("everything released", async || {
            handle.queued().await;
            gauge(&stats.held_bytes) == 0
                && gauge(&stats.reserved_bytes) == 0
                && stats.failure_count(FailureKind::TimedOut) == 1
        })
        .await;
        assert_eq!(
            gauge(&stats.reserve_timeouts),
            0,
            "a peer timeout, not a wait"
        );
        stop.send(true).unwrap();
        task.await.unwrap();
    }

    /// A partial claim that passes its own rule but lacks room goes before
    /// whole first claims and stops dispatch, so steady small traffic can't
    /// keep a large dict out; once room comes, it's granted first.
    #[tokio::test(start_paused = true)]
    async fn a_room_starved_partial_claim_goes_before_whole_ones() {
        let limit = 10_000u64;
        let b = Arc::new(Budget::new(limit as usize));
        // Whole claims fill most of the budget.
        let fills: Vec<Reservation> = (0..4).map(|_| Reservation::new(b.clone())).collect();
        for f in &fills {
            f.reserve(2_000, 2_000).await;
        }
        assert!(b.has_room());
        // A partial claim whose window doesn't fit, though its rule passes.
        let large = Reservation::new(b.clone());
        let mut large_wait = tokio::spawn({
            let large = large.clone();
            async move { large.reserve(2_000, 6_000).await }
        });
        queued(&b, |s| s.starved.is_some()).await;
        assert!(!b.has_room(), "dispatch stops");
        // A small whole claim that would fit waits behind it.
        let small = Reservation::new(b.clone());
        let mut small_wait = tokio::spawn({
            let small = small.clone();
            async move { small.reserve(10, 10).await }
        });
        tokio::time::sleep(Duration::from_secs(10)).await;
        assert!(!large_wait.is_finished() && !small_wait.is_finished());
        // Room comes: the large one first, then the small one beside it.
        let mut fills = fills.into_iter();
        drop(fills.next());
        (&mut large_wait).await.unwrap();
        (&mut small_wait).await.unwrap();
        assert!(b.lock().starved.is_none());
        drop((fills, large, small));
        assert_eq!(used(&b), 0);
    }
}
