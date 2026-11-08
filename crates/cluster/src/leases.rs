//! Fleet-wide in-flight leases with fencing tokens (architecture §19.3).
//!
//! Every new key is fetched by exactly one worker: the coordinator grants
//! it to the first worker that claims it, and later claims for the same
//! key only add their source to the lease. "Harvested" is counted at grant
//! time, once per key across the fleet, which keeps "new to us" globally
//! true.
//!
//! A lease ends exactly once:
//! - the holder commits the torrent or reports it lost, with the lease's
//!   token; or
//! - the holder goes silent for longer than the TTL, and the lease expires
//!   (to be granted again elsewhere with its cohort and sources kept).
//!
//! A report carrying any other token is stale (from a holder whose lease
//! expired) and changes nothing, so nothing is ever counted twice.
//!
//! [`LeaseTable`] is the bare table. [`LeaseAuthority`] is the one instance
//! the coordinator runs (WK-0): the table, the queue of keys waiting to be
//! granted again, and the [`LeaseEvents`] hook that does the funnel
//! accounting. The pipeline holds it and calls it directly for its built-in
//! worker; the cluster server gets an `Arc` to the same authority only when
//! remote workers are allowed, so a key can never be leased twice (once
//! locally, once remotely).

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use lodestar_metainfo::InfoHash;
use lodestar_store::SourceId;

/// A worker in the fleet. [`WorkerId::LOCAL`] is the coordinator's
/// built-in worker.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct WorkerId(pub u32);

impl WorkerId {
    pub const LOCAL: WorkerId = WorkerId(0);
}

/// A fencing token. The high 32 bits are the coordinator's epoch (its
/// start time), so a token issued before a restart never equals one
/// issued after it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Token(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Lease {
    pub holder: WorkerId,
    pub token: Token,
    /// Discovery hour, for cohort attribution (§7).
    pub cohort: i64,
    /// The source that found it first.
    pub source: SourceId,
    /// Every source that reported it while it was leased (a bitset).
    pub sources: u64,
    pub strategy: u8,
    /// Unix seconds of the first discovery.
    pub discovered_at: i64,
    /// The worker the key was first granted to, while no other worker
    /// has claimed it: cleared, under the authority's lock, the first time
    /// another does (held, or waiting for a re-grant), and `None` for a
    /// lease restored from an earlier run. Kept across re-grants. Insights'
    /// marginal gain (WK-15) counts a lease that ends with it set.
    pub finder: Option<WorkerId>,
}

/// What a claim got.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Claimed {
    /// The claimer holds the key now. The caller counts it as harvested.
    Granted(Token),
    /// Already leased (possibly to the claimer itself). The claim's
    /// source was added to the lease; its peer hints belong to `holder`.
    Held { holder: WorkerId, token: Token },
}

/// A key as a worker found it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Claim {
    pub key: InfoHash,
    pub source: SourceId,
    pub strategy: u8,
    /// Unix seconds.
    pub at: i64,
}

#[derive(Debug)]
pub struct LeaseTable {
    epoch: u32,
    seq: u32,
    /// Seconds a worker may stay silent before its leases expire.
    ttl: i64,
    leases: HashMap<InfoHash, Lease>,
    /// Unix seconds each worker was last heard from.
    workers: HashMap<WorkerId, i64>,
}

impl LeaseTable {
    pub fn new(epoch: u32, ttl_secs: i64) -> LeaseTable {
        LeaseTable {
            epoch,
            seq: 0,
            ttl: ttl_secs,
            leases: HashMap::new(),
            workers: HashMap::new(),
        }
    }

    fn token(&mut self) -> Token {
        // 4 billion grants in one run would wrap; restart a new epoch
        // instead of ever reusing a token.
        self.seq = self.seq.checked_add(1).unwrap_or_else(|| {
            self.epoch = self.epoch.wrapping_add(1);
            1
        });
        Token((u64::from(self.epoch) << 32) | u64::from(self.seq))
    }

    /// Records that `worker` is alive, renewing all its leases at once.
    pub fn heartbeat(&mut self, worker: WorkerId, now: i64) {
        self.workers.insert(worker, now);
    }

    /// Claims a newly found key for `worker`. A claim is also a sign of
    /// life.
    pub fn claim(&mut self, worker: WorkerId, c: Claim, now: i64) -> Claimed {
        self.heartbeat(worker, now);
        let bit = 1u64 << c.source.get();
        if let Some(lease) = self.leases.get_mut(&c.key) {
            lease.sources |= bit;
            if lease.finder != Some(worker) {
                lease.finder = None;
            }
            return Claimed::Held {
                holder: lease.holder,
                token: lease.token,
            };
        }
        let token = self.token();
        self.leases.insert(
            c.key,
            Lease {
                holder: worker,
                token,
                cohort: c.at.div_euclid(3600),
                source: c.source,
                sources: bit,
                strategy: c.strategy,
                discovered_at: c.at,
                finder: Some(worker),
            },
        );
        Claimed::Granted(token)
    }

    /// Re-establishes a lease that outlived a coordinator restart (a key
    /// saved in the fetch queue). It was counted as harvested in its own
    /// run, so this isn't a grant. A key that's already leased keeps its
    /// lease.
    pub fn restore(
        &mut self,
        worker: WorkerId,
        key: InfoHash,
        lease: RestoredLease,
        now: i64,
    ) -> Token {
        self.heartbeat(worker, now);
        if let Some(l) = self.leases.get(&key) {
            return l.token;
        }
        let token = self.token();
        self.leases.insert(
            key,
            Lease {
                holder: worker,
                token,
                cohort: lease.cohort,
                source: lease.source,
                sources: lease.sources,
                strategy: lease.strategy,
                discovered_at: lease.discovered_at,
                finder: None,
            },
        );
        token
    }

    /// Whether `token` is the key's current lease.
    pub fn is_current(&self, key: &InfoHash, token: Token) -> bool {
        self.leases.get(key).is_some_and(|l| l.token == token)
    }

    pub fn get(&self, key: &InfoHash) -> Option<&Lease> {
        self.leases.get(key)
    }

    /// Ends a lease: its key was committed or lost. A stale token (from a
    /// lease that already ended) returns `None` and changes nothing.
    pub fn release(&mut self, key: &InfoHash, token: Token) -> Option<Lease> {
        if !self.is_current(key, token) {
            return None;
        }
        self.leases.remove(key)
    }

    /// Ends every lease held by a worker silent for longer than the TTL,
    /// and forgets the worker. The caller grants the keys again (see
    /// [`regrant`](Self::regrant)).
    pub fn expire(&mut self, now: i64) -> Vec<(InfoHash, Lease)> {
        let ttl = self.ttl;
        let dead: Vec<WorkerId> = self
            .workers
            .iter()
            .filter(|&(_, &seen)| seen + ttl < now)
            .map(|(&w, _)| w)
            .collect();
        if dead.is_empty() {
            return Vec::new();
        }
        for w in &dead {
            self.workers.remove(w);
        }
        let expired: Vec<InfoHash> = self
            .leases
            .iter()
            .filter(|(_, l)| dead.contains(&l.holder))
            .map(|(k, _)| *k)
            .collect();
        expired
            .into_iter()
            .filter_map(|k| self.leases.remove(&k).map(|l| (k, l)))
            .collect()
    }

    /// Grants an expired lease to another worker under a new token,
    /// keeping its cohort, sources and strategy. Not a new harvest.
    pub fn regrant(&mut self, worker: WorkerId, key: InfoHash, lease: Lease, now: i64) -> Token {
        let token = self.restore(worker, key, RestoredLease::from(lease), now);
        if let Some(l) = self.leases.get_mut(&key).filter(|l| l.token == token) {
            l.finder = lease.finder;
        }
        token
    }

    /// Ends every lease `worker` holds at once and forgets it: it was
    /// revoked or finished draining (§19.6, "returns its leases"). The
    /// caller grants the keys again. Its late reports are stale from now on.
    pub fn evict(&mut self, worker: WorkerId) -> Vec<(InfoHash, Lease)> {
        self.workers.remove(&worker);
        let keys: Vec<InfoHash> = self
            .leases
            .iter()
            .filter(|(_, l)| l.holder == worker)
            .map(|(k, _)| *k)
            .collect();
        keys.into_iter()
            .filter_map(|k| self.leases.remove(&k).map(|l| (k, l)))
            .collect()
    }

    pub fn len(&self) -> usize {
        self.leases.len()
    }

    pub fn is_empty(&self) -> bool {
        self.leases.is_empty()
    }

    pub fn held_by(&self, worker: WorkerId) -> usize {
        self.leases.values().filter(|l| l.holder == worker).count()
    }

    pub fn ttl_secs(&self) -> i64 {
        self.ttl
    }
}

/// What the [`LeaseAuthority`] reports to whoever does the accounting.
///
/// `harvested`, `lost` and `expired` are called after the authority's lock
/// is released, exactly once per event; `outstanding` is called under it
/// (so gauges are never out of order) and must only store a number. A
/// caller may hold its own locks (the cluster server holds its fleet lock
/// around a claim), so no method may call back into the authority or the
/// server: record and return.
pub trait LeaseEvents: Send + Sync + 'static {
    /// A key was granted for the first time, to any worker: harvested,
    /// once per key across the fleet (§19.3). Re-grants and restores never
    /// call it.
    fn harvested(&self, _key: &InfoHash, _lease: &Lease) {}
    /// A lease ended with a terminal loss reported under its current
    /// token. `reason` is the funnel's loss code (`Loss as u8`).
    fn lost(&self, _key: &InfoHash, _lease: &Lease, _reason: u8) {}
    /// Leases held plus keys waiting for a re-grant, after any change.
    fn outstanding(&self, _n: usize) {}
    /// Leases of silent workers ended by an expiry, whoever ran it.
    fn expired(&self, _n: usize) {}
    /// Remote workers may now be granted keys
    /// ([`LeaseAuthority::serve_remote`]): once per authority, after the
    /// lock is released. Insights' per-worker counts (WK-15) start here.
    fn serving(&self) {}
}

/// Keys committed recently, remembered so a remote claim that raced the
/// commit (its store check ran before the row was written) isn't granted
/// and harvested again. About ten seconds of commits at the §19.8 ingest
/// target.
pub const RECENT_COMMITS: usize = 16_384;

/// One answer of [`LeaseAuthority::claim_batch`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BatchAnswer {
    /// `harvested`: a new key, counted once.
    Claimed { claimed: Claimed, harvested: bool },
    /// A new key over what the admission allowed: not leased.
    Busy,
    /// Committed moments ago (a remote claim racing the commit): stored.
    Committed,
}

/// Keys granted again to one worker, with their new tokens.
pub type Regranted = Vec<(InfoHash, Token)>;

/// No accounting (a cluster server running on its own, and tests).
#[derive(Debug, Clone, Copy, Default)]
pub struct NoEvents;

impl LeaseEvents for NoEvents {}

/// Recent commits: each key's latest commit number, and `(number, key)` in
/// commit order.
type Recent = (HashMap<InfoHash, u64>, VecDeque<(u64, InfoHash)>);

#[derive(Debug)]
struct AuthorityState {
    table: LeaseTable,
    /// Leases that ended without an outcome (expired, evicted), waiting to
    /// be granted again. A key here is still in flight: a claim for it
    /// takes it over as a re-grant, never as a new harvest.
    pending: HashMap<InfoHash, Lease>,
    /// `pending`'s keys in waiting order: oldest first, except that a key
    /// whose store check couldn't answer goes to the back and one whose
    /// delivery failed to the front. May hold keys no longer pending;
    /// they're skipped, and it's cleared whenever `pending` empties.
    order: VecDeque<InfoHash>,
    /// Kept only once remote workers are allowed ([`LeaseAuthority::serve_remote`]),
    /// so a single machine pays nothing. Each key maps to the sequence
    /// number of its latest commit; the queue holds `(seq, key)` in commit
    /// order, and an entry is evicted only if it is still that key's latest
    /// (a re-commit takes a new slot, so its old one never evicts it).
    recent: Option<Recent>,
    /// Commits remembered so far (a sequence number, see
    /// [`LeaseAuthority::commit_seq`]).
    commits: u64,
}

impl AuthorityState {
    fn in_flight(&self) -> usize {
        self.table.len() + self.pending.len()
    }

    fn park(&mut self, key: InfoHash, lease: Lease) {
        if self.pending.insert(key, lease).is_none() {
            self.order.push_back(key);
        }
    }

    /// Whether a claim for `key` would make a new lease (not one held).
    fn would_lease(&self, key: &InfoHash) -> bool {
        self.table.get(key).is_none()
    }

    fn committed_recently(&self, worker: WorkerId, key: &InfoHash) -> bool {
        worker != WorkerId::LOCAL
            && self.table.get(key).is_none()
            && !self.pending.contains_key(key)
            && self
                .recent
                .as_ref()
                .is_some_and(|(seqs, _)| seqs.contains_key(key))
    }

    fn remember_commit(&mut self, key: InfoHash) {
        if let Some((seqs, order)) = &mut self.recent {
            self.commits += 1;
            let seq = self.commits;
            seqs.insert(key, seq);
            order.push_back((seq, key));
            while order.len() > RECENT_COMMITS {
                let Some((old_seq, old)) = order.pop_front() else {
                    break;
                };
                if seqs.get(&old) == Some(&old_seq) {
                    seqs.remove(&old);
                }
            }
        }
    }

    /// One claim. The second value is the new lease, if it was a harvest.
    /// A claim by any worker but the key's finder clears
    /// [`Lease::finder`], held or waiting.
    fn claim(&mut self, worker: WorkerId, c: Claim, now: i64) -> (Claimed, Option<Lease>) {
        if let Some(mut lease) = self.pending.remove(&c.key) {
            if self.pending.is_empty() {
                self.order.clear();
            }
            lease.sources |= 1u64 << c.source.get();
            if lease.finder != Some(worker) {
                lease.finder = None;
            }
            let token = self.table.regrant(worker, c.key, lease, now);
            return (Claimed::Granted(token), None);
        }
        let claimed = self.table.claim(worker, c, now);
        let fresh = match claimed {
            Claimed::Granted(_) => self.table.get(&c.key).copied(),
            Claimed::Held { .. } => None,
        };
        (claimed, fresh)
    }
}

/// The coordinator's one lease authority (§19.3): every grant, local or
/// remote, goes through it. Cheap to share (`Arc`); the lock is held only
/// for map operations, never across an `.await`, and never while calling
/// [`LeaseEvents::harvested`] or [`LeaseEvents::lost`].
///
/// The built-in worker ([`WorkerId::LOCAL`]) runs in the coordinator's own
/// process and can't go silent, so its leases never expire; they end only
/// by commit, loss or eviction.
pub struct LeaseAuthority {
    state: Mutex<AuthorityState>,
    events: Arc<dyn LeaseEvents>,
}

impl std::fmt::Debug for LeaseAuthority {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = self.lock();
        f.debug_struct("LeaseAuthority")
            .field("leases", &s.table.len())
            .field("pending", &s.pending.len())
            .finish_non_exhaustive()
    }
}

impl LeaseAuthority {
    pub fn new(epoch: u32, ttl_secs: i64, events: Arc<dyn LeaseEvents>) -> LeaseAuthority {
        LeaseAuthority {
            state: Mutex::new(AuthorityState {
                table: LeaseTable::new(epoch, ttl_secs),
                pending: HashMap::new(),
                order: VecDeque::new(),
                recent: None,
                commits: 0,
            }),
            events,
        }
    }

    /// Starts remembering recent commits, for remote claims (the cluster
    /// server calls it when it's built on this authority).
    pub fn serve_remote(&self) {
        let mut s = self.lock();
        let first = s.recent.is_none();
        if first {
            s.recent = Some((HashMap::new(), VecDeque::new()));
        }
        drop(s);
        if first {
            self.events.serving();
        }
    }

    pub fn ttl_secs(&self) -> i64 {
        self.lock().table.ttl_secs()
    }

    /// The commit sequence number. A caller takes it before checking the
    /// store and passes it to [`claim_batch`](Self::claim_batch), which then
    /// knows whether every commit since that check is still remembered.
    pub fn commit_seq(&self) -> u64 {
        self.lock().commits
    }

    fn lock(&self) -> MutexGuard<'_, AuthorityState> {
        // Every operation leaves the maps consistent, so a panic elsewhere
        // while holding the lock doesn't poison the fleet.
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn gauge(&self, s: &AuthorityState) {
        self.events.outstanding(s.in_flight());
    }

    /// Claims a newly found key for `worker`. A key waiting for a re-grant
    /// is granted to the claimer under a new token, keeping its cohort, and
    /// isn't harvested again.
    ///
    /// For the built-in worker. A remote worker's claims go through
    /// [`claim_batch`](Self::claim_batch).
    pub fn claim(&self, worker: WorkerId, c: Claim, now: i64) -> Claimed {
        let mut s = self.lock();
        let (claimed, fresh) = s.claim(worker, c, now);
        self.gauge(&s);
        drop(s);
        if let Some(lease) = fresh {
            self.events.harvested(&c.key, &lease);
        }
        claimed
    }

    /// Claims a batch under one lock, for a remote worker. `admit`, if
    /// given, is asked once how many of the keys that would get a new lease
    /// (not ones already held) may get one now; the rest are
    /// [`BatchAnswer::Busy`]. A remote claim for a key committed moments ago
    /// is [`BatchAnswer::Committed`]. `checked_at` is the
    /// [`commit_seq`](Self::commit_seq) taken before the caller's store
    /// check: if more than [`RECENT_COMMITS`] commits have happened since,
    /// some may have been forgotten, so no key gets a new lease (all are
    /// `Busy`; the worker claims again).
    pub fn claim_batch(
        &self,
        worker: WorkerId,
        claims: &[Claim],
        now: i64,
        admit: Option<&dyn Fn(usize) -> usize>,
        checked_at: Option<u64>,
    ) -> Vec<BatchAnswer> {
        let mut s = self.lock();
        let forgotten =
            checked_at.is_some_and(|seq| s.commits.saturating_sub(seq) > RECENT_COMMITS as u64);
        let mut allowed = match admit {
            _ if forgotten => 0,
            Some(admit) => {
                let new = claims
                    .iter()
                    .filter(|c| s.would_lease(&c.key) && !s.committed_recently(worker, &c.key))
                    .count();
                if new == 0 { 0 } else { admit(new).min(new) }
            }
            None => usize::MAX,
        };
        let mut fresh_leases = Vec::new();
        let answers: Vec<BatchAnswer> = claims
            .iter()
            .map(|c| {
                if s.committed_recently(worker, &c.key) {
                    return BatchAnswer::Committed;
                }
                if s.would_lease(&c.key) {
                    if allowed == 0 {
                        return BatchAnswer::Busy;
                    }
                    allowed -= 1;
                }
                let (claimed, fresh) = s.claim(worker, *c, now);
                if let Some(lease) = fresh {
                    fresh_leases.push((c.key, lease));
                }
                BatchAnswer::Claimed {
                    claimed,
                    harvested: fresh.is_some(),
                }
            })
            .collect();
        self.gauge(&s);
        drop(s);
        for (key, lease) in &fresh_leases {
            self.events.harvested(key, lease);
        }
        answers
    }

    /// Re-establishes a lease saved by the previous run (not a harvest).
    pub fn restore(
        &self,
        worker: WorkerId,
        key: InfoHash,
        lease: RestoredLease,
        now: i64,
    ) -> Token {
        let mut s = self.lock();
        s.pending.remove(&key);
        let token = s.table.restore(worker, key, lease, now);
        self.gauge(&s);
        token
    }

    /// [`restore`](Self::restore), only if `key` is neither leased, nor
    /// waiting for a re-grant, nor committed recently (a commit since the
    /// caller's store check: [`RECENT_COMMITS`]), checked and done under one
    /// lock: the token, or `None` and nothing changed. For a late result (plan Q7, WK-9),
    /// which may never take over a key in flight.
    ///
    /// `checked_at` is the [`commit_seq`](Self::commit_seq) taken before the
    /// caller's store check: if more commits than are remembered happened
    /// since, one of them may have been this key's, so it's refused (as
    /// [`claim_batch`](Self::claim_batch) does).
    pub fn restore_if_free(
        &self,
        worker: WorkerId,
        key: InfoHash,
        lease: RestoredLease,
        now: i64,
        checked_at: u64,
    ) -> Option<Token> {
        let mut s = self.lock();
        let forgotten = s.commits.saturating_sub(checked_at) > RECENT_COMMITS as u64;
        let committed = forgotten
            || s.recent
                .as_ref()
                .is_some_and(|(seqs, _)| seqs.contains_key(&key));
        if s.table.get(&key).is_some() || s.pending.contains_key(&key) || committed {
            return None;
        }
        let token = s.table.restore(worker, key, lease, now);
        self.gauge(&s);
        Some(token)
    }

    pub fn heartbeat(&self, worker: WorkerId, now: i64) {
        self.lock().table.heartbeat(worker, now);
    }

    /// The key's lease, if `token` is still current.
    pub fn current(&self, key: &InfoHash, token: Token) -> Option<Lease> {
        self.lock()
            .table
            .get(key)
            .filter(|l| l.token == token)
            .copied()
    }

    pub fn get(&self, key: &InfoHash) -> Option<Lease> {
        self.lock().table.get(key).copied()
    }

    /// Ends a lease whose key was committed, by token alone. `None` if
    /// `token` was stale. Tokens are predictable (`epoch << 32 | seq`), so
    /// this is for the coordinator's own commit path; a commit on a remote
    /// worker's behalf goes through [`commit`](Self::commit), which checks
    /// the holder too.
    pub fn release(&self, key: &InfoHash, token: Token) -> Option<Lease> {
        let mut s = self.lock();
        let lease = s.table.release(key, token);
        if lease.is_some() {
            s.remember_commit(*key);
        }
        self.gauge(&s);
        lease
    }

    /// Ends a lease whose key was committed, only if `worker` holds it under
    /// `token`.
    pub fn commit(&self, worker: WorkerId, key: &InfoHash, token: Token) -> Option<Lease> {
        let mut s = self.lock();
        let holds = s
            .table
            .get(key)
            .is_some_and(|l| l.holder == worker && l.token == token);
        if !holds {
            return None;
        }
        let lease = s.table.release(key, token);
        if lease.is_some() {
            s.remember_commit(*key);
        }
        self.gauge(&s);
        lease
    }

    /// Whether `worker` holds `key` under `token` now: the lease, if so.
    pub fn held(&self, worker: WorkerId, key: &InfoHash, token: Token) -> Option<Lease> {
        self.lock()
            .table
            .get(key)
            .filter(|l| l.holder == worker && l.token == token)
            .copied()
    }

    /// The holder lost the key: ends the lease and reports the loss once.
    /// `None` (nothing counted) if `token` was stale.
    pub fn lost(&self, key: &InfoHash, token: Token, reason: u8) -> Option<Lease> {
        let lease = {
            let mut s = self.lock();
            let lease = s.table.release(key, token);
            self.gauge(&s);
            lease?
        };
        self.events.lost(key, &lease, reason);
        Some(lease)
    }

    /// A terminal outcome reported by `worker`: it ends the lease only if
    /// `worker` holds it under `token` (anything else is stale, `None`).
    /// With `loss`, the loss is reported once.
    pub fn report(
        &self,
        worker: WorkerId,
        key: &InfoHash,
        token: Token,
        loss: Option<u8>,
    ) -> Option<Lease> {
        let mut s = self.lock();
        let holds = s
            .table
            .get(key)
            .is_some_and(|l| l.holder == worker && l.token == token);
        if !holds {
            return None;
        }
        let lease = s.table.release(key, token);
        self.gauge(&s);
        drop(s);
        if let (Some(lease), Some(reason)) = (&lease, loss) {
            self.events.lost(key, lease, reason);
        }
        lease
    }

    /// Ends the leases of workers silent past the TTL; their keys wait for
    /// a re-grant. Returns how many. The built-in worker never expires.
    pub fn expire(&self, now: i64) -> usize {
        let mut s = self.lock();
        s.table.heartbeat(WorkerId::LOCAL, now);
        let expired = s.table.expire(now);
        let n = expired.len();
        for (key, lease) in expired {
            s.park(key, lease);
        }
        self.gauge(&s);
        drop(s);
        if n > 0 {
            self.events.expired(n);
        }
        n
    }

    /// Ends every lease `worker` holds (revoked or drained); the keys wait
    /// for a re-grant and its late reports are stale. Returns how many.
    pub fn evict(&self, worker: WorkerId) -> usize {
        let mut s = self.lock();
        let evicted = s.table.evict(worker);
        let n = evicted.len();
        for (key, lease) in evicted {
            s.park(key, lease);
        }
        self.gauge(&s);
        n
    }

    /// Grants waiting keys, in waiting order (oldest first, except keys
    /// whose check couldn't answer, which wait at the back), to `targets`,
    /// each time to the one
    /// holding the fewest leases, at most `per_target` keys each. Keys left
    /// over keep waiting. Not a harvest.
    pub fn regrant_pending(
        &self,
        targets: &[WorkerId],
        per_target: usize,
        now: i64,
    ) -> Vec<(WorkerId, Regranted)> {
        let mut s = self.lock();
        if s.pending.is_empty() || targets.is_empty() || per_target == 0 {
            return Vec::new();
        }
        let mut load: Vec<(usize, WorkerId, Regranted)> = targets
            .iter()
            .map(|&w| (s.table.held_by(w), w, Vec::new()))
            .collect();
        while let Some(key) = s.order.pop_front() {
            let Some(target) = load
                .iter_mut()
                .filter(|(_, _, got)| got.len() < per_target)
                .min_by_key(|(held, w, _)| (*held, *w))
            else {
                s.order.push_front(key);
                break;
            };
            let Some(lease) = s.pending.remove(&key) else {
                continue;
            };
            if s.pending.is_empty() {
                s.order.clear();
            }
            let token = s.table.regrant(target.1, key, lease, now);
            target.0 += 1;
            target.2.push((key, token));
        }
        self.gauge(&s);
        load.into_iter()
            .filter(|(_, _, got)| !got.is_empty())
            .map(|(_, w, got)| (w, got))
            .collect()
    }

    /// Takes back a re-grant that couldn't be delivered: the key waits
    /// again, at the front. `false` if `token` was no longer current.
    pub fn requeue(&self, key: &InfoHash, token: Token) -> bool {
        self.take_back(key, token, true)
    }

    /// [`requeue`](Self::requeue), but the key waits at the back: one whose
    /// store check couldn't answer goes behind the rest, so a key whose
    /// read keeps failing doesn't stand first in every re-grant (#615's
    /// review).
    pub fn requeue_back(&self, key: &InfoHash, token: Token) -> bool {
        self.take_back(key, token, false)
    }

    fn take_back(&self, key: &InfoHash, token: Token, front: bool) -> bool {
        let mut s = self.lock();
        let Some(lease) = s.table.release(key, token) else {
            return false;
        };
        if s.pending.insert(*key, lease).is_none() {
            if front {
                s.order.push_front(*key);
            } else {
                s.order.push_back(*key);
            }
        }
        self.gauge(&s);
        true
    }

    /// Whether `key` is in flight: leased to any worker, or waiting for a
    /// re-grant (late results, plan Q7).
    pub fn in_flight(&self, key: &InfoHash) -> bool {
        let s = self.lock();
        s.table.get(key).is_some() || s.pending.contains_key(key)
    }

    #[cfg(test)]
    fn committed_recently_for_test(&self, key: &InfoHash) -> bool {
        self.lock()
            .recent
            .as_ref()
            .is_some_and(|(seqs, _)| seqs.contains_key(key))
    }

    /// Every lease a remote worker holds and every key waiting for a
    /// re-grant, for the coordinator's fetch queue (§19.3: outstanding
    /// leases survive restarts), read under one lock into an owned list.
    /// The built-in worker's leases are in its pool's own snapshot.
    pub fn outstanding_remote(&self) -> Vec<(InfoHash, RestoredLease)> {
        let s = self.lock();
        s.table
            .leases
            .iter()
            .filter(|(_, l)| l.holder != WorkerId::LOCAL)
            .chain(s.pending.iter())
            .map(|(k, l)| (*k, RestoredLease::from(*l)))
            .collect()
    }

    /// Leases held now.
    pub fn len(&self) -> usize {
        self.lock().table.len()
    }

    pub fn is_empty(&self) -> bool {
        self.lock().in_flight() == 0
    }

    /// Keys waiting for a re-grant.
    pub fn pending(&self) -> usize {
        self.lock().pending.len()
    }

    pub fn held_by(&self, worker: WorkerId) -> usize {
        self.lock().table.held_by(worker)
    }

    /// Leases held by each of `workers`, under one lock.
    pub fn held_by_each(&self, workers: &[WorkerId]) -> Vec<usize> {
        let s = self.lock();
        workers.iter().map(|&w| s.table.held_by(w)).collect()
    }
}

/// What a lease keeps across a restart or a re-grant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RestoredLease {
    pub cohort: i64,
    pub source: SourceId,
    pub sources: u64,
    pub strategy: u8,
    pub discovered_at: i64,
}

impl From<Lease> for RestoredLease {
    fn from(l: Lease) -> RestoredLease {
        RestoredLease {
            cohort: l.cohort,
            source: l.source,
            sources: l.sources,
            strategy: l.strategy,
            discovered_at: l.discovered_at,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: WorkerId = WorkerId(1);
    const B: WorkerId = WorkerId(2);

    fn src(n: u8) -> SourceId {
        SourceId::new(n).unwrap()
    }

    fn claim(n: u8, source: u8) -> Claim {
        Claim {
            key: InfoHash([n; 20]),
            source: src(source),
            strategy: 1,
            at: 7_200 + 5,
        }
    }

    #[test]
    fn first_claim_wins_and_later_ones_merge_their_source() {
        let mut t = LeaseTable::new(1, 600);
        let Claimed::Granted(token) = t.claim(A, claim(1, 0), 0) else {
            panic!("granted")
        };
        assert_eq!(
            t.claim(B, claim(1, 3), 1),
            Claimed::Held { holder: A, token }
        );
        let lease = t.get(&InfoHash([1; 20])).unwrap();
        assert_eq!((lease.cohort, lease.sources), (2, 0b1001));
        assert_eq!(
            t.release(&InfoHash([1; 20]), token).unwrap().sources,
            0b1001
        );
        assert!(t.is_empty());
    }

    #[test]
    fn stale_tokens_change_nothing() {
        let mut t = LeaseTable::new(1, 600);
        let key = InfoHash([1; 20]);
        let Claimed::Granted(old) = t.claim(A, claim(1, 0), 0) else {
            panic!()
        };
        assert!(t.release(&key, old).is_some());
        assert!(t.release(&key, old).is_none(), "released twice");
        let Claimed::Granted(new) = t.claim(A, claim(1, 0), 0) else {
            panic!()
        };
        assert_ne!(old, new);
        assert!(t.release(&key, old).is_none(), "an old token");
        assert!(t.is_current(&key, new));
    }

    #[test]
    fn silent_workers_lose_their_leases_to_regrant() {
        let mut t = LeaseTable::new(1, 600);
        let Claimed::Granted(a_token) = t.claim(A, claim(1, 0), 0) else {
            panic!()
        };
        t.claim(B, claim(2, 0), 0);
        t.heartbeat(B, 500);
        assert!(t.expire(600).is_empty(), "at the TTL, still alive");
        let expired = t.expire(601);
        assert_eq!(expired.len(), 1);
        let (key, lease) = expired[0];
        assert_eq!(lease.holder, A);
        let b_token = t.regrant(B, key, lease, 601);
        // The old holder's late report is ignored; the new one counts.
        assert!(t.release(&key, a_token).is_none());
        assert_eq!(t.get(&key).unwrap().cohort, 2, "cohort kept");
        assert!(t.release(&key, b_token).is_some());
    }

    #[test]
    fn eviction_returns_only_that_workers_leases() {
        let mut t = LeaseTable::new(1, 600);
        let Claimed::Granted(a_token) = t.claim(A, claim(1, 0), 0) else {
            panic!()
        };
        t.claim(B, claim(2, 0), 0);
        let evicted = t.evict(A);
        assert_eq!(evicted.len(), 1);
        assert_eq!(evicted[0].0, InfoHash([1; 20]));
        assert!(t.release(&InfoHash([1; 20]), a_token).is_none());
        assert_eq!((t.len(), t.held_by(A), t.held_by(B)), (1, 0, 1));
        assert!(t.evict(A).is_empty());
    }

    #[test]
    fn tokens_carry_the_epoch() {
        let mut before = LeaseTable::new(100, 600);
        let mut after = LeaseTable::new(101, 600);
        let Claimed::Granted(a) = before.claim(A, claim(1, 0), 0) else {
            panic!()
        };
        let Claimed::Granted(b) = after.claim(A, claim(1, 0), 0) else {
            panic!()
        };
        assert_ne!(a, b);
        assert_eq!(b.0 >> 32, 101);
    }

    #[derive(Default)]
    struct Counted {
        harvested: std::sync::atomic::AtomicUsize,
        lost: std::sync::atomic::AtomicUsize,
        outstanding: std::sync::atomic::AtomicUsize,
        serving: std::sync::atomic::AtomicUsize,
    }

    impl LeaseEvents for Counted {
        fn serving(&self) {
            self.serving
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        fn harvested(&self, _: &InfoHash, _: &Lease) {
            self.harvested
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        fn lost(&self, _: &InfoHash, _: &Lease, _: u8) {
            self.lost.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        fn outstanding(&self, n: usize) {
            self.outstanding
                .store(n, std::sync::atomic::Ordering::Relaxed);
        }
    }

    fn counted() -> (LeaseAuthority, Arc<Counted>) {
        let events = Arc::new(Counted::default());
        (LeaseAuthority::new(1, 600, events.clone()), events)
    }

    fn get(a: &std::sync::atomic::AtomicUsize) -> usize {
        a.load(std::sync::atomic::Ordering::Relaxed)
    }

    #[test]
    fn a_pending_key_is_taken_over_by_a_claim_without_a_new_harvest() {
        let (a, ev) = counted();
        let key = InfoHash([1; 20]);
        let Claimed::Granted(old) = a.claim(A, claim(1, 0), 0) else {
            panic!()
        };
        assert_eq!(a.evict(A), 1);
        assert_eq!((a.len(), a.pending(), get(&ev.outstanding)), (0, 1, 1));
        // Another worker finds it again: a re-grant, not a second harvest.
        let Claimed::Granted(new) = a.claim(WorkerId::LOCAL, claim(1, 2), 1) else {
            panic!()
        };
        assert_ne!(old, new);
        assert_eq!(get(&ev.harvested), 1);
        let lease = a.get(&key).unwrap();
        assert_eq!(
            (lease.holder, lease.sources, lease.cohort),
            (WorkerId::LOCAL, 0b101, 2)
        );
        assert_eq!(a.pending(), 0);
        assert!(
            a.regrant_pending(&[B], 10, 2).is_empty(),
            "nothing left to re-grant"
        );
        assert!(a.report(A, &key, old, Some(1)).is_none(), "stale");
        assert!(a.lost(&key, new, 1).is_some());
        assert_eq!((get(&ev.lost), get(&ev.outstanding)), (1, 0));
    }

    #[test]
    fn a_fetched_lease_lasts_until_its_commit_or_expiry() {
        let (a, ev) = counted();
        let key = InfoHash([1; 20]);
        let Claimed::Granted(t) = a.claim(A, claim(1, 0), 0) else {
            panic!()
        };
        // `Fetched` only checks the lease; it stays for the commit.
        assert!(a.held(A, &key, t).is_some());
        assert!(a.held(B, &key, t).is_none());
        assert!(a.current(&key, t).is_some());
        // A commit for another worker, with the right token, is refused;
        // the holder's ends it once.
        assert!(a.commit(B, &key, t).is_none());
        assert!(a.commit(A, &key, t).is_some());
        assert!(a.commit(A, &key, t).is_none());

        // Fetched, then silent with no commit: expired and re-issued.
        let Claimed::Granted(t) = a.claim(A, claim(2, 0), 0) else {
            panic!()
        };
        let key = InfoHash([2; 20]);
        assert!(a.held(A, &key, t).is_some());
        assert_eq!(a.expire(601), 1);
        let out = a.regrant_pending(&[B], 10, 601);
        let new = out[0].1[0].1;
        assert_ne!(new, t);
        // A late commit or report under the old token is refused.
        assert!(a.release(&key, t).is_none());
        assert!(a.report(A, &key, t, Some(1)).is_none());
        assert!(a.held(A, &key, t).is_none());
        assert!(a.release(&key, new).is_some());
        assert_eq!((get(&ev.harvested), get(&ev.lost)), (2, 0));
    }

    /// #615's review: a key whose store check couldn't answer goes back
    /// behind the others; one whose delivery failed goes back in front.
    #[test]
    fn an_unknown_key_waits_behind_the_rest() {
        let (a, _) = counted();
        for n in 1..=3 {
            a.claim(A, claim(n, 0), 0);
        }
        a.evict(A);
        let out = a.regrant_pending(&[B], 3, 1);
        let keys = &out[0].1;
        assert_eq!(keys.len(), 3);
        let (first, t1) = keys[0];
        let (second, t2) = keys[1];
        let (third, t3) = keys[2];
        // The first unknown: behind. The second failed delivery: in front.
        assert!(a.requeue_back(&first, t1));
        assert!(a.requeue(&second, t2));
        assert!(a.requeue_back(&third, t3));
        let order: Vec<InfoHash> = (0..3)
            .map(|_| a.regrant_pending(&[B], 1, 2)[0].1[0].0)
            .collect();
        assert_eq!(order, vec![second, first, third]);
        // A stale token takes nothing back.
        assert!(!a.requeue_back(&first, t1));
    }

    #[test]
    fn the_built_in_worker_never_expires() {
        let (a, _) = counted();
        a.claim(WorkerId::LOCAL, claim(1, 0), 0);
        a.claim(A, claim(2, 0), 0);
        assert_eq!(a.expire(10_000), 1);
        assert_eq!(a.get(&InfoHash([1; 20])).unwrap().holder, WorkerId::LOCAL);
        assert_eq!(a.pending(), 1);
    }

    #[test]
    fn regrants_go_to_the_least_loaded_and_can_be_taken_back() {
        let (a, ev) = counted();
        for n in 1..=4 {
            a.claim(A, claim(n, 0), 0);
        }
        a.claim(B, claim(9, 0), 0);
        a.evict(A);
        let c = WorkerId(3);
        let out = a.regrant_pending(&[B, c], 3, 1);
        let got: HashMap<WorkerId, usize> = out.iter().map(|(w, k)| (*w, k.len())).collect();
        // c starts empty, so it takes the first two; then they're level and
        // the lower id goes first, up to the cap.
        assert_eq!(got[&c], 2);
        assert_eq!(got[&B], 2);
        assert_eq!(a.pending(), 0);
        let (w, keys) = &out[0];
        let (key, token) = keys[0];
        assert!(a.requeue(&key, token));
        assert!(!a.requeue(&key, token), "already taken back");
        assert_eq!(a.held_by(*w), keys.len() - 1 + usize::from(*w == B));
        assert_eq!(a.pending(), 1);
        assert_eq!(get(&ev.harvested), 5);
        assert_eq!(get(&ev.outstanding), 5);
    }

    #[test]
    fn reports_count_only_from_the_holder() {
        let (a, ev) = counted();
        let key = InfoHash([1; 20]);
        let Claimed::Granted(t) = a.claim(A, claim(1, 0), 0) else {
            panic!()
        };
        assert!(a.report(B, &key, t, Some(1)).is_none());
        assert!(a.report(A, &key, Token(t.0 + 1), Some(1)).is_none());
        assert!(a.report(A, &key, t, None).is_some());
        assert_eq!(get(&ev.lost), 0, "fetched isn't a loss");
        let one = |n: usize| move |_: usize| n;
        let batch = a.claim_batch(B, &[claim(1, 0), claim(1, 1), claim(2, 0)], 1, None, None);
        let granted = |b: &BatchAnswer| {
            matches!(
                b,
                BatchAnswer::Claimed {
                    claimed: Claimed::Granted(_),
                    harvested: true
                }
            )
        };
        assert!(granted(&batch[0]));
        assert!(matches!(
            batch[1],
            BatchAnswer::Claimed {
                claimed: Claimed::Held { holder: B, .. },
                harvested: false
            }
        ));
        assert!(granted(&batch[2]));
        assert_eq!(get(&ev.harvested), 3);
        // Admission counts only keys that would get a new lease: the held
        // keys still merge their source.
        let asked = std::sync::atomic::AtomicUsize::new(0);
        let admit = |n: usize| {
            asked.store(n, std::sync::atomic::Ordering::Relaxed);
            1
        };
        let batch = a.claim_batch(
            B,
            &[claim(1, 3), claim(5, 0), claim(6, 0)],
            2,
            Some(&admit),
            None,
        );
        assert_eq!(get(&asked), 2);
        assert!(matches!(
            batch[0],
            BatchAnswer::Claimed {
                claimed: Claimed::Held { .. },
                ..
            }
        ));
        assert!(granted(&batch[1]));
        assert_eq!(batch[2], BatchAnswer::Busy);
        assert_eq!(a.get(&InfoHash([1; 20])).unwrap().sources, 0b1011);
        assert!(a.get(&InfoHash([6; 20])).is_none());
        let none = one(0);
        assert_eq!(
            a.claim_batch(B, &[claim(7, 0)], 2, Some(&none), None),
            vec![BatchAnswer::Busy]
        );
    }

    #[test]
    fn a_remote_claim_racing_a_commit_is_answered_stored() {
        let (a, ev) = counted();
        let key = InfoHash([1; 20]);
        let Claimed::Granted(t) = a.claim(WorkerId::LOCAL, claim(1, 0), 0) else {
            panic!()
        };
        // Before remote workers are allowed nothing is remembered.
        assert!(a.release(&key, t).is_some());
        assert!(matches!(
            a.claim_batch(A, &[claim(1, 0)], 1, None, None)[0],
            BatchAnswer::Claimed {
                harvested: true,
                ..
            }
        ));
        let Some(l) = a.get(&key) else { panic!() };
        assert!(a.release(&key, l.token).is_some());
        a.serve_remote();
        let Claimed::Granted(t) = a.claim(WorkerId::LOCAL, claim(1, 0), 2) else {
            panic!()
        };
        assert!(a.release(&key, t).is_some());
        assert_eq!(
            a.claim_batch(A, &[claim(1, 0)], 3, None, Some(a.commit_seq())),
            vec![BatchAnswer::Committed]
        );
        // The built-in worker's own path is untouched (its have-filter
        // already knows the key).
        assert!(matches!(
            a.claim(WorkerId::LOCAL, claim(1, 0), 3),
            Claimed::Granted(_)
        ));
        assert_eq!(get(&ev.harvested), 4);
        assert!(a.held(WorkerId::LOCAL, &key, Token(0)).is_none());
        assert_eq!(a.ttl_secs(), 600);

        // More commits since the store check than are remembered: no new
        // lease, whatever the key, until the worker claims again.
        let seq = a.commit_seq();
        for n in 0..=RECENT_COMMITS {
            let c = Claim {
                key: InfoHash({
                    let mut k = [9; 20];
                    k[..8].copy_from_slice(&(n as u64).to_le_bytes());
                    k
                }),
                ..claim(9, 0)
            };
            let Claimed::Granted(t) = a.claim(WorkerId::LOCAL, c, 4) else {
                panic!()
            };
            assert!(a.release(&c.key, t).is_some());
        }
        assert_eq!(
            a.claim_batch(A, &[claim(30, 0)], 5, None, Some(seq)),
            vec![BatchAnswer::Busy]
        );
        assert!(matches!(
            a.claim_batch(A, &[claim(30, 0)], 5, None, Some(a.commit_seq()))[0],
            BatchAnswer::Claimed {
                harvested: true,
                ..
            }
        ));
    }

    /// A key committed again takes a new slot in the recent commits: the
    /// entry of its first commit, evicted later, never evicts it (WK-9,
    /// from WK-0's review). Before, a re-commit kept only its first slot,
    /// so a claim racing the second commit could be granted and harvested
    /// again once the first slot rolled over.
    #[test]
    fn a_re_commit_keeps_its_place_in_the_recent_commits() {
        let (a, _) = counted();
        a.serve_remote();
        let key = |n: u64| {
            let mut k = [7; 20];
            k[..8].copy_from_slice(&n.to_le_bytes());
            InfoHash(k)
        };
        let commit = |k: InfoHash| {
            let c = Claim {
                key: k,
                ..claim(1, 0)
            };
            let Claimed::Granted(t) = a.claim(WorkerId::LOCAL, c, 1) else {
                panic!()
            };
            assert!(a.release(&k, t).is_some());
        };
        let first = InfoHash([1; 20]);
        commit(first);
        for n in 0..(RECENT_COMMITS as u64 - 1) {
            commit(key(n));
        }
        // Committed again (a late result, say), then one more commit pushes
        // its first slot out.
        commit(first);
        let seq = a.commit_seq();
        commit(key(u64::MAX));
        assert_eq!(
            a.claim_batch(A, &[claim(1, 0)], 2, None, Some(seq)),
            vec![BatchAnswer::Committed],
            "remembered by its second commit"
        );
        assert!(!a.in_flight(&first));
    }

    /// A late result's lease is taken only for a key that is neither held
    /// nor waiting, in one step: never over a waiting key's lease, and only
    /// once however often it's asked.
    #[test]
    fn a_restore_if_free_never_takes_a_key_in_flight() {
        let (a, ev) = counted();
        let lease = RestoredLease {
            cohort: 2,
            source: src(1),
            sources: 2,
            strategy: 0,
            discovered_at: 7_200,
        };
        let key = InfoHash([3; 20]);
        let Claimed::Granted(_) = a.claim(B, claim(3, 1), 0) else {
            panic!()
        };
        assert_eq!(
            a.restore_if_free(A, key, lease, 1, a.commit_seq()),
            None,
            "held"
        );
        assert_eq!(a.evict(B), 1);
        assert_eq!(
            a.restore_if_free(A, key, lease, 1, a.commit_seq()),
            None,
            "waiting"
        );
        assert_eq!(a.pending(), 1, "the waiting lease is untouched");
        let free = InfoHash([4; 20]);
        let t = a
            .restore_if_free(A, free, lease, 1, a.commit_seq())
            .unwrap();
        assert_eq!(
            a.restore_if_free(A, free, lease, 1, a.commit_seq()),
            None,
            "only once"
        );
        assert_eq!(a.held(A, &free, t).unwrap().cohort, 2);
        assert_eq!(get(&ev.harvested), 1, "a restore is never a harvest");
    }

    /// The interleaving the late-result path must survive (WK-9), injected
    /// step by step: the intake finds the key free, then the server's tick
    /// expires another worker's lease on it (it waits for a re-grant, still
    /// in flight, harvested once and with no outcome yet), then the intake
    /// leases it. The old check-then-`restore` takes the waiting key over,
    /// so the waiting lease vanishes without an outcome and the late
    /// result's correction is counted against a key that never ended;
    /// `restore_if_free` refuses, and the waiting key keeps its lease.
    #[test]
    fn a_late_lease_never_takes_over_a_key_expired_in_between() {
        let lease = RestoredLease {
            cohort: 2,
            source: src(1),
            sources: 2,
            strategy: 0,
            discovered_at: 7_200,
        };
        let key = InfoHash([5; 20]);
        let run = |atomic: bool| {
            let (a, _) = counted();
            // 1. The intake's check: free. 2. Another worker claims it.
            // 3. That worker goes silent and its lease expires: the key
            // waits. 4. The intake leases the key it found free.
            let free_at_check = !a.in_flight(&key);
            let Claimed::Granted(_) = a.claim(B, claim(5, 1), 0) else {
                panic!()
            };
            assert_eq!(a.expire(10_000), 1, "B went silent: the key waits");
            assert!(free_at_check);
            let leased = if atomic {
                a.restore_if_free(A, key, lease, 10_000, 0).is_some()
            } else {
                a.restore(A, key, lease, 10_000);
                true
            };
            (leased, a.pending())
        };
        // The old path: leased, and the waiting lease is gone (no outcome).
        assert_eq!(run(false), (true, 0));
        // The atomic path: refused, and the key still waits for a re-grant.
        assert_eq!(run(true), (false, 1));
    }

    /// A key granted, harvested and committed between the late-result
    /// intake's store check and its lease (WK-9, from the security review):
    /// injected step by step. The commit is remembered, so the lease is
    /// refused and nothing is restored or corrected.
    #[test]
    fn a_late_lease_never_follows_a_commit_since_the_check() {
        let (a, ev) = counted();
        a.serve_remote();
        let lease = RestoredLease {
            cohort: 2,
            source: src(1),
            sources: 2,
            strategy: 0,
            discovered_at: 7_200,
        };
        let key = InfoHash([6; 20]);
        // 1. The intake's checks: not stored, not in flight.
        assert!(!a.in_flight(&key));
        // 2. Another worker is granted it (a harvest) and commits it.
        let Claimed::Granted(t) = a.claim(B, claim(6, 1), 0) else {
            panic!()
        };
        assert!(a.commit(B, &key, t).is_some());
        // 3. The intake's lease: refused.
        assert_eq!(a.restore_if_free(A, key, lease, 1, a.commit_seq()), None);
        assert!(!a.in_flight(&key));
        assert_eq!(get(&ev.harvested), 1);
    }

    /// More commits than `RECENT_COMMITS` remembers between the late-result
    /// check and its lease: the key's own commit may have been forgotten,
    /// so the lease is refused, whatever the key (as a remote claim's is).
    #[test]
    fn a_late_lease_is_refused_when_commits_since_were_forgotten() {
        let (a, _) = counted();
        a.serve_remote();
        let lease = RestoredLease {
            cohort: 2,
            source: src(1),
            sources: 2,
            strategy: 0,
            discovered_at: 7_200,
        };
        let key = InfoHash([8; 20]);
        // 1. The intake takes the sequence, then checks the store: free.
        let checked_at = a.commit_seq();
        // 2. The key is granted and committed, then more commits than are
        // remembered push it out.
        let Claimed::Granted(t) = a.claim(B, claim(8, 1), 0) else {
            panic!()
        };
        assert!(a.commit(B, &key, t).is_some());
        for n in 0..RECENT_COMMITS as u64 {
            let mut k = [9; 20];
            k[..8].copy_from_slice(&n.to_le_bytes());
            let c = Claim {
                key: InfoHash(k),
                ..claim(9, 1)
            };
            let Claimed::Granted(t) = a.claim(WorkerId::LOCAL, c, 1) else {
                panic!()
            };
            assert!(a.release(&c.key, t).is_some());
        }
        assert!(!a.committed_recently_for_test(&key), "forgotten");
        // 3. The intake's lease: refused, though the key looks free.
        assert_eq!(a.restore_if_free(A, key, lease, 2, checked_at), None);
        // Taken after the commits, the sequence allows it.
        assert!(
            a.restore_if_free(A, key, lease, 2, a.commit_seq())
                .is_some()
        );
    }

    /// The fetch queue gets remote-held and waiting leases, never the
    /// built-in worker's (they're in its pool's snapshot), with their
    /// cohort and sources kept (WK-9b).
    #[test]
    fn remote_and_waiting_leases_are_listed_for_the_fetch_queue() {
        let (a, _) = counted();
        let Claimed::Granted(_) = a.claim(WorkerId::LOCAL, claim(1, 1), 0) else {
            panic!()
        };
        let Claimed::Granted(_) = a.claim(A, claim(2, 2), 0) else {
            panic!()
        };
        let Claimed::Granted(_) = a.claim(B, claim(3, 3), 0) else {
            panic!()
        };
        assert_eq!(a.evict(B), 1);
        let mut out: Vec<u8> = a.outstanding_remote().iter().map(|(k, _)| k.0[0]).collect();
        out.sort_unstable();
        assert_eq!(out, [2, 3], "held by A, and waiting; not the local one");
        let (_, l) = a
            .outstanding_remote()
            .into_iter()
            .find(|(k, _)| k.0[0] == 3)
            .unwrap();
        assert_eq!((l.sources, l.cohort), (1 << 3, (7_200 + 5) / 3600));
    }

    /// WK-15: a lease keeps its finder only while no other worker (the
    /// built-in one included) claims the key, held or waiting for a
    /// re-grant; the finder's own claims, and a re-grant without a claim,
    /// keep it. A restored lease has none. Serving remote workers is
    /// reported once.
    #[test]
    fn a_lease_keeps_its_finder_until_another_worker_claims_it() {
        let (a, ev) = counted();
        let finder = |n: u8| a.get(&InfoHash([n; 20])).unwrap().finder;
        let Claimed::Granted(t) = a.claim(A, claim(1, 0), 0) else {
            panic!()
        };
        for n in 2..=4 {
            let Claimed::Granted(_) = a.claim(A, claim(n, 0), 0) else {
                panic!()
            };
        }
        // The finder again, alone and in a batch: kept.
        assert_eq!(
            a.claim(A, claim(1, 0), 0),
            Claimed::Held {
                holder: A,
                token: t
            }
        );
        a.claim_batch(A, &[claim(1, 0)], 0, None, None);
        assert_eq!(finder(1), Some(A));
        // The built-in worker on key 1, B in a batch on key 2.
        a.claim(WorkerId::LOCAL, claim(1, 2), 0);
        a.claim_batch(B, &[claim(2, 3)], 0, None, None);
        assert_eq!((finder(1), finder(2), finder(3)), (None, None, Some(A)));
        // Waiting for a re-grant: key 4 claimed by B loses it; key 3,
        // re-granted to B without a claim, keeps A.
        assert_eq!(a.evict(A), 4);
        let Claimed::Granted(_) = a.claim(B, claim(4, 3), 1) else {
            panic!()
        };
        assert_eq!(finder(4), None);
        a.regrant_pending(&[B], 10, 1);
        let three = a.get(&InfoHash([3; 20])).unwrap();
        assert_eq!((three.holder, three.finder), (B, Some(A)));
        // Restored from an earlier run: no finder.
        a.restore(B, InfoHash([9; 20]), RestoredLease::from(three), 1);
        assert_eq!(finder(9), None);
        // Nothing was harvested twice; serving is reported once.
        assert_eq!(get(&ev.harvested), 4);
        a.serve_remote();
        a.serve_remote();
        assert_eq!(get(&ev.serving), 1);
    }
}
