//! Each worker's leases and its marginal gain, for Insights (architecture
//! §19.7; workers plan Q12, WK-15).
//!
//! The coordinator counts, per worker and hour, into
//! [`metric::WORKER_LEASES`] (dim = the worker id, the built-in worker 0):
//! - [`key::GRANTED`]: keys first granted to it (harvested, §19.3), at
//!   their discovery time, as the funnel's cohorts are;
//! - [`key::MARGINAL`]: those of its finds that **nothing else claimed
//!   while the key was in flight** (Q12): a lease that ends, committed or
//!   lost, with its [`Lease::finder`] still set, counted for the finder at
//!   the key's discovery time. The authority clears the finder under its
//!   own lock the first time another worker, the built-in one included,
//!   claims the key, so no ordering of threads can count a shared key. It
//!   approximates "keys no other worker would have seen": a key found again
//!   only after its commit is a rediscovery and doesn't count against it.
//!   Keys still in flight aren't counted yet, and a lease restored from an
//!   earlier run has no finder;
//! - [`key::COMMITTED`] and [`key::LOST`]: its leases that ended in a
//!   commit or a terminal loss, at the hour they ended;
//! - [`key::REDISCOVERED`] and [`key::DROPPED`]: a remote worker's own
//!   counters, summed again by the remote intake beside its `BY_WORKER`
//!   rows, so Discover reads one small metric (the 50 ms budget).
//!
//! **Counts only, and no state.** Everything is decided from the lease the
//! authority hands over, so nothing here holds a key, and nothing grows
//! with the torrents in flight.
//!
//! **Single-machine parity (§19.8):** nothing is counted until the
//! authority serves remote workers (`LeaseEvents::serving`, when a cluster
//! server is built on it, at every start while workers are allowed). On a
//! single machine the rollups gain no row.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use lodestar_cluster::{Lease, WorkerId};

use crate::rollups::{Rollups, metric};

/// [`metric::WORKER_LEASES`] keys, append only.
pub mod key {
    /// Keys first granted to the worker.
    pub const GRANTED: i64 = 0;
    /// Its finds that no other worker claimed while they were in flight.
    pub const MARGINAL: i64 = 1;
    /// Leases that ended in a commit.
    pub const COMMITTED: i64 = 2;
    /// Leases that ended in a terminal loss.
    pub const LOST: i64 = 3;
    /// A remote worker's own rediscovered counter, as `Admit` let it in
    /// (the remote intake writes it beside `BY_WORKER`).
    pub const REDISCOVERED: i64 = 4;
    /// A remote worker's own dropped counter, likewise.
    pub const DROPPED: i64 = 5;
}

/// The [`metric::WORKER_LEASES`] key a remote worker's admitted counter
/// is also summed under, if Discover shows it per worker.
pub fn counted_key(counter_metric: u32) -> Option<i64> {
    match counter_metric {
        metric::REDISCOVERED => Some(key::REDISCOVERED),
        metric::DROPPED => Some(key::DROPPED),
        _ => None,
    }
}

/// The per-worker lease counts (see the module docs).
#[derive(Debug)]
pub struct Marginal {
    rollups: Arc<Rollups>,
    on: AtomicBool,
}

impl Marginal {
    pub fn new(rollups: Arc<Rollups>) -> Marginal {
        Marginal {
            rollups,
            on: AtomicBool::new(false),
        }
    }

    /// Starts counting: remote workers may now be granted keys.
    pub fn serve(&self) {
        self.on.store(true, Ordering::Release);
    }

    fn on(&self) -> bool {
        self.on.load(Ordering::Acquire)
    }

    fn add(&self, at: i64, worker: WorkerId, key: i64) {
        self.rollups
            .add_dim(at, metric::WORKER_LEASES, worker.0, key, 1);
    }

    /// A key was first granted (harvested) to `lease.holder`.
    pub fn granted(&self, lease: &Lease) {
        if self.on() {
            self.add(lease.discovered_at, lease.holder, key::GRANTED);
        }
    }

    /// The lease ended in a commit, at `now` (Unix seconds).
    pub fn committed(&self, lease: &Lease, now: i64) {
        self.ended(lease, key::COMMITTED, now);
    }

    /// The lease ended in a terminal loss, at `now`.
    pub fn lost(&self, lease: &Lease, now: i64) {
        self.ended(lease, key::LOST, now);
    }

    fn ended(&self, lease: &Lease, which: i64, now: i64) {
        if !self.on() {
            return;
        }
        self.add(now, lease.holder, which);
        if let Some(finder) = lease.finder {
            self.add(lease.discovered_at, finder, key::MARGINAL);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lodestar_cluster::{Claim, Claimed, Token};
    use lodestar_metainfo::InfoHash;
    use lodestar_sources::{FunnelCounts, Loss};
    use lodestar_store::{Counter, SourceId};

    use crate::coordinator::Coordinator;

    const AT: i64 = 3600 * 100 + 5;

    fn lease(holder: u32, finder: Option<u32>) -> Lease {
        Lease {
            holder: WorkerId(holder),
            token: Token(1),
            cohort: AT / 3600,
            source: SourceId::new(1).unwrap(),
            sources: 2,
            strategy: 0,
            discovered_at: AT,
            finder: finder.map(WorkerId),
        }
    }

    /// Rows as (hour, worker, key, value), summed and sorted.
    fn rows(r: &Rollups) -> Vec<(i64, u32, i64, i64)> {
        let mut out: std::collections::BTreeMap<(i64, u32, i64), i64> = Default::default();
        for Counter {
            hour,
            metric,
            dim,
            key,
            value,
        } in r.drain()
        {
            assert_eq!(metric, metric::WORKER_LEASES);
            *out.entry((hour, dim, key)).or_default() += value;
        }
        out.into_iter().map(|((h, d, k), v)| (h, d, k, v)).collect()
    }

    /// Parity: before `serve`, nothing is counted.
    #[test]
    fn a_single_machine_counts_nothing() {
        let r = Arc::new(Rollups::default());
        let m = Marginal::new(r.clone());
        m.granted(&lease(3, Some(3)));
        m.committed(&lease(3, Some(3)), AT);
        m.lost(&lease(0, None), AT);
        assert!(r.is_empty());
    }

    /// Granted and marginal land at the discovery hour, the end at its
    /// own; marginal goes to the finder, the end to the holder.
    #[test]
    fn counts_land_on_their_worker_and_hour() {
        let r = Arc::new(Rollups::default());
        let m = Marginal::new(r.clone());
        m.serve();
        m.granted(&lease(3, Some(3)));
        // Re-granted to 4 after 3 went silent, and committed by 4.
        m.committed(&lease(4, Some(3)), AT + 7200);
        // Another worker claimed it: no marginal.
        m.lost(&lease(0, None), AT + 7200);
        let h = AT / 3600;
        assert_eq!(
            rows(&r),
            vec![
                (h, 3, key::GRANTED, 1),
                (h, 3, key::MARGINAL, 1),
                (h + 2, 0, key::LOST, 1),
                (h + 2, 4, key::COMMITTED, 1),
            ]
        );
    }

    /// Through the coordinator: many leases granted locally and remotely,
    /// a fifth claimed by another worker, B's expired and taken over by A,
    /// the rest committed or lost. Each worker's grants and ends
    /// reconcile, and marginal is exactly the finds nobody else claimed.
    #[test]
    fn leases_through_the_coordinator_reconcile() {
        let r = Arc::new(Rollups::default());
        let c = Coordinator::with_rollups(1, 60, Arc::new(FunnelCounts::default()), r.clone());
        c.marginal.serve();
        let claim = |n: u32| Claim {
            key: InfoHash({
                let mut b = [0; 20];
                b[..4].copy_from_slice(&n.to_be_bytes());
                b
            }),
            source: SourceId::new(1).unwrap(),
            strategy: 0,
            at: AT,
        };
        let (a, b) = (WorkerId(3), WorkerId(4));
        let all = [WorkerId::LOCAL, a, b];
        let mut alone = [0i64; 3];
        let mut held = Vec::new();
        for i in 0..3000u32 {
            let n = (i % 3) as usize;
            let w = all[n];
            let Claimed::Granted(t) = c.claim(w, claim(i), AT) else {
                panic!("a new key is granted")
            };
            if i % 5 == 0 {
                c.claim(all[(n + 1) % 3], claim(i), AT);
            } else if w != b {
                alone[n] += 1;
            }
            held.push((i, w, t));
        }
        // B goes silent; A takes its keys over by finding them.
        c.heartbeat(a, AT + 61);
        assert!(c.expire(AT + 61) > 0);
        for (i, w, t) in held {
            let key = claim(i).key;
            if w == b {
                let Claimed::Granted(t) = c.claim(a, claim(i), AT + 62) else {
                    panic!("a waiting key is re-granted")
                };
                assert!(c.lost_for(a, &key, t, Loss::NoPeers));
            } else if i % 2 == 0 {
                assert!(c.complete_for(w, &key, t).is_some());
            } else {
                assert!(c.lost_for(w, &key, t, Loss::TimedOut));
            }
        }
        let got = rows(&r);
        let sum = |w: WorkerId, k: i64| -> i64 {
            got.iter()
                .filter(|(_, d, kk, _)| *d == w.0 && *kk == k)
                .map(|r| r.3)
                .sum()
        };
        for (n, w) in all.iter().enumerate() {
            assert_eq!(sum(*w, key::GRANTED), 1000, "{w:?}");
            assert_eq!(sum(*w, key::MARGINAL), alone[n], "{w:?}");
        }
        let ended: i64 = all
            .iter()
            .map(|&w| sum(w, key::COMMITTED) + sum(w, key::LOST))
            .sum();
        assert_eq!(ended, 3000);
        assert_eq!(sum(b, key::COMMITTED) + sum(b, key::LOST), 0);
    }
}
