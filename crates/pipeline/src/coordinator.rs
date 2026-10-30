//! The coordinator's side of the worker boundary (architecture §19.3):
//! the fleet-wide lease table, and the funnel accounting that has to
//! happen exactly once per key, whichever worker holds it.
//!
//! - "Harvested" is counted when a lease is granted.
//! - A terminal loss is counted when the holder reports it under the
//!   lease's current token, which also ends the lease.
//! - Commits end leases in the writer ([`Coordinator::complete`]).
//!
//! A report under any other token is stale and ignored. The lock is held
//! only for table operations, never across an await.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use lodestar_cluster::{Claim, Claimed, Lease, LeaseTable, RestoredLease, Token, WorkerId};
use lodestar_metainfo::InfoHash;
use lodestar_sources::{Funnel, FunnelCounts, Loss, Stage};

/// Counters for metrics and the status page.
#[derive(Debug, Default)]
pub struct LeaseStats {
    pub granted: AtomicU64,
    /// Claims for keys already leased (their source was merged in).
    pub held: AtomicU64,
    /// Reports and results under a token that was no longer current.
    pub stale: AtomicU64,
    pub expired: AtomicU64,
    /// Leases outstanding (a gauge).
    pub outstanding: AtomicU64,
}

#[derive(Debug)]
pub(crate) struct Coordinator {
    leases: Mutex<LeaseTable>,
    funnel: Arc<FunnelCounts>,
    pub stats: Arc<LeaseStats>,
}

impl Coordinator {
    pub fn new(epoch: u32, ttl_secs: i64, funnel: Arc<FunnelCounts>) -> Coordinator {
        Coordinator {
            leases: Mutex::new(LeaseTable::new(epoch, ttl_secs)),
            funnel,
            stats: Arc::default(),
        }
    }

    fn table(&self) -> MutexGuard<'_, LeaseTable> {
        // A panic while holding the lock leaves a consistent table: every
        // operation is a single map update.
        self.leases.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn gauge(&self, t: &LeaseTable) {
        self.stats
            .outstanding
            .store(t.len() as u64, Ordering::Relaxed);
    }

    pub fn claim(&self, worker: WorkerId, c: Claim, now: i64) -> Claimed {
        let mut t = self.table();
        let claimed = t.claim(worker, c, now);
        self.gauge(&t);
        drop(t);
        match claimed {
            Claimed::Granted(_) => {
                self.stats.granted.fetch_add(1, Ordering::Relaxed);
                self.funnel
                    .record(c.at.div_euclid(3600), c.source, Stage::Harvested, None);
            }
            Claimed::Held { .. } => {
                self.stats.held.fetch_add(1, Ordering::Relaxed);
            }
        }
        claimed
    }

    /// Re-leases a key saved in the fetch queue by the previous run.
    pub fn restore(
        &self,
        worker: WorkerId,
        key: InfoHash,
        lease: RestoredLease,
        now: i64,
    ) -> Token {
        let mut t = self.table();
        let token = t.restore(worker, key, lease, now);
        self.gauge(&t);
        token
    }

    /// The key's lease, if `token` is still current.
    pub fn current(&self, key: &InfoHash, token: Token) -> Option<Lease> {
        let t = self.table();
        let lease = t.get(key).filter(|l| l.token == token).copied();
        if lease.is_none() {
            self.stats.stale.fetch_add(1, Ordering::Relaxed);
        }
        lease
    }

    /// Ends a lease whose key was committed. `None` if it was stale.
    pub fn complete(&self, key: &InfoHash, token: Token) -> Option<Lease> {
        let mut t = self.table();
        let lease = t.release(key, token);
        self.gauge(&t);
        if lease.is_none() {
            self.stats.stale.fetch_add(1, Ordering::Relaxed);
        }
        lease
    }

    /// The holder lost the key: count it once and end the lease. False
    /// if the token was stale (nothing was counted).
    pub fn lost(&self, key: &InfoHash, token: Token, loss: Loss) -> bool {
        let Some(l) = self.complete(key, token) else {
            return false;
        };
        self.funnel
            .record(l.cohort, l.source, Stage::Lost, Some(loss));
        true
    }

    pub fn heartbeat(&self, worker: WorkerId, now: i64) {
        self.table().heartbeat(worker, now);
    }

    /// Ends the leases of silent workers and grants them to `to`, which
    /// gets the new tokens. Their cohort and sources are kept, and they
    /// aren't harvested again.
    pub fn expire_to(&self, to: WorkerId, now: i64) -> Vec<(InfoHash, Token)> {
        let mut t = self.table();
        let expired = t.expire(now);
        let moved: Vec<(InfoHash, Token)> = expired
            .into_iter()
            .map(|(key, lease)| (key, t.regrant(to, key, lease, now)))
            .collect();
        self.gauge(&t);
        self.stats
            .expired
            .fetch_add(moved.len() as u64, Ordering::Relaxed);
        moved
    }

    #[cfg(test)]
    pub fn outstanding(&self) -> usize {
        self.table().len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lodestar_store::SourceId;

    fn claim(n: u8) -> Claim {
        Claim {
            key: InfoHash([n; 20]),
            source: SourceId::new(1).unwrap(),
            strategy: 0,
            at: 3600 * 5 + 1,
        }
    }

    #[test]
    fn harvest_and_loss_are_counted_once() {
        let funnel = Arc::new(FunnelCounts::default());
        let c = Coordinator::new(1, 600, funnel.clone());
        let Claimed::Granted(token) = c.claim(WorkerId::LOCAL, claim(1), 0) else {
            panic!("granted")
        };
        // A second claim, a stale report and a double report count nothing.
        c.claim(WorkerId::LOCAL, claim(1), 0);
        assert!(!c.lost(&InfoHash([1; 20]), Token(token.0 + 1), Loss::NoPeers));
        assert!(c.lost(&InfoHash([1; 20]), token, Loss::NoPeers));
        assert!(!c.lost(&InfoHash([1; 20]), token, Loss::NoPeers));
        assert_eq!(funnel.total(Stage::Harvested, None), 1);
        assert_eq!(funnel.total(Stage::Lost, Some(Loss::NoPeers)), 1);
        assert_eq!(c.outstanding(), 0);
        assert_eq!(c.stats.stale.load(Ordering::Relaxed), 2);
        assert_eq!(c.stats.held.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn expired_leases_move_without_a_new_harvest() {
        let funnel = Arc::new(FunnelCounts::default());
        let c = Coordinator::new(1, 600, funnel.clone());
        let Claimed::Granted(old) = c.claim(WorkerId(7), claim(1), 0) else {
            panic!()
        };
        let moved = c.expire_to(WorkerId::LOCAL, 601);
        assert_eq!(moved.len(), 1);
        assert!(c.complete(&InfoHash([1; 20]), old).is_none());
        assert!(c.complete(&InfoHash([1; 20]), moved[0].1).is_some());
        assert_eq!(funnel.total(Stage::Harvested, None), 1);
    }
}
