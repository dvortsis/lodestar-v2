//! The coordinator's side of the worker boundary (architecture §19.3):
//! the fleet's one [`LeaseAuthority`], and the funnel accounting that has
//! to happen exactly once per key, whichever worker holds it.
//!
//! - "Harvested" is counted when a lease is granted, by the authority's
//!   [`LeaseEvents`] hook, so a grant to a remote worker (through the
//!   cluster server sharing the authority) is counted the same way.
//! - A terminal loss is counted when the holder reports it under the
//!   lease's current token, which also ends the lease.
//! - Commits end leases in the writer ([`Coordinator::complete`]).
//!
//! A report under any other token is stale and ignored. The built-in
//! worker calls the authority directly (plan Q4): no cluster server, no
//! certificate and no fleet lock on the local path.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use lodestar_cluster::{
    Claim, Claimed, Lease, LeaseAuthority, LeaseEvents, RestoredLease, Token, WorkerId,
};
use lodestar_metainfo::InfoHash;
use lodestar_sources::{Funnel, FunnelCounts, Loss, Stage};

/// Counters for metrics and the status page.
#[derive(Debug, Default)]
pub struct LeaseStats {
    /// First grants, to any worker (harvested).
    pub granted: AtomicU64,
    /// Local claims for keys already leased (their source was merged in).
    pub held: AtomicU64,
    /// Reports and results under a token that was no longer current.
    pub stale: AtomicU64,
    pub expired: AtomicU64,
    /// Leases outstanding, held or waiting for a re-grant (a gauge).
    pub outstanding: AtomicU64,
}

/// A loss code as it crosses the worker boundary. A remote worker's code
/// the funnel doesn't know is counted as `Invalid`, so it still ends once.
pub(crate) fn loss_from_code(code: u8) -> Loss {
    match code {
        1 => Loss::NoPeers,
        2 => Loss::TimedOut,
        3 => Loss::Refused,
        4 => Loss::EncryptionFailed,
        5 => Loss::Oversize,
        7 => Loss::Blocked,
        8 => Loss::Evicted,
        9 => Loss::GaveUp,
        10 => Loss::Unreachable,
        _ => Loss::Invalid,
    }
}

/// The funnel accounting on the authority's events.
struct FunnelEvents {
    funnel: Arc<FunnelCounts>,
    stats: Arc<LeaseStats>,
}

impl LeaseEvents for FunnelEvents {
    fn harvested(&self, _key: &InfoHash, lease: &Lease) {
        self.stats.granted.fetch_add(1, Ordering::Relaxed);
        self.funnel
            .record(lease.cohort, lease.source, Stage::Harvested, None);
    }

    /// A refetch's loss ends its lease but isn't the funnel's.
    fn lost(&self, _key: &InfoHash, lease: &Lease, reason: u8) {
        if crate::in_funnel(lease.cohort) {
            self.funnel.record(
                lease.cohort,
                lease.source,
                Stage::Lost,
                Some(loss_from_code(reason)),
            );
        }
    }

    fn outstanding(&self, n: usize) {
        self.stats.outstanding.store(n as u64, Ordering::Relaxed);
    }

    /// Whoever ran the expiry (this pipeline or the cluster server).
    fn expired(&self, n: usize) {
        self.stats.expired.fetch_add(n as u64, Ordering::Relaxed);
    }
}

#[derive(Debug)]
pub(crate) struct Coordinator {
    authority: Arc<LeaseAuthority>,
    pub stats: Arc<LeaseStats>,
}

impl Coordinator {
    pub fn new(epoch: u32, ttl_secs: i64, funnel: Arc<FunnelCounts>) -> Coordinator {
        let stats = Arc::new(LeaseStats::default());
        let events = Arc::new(FunnelEvents {
            funnel,
            stats: stats.clone(),
        });
        Coordinator {
            authority: Arc::new(LeaseAuthority::new(epoch, ttl_secs, events)),
            stats,
        }
    }

    /// The authority, for the cluster server once workers are allowed.
    pub fn authority(&self) -> &Arc<LeaseAuthority> {
        &self.authority
    }

    pub fn claim(&self, worker: WorkerId, c: Claim, now: i64) -> Claimed {
        let claimed = self.authority.claim(worker, c, now);
        if let Claimed::Held { .. } = claimed {
            self.stats.held.fetch_add(1, Ordering::Relaxed);
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
        self.authority.restore(worker, key, lease, now)
    }

    fn stale<T>(&self, v: Option<T>) -> Option<T> {
        if v.is_none() {
            self.stats.stale.fetch_add(1, Ordering::Relaxed);
        }
        v
    }

    /// The key's lease, if `token` is still current.
    pub fn current(&self, key: &InfoHash, token: Token) -> Option<Lease> {
        self.stale(self.authority.current(key, token))
    }

    /// Ends a lease whose key was committed. `None` if it was stale.
    pub fn complete(&self, key: &InfoHash, token: Token) -> Option<Lease> {
        self.stale(self.authority.release(key, token))
    }

    /// The holder lost the key: count it once and end the lease. False
    /// if the token was stale (nothing was counted). A refetch's loss
    /// ends its lease but isn't the funnel's.
    pub fn lost(&self, key: &InfoHash, token: Token, loss: Loss) -> bool {
        self.stale(self.authority.lost(key, token, loss as u8))
            .is_some()
    }

    pub fn heartbeat(&self, worker: WorkerId, now: i64) {
        self.authority.heartbeat(worker, now);
    }

    /// Ends the leases of silent workers; their keys wait in the
    /// authority for a re-grant, with their cohort and sources kept, and
    /// aren't harvested again. The built-in worker's never expire.
    pub fn expire(&self, now: i64) -> usize {
        self.authority.expire(now)
    }

    #[cfg(test)]
    pub fn outstanding(&self) -> usize {
        self.authority.len() + self.authority.pending()
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
        let Claimed::Granted(local) = c.claim(WorkerId::LOCAL, claim(2), 0) else {
            panic!()
        };
        assert_eq!(c.expire(601), 1, "the built-in worker never expires");
        assert_eq!(c.outstanding(), 2, "still in flight while it waits");
        // Found again locally: granted as the same key, not a new harvest.
        let Claimed::Granted(new) = c.claim(WorkerId::LOCAL, claim(1), 602) else {
            panic!()
        };
        assert!(c.complete(&InfoHash([1; 20]), old).is_none());
        assert!(c.complete(&InfoHash([1; 20]), new).is_some());
        assert!(c.complete(&InfoHash([2; 20]), local).is_some());
        assert_eq!(funnel.total(Stage::Harvested, None), 2);
        assert_eq!(c.stats.granted.load(Ordering::Relaxed), 2);
        assert_eq!(c.stats.outstanding.load(Ordering::Relaxed), 0);
        assert_eq!(c.stats.expired.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn every_loss_code_round_trips() {
        for loss in [
            Loss::NoPeers,
            Loss::TimedOut,
            Loss::Refused,
            Loss::EncryptionFailed,
            Loss::Oversize,
            Loss::Invalid,
            Loss::Blocked,
            Loss::Evicted,
            Loss::GaveUp,
            Loss::Unreachable,
        ] {
            assert_eq!(loss_from_code(loss as u8), loss);
        }
        assert_eq!(loss_from_code(0), Loss::Invalid);
        assert_eq!(loss_from_code(200), Loss::Invalid);
    }
}
