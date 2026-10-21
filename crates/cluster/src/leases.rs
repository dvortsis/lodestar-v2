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

use std::collections::HashMap;

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
        self.restore(worker, key, RestoredLease::from(lease), now)
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
}
