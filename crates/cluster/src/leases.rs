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
//!   token;
//! - the holder goes silent for longer than the TTL, and the lease expires
//!   (to be granted again elsewhere with its cohort and sources kept); or
//! - the holder reported its result `Fetched` (its batch was accepted) and
//!   no commit came within a TTL of that, holder alive or not: it expires
//!   the same way (X-wk-fetched-lease).
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

use std::collections::{HashMap, HashSet, VecDeque};
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
    /// Leases whose results the holder's batch delivered (`Fetched`, sent
    /// on the coordinator's `Accepted`): token → key, and when it was
    /// marked (X-wk-fetched-lease). Only the commit, a loss, an eviction or
    /// the TTL after the mark ends one; meanwhile the holder's own claims
    /// are answered as committed, never with the lease.
    fetched: HashMap<Token, (InfoHash, i64)>,
    /// The session each lease was last handed to its holder on (WK-11b):
    /// a grant, a re-grant, or the holder's own claim answered with it.
    /// [`PINNED`] for a late result's lease, which the coordinator holds
    /// for a result already in hand. Ends with the lease.
    delivered: HashMap<Token, u64>,
    /// The keys each worker holds, kept with `leases` on every insert and
    /// removal: a count is one lookup (the per-worker cap, CLUSTER-02), and
    /// an eviction, expiry or reconcile visits only that worker's leases,
    /// not the whole table (X-worker-839-followups). A worker with none has
    /// no entry.
    by_worker: HashMap<WorkerId, HashSet<InfoHash>>,
}

/// [`LeaseTable`]'s delivery mark for a lease the coordinator holds on a
/// worker's behalf for a result already in hand (a late result, plan Q7):
/// never returned by a reconcile.
const PINNED: u64 = u64::MAX;

impl LeaseTable {
    pub fn new(epoch: u32, ttl_secs: i64) -> LeaseTable {
        LeaseTable {
            epoch,
            seq: 0,
            ttl: ttl_secs,
            leases: HashMap::new(),
            workers: HashMap::new(),
            fetched: HashMap::new(),
            delivered: HashMap::new(),
            by_worker: HashMap::new(),
        }
    }

    /// Adds a lease; `key` must have none.
    fn put(&mut self, key: InfoHash, lease: Lease) {
        self.by_worker.entry(lease.holder).or_default().insert(key);
        let old = self.leases.insert(key, lease);
        debug_assert!(old.is_none(), "a key leased twice");
    }

    /// Removes `key`'s lease, if any, keeping the counts.
    fn take(&mut self, key: &InfoHash) -> Option<Lease> {
        let lease = self.leases.remove(key)?;
        if let Some(keys) = self.by_worker.get_mut(&lease.holder) {
            keys.remove(key);
            if keys.is_empty() {
                self.by_worker.remove(&lease.holder);
            }
        }
        Some(lease)
    }

    /// Forgets the per-token marks of a lease that ended.
    fn ended(&mut self, token: Token) {
        self.fetched.remove(&token);
        self.delivered.remove(&token);
    }

    /// Ends the leases of `keys` (those still leased), with their marks.
    fn end_all(&mut self, keys: Vec<InfoHash>) -> Vec<(InfoHash, Lease)> {
        let mut out = Vec::with_capacity(keys.len());
        for k in keys {
            if let Some(l) = self.take(&k) {
                self.ended(l.token);
                out.push((k, l));
            }
        }
        out
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
        self.put(
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
        self.put(
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
        self.ended(token);
        self.take(key)
    }

    /// The holder's batch carrying `key`'s result was accepted: marked
    /// fetched, from the first mark (a repeat keeps it). `None`, nothing
    /// marked, unless `worker` holds the key under `token`.
    pub fn mark_fetched(
        &mut self,
        worker: WorkerId,
        key: &InfoHash,
        token: Token,
        now: i64,
    ) -> Option<Lease> {
        let lease = *self
            .leases
            .get(key)
            .filter(|l| l.holder == worker && l.token == token)?;
        self.fetched.entry(token).or_insert((*key, now));
        Some(lease)
    }

    /// Whether `worker` holds `key` under a lease marked fetched.
    pub fn is_fetched(&self, worker: WorkerId, key: &InfoHash) -> bool {
        self.leases
            .get(key)
            .is_some_and(|l| l.holder == worker && self.fetched.contains_key(&l.token))
    }

    /// Ends every lease held by a worker silent for longer than the TTL,
    /// and forgets the worker, and every lease marked fetched longer than
    /// the TTL ago, its holder alive or not. The caller grants the keys
    /// again (see [`regrant`](Self::regrant)).
    pub fn expire(&mut self, now: i64) -> Vec<(InfoHash, Lease)> {
        let ttl = self.ttl;
        let dead: Vec<WorkerId> = self
            .workers
            .iter()
            .filter(|&(_, &seen)| seen + ttl < now)
            .map(|(&w, _)| w)
            .collect();
        // A fetched lease whose commit hasn't come a TTL after the mark
        // expires too, holder alive or not: its batch never reached the
        // store (X-wk-fetched-lease).
        let stuck: Vec<InfoHash> = self
            .fetched
            .iter()
            .filter(|&(t, &(k, marked))| {
                marked + ttl < now && self.leases.get(&k).is_some_and(|l| l.token == *t)
            })
            .map(|(_, &(k, _))| k)
            .collect();
        if dead.is_empty() && stuck.is_empty() {
            return Vec::new();
        }
        for w in &dead {
            self.workers.remove(w);
        }
        let mut expired: Vec<InfoHash> = dead.iter().flat_map(|&w| self.keys_of(w)).collect();
        expired.extend(stuck.into_iter().filter(|k| {
            self.leases
                .get(k)
                .is_some_and(|l| !dead.contains(&l.holder))
        }));
        self.end_all(expired)
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
        let keys = self.keys_of(worker).collect();
        self.end_all(keys)
    }

    /// Records that `key`'s lease was handed to `worker` on `session`,
    /// if `worker` holds it under `token` and it isn't [`PINNED`].
    pub fn mark_delivered(&mut self, worker: WorkerId, key: &InfoHash, token: Token, session: u64) {
        if self
            .leases
            .get(key)
            .is_some_and(|l| l.holder == worker && l.token == token)
        {
            let mark = self.delivered.entry(token).or_insert(session);
            if *mark != PINNED {
                *mark = session;
            }
        }
    }

    /// Ends the leases `worker` holds that a reconcile on `session` may
    /// return (WK-11b): not in `listed`, not marked fetched, and not
    /// handed to it on `session` itself (one may still be on its way) nor
    /// [`PINNED`]. The worker stays alive; the caller grants the keys
    /// again.
    pub fn reconcile(
        &mut self,
        worker: WorkerId,
        session: u64,
        listed: &HashSet<Token>,
    ) -> Vec<(InfoHash, Lease)> {
        let keys: Vec<InfoHash> = self
            .keys_of(worker)
            .filter_map(|k| self.leases.get(&k).map(|l| (k, l)))
            .filter(|(_, l)| {
                l.holder == worker
                    && !listed.contains(&l.token)
                    && !self.fetched.contains_key(&l.token)
                    && self
                        .delivered
                        .get(&l.token)
                        .is_none_or(|&s| s != session && s != PINNED)
            })
            .map(|(k, _)| k)
            .collect();
        self.end_all(keys)
    }

    pub fn len(&self) -> usize {
        self.leases.len()
    }

    pub fn is_empty(&self) -> bool {
        self.leases.is_empty()
    }

    /// The keys `worker` holds, from the index.
    fn keys_of(&self, worker: WorkerId) -> impl Iterator<Item = InfoHash> + '_ {
        self.by_worker.get(&worker).into_iter().flatten().copied()
    }

    pub fn held_by(&self, worker: WorkerId) -> usize {
        self.by_worker.get(&worker).map_or(0, HashSet::len)
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
/// target. A key reported lost as [`LOSS_BLOCKED`] is remembered the same
/// way ([`LeaseAuthority::report`]).
pub const RECENT_COMMITS: usize = 16_384;

/// `funnel::Loss::Blocked` (`lodestar-sources`, persisted): a floor drop.
/// A worker reports it for a key the hash lists or the floor refused, and
/// a re-grant reports it for a key the store check found held.
pub(crate) const LOSS_BLOCKED: u8 = 7;

/// What [`LeaseAuthority::fetched`] made of a `Fetched` report, decided
/// under one lock.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FetchedReport {
    /// The sender holds the key under that token: the lease is marked.
    Marked(Lease),
    /// The key has no lease, isn't waiting for a re-grant, and was
    /// committed (or lost as blocked) lately: the commit beat the report,
    /// the usual order (`Fetched` goes out on `Accepted`). The recent
    /// commits are keys only, so this doesn't check the sender or token.
    Committed,
    /// Anything else: a lease under another token or holder, a key
    /// waiting for a re-grant, or a key this run doesn't know (or
    /// committed too long ago to remember: [`RECENT_COMMITS`]).
    Stale,
}

/// One answer of [`LeaseAuthority::claim_batch`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BatchAnswer {
    /// `harvested`: a new key, counted once.
    Claimed { claimed: Claimed, harvested: bool },
    /// A new key over what the admission allowed: not leased.
    Busy,
    /// A new key over the claimer's own lease cap
    /// ([`LeaseAuthority::set_lease_cap`], CLUSTER-02): not leased. The
    /// worker claims again once it holds fewer.
    Capped,
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
    /// The most leases one remote worker may hold (CLUSTER-02): `caps`'
    /// entry for it, else `cap`. `None` until the cluster server sets one:
    /// no cap. The built-in worker is never capped.
    cap: Option<usize>,
    caps: HashMap<WorkerId, usize>,
}

impl AuthorityState {
    fn in_flight(&self) -> usize {
        self.table.len() + self.pending.len()
    }

    /// The worker's lease cap; `None`: uncapped.
    fn cap_of(&self, worker: WorkerId) -> Option<usize> {
        if worker == WorkerId::LOCAL {
            return None;
        }
        self.caps.get(&worker).copied().or(self.cap)
    }

    /// New leases `worker` may still be given under its cap.
    fn room(&self, worker: WorkerId) -> usize {
        self.cap_of(worker).map_or(usize::MAX, |cap| {
            cap.saturating_sub(self.table.held_by(worker))
        })
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
/// process and can't go silent, and never reports `Fetched` (its results
/// go straight to the writer), so its leases never expire; they end only
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
                cap: None,
                caps: HashMap::new(),
            }),
            events,
        }
    }

    /// Sets the most leases one remote worker may hold, for workers
    /// without a cap of their own (CLUSTER-02). Claims for new keys past
    /// it are [`BatchAnswer::Capped`], and re-grants pass it by; leases
    /// already held are kept. The built-in worker is never capped.
    pub fn set_lease_cap(&self, cap: Option<usize>) {
        self.lock().cap = cap;
    }

    /// Sets `worker`'s own lease cap (from its assignment); `None` goes
    /// back to the default ([`set_lease_cap`](Self::set_lease_cap)).
    pub fn set_worker_cap(&self, worker: WorkerId, cap: Option<usize>) {
        let mut s = self.lock();
        match cap {
            Some(cap) => s.caps.insert(worker, cap),
            None => s.caps.remove(&worker),
        };
    }

    /// `worker`'s lease cap now; `None`: uncapped.
    pub fn lease_cap(&self, worker: WorkerId) -> Option<usize> {
        self.lock().cap_of(worker)
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
                // Keys past the worker's cap aren't asked about.
                let new = claims
                    .iter()
                    .filter(|c| s.would_lease(&c.key) && !s.committed_recently(worker, &c.key))
                    .count()
                    .min(s.room(worker));
                if new == 0 { 0 } else { admit(new).min(new) }
            }
            None => usize::MAX,
        };
        // The worker's own cap (CLUSTER-02), on top of the admission.
        let mut room = s.room(worker);
        let mut fresh_leases = Vec::new();
        let answers: Vec<BatchAnswer> = claims
            .iter()
            .map(|c| {
                if s.committed_recently(worker, &c.key) {
                    return BatchAnswer::Committed;
                }
                // The holder's own claim on a lease whose result it already
                // delivered: answered as committed, never with the lease,
                // however long the commit takes (X-wk-fetched-lease). The
                // claim's source still joins the lease.
                if s.table.is_fetched(worker, &c.key) {
                    let _ = s.claim(worker, *c, now);
                    return BatchAnswer::Committed;
                }
                if s.would_lease(&c.key) {
                    // Forgotten commits deny everything as busy, cap or not.
                    if room == 0 && !forgotten {
                        return BatchAnswer::Capped;
                    }
                    if allowed == 0 {
                        return BatchAnswer::Busy;
                    }
                    allowed -= 1;
                    room -= 1;
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
        // Held for a result already in hand: a reconcile never returns it
        // (WK-11b).
        s.table.delivered.insert(token, PINNED);
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

    /// The holder's batch carrying `key`'s result was accepted
    /// (`Outcome::Fetched`): the lease stays for the commit, marked so the
    /// holder's own claims no longer get it back, and expires a TTL after
    /// the mark if the commit never comes: [`FetchedReport::Marked`]. Only
    /// if `worker` holds the key under `token`; otherwise nothing is
    /// marked, and the report is [`FetchedReport::Committed`] if the key
    /// was committed lately (with no lease or re-grant since), else
    /// [`FetchedReport::Stale`]. One lock for the mark and the answer, so a
    /// commit can't land in between.
    pub fn fetched(
        &self,
        worker: WorkerId,
        key: &InfoHash,
        token: Token,
        now: i64,
    ) -> FetchedReport {
        let mut s = self.lock();
        if let Some(lease) = s.table.mark_fetched(worker, key, token, now) {
            return FetchedReport::Marked(lease);
        }
        let committed = s.table.get(key).is_none()
            && !s.pending.contains_key(key)
            && s.recent
                .as_ref()
                .is_some_and(|(seqs, _)| seqs.contains_key(key));
        if committed {
            FetchedReport::Committed
        } else {
            FetchedReport::Stale
        }
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
    ///
    /// A [`LOSS_BLOCKED`] loss is remembered with the recent commits, under
    /// the same lock that ends the lease (X-lease-report-atomic, closing
    /// WK-10d's window). From the moment this returns, a remote claim of
    /// the key is refused as stored, and so is a late result's lease
    /// ([`restore_if_free`](Self::restore_if_free)). That holds for a claim
    /// already in flight, whose store check ran before the report: its
    /// `checked_at` is older, so the report counts as a commit since. It
    /// stays refused for about [`RECENT_COMMITS`] later commits; by then the
    /// sink's floor-report map, which the server fills right after this,
    /// and then the store hold it. A report the sink doesn't keep (over the
    /// worker's budget, or turned away at the map's cap) is refused only
    /// that long, then can be claimed again, as before this change. Any
    /// outcome carrying the reason counts, `PeerFound` included; the sink
    /// keeps only `Lost`, so a `PeerFound` with it is refused only that
    /// long, which is harmless.
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
        if lease.is_some() && loss == Some(LOSS_BLOCKED) {
            s.remember_commit(*key);
        }
        self.gauge(&s);
        drop(s);
        if let (Some(lease), Some(reason)) = (&lease, loss) {
            self.events.lost(key, lease, reason);
        }
        lease
    }

    /// Ends the leases of workers silent past the TTL, and leases marked
    /// fetched longer than the TTL ago; their keys wait for a re-grant.
    /// Returns how many. The built-in worker never expires.
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
    /// holding the fewest leases, at most `per_target` keys each and never
    /// past a target's lease cap (CLUSTER-02). Keys left over keep
    /// waiting. Not a harvest.
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
        // Each target's leases and how many more it may take: at most
        // `per_target` now, and never past its cap (CLUSTER-02).
        let mut load: Vec<(usize, WorkerId, Regranted, usize)> = targets
            .iter()
            .map(|&w| (s.table.held_by(w), w, Vec::new(), s.room(w).min(per_target)))
            .collect();
        while let Some(key) = s.order.pop_front() {
            let Some(target) = load
                .iter_mut()
                .filter(|(_, _, got, most)| got.len() < *most)
                .min_by_key(|(held, w, _, _)| (*held, *w))
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
            .filter(|(_, _, got, _)| !got.is_empty())
            .map(|(_, w, got, _)| (w, got))
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

    /// Records that these leases were handed to `worker` on its session
    /// `session` (a grant, a re-grant, or its own claim answered with the
    /// lease), under one lock. Leases `worker` doesn't hold under that
    /// token are left alone. A reconcile on `session` never returns them
    /// (WK-11b): the worker may not have seen them yet.
    pub fn delivered(&self, worker: WorkerId, session: u64, leases: &[(InfoHash, Token)]) {
        if leases.is_empty() {
            return;
        }
        let mut s = self.lock();
        for (key, token) in leases {
            s.table.mark_delivered(worker, key, *token, session);
        }
    }

    /// Of `leases`, the tokens `worker` holds under their key now, under
    /// one lock (a reconcile's list, WK-11b).
    pub fn held_tokens(&self, worker: WorkerId, leases: &[(InfoHash, Token)]) -> Vec<Token> {
        let s = self.lock();
        leases
            .iter()
            .filter(|(k, t)| {
                s.table
                    .get(k)
                    .is_some_and(|l| l.holder == worker && l.token == *t)
            })
            .map(|&(_, t)| t)
            .collect()
    }

    /// A reconcile (WK-11b): the leases `worker` holds that its list
    /// (`listed`, from session `session`) left out go back to wait for a
    /// re-grant, as after an expiry: cohort, sources and finder kept, no
    /// loss counted, and their tokens stale from now on (a report or
    /// result under one is late, WK-9). Kept: a lease marked fetched (its
    /// result is in the coordinator's hands), one handed to the worker on
    /// `session` itself (it may still be on its way), and a late result's.
    /// Returns how many went back. The built-in worker is never
    /// reconciled.
    pub fn reconcile(
        &self,
        worker: WorkerId,
        session: u64,
        listed: &std::collections::HashSet<Token>,
    ) -> usize {
        if worker == WorkerId::LOCAL {
            return 0;
        }
        let mut s = self.lock();
        let returned = s.table.reconcile(worker, session, listed);
        let n = returned.len();
        for (key, lease) in returned {
            s.park(key, lease);
        }
        self.gauge(&s);
        n
    }

    /// Whether `key` is in flight: leased to any worker, or waiting for a
    /// re-grant (late results, plan Q7).
    pub fn in_flight(&self, key: &InfoHash) -> bool {
        let s = self.lock();
        s.table.get(key).is_some() || s.pending.contains_key(key)
    }

    #[cfg(test)]
    fn fetched_marks_for_test(&self) -> usize {
        self.lock().table.fetched.len()
    }

    #[cfg(test)]
    fn delivered_marks_for_test(&self) -> usize {
        self.lock().table.delivered.len()
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

    /// X-lease-report-atomic: a floor drop (`LOSS_BLOCKED`) is refused to
    /// every remote claim and late lease as soon as `report` returns. Any
    /// other loss leaves the key free to claim again, as before.
    #[test]
    fn a_blocked_report_refuses_the_key_at_once() {
        let (a, ev) = counted();
        a.serve_remote();
        let (blocked, other) = (InfoHash([5; 20]), InfoHash([6; 20]));
        let Claimed::Granted(t) = a.claim(A, claim(5, 1), 0) else {
            panic!()
        };
        let Claimed::Granted(u) = a.claim(A, claim(6, 1), 0) else {
            panic!()
        };
        let checked_at = a.commit_seq();
        assert!(a.report(A, &blocked, t, Some(LOSS_BLOCKED)).is_some());
        assert!(a.report(A, &other, u, Some(1)).is_some());
        assert_eq!(get(&ev.lost), 2);
        let answers = a.claim_batch(B, &[claim(5, 2), claim(6, 2)], 1, None, Some(checked_at));
        assert_eq!(answers[0], BatchAnswer::Committed, "refused as stored");
        assert!(matches!(
            answers[1],
            BatchAnswer::Claimed {
                claimed: Claimed::Granted(_),
                ..
            }
        ));
        let lease = RestoredLease {
            cohort: 2,
            source: src(1),
            sources: 1,
            strategy: 0,
            discovered_at: 7_200,
        };
        assert_eq!(
            a.restore_if_free(B, blocked, lease, 1, a.commit_seq()),
            None
        );
        // A stale report changes nothing.
        let key = InfoHash([7; 20]);
        let Claimed::Granted(_) = a.claim(A, claim(7, 1), 0) else {
            panic!()
        };
        assert!(a.report(B, &key, Token(1), Some(LOSS_BLOCKED)).is_none());
        assert!(!a.committed_recently_for_test(&key));
    }

    /// X-lease-report-atomic: a claim already waiting when the report lands
    /// (its store check ran before the report, so the store and the sink's
    /// map didn't know the key yet) is refused. Two threads, interleaved by
    /// barriers: the claim's check, then the report, then the claim.
    #[test]
    fn a_claim_waiting_during_a_blocked_report_is_refused() {
        let (a, _) = counted();
        a.serve_remote();
        let a = Arc::new(a);
        let key = InfoHash([5; 20]);
        let Claimed::Granted(t) = a.claim(A, claim(5, 1), 0) else {
            panic!()
        };
        let checked = Arc::new(std::sync::Barrier::new(2));
        let reported = Arc::new(std::sync::Barrier::new(2));
        let claimer = {
            let (a, checked, reported) = (a.clone(), checked.clone(), reported.clone());
            std::thread::spawn(move || {
                // The claim's store check: before the report.
                let checked_at = a.commit_seq();
                checked.wait();
                reported.wait();
                a.claim_batch(B, &[claim(5, 2)], 1, None, Some(checked_at))
            })
        };
        checked.wait();
        assert!(a.report(A, &key, t, Some(LOSS_BLOCKED)).is_some());
        reported.wait();
        assert_eq!(claimer.join().unwrap(), vec![BatchAnswer::Committed]);
        assert!(a.get(&key).is_none(), "no lease");
    }

    /// X-lease-report-atomic: a claimer racing a blocked report on another
    /// thread never gets the key. From a shared start, the claimer claims
    /// again and again until the report has returned, then once more. So
    /// every round has at least one claim after the report (refused, and
    /// asserted); a round whose claimer is already spinning when the report
    /// runs also has claims before it (the key held by A) and around it.
    /// In most rounds the first claim comes after the report. A report
    /// whose refusal isn't taken under the lock that ends the lease lets a
    /// claim in the gap be granted (#647's review).
    #[test]
    fn a_claim_racing_a_blocked_report_never_gets_the_key() {
        use std::sync::atomic::{AtomicBool, Ordering};
        for n in 0..200u32 {
            let (a, _) = counted();
            a.serve_remote();
            let a = Arc::new(a);
            let key = InfoHash([5; 20]);
            let Claimed::Granted(t) = a.claim(A, claim(5, 1), 0) else {
                panic!()
            };
            let checked_at = a.commit_seq();
            let start = Arc::new(std::sync::Barrier::new(2));
            let reported = Arc::new(AtomicBool::new(false));
            let claimer = {
                let (a, start, reported) = (a.clone(), start.clone(), reported.clone());
                std::thread::spawn(move || {
                    start.wait();
                    let mut answers = Vec::new();
                    // Ends one call after the report has returned.
                    loop {
                        let done = reported.load(Ordering::Acquire);
                        let answer = a.claim_batch(B, &[claim(5, 2)], 1, None, Some(checked_at));
                        answers.push(answer[0]);
                        if done {
                            return answers;
                        }
                    }
                })
            };
            start.wait();
            assert!(a.report(A, &key, t, Some(LOSS_BLOCKED)).is_some());
            reported.store(true, Ordering::Release);
            let answers = claimer.join().unwrap();
            for (i, answer) in answers.iter().enumerate() {
                assert!(
                    matches!(
                        answer,
                        BatchAnswer::Committed
                            | BatchAnswer::Claimed {
                                claimed: Claimed::Held { holder: A, .. },
                                ..
                            }
                    ),
                    "round {n}: answer {i} of {}: {answer:?}",
                    answers.len()
                );
            }
            // The last claim came after the report: refused.
            assert_eq!(answers.last(), Some(&BatchAnswer::Committed), "round {n}");
            assert!(a.get(&key).is_none(), "round {n}: no lease after both");
        }
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

    /// X-wk-fetched-lease: once the holder reports its result `Fetched`
    /// (its batch was accepted), its own claims are answered as committed,
    /// never with the lease, however long the writer takes (here 500 s,
    /// past the worker's 180 s grace): the lease isn't lost and stays for
    /// the commit. Another worker's claim is answered as before.
    #[test]
    fn a_fetched_lease_answers_its_holders_claims_as_committed() {
        let (a, ev) = counted();
        let key = InfoHash([1; 20]);
        let BatchAnswer::Claimed {
            claimed: Claimed::Granted(t),
            ..
        } = a.claim_batch(A, &[claim(1, 0)], 0, None, None)[0]
        else {
            panic!()
        };
        assert!(
            !matches!(a.fetched(B, &key, t, 1), FetchedReport::Marked(_)),
            "only the holder"
        );
        assert!(
            !matches!(
                a.fetched(A, &key, Token(t.0 + 1), 1),
                FetchedReport::Marked(_)
            ),
            "its token"
        );
        assert!(matches!(a.fetched(A, &key, t, 1), FetchedReport::Marked(_)));
        assert_eq!(
            a.claim_batch(A, &[claim(1, 3)], 500, None, None)[0],
            BatchAnswer::Committed
        );
        let lease = a.get(&key).expect("still leased, for the commit");
        assert_eq!((lease.token, lease.holder), (t, A));
        assert_eq!(lease.sources, 0b1001, "the claim's source joined it");
        assert_eq!(get(&ev.lost), 0);
        assert!(matches!(
            a.claim_batch(B, &[claim(1, 0)], 500, None, None)[0],
            BatchAnswer::Claimed {
                claimed: Claimed::Held { holder: A, token },
                harvested: false,
            } if token == t
        ));
        // The writer's commit ends it, and its mark with it.
        assert!(a.commit(A, &key, t).is_some());
        assert_eq!(a.fetched_marks_for_test(), 0);
    }

    /// A worker that never sends `Fetched` (an older one) gets its lease
    /// back on its own claim, as before.
    #[test]
    fn a_holder_that_never_reports_fetched_gets_its_lease_back() {
        let (a, _) = counted();
        let BatchAnswer::Claimed {
            claimed: Claimed::Granted(t),
            ..
        } = a.claim_batch(A, &[claim(1, 0)], 0, None, None)[0]
        else {
            panic!()
        };
        assert!(matches!(
            a.claim_batch(A, &[claim(1, 0)], 500, None, None)[0],
            BatchAnswer::Claimed {
                claimed: Claimed::Held { holder: A, token },
                ..
            } if token == t
        ));
    }

    /// A fetched lease whose commit never comes expires a TTL after the
    /// mark, though its holder still heartbeats; its key waits for a
    /// re-grant like any expired lease, and a late report under the old
    /// token is stale. Ended any other way, the mark goes with the lease.
    #[test]
    fn a_fetched_lease_with_no_commit_expires_a_ttl_after_the_mark() {
        let (a, _) = counted();
        let key = InfoHash([1; 20]);
        let Claimed::Granted(t) = a.claim(A, claim(1, 0), 0) else {
            panic!()
        };
        assert!(matches!(
            a.fetched(A, &key, t, 10),
            FetchedReport::Marked(_)
        ));
        assert!(
            matches!(a.fetched(A, &key, t, 300), FetchedReport::Marked(_)),
            "the first mark counts"
        );
        a.heartbeat(A, 600);
        assert_eq!(a.expire(610), 0, "a TTL after the mark, not before");
        a.heartbeat(A, 611);
        assert_eq!(a.expire(611), 1);
        assert_eq!((a.len(), a.pending()), (0, 1));
        assert_eq!(a.fetched_marks_for_test(), 0);
        assert!(a.commit(A, &key, t).is_none(), "the old token is stale");
        // Lost or evicted: the mark goes too.
        let Claimed::Granted(t) = a.claim(A, claim(2, 0), 700) else {
            panic!()
        };
        assert!(matches!(
            a.fetched(A, &InfoHash([2; 20]), t, 700),
            FetchedReport::Marked(_)
        ));
        assert!(a.lost(&InfoHash([2; 20]), t, 1).is_some());
        let Claimed::Granted(t) = a.claim(A, claim(3, 0), 700) else {
            panic!()
        };
        assert!(matches!(
            a.fetched(A, &InfoHash([3; 20]), t, 700),
            FetchedReport::Marked(_)
        ));
        assert_eq!(a.evict(A), 1);
        assert_eq!(a.fetched_marks_for_test(), 0);
    }

    /// A marked lease whose holder is also silent past the TTL expires
    /// once, counted once, not once per reason (X-wk-fetched-lease-nits).
    #[test]
    fn a_marked_lease_of_a_silent_holder_expires_once() {
        let (a, ev) = counted();
        let key = InfoHash([1; 20]);
        let Claimed::Granted(t) = a.claim(A, claim(1, 0), 0) else {
            panic!()
        };
        assert!(matches!(a.fetched(A, &key, t, 0), FetchedReport::Marked(_)));
        assert_eq!(a.expire(700), 1, "silent and stuck: one expiry");
        assert_eq!((a.len(), a.pending(), get(&ev.lost)), (0, 1, 0));
        assert_eq!(a.fetched_marks_for_test(), 0);
        assert_eq!(a.expire(1_400), 0, "nothing left to expire");
    }

    /// After a marked lease expires, its holder's claim is a re-grant under
    /// a new token, not a denial and not a second harvest.
    #[test]
    fn the_holders_claim_after_a_marked_lease_expires_is_granted_anew() {
        let (a, ev) = counted();
        let key = InfoHash([1; 20]);
        let BatchAnswer::Claimed {
            claimed: Claimed::Granted(t),
            ..
        } = a.claim_batch(A, &[claim(1, 0)], 0, None, None)[0]
        else {
            panic!()
        };
        assert!(matches!(a.fetched(A, &key, t, 0), FetchedReport::Marked(_)));
        a.heartbeat(A, 601);
        assert_eq!(a.expire(601), 1);
        let BatchAnswer::Claimed {
            claimed: Claimed::Granted(new),
            harvested: false,
        } = a.claim_batch(A, &[claim(1, 0)], 602, None, None)[0]
        else {
            panic!("a re-grant")
        };
        assert_ne!(new, t);
        assert_eq!(get(&ev.harvested), 1, "harvested once");
        assert_eq!(a.get(&key).map(|l| (l.holder, l.token)), Some((A, new)));
    }

    /// A `Fetched` report is marked, committed (the commit beat it) or
    /// stale, decided under one lock.
    #[test]
    fn a_fetched_report_is_marked_committed_or_stale() {
        let (a, _) = counted();
        a.serve_remote();
        let key = InfoHash([1; 20]);
        let Claimed::Granted(t) = a.claim(A, claim(1, 0), 0) else {
            panic!()
        };
        assert_eq!(a.fetched(A, &key, Token(t.0 + 1), 1), FetchedReport::Stale);
        assert_eq!(a.fetched(B, &key, t, 1), FetchedReport::Stale);
        assert!(matches!(a.fetched(A, &key, t, 1), FetchedReport::Marked(_)));
        assert!(a.commit(A, &key, t).is_some());
        assert_eq!(a.fetched(A, &key, t, 2), FetchedReport::Committed);
        assert_eq!(
            a.fetched(A, &InfoHash([9; 20]), t, 2),
            FetchedReport::Stale,
            "a key this run doesn't know"
        );
        // Leased again since its commit (the built-in worker may), then
        // waiting for a re-grant: the old token's report is stale both ways.
        let Claimed::Granted(_) = a.claim(WorkerId::LOCAL, claim(1, 0), 3) else {
            panic!()
        };
        assert_eq!(a.fetched(A, &key, t, 3), FetchedReport::Stale, "leased");
        assert_eq!(a.evict(WorkerId::LOCAL), 1);
        assert_eq!(a.fetched(A, &key, t, 3), FetchedReport::Stale, "pending");
    }

    fn granted(a: &LeaseAuthority, w: WorkerId, n: u8) -> Token {
        let Claimed::Granted(t) = a.claim(w, claim(n, 0), 0) else {
            panic!("not granted")
        };
        t
    }

    /// A reconcile returns only what the list left out of the worker's own
    /// leases delivered on an earlier session, as an expiry would: waiting
    /// for a re-grant with its cohort kept, no loss counted.
    #[test]
    fn a_reconcile_returns_only_the_workers_unlisted_leases_from_earlier_sessions() {
        let (a, ev) = counted();
        let [k1, k2, k3, k4] = [1, 2, 3, 4].map(|n| InfoHash([n; 20]));
        let t1 = granted(&a, A, 1);
        let t2 = granted(&a, A, 2);
        let t3 = granted(&a, A, 3);
        let tb = granted(&a, B, 4);
        a.delivered(A, 1, &[(k1, t1), (k2, t2), (k3, t3)]);
        // Handed to A again on its session 2 (its own claim answered with
        // the lease): it may still be on its way, so it stays.
        a.delivered(A, 2, &[(k3, t3)]);
        // B's lease, under A's name: no mark, and never A's to return.
        a.delivered(A, 2, &[(k4, tb)]);
        let listed = std::collections::HashSet::from([t1, tb]);
        let cohort = a.get(&k2).unwrap().cohort;
        assert_eq!(a.reconcile(A, 2, &listed), 1);
        assert_eq!(a.get(&k2), None, "returned");
        assert_eq!(a.pending(), 1);
        assert_eq!(a.get(&k1).map(|l| l.token), Some(t1), "listed");
        assert_eq!(a.get(&k3).map(|l| l.token), Some(t3), "this session's");
        assert_eq!(a.get(&k4).map(|l| (l.holder, l.token)), Some((B, tb)));
        // Its old token is stale: a report under it counts nothing.
        assert!(a.report(A, &k2, t2, Some(1)).is_none());
        assert_eq!(get(&ev.lost), 0);
        // Granted again under a new token, cohort kept, not a new harvest.
        let before = get(&ev.harvested);
        let regranted = a.regrant_pending(&[B], 8, 1);
        assert_eq!(regranted.len(), 1);
        let (_, keys) = &regranted[0];
        assert_eq!(keys[0].0, k2);
        assert_ne!(keys[0].1, t2);
        assert_eq!(a.get(&k2).unwrap().cohort, cohort, "cohort kept");
        assert_eq!(get(&ev.harvested), before);
        // The built-in worker is never reconciled.
        let tl = granted(&a, WorkerId::LOCAL, 5);
        assert_eq!(a.reconcile(WorkerId::LOCAL, 9, &Default::default()), 0);
        assert!(a.current(&InfoHash([5; 20]), tl).is_some());
    }

    /// A lease marked fetched (its result is in the coordinator's hands)
    /// and a late result's lease are never returned, listed or not.
    #[test]
    fn a_reconcile_keeps_fetched_and_late_result_leases() {
        let (a, _) = counted();
        a.serve_remote();
        let (k1, k2) = (InfoHash([1; 20]), InfoHash([2; 20]));
        let t1 = granted(&a, A, 1);
        assert!(matches!(a.fetched(A, &k1, t1, 0), FetchedReport::Marked(_)));
        let restored = RestoredLease {
            cohort: 0,
            source: src(0),
            sources: 1,
            strategy: 0,
            discovered_at: 0,
        };
        let seq = a.commit_seq();
        let t2 = a.restore_if_free(A, k2, restored, 0, seq).unwrap();
        // Even a mark from a session can't unpin it.
        a.delivered(A, 1, &[(k2, t2)]);
        assert_eq!(a.reconcile(A, 2, &Default::default()), 0);
        assert!(a.current(&k1, t1).is_some());
        assert!(a.current(&k2, t2).is_some());
    }

    /// The delivery marks end with their lease, however it ends.
    #[test]
    fn delivery_marks_end_with_their_lease() {
        let (a, _) = counted();
        let keys: Vec<InfoHash> = (1..=4).map(|n| InfoHash([n; 20])).collect();
        let tokens: Vec<Token> = (1..=4).map(|n| granted(&a, A, n)).collect();
        let pairs: Vec<(InfoHash, Token)> = keys.iter().copied().zip(tokens.clone()).collect();
        a.delivered(A, 1, &pairs);
        // Another worker's mark, or a stale token's, is never made.
        a.delivered(B, 1, &pairs);
        a.delivered(A, 1, &[(keys[0], Token(tokens[0].0 + 100))]);
        assert_eq!(a.delivered_marks_for_test(), 4);
        assert!(a.commit(A, &keys[0], tokens[0]).is_some());
        assert!(a.report(A, &keys[1], tokens[1], Some(1)).is_some());
        assert_eq!(a.reconcile(A, 2, &[tokens[3]].into()), 1);
        assert_eq!(a.evict(A), 1);
        assert_eq!(a.delivered_marks_for_test(), 0);
        assert_eq!(a.fetched_marks_for_test(), 0);
    }

    /// `held_tokens` answers only for the worker's own current leases.
    #[test]
    fn held_tokens_are_the_workers_own_current_ones() {
        let (a, _) = counted();
        let (k1, k2) = (InfoHash([1; 20]), InfoHash([2; 20]));
        let t1 = granted(&a, A, 1);
        let t2 = granted(&a, B, 2);
        let asked = [
            (k1, t1),
            (k2, t2),
            (k1, Token(t1.0 + 9)),
            (InfoHash([3; 20]), t1),
        ];
        assert_eq!(a.held_tokens(A, &asked), [t1]);
        assert_eq!(a.held_tokens(B, &asked), [t2]);
    }

    /// The table's per-worker key index (CLUSTER-02, X-worker-839-followups)
    /// against a full scan: the same keys for each worker, and no entry for
    /// a worker holding none.
    fn assert_counts(t: &LeaseTable) {
        let mut scanned: HashMap<WorkerId, HashSet<InfoHash>> = HashMap::new();
        for (k, l) in &t.leases {
            scanned.entry(l.holder).or_default().insert(*k);
        }
        assert_eq!(t.by_worker, scanned);
        for (w, keys) in &scanned {
            assert_eq!(t.held_by(*w), keys.len());
        }
    }

    proptest::proptest! {
        /// Whatever happens to the table (grants, renewals, releases,
        /// expiries, evictions, re-grants, reconciles, fetched marks), each
        /// worker's index holds exactly the keys it leases, and a worker
        /// holding none has no entry.
        #[test]
        fn held_counts_follow_every_change(
            ops in proptest::collection::vec((0u8..9, 1u32..4, 0u8..12), 1..200)
        ) {
            let mut t = LeaseTable::new(1, 600);
            let mut now = 0;
            for (op, w, k) in ops {
                let (w, key) = (WorkerId(w), InfoHash([k; 20]));
                match op {
                    0 | 1 => {
                        let _ = t.claim(w, claim(k, 0), now);
                    }
                    2 => {
                        if let Some(l) = t.get(&key).copied() {
                            t.release(&key, l.token);
                        }
                    }
                    3 => {
                        let _ = t.evict(w);
                    }
                    4 => {
                        now += 400;
                        t.heartbeat(w, now);
                        let _ = t.expire(now);
                    }
                    5 => {
                        // A re-grant to another worker, as after an expiry.
                        if let Some(l) = t.get(&key).copied() {
                            let l = t.release(&key, l.token).unwrap();
                            t.regrant(w, key, l, now);
                        }
                    }
                    6 => {
                        // A list naming this key's lease, if it's the worker's.
                        let listed: HashSet<Token> =
                            t.get(&key).map(|l| l.token).into_iter().collect();
                        let _ = t.reconcile(w, 9, &listed);
                    }
                    7 => {
                        // Marked fetched: a later expiry takes it as stuck.
                        if let Some(l) = t.get(&key).copied() {
                            let _ = t.mark_fetched(l.holder, &key, l.token, now);
                        }
                    }
                    _ => {
                        // A renewal only.
                        now += 100;
                        t.heartbeat(w, now);
                    }
                }
                assert_counts(&t);
            }
        }
    }

    /// CLUSTER-02: claims for new keys past the worker's cap are
    /// `Capped`, never leased; its own held keys still answer with their
    /// lease; the built-in worker is never capped; a worker's own cap
    /// overrides the default and can be cleared.
    #[test]
    fn claims_past_the_cap_are_capped_and_never_leased() {
        let (a, ev) = counted();
        a.set_lease_cap(Some(3));
        let claims: Vec<Claim> = (1..=5).map(|n| claim(n, 0)).collect();
        let answers = a.claim_batch(A, &claims, 1, None, None);
        let capped = answers
            .iter()
            .filter(|x| matches!(x, BatchAnswer::Capped))
            .count();
        assert_eq!((capped, a.held_by(A)), (2, 3), "{answers:?}");
        assert!(matches!(
            answers[3..],
            [BatchAnswer::Capped, BatchAnswer::Capped]
        ));
        assert_eq!(get(&ev.harvested), 3);
        assert!(a.get(&InfoHash([4; 20])).is_none());
        // Its own key again: the lease, not a cap.
        assert!(matches!(
            a.claim_batch(A, &[claim(1, 1)], 1, None, None)[0],
            BatchAnswer::Claimed {
                claimed: Claimed::Held { holder: A, .. },
                ..
            }
        ));
        // Another worker has its own room.
        assert!(matches!(
            a.claim_batch(B, &[claim(4, 0)], 1, None, None)[0],
            BatchAnswer::Claimed {
                harvested: true,
                ..
            }
        ));
        // The built-in worker is never capped.
        let local: Vec<Claim> = (20..30).map(|n| claim(n, 0)).collect();
        assert!(
            a.claim_batch(WorkerId::LOCAL, &local, 1, None, None)
                .iter()
                .all(|x| matches!(x, BatchAnswer::Claimed { .. }))
        );
        assert_eq!(a.lease_cap(WorkerId::LOCAL), None);
        // Its own cap, then back to the default.
        a.set_worker_cap(A, Some(4));
        assert_eq!(a.lease_cap(A), Some(4));
        let answers = a.claim_batch(A, &[claim(5, 0), claim(6, 0)], 1, None, None);
        assert!(
            matches!(
                answers[..],
                [BatchAnswer::Claimed { .. }, BatchAnswer::Capped]
            ),
            "{answers:?}"
        );
        a.set_worker_cap(A, None);
        assert_eq!(a.lease_cap(A), Some(3));
        // A cap lowered under what's held takes nothing back.
        assert_eq!(a.held_by(A), 4);
        assert!(matches!(
            a.claim_batch(A, &[claim(7, 0)], 1, None, None)[0],
            BatchAnswer::Capped
        ));
    }

    /// The admission is asked only about keys within the cap, and a key
    /// waiting for a re-grant counts against the cap like a new one.
    #[test]
    fn the_cap_bounds_what_admission_sees_and_takeovers() {
        let (a, _) = counted();
        a.set_lease_cap(Some(2));
        let asked = std::sync::Mutex::new(Vec::new());
        let admit = |n: usize| {
            asked.lock().unwrap().push(n);
            n
        };
        let claims: Vec<Claim> = (1..=5).map(|n| claim(n, 0)).collect();
        let answers = a.claim_batch(A, &claims, 1, Some(&admit), None);
        assert_eq!(*asked.lock().unwrap(), [2]);
        assert_eq!(
            answers
                .iter()
                .filter(|x| matches!(x, BatchAnswer::Capped))
                .count(),
            3
        );
        // B's lease expires: the key waits. A, at its cap, can't take it
        // over by claiming it.
        let _ = a.claim_batch(B, &[claim(9, 0)], 1, None, None);
        a.heartbeat(A, 700);
        assert_eq!(a.expire(700), 1);
        assert_eq!(a.pending(), 1);
        assert!(matches!(
            a.claim_batch(A, &[claim(9, 0)], 700, None, None)[0],
            BatchAnswer::Capped
        ));
        assert_eq!(a.pending(), 1);
    }

    /// With commits forgotten since the store check (a stale
    /// `checked_at`), every new key is busy, even for a worker at its cap:
    /// the claim is asked again, not counted as capped.
    #[test]
    fn a_stale_check_answers_busy_even_at_the_cap() {
        let (a, _) = counted();
        a.serve_remote();
        a.set_lease_cap(Some(1));
        let _ = a.claim_batch(A, &[claim(1, 0)], 1, None, None);
        assert_eq!(a.held_by(A), 1);
        let stale = a.commit_seq();
        for n in 0..=RECENT_COMMITS as u32 {
            let key = InfoHash({
                let mut k = [0xEE; 20];
                k[..4].copy_from_slice(&n.to_be_bytes());
                k
            });
            let c = Claim { key, ..claim(2, 0) };
            let Claimed::Granted(t) = a.claim(WorkerId::LOCAL, c, 1) else {
                panic!("a fresh key wasn't granted")
            };
            assert!(a.commit(WorkerId::LOCAL, &key, t).is_some());
        }
        assert_eq!(
            a.claim_batch(A, &[claim(3, 0)], 1, None, Some(stale))[0],
            BatchAnswer::Busy
        );
        // A fresh check: the cap.
        assert_eq!(
            a.claim_batch(A, &[claim(3, 0)], 1, None, Some(a.commit_seq()))[0],
            BatchAnswer::Capped
        );
    }

    /// Re-grants pass a target at its cap by and stop at each target's
    /// room; what's left keeps waiting.
    #[test]
    fn regrants_never_take_a_worker_past_its_cap() {
        let (a, _) = counted();
        a.set_lease_cap(Some(3));
        let c = WorkerId(3);
        let _ = a.claim_batch(A, &[claim(1, 0), claim(2, 0), claim(3, 0)], 1, None, None);
        let _ = a.claim_batch(B, &[claim(4, 0)], 1, None, None);
        let waiting: Vec<Claim> = (10..16).map(|n| claim(n, 0)).collect();
        let _ = a.claim_batch(c, &waiting, 1, None, None);
        // `c` got three (its cap), then goes silent: they wait.
        assert_eq!(a.held_by(c), 3);
        a.heartbeat(A, 700);
        a.heartbeat(B, 700);
        assert_eq!(a.expire(700), 3);
        let out = a.regrant_pending(&[A, B], 10, 700);
        assert_eq!(out.len(), 1, "{out:?}");
        assert_eq!((out[0].0, out[0].1.len()), (B, 2));
        assert_eq!((a.held_by(A), a.held_by(B), a.pending()), (3, 3, 1));
    }

    /// At the largest pool an assignment allows, the cap is within what a
    /// reconcile list can carry, so a worker under its cap can always list
    /// what it holds (WK-11b).
    #[test]
    fn the_cap_at_the_largest_pool_is_the_reconcile_lists_limit() {
        assert!(
            crate::server::ClusterServer::lease_cap_for(crate::assign::MAX_POOL_CAP)
                <= crate::proto::MAX_HOLDING
        );
        assert_eq!(
            crate::server::ClusterServer::lease_cap_for(crate::assign::DEFAULT_POOL_CAP),
            100_000
        );
    }
}
