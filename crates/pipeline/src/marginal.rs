//! Each worker's leases and its marginal gain, for Insights (architecture
//! §19.7; workers plan Q12, WK-15).
//!
//! The coordinator counts, per worker and hour, into
//! [`metric::WORKER_LEASES`] (dim = the worker id, the built-in worker 0):
//! - [`key::GRANTED`]: keys first granted to it (harvested, §19.3), at
//!   their discovery time, as the funnel's cohorts are;
//! - [`key::MARGINAL`]: the part of those that **no other worker claimed
//!   while the key was in flight** (Q12). Counted at the grant, and taken
//!   back from the same hour the first time another worker claims the key
//!   (held, or waiting for a re-grant). It approximates "keys no other
//!   worker would have seen": a key found again only after its commit is a
//!   rediscovery and doesn't count against it, and Insights labels it so;
//! - [`key::COMMITTED`] and [`key::LOST`]: its leases that ended in a
//!   commit or a terminal loss, at the hour they ended.
//!
//! **Counts only.** Nothing here names a torrent: the map below holds keys
//! in memory while they are in flight and is never written, so a private
//! source's or a remote-unverified key reaches Insights only as one of
//! these counts (WK-15's privacy rule).
//!
//! **Single-machine parity (§19.8):** nothing is counted and no key is held
//! until the first grant to a remote worker, which only a serving cluster
//! server makes. On a single machine the rollups gain no row. Keys granted
//! before it aren't tracked; they're counted only as they end.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use lodestar_cluster::{Lease, WorkerId};
use lodestar_metainfo::InfoHash;

use crate::rollups::{Rollups, metric};

/// [`metric::WORKER_LEASES`] keys.
pub mod key {
    /// Keys first granted to the worker.
    pub const GRANTED: i64 = 0;
    /// Granted keys no other worker claimed while they were in flight.
    pub const MARGINAL: i64 = 1;
    /// Leases that ended in a commit.
    pub const COMMITTED: i64 = 2;
    /// Leases that ended in a terminal loss.
    pub const LOST: i64 = 3;
}

/// Keys tracked at most. Far above what the fleet holds in flight (the
/// built-in pool's 200,000 plus each worker's pool cap); past it a grant
/// is still counted as granted, but not as marginal, so the view can only
/// under-count and never claims a gain it couldn't check. About 48 bytes a
/// key: 48 MiB at the cap.
pub const MAX_TRACKED: usize = 1 << 20;

/// A key in flight: who it was first granted to, at what time (the hour its
/// marginal count was added to), and whether another worker claimed it.
#[derive(Debug, Clone, Copy)]
struct InFlight {
    first: WorkerId,
    at: i64,
    shared: bool,
}

/// The per-worker lease counts (see the module docs).
#[derive(Debug)]
pub struct Marginal {
    rollups: Arc<Rollups>,
    on: AtomicBool,
    live: Mutex<HashMap<InfoHash, InFlight>>,
}

impl Marginal {
    pub fn new(rollups: Arc<Rollups>) -> Marginal {
        Marginal {
            rollups,
            on: AtomicBool::new(false),
            live: Mutex::new(HashMap::new()),
        }
    }

    fn on(&self) -> bool {
        self.on.load(Ordering::Acquire)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<InfoHash, InFlight>> {
        // Every operation leaves the map consistent.
        self.live.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn add(&self, at: i64, worker: WorkerId, key: i64, n: i64) {
        self.rollups
            .add_dim(at, metric::WORKER_LEASES, worker.0, key, n);
    }

    /// A key was first granted (harvested) to `lease.holder`.
    pub fn granted(&self, key: &InfoHash, lease: &Lease) {
        if lease.holder != WorkerId::LOCAL {
            self.on.store(true, Ordering::Release);
        }
        if !self.on() {
            return;
        }
        let (w, at) = (lease.holder, lease.discovered_at);
        self.add(at, w, key::GRANTED, 1);
        let mut live = self.lock();
        if live.len() < MAX_TRACKED || live.contains_key(key) {
            live.insert(
                *key,
                InFlight {
                    first: w,
                    at,
                    shared: false,
                },
            );
            drop(live);
            self.add(at, w, key::MARGINAL, 1);
        }
    }

    /// `claimer` claimed a key already in flight (held by another worker,
    /// or waiting for a re-grant). The first time a worker other than the
    /// one it was first granted to does, that worker's marginal count for
    /// the key is taken back.
    pub fn claimed_elsewhere(&self, key: &InfoHash, claimer: WorkerId) {
        if !self.on() {
            return;
        }
        let taken = {
            let mut live = self.lock();
            match live.get_mut(key) {
                Some(k) if !k.shared && k.first != claimer => {
                    k.shared = true;
                    Some((k.at, k.first))
                }
                _ => None,
            }
        };
        if let Some((at, first)) = taken {
            self.add(at, first, key::MARGINAL, -1);
        }
    }

    /// The lease ended in a commit, at `now` (Unix seconds).
    pub fn committed(&self, key: &InfoHash, lease: &Lease, now: i64) {
        self.ended(key, lease, key::COMMITTED, now);
    }

    /// The lease ended in a terminal loss, at `now`.
    pub fn lost(&self, key: &InfoHash, lease: &Lease, now: i64) {
        self.ended(key, lease, key::LOST, now);
    }

    fn ended(&self, key: &InfoHash, lease: &Lease, which: i64, now: i64) {
        if !self.on() {
            return;
        }
        self.lock().remove(key);
        self.add(now, lease.holder, which, 1);
    }

    /// Keys tracked now.
    pub fn tracked(&self) -> usize {
        self.lock().len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lodestar_cluster::Token;
    use lodestar_store::{Counter, SourceId};

    const AT: i64 = 3600 * 100 + 5;

    fn lease(holder: u32) -> Lease {
        Lease {
            holder: WorkerId(holder),
            token: Token(1),
            cohort: AT / 3600,
            source: SourceId::new(1).unwrap(),
            sources: 2,
            strategy: 0,
            discovered_at: AT,
        }
    }

    fn k(n: u8) -> InfoHash {
        InfoHash([n; 20])
    }

    /// Sums per (worker, key) of what was counted.
    fn sums(r: &Rollups) -> Vec<(u32, i64, i64)> {
        let mut out: std::collections::BTreeMap<(u32, i64), i64> = Default::default();
        for Counter {
            metric,
            dim,
            key,
            value,
            ..
        } in r.drain()
        {
            assert_eq!(metric, metric::WORKER_LEASES);
            *out.entry((dim, key)).or_default() += value;
        }
        out.into_iter()
            .filter(|(_, v)| *v != 0)
            .map(|((d, k), v)| (d, k, v))
            .collect()
    }

    /// Counting, after a first remote grant (whose rows are dropped).
    fn on() -> (Arc<Rollups>, Marginal) {
        let r = Arc::new(Rollups::default());
        let m = Marginal::new(r.clone());
        m.granted(&k(255), &lease(9));
        m.lost(&k(255), &lease(9), AT);
        r.drain();
        (r, m)
    }

    /// Parity: while only the built-in worker is granted keys, nothing is
    /// counted or held, whatever else happens.
    #[test]
    fn a_single_machine_counts_nothing() {
        let r = Arc::new(Rollups::default());
        let m = Marginal::new(r.clone());
        m.granted(&k(1), &lease(0));
        m.claimed_elsewhere(&k(1), WorkerId(3));
        m.committed(&k(1), &lease(0), AT);
        m.lost(&k(2), &lease(0), AT);
        assert!(r.is_empty());
        assert_eq!(m.tracked(), 0);
    }

    /// Q12: a key only its first worker claimed is marginal; one another
    /// worker also claimed isn't, however many times it's claimed again;
    /// the holder's own repeated claim takes nothing back.
    #[test]
    fn marginal_is_what_no_other_worker_claimed() {
        let (r, m) = on();
        m.granted(&k(1), &lease(3));
        m.granted(&k(2), &lease(3));
        m.granted(&k(3), &lease(0));
        m.claimed_elsewhere(&k(1), WorkerId(3));
        m.claimed_elsewhere(&k(2), WorkerId(4));
        m.claimed_elsewhere(&k(2), WorkerId(0));
        m.claimed_elsewhere(&k(2), WorkerId(4));
        m.claimed_elsewhere(&k(3), WorkerId(4));
        // Committed by worker 3; a re-granted one lost by worker 4.
        m.committed(&k(1), &lease(3), AT + 7200);
        m.lost(&k(2), &lease(4), AT + 7200);
        m.committed(&k(3), &lease(0), AT + 7200);
        assert_eq!(m.tracked(), 0, "an ended lease is forgotten");
        assert_eq!(
            sums(&r),
            vec![
                (0, key::GRANTED, 1),
                (0, key::COMMITTED, 1),
                (3, key::GRANTED, 2),
                (3, key::MARGINAL, 1),
                (3, key::COMMITTED, 1),
                (4, key::LOST, 1),
            ]
        );
    }

    /// The take-back lands in the grant's hour, so each hour nets out; the
    /// ends land at their own.
    #[test]
    fn counts_land_in_their_hours() {
        let (r, m) = on();
        m.granted(&k(1), &lease(3));
        m.claimed_elsewhere(&k(1), WorkerId(5));
        m.committed(&k(1), &lease(3), AT + 7200);
        let mut rows: Vec<(i64, i64, i64)> = r
            .drain()
            .into_iter()
            .map(|c| (c.hour, c.key, c.value))
            .collect();
        rows.sort();
        let h = AT / 3600;
        assert_eq!(
            rows,
            vec![
                (h, key::GRANTED, 1),
                (h, key::MARGINAL, 0),
                (h + 2, key::COMMITTED, 1)
            ]
        );
    }

    /// A claim for a key granted before counting began (or past the cap) changes
    /// nothing: no marginal was counted for it.
    #[test]
    fn an_untracked_key_takes_nothing_back() {
        let (r, m) = on();
        m.claimed_elsewhere(&k(9), WorkerId(4));
        assert!(r.is_empty());
        m.committed(&k(9), &lease(3), AT);
        assert_eq!(sums(&r), vec![(3, key::COMMITTED, 1)]);
    }

    /// The soak's rule: nothing grows per torrent. Through the coordinator,
    /// many leases granted locally and remotely, claimed elsewhere,
    /// expired and re-granted, committed and lost: each worker's counts
    /// reconcile, and the map is empty again once every lease has ended.
    #[test]
    fn churn_leaves_nothing_behind() {
        use crate::coordinator::Coordinator;
        use lodestar_cluster::{Claim, Claimed};
        use lodestar_sources::{FunnelCounts, Loss};

        let r = Arc::new(Rollups::default());
        let c = Coordinator::with_rollups(1, 60, Arc::new(FunnelCounts::default()), r.clone());
        let src = SourceId::new(1).unwrap();
        let claim = |n: u32| Claim {
            key: InfoHash({
                let mut b = [0; 20];
                b[..4].copy_from_slice(&n.to_be_bytes());
                b
            }),
            source: src,
            strategy: 0,
            at: AT,
        };
        let (a, b) = (WorkerId(3), WorkerId(4));
        let mut now = AT;
        for round in 0..20u32 {
            let mut held = Vec::new();
            for i in 0..500u32 {
                let n = round * 1000 + i;
                let w = [WorkerId::LOCAL, a, b][(i % 3) as usize];
                let Claimed::Granted(t) = c.claim(w, claim(n), now) else {
                    panic!("a new key is granted")
                };
                // A fifth are found again by the next worker.
                if i % 5 == 0 {
                    c.claim([a, b, WorkerId::LOCAL][(i % 3) as usize], claim(n), now);
                }
                held.push((n, w, t));
            }
            // Every key in flight (in the first round, all but the built-in
            // worker's first, granted before any remote grant).
            assert_eq!(c.marginal.tracked(), if round == 0 { 499 } else { 500 });
            // B goes silent: its leases expire and wait for a re-grant,
            // still in flight, then A takes them over by finding them.
            c.heartbeat(a, now + 61);
            assert!(c.expire(now + 61) > 0);
            for (n, w, t) in held {
                let key = claim(n).key;
                if w == b {
                    let Claimed::Granted(t) = c.claim(a, claim(n), now + 62) else {
                        panic!("a waiting key is re-granted")
                    };
                    assert!(c.lost_for(a, &key, t, Loss::NoPeers));
                } else if n % 2 == 0 {
                    assert!(c.complete_for(w, &key, t).is_some());
                } else {
                    assert!(c.lost_for(w, &key, t, Loss::TimedOut));
                }
            }
            assert_eq!(c.marginal.tracked(), 0, "round {round}");
            now += 3600;
        }
        let rows = sums(&r);
        let get = |w: WorkerId, k: i64| {
            rows.iter()
                .find(|(d, kk, _)| *d == w.0 && *kk == k)
                .map_or(0, |r| r.2)
        };
        let granted: i64 = [WorkerId::LOCAL, a, b]
            .iter()
            .map(|&w| get(w, key::GRANTED))
            .sum();
        let ended: i64 = [WorkerId::LOCAL, a, b]
            .iter()
            .map(|&w| get(w, key::COMMITTED) + get(w, key::LOST))
            .sum();
        // The very first grant (the built-in worker's) came before any
        // remote one, so it isn't counted; its end is.
        assert_eq!((granted, ended), (9_999, 10_000));
        // A took over every key of B's, by finding it while it waited:
        // B ended none, and none of B's is marginal. A fifth of the
        // others' were found by another worker too.
        assert_eq!(get(b, key::COMMITTED) + get(b, key::LOST), 0);
        for w in [WorkerId::LOCAL, a] {
            assert!(get(w, key::MARGINAL) < get(w, key::GRANTED), "{w:?}");
        }
        assert_eq!(get(b, key::MARGINAL), 0);
    }
}
