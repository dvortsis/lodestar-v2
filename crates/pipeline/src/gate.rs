//! The crawler gate's counters (owner-decisions §2.25): unique infohashes
//! whose info dictionary was fetched and verified from the network in
//! this run and committed as new, the same records once the search index
//! has committed them, and every excluded outcome on its own, so a
//! stricter filter can't pass for a faster crawler.
//!
//! - **Counted at commit points only:** the writer's transaction (new,
//!   already stored, filtered, quarantined, rediscovered, duplicate,
//!   store failed), the prepare stage's terminal losses (invalid
//!   metainfo, the floor's hash list) and the pool's terminal loss of a
//!   key to bad data (failed verification). Never per fetch attempt.
//! - **Searchable** is known only at the index's commit, which reports
//!   the highest id it made searchable ([`GateStats::searchable`]). The
//!   writer hands each new id and its first sighting over a bounded
//!   channel; the commit drains it into a bounded queue and counts the
//!   ids at or below its watermark. Memory never grows with the DHT: at
//!   either bound the latency measurement is dropped and counted
//!   ([`GateStats::dropped`]), but the record still counts as searchable
//!   once a commit passes the highest id dropped.
//! - **A record can be counted one index commit late:** one handed over
//!   while a commit is draining waits for the next commit that adds
//!   documents. Never twice, and never lost.
//! - **Observation only:** one relaxed atomic per event, and the writer
//!   never takes a lock for the gate (the channel is lock-free). The
//!   queue's lock is taken once per index commit, on the indexer's
//!   thread, and nothing else contends for it.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, TrySendError, sync_channel};
use std::sync::{Mutex, PoisonError};

/// Where a new verified record came from. Only the network today;
/// imports and recovery get their own origins when they're counted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Origin {
    Network = 0,
}

/// Every origin, with its label.
pub const ORIGINS: [(Origin, &str); 1] = [(Origin::Network, "network")];

/// An outcome that doesn't count towards the gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Excluded {
    /// Fetched and verified, but the store already had it.
    AlreadyStored = 0,
    /// A stored torrent seen again (not fetched).
    Rediscovered = 1,
    /// Dropped by a content filter rule or the floor's hash list.
    Filtered = 2,
    /// Held in quarantine by a content filter rule.
    Quarantined = 3,
    /// Hashed to the key but isn't usable metainfo.
    InvalidMetainfo = 4,
    /// The pool gave the key up on metadata that didn't hash to it on its
    /// final attempt (`FailureKind::HashMismatch`). A protocol or
    /// handshake violation is the same funnel loss but not counted here.
    FailedVerification = 5,
    /// A result under a lease that was no longer current: the key's
    /// current holder delivers it.
    Duplicate = 6,
    /// The store transaction failed; the batch was lost.
    StoreFailed = 7,
}

/// Every excluded outcome, with its `reason` label.
pub const EXCLUDED: [(Excluded, &str); 8] = [
    (Excluded::AlreadyStored, "already_stored"),
    (Excluded::Rediscovered, "rediscovered"),
    (Excluded::Filtered, "filtered"),
    (Excluded::Quarantined, "quarantined"),
    (Excluded::InvalidMetainfo, "invalid_metainfo"),
    (Excluded::FailedVerification, "failed_verification"),
    (Excluded::Duplicate, "duplicate"),
    (Excluded::StoreFailed, "store_failed"),
];

/// Discovery-to-searchable bucket bounds, in seconds (`le`). The
/// timestamps are whole unix seconds, so the lowest bucket is 1 s.
pub const LATENCY_BUCKETS: [u64; 12] = [1, 2, 5, 10, 30, 60, 120, 300, 600, 1800, 3600, 7200];

/// New ids handed from the writer and not yet drained.
const CHANNEL: usize = 4096;
/// New ids waiting for the index: 24 bytes each, about 1.5 MiB at most.
const WAITING: usize = 65_536;

/// A fixed-bucket histogram; each bucket counts its own range (the
/// render makes them cumulative).
#[derive(Debug, Default)]
pub struct Histogram {
    /// Per bound in [`LATENCY_BUCKETS`], then the overflow.
    buckets: [AtomicU64; LATENCY_BUCKETS.len() + 1],
    sum: AtomicU64,
}

impl Histogram {
    pub fn observe(&self, secs: u64) {
        let i = LATENCY_BUCKETS.partition_point(|&le| le < secs);
        self.buckets[i].fetch_add(1, Ordering::Relaxed);
        self.sum.fetch_add(secs, Ordering::Relaxed);
    }

    /// Cumulative counts per bound in [`LATENCY_BUCKETS`], then `+Inf`.
    pub fn cumulative(&self) -> [u64; LATENCY_BUCKETS.len() + 1] {
        let mut out = [0; LATENCY_BUCKETS.len() + 1];
        let mut total = 0;
        for (o, b) in out.iter_mut().zip(&self.buckets) {
            total += b.load(Ordering::Relaxed);
            *o = total;
        }
        out
    }

    pub fn sum(&self) -> u64 {
        self.sum.load(Ordering::Relaxed)
    }
}

/// A new record waiting to become searchable.
#[derive(Debug, Clone, Copy)]
struct Pending {
    id: i64,
    first_seen: i64,
    origin: Origin,
}

#[derive(Debug)]
struct Waiting {
    rx: Receiver<Pending>,
    queue: VecDeque<Pending>,
    limit: usize,
}

/// The gate's counters (see the module docs).
#[derive(Debug)]
pub struct GateStats {
    /// Unix seconds this run started: a key first seen in an earlier run
    /// (restored from the fetch queue) was first seen in this one then.
    started_at: i64,
    new_verified: [AtomicU64; ORIGINS.len()],
    searchable_new: [AtomicU64; ORIGINS.len()],
    excluded: [AtomicU64; EXCLUDED.len()],
    pub latency: Histogram,
    /// Measurements dropped because a bound was full.
    pub dropped: AtomicU64,
    /// Whether an index reports its commits; until one does, the writer
    /// hands nothing over (with search off nothing becomes searchable).
    following: AtomicBool,
    /// The highest id an index commit has reported searchable.
    through: AtomicI64,
    /// Records whose measurement was dropped, per origin, not yet
    /// counted searchable, and the highest id among them.
    dropped_pending: [AtomicU64; ORIGINS.len()],
    dropped_max_id: AtomicI64,
    tx: SyncSender<Pending>,
    waiting: Mutex<Waiting>,
}

impl Default for GateStats {
    fn default() -> Self {
        GateStats::new(0)
    }
}

impl GateStats {
    pub fn new(started_at: i64) -> GateStats {
        GateStats::with_bounds(started_at, CHANNEL, WAITING)
    }

    fn with_bounds(started_at: i64, channel: usize, waiting: usize) -> GateStats {
        let (tx, rx) = sync_channel(channel);
        GateStats {
            started_at,
            new_verified: Default::default(),
            searchable_new: Default::default(),
            excluded: Default::default(),
            latency: Histogram::default(),
            dropped: AtomicU64::new(0),
            following: AtomicBool::new(false),
            tx,
            waiting: Mutex::new(Waiting {
                rx,
                queue: VecDeque::with_capacity(waiting.min(1024)),
                limit: waiting,
            }),
            through: AtomicI64::new(i64::MIN),
            dropped_pending: Default::default(),
            dropped_max_id: AtomicI64::new(i64::MIN),
        }
    }

    pub fn new_verified(&self, origin: Origin) -> u64 {
        self.new_verified[origin as usize].load(Ordering::Relaxed)
    }

    pub fn searchable_new(&self, origin: Origin) -> u64 {
        self.searchable_new[origin as usize].load(Ordering::Relaxed)
    }

    pub fn excluded(&self, reason: Excluded) -> u64 {
        self.excluded[reason as usize].load(Ordering::Relaxed)
    }

    pub fn exclude(&self, reason: Excluded, n: u64) {
        if n > 0 {
            self.excluded[reason as usize].fetch_add(n, Ordering::Relaxed);
        }
    }

    /// The writer committed `id` as new at `now`; its key was first
    /// discovered at `discovered_at` (both unix seconds).
    pub fn committed_new(&self, origin: Origin, id: i64, discovered_at: i64, now: i64) {
        self.new_verified[origin as usize].fetch_add(1, Ordering::Relaxed);
        if !self.following.load(Ordering::Relaxed) {
            return;
        }
        let pending = Pending {
            id,
            first_seen: discovered_at.max(self.started_at),
            origin,
        };
        // The index may have committed it before the writer got here (it
        // follows SQLite, not this channel): searchable now.
        if id <= self.through.load(Ordering::Relaxed) {
            self.count_searchable(pending, now);
            return;
        }
        match self.tx.try_send(pending) {
            Ok(()) => {}
            Err(TrySendError::Full(p)) => self.drop_measurement(p),
            // The receiver lives in `self`.
            Err(TrySendError::Disconnected(_)) => unreachable!("gate receiver dropped"),
        }
    }

    /// From now on the writer hands new ids over for
    /// [`GateStats::searchable`]. Call it once an index reports commits.
    pub fn follow(&self) {
        self.following.store(true, Ordering::Relaxed);
    }

    /// An index commit made every id up to `through` searchable at `at`
    /// (unix seconds). Runs on the indexer's thread; it never waits on
    /// the writer.
    pub fn searchable(&self, through: i64, at: i64) {
        let mut w = self.waiting.lock().unwrap_or_else(PoisonError::into_inner);
        let Waiting { rx, queue, limit } = &mut *w;
        while let Ok(p) = rx.try_recv() {
            // The oldest gives way: the measurement is lost, not the
            // memory bound.
            if queue.len() == *limit
                && let Some(old) = queue.pop_front()
            {
                self.drop_measurement(old);
            }
            queue.push_back(p);
        }
        queue.retain(|&p| {
            if p.id > through {
                return true;
            }
            self.count_searchable(p, at);
            false
        });
        self.through.fetch_max(through, Ordering::Relaxed);
        // Dropped measurements still count, once the index passes them.
        // A record dropped while this runs may be counted one commit
        // early; never twice.
        if self.dropped_max_id.load(Ordering::Relaxed) <= through {
            for (pending, total) in self.dropped_pending.iter().zip(&self.searchable_new) {
                let n = pending.swap(0, Ordering::Relaxed);
                if n > 0 {
                    total.fetch_add(n, Ordering::Relaxed);
                }
            }
        }
    }

    /// The bound was full: the latency is lost, the count is kept.
    fn drop_measurement(&self, p: Pending) {
        self.dropped.fetch_add(1, Ordering::Relaxed);
        self.dropped_max_id.fetch_max(p.id, Ordering::Relaxed);
        self.dropped_pending[p.origin as usize].fetch_add(1, Ordering::Relaxed);
    }

    fn count_searchable(&self, p: Pending, at: i64) {
        self.searchable_new[p.origin as usize].fetch_add(1, Ordering::Relaxed);
        // A clock stepped back reads as 0, never as a huge wait.
        self.latency
            .observe(u64::try_from(at - p.first_seen).unwrap_or(0));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn latency_lands_in_the_first_bucket_at_or_above_it() {
        // Virtual time: the first sighting and the commit are given.
        let g = GateStats::new(1_000);
        g.follow();
        for (id, waited) in [
            (1, 0),
            (2, 1),
            (3, 2),
            (4, 3),
            (5, 3600),
            (6, 3601),
            (7, 99_999),
        ] {
            g.committed_new(Origin::Network, id, 1_000, 1_000);
            g.searchable(id, 1_000 + waited);
        }
        let c = g.latency.cumulative();
        let at = |le: u64| c[LATENCY_BUCKETS.iter().position(|&b| b == le).unwrap()];
        assert_eq!(at(1), 2, "0 s and 1 s");
        assert_eq!(at(2), 3);
        assert_eq!(at(5), 4);
        assert_eq!(at(1800), 4);
        assert_eq!(at(3600), 5);
        assert_eq!(at(7200), 6);
        assert_eq!(c[LATENCY_BUCKETS.len()], 7, "+Inf");
        assert_eq!(g.latency.sum(), 1 + 2 + 3 + 3600 + 3601 + 99_999);
        assert_eq!(g.searchable_new(Origin::Network), 7);
    }

    #[test]
    fn a_key_from_an_earlier_run_is_first_seen_at_this_runs_start() {
        let g = GateStats::new(5_000);
        g.follow();
        // Restored from the fetch queue: discovered in the last run.
        g.committed_new(Origin::Network, 1, 10, 5_001);
        g.searchable(1, 5_004);
        assert_eq!(g.latency.sum(), 4);
        // A clock stepped back reads 0.
        g.committed_new(Origin::Network, 2, 6_000, 6_000);
        g.searchable(2, 5_500);
        assert_eq!(g.latency.sum(), 4);
        assert_eq!(g.latency.cumulative()[0], 1);
    }

    #[test]
    fn only_ids_the_index_reached_are_searchable() {
        let g = GateStats::new(0);
        g.follow();
        for id in [3, 1, 2, 5] {
            g.committed_new(Origin::Network, id, 0, 0);
        }
        g.searchable(2, 10);
        assert_eq!(g.searchable_new(Origin::Network), 2);
        g.searchable(4, 10);
        assert_eq!(g.searchable_new(Origin::Network), 3);
        // Committed after the index already reached it: searchable at once.
        g.committed_new(Origin::Network, 4, 0, 7);
        assert_eq!(g.searchable_new(Origin::Network), 4);
        g.searchable(5, 10);
        assert_eq!(g.searchable_new(Origin::Network), 5);
        assert_eq!(g.new_verified(Origin::Network), 5);
        assert_eq!(g.latency.sum(), 10 * 4 + 7);
    }

    #[test]
    fn without_an_index_nothing_waits() {
        let g = GateStats::with_bounds(0, 1, 1);
        for id in 1..=10 {
            g.committed_new(Origin::Network, id, 0, 0);
        }
        assert_eq!(g.new_verified(Origin::Network), 10);
        assert_eq!(g.dropped.load(Ordering::Relaxed), 0);
        g.searchable(10, 1);
        assert_eq!(g.searchable_new(Origin::Network), 0);
    }

    #[test]
    fn both_bounds_drop_and_count() {
        let g = GateStats::with_bounds(0, 2, 3);
        g.follow();
        // The channel holds two; the third is dropped.
        for id in 1..=3 {
            g.committed_new(Origin::Network, id, 0, 0);
        }
        assert_eq!(g.dropped.load(Ordering::Relaxed), 1);
        g.searchable(0, 0);
        // Drained to the queue (2 of 3), then two more: the oldest gives way.
        for id in 4..=5 {
            g.committed_new(Origin::Network, id, 0, 0);
        }
        g.searchable(0, 0);
        assert_eq!(g.dropped.load(Ordering::Relaxed), 2);
        // Dropped ids 3 (channel) and 1 (evicted): counted only once a
        // commit passes the highest of them, without a latency.
        g.searchable(2, 0);
        assert_eq!(g.searchable_new(Origin::Network), 1, "id 2 only");
        g.searchable(5, 0);
        assert_eq!(g.searchable_new(Origin::Network), 5);
        assert_eq!(g.latency.cumulative()[LATENCY_BUCKETS.len()], 3);
        assert_eq!(g.new_verified(Origin::Network), 5);
        g.searchable(9, 0);
        assert_eq!(g.searchable_new(Origin::Network), 5, "counted once");
    }

    #[test]
    fn exclusions_count_by_reason() {
        let g = GateStats::default();
        for (i, (reason, _)) in EXCLUDED.iter().enumerate() {
            g.exclude(*reason, i as u64 + 1);
        }
        g.exclude(Excluded::Filtered, 0);
        for (i, (reason, _)) in EXCLUDED.iter().enumerate() {
            assert_eq!(g.excluded(*reason), i as u64 + 1, "{reason:?}");
        }
        assert_eq!(g.new_verified(Origin::Network), 0);
    }
}
