//! The discovery-cohort funnel (architecture §7, §10.9): every key that's
//! new to us is counted once as harvested, then at each stage it reaches,
//! or once with the reason it was lost.

use std::collections::HashMap;
use std::sync::Mutex;

use lodestar_metainfo::InfoHash;
use lodestar_peer_wire::FailureKind;
use lodestar_store::{CohortCount, SourceId};

/// `cohort_hour.stage`. Persisted: append only.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum Stage {
    Harvested = 1,
    PeerFound = 2,
    Fetched = 3,
    Stored = 4,
    Searchable = 5,
    /// Left without metadata; the row's reason says why.
    Lost = 6,
}

/// `cohort_hour.reason` for [`Stage::Lost`]. Persisted: append only.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum Loss {
    /// Peer finding never turned up a peer.
    NoPeers = 1,
    TimedOut = 2,
    Refused = 3,
    EncryptionFailed = 4,
    Oversize = 5,
    Invalid = 6,
    /// Dropped by a blocklist before storage.
    Blocked = 7,
    /// Dropped from the full queue.
    Evicted = 8,
    /// Still waiting when the retry horizon passed.
    GaveUp = 9,
    /// Peers were found but none accepted a connection.
    Unreachable = 10,
}

impl From<FailureKind> for Loss {
    fn from(kind: FailureKind) -> Loss {
        match kind {
            FailureKind::Unreachable => Loss::Unreachable,
            FailureKind::TimedOut => Loss::TimedOut,
            FailureKind::Refused => Loss::Refused,
            FailureKind::EncryptionFailed => Loss::EncryptionFailed,
            FailureKind::Invalid => Loss::Invalid,
            FailureKind::Oversize => Loss::Oversize,
        }
    }
}

/// Where funnel transitions go. The pipeline flushes them to the store's
/// `cohort_hour` inside the writer's transaction.
pub trait Funnel: Send + Sync {
    fn record(&self, cohort: i64, source: SourceId, stage: Stage, loss: Option<Loss>);

    /// A new key entered the fetch pool: harvested. A worker whose
    /// coordinator counts harvests when it grants the lease (§19.3)
    /// overrides this to count nothing.
    fn admitted(&self, _key: &InfoHash, cohort: i64, source: SourceId) {
        self.record(cohort, source, Stage::Harvested, None);
    }

    /// A key left the pool without being fetched. A worker overrides this
    /// to report the outcome to the lease's holder of record.
    fn lost(&self, _key: &InfoHash, cohort: i64, source: SourceId, loss: Loss) {
        self.record(cohort, source, Stage::Lost, Some(loss));
    }
}

/// In-memory per-key accumulators, drained at each flush. A few hundred
/// keys per hour, so one short-held lock is plenty.
#[derive(Debug, Default)]
pub struct FunnelCounts {
    counts: Mutex<HashMap<(i64, SourceId, Stage, u8), i64>>,
}

impl FunnelCounts {
    pub fn drain(&self) -> Vec<CohortCount> {
        let counts = std::mem::take(&mut *self.counts.lock().expect("funnel lock"));
        let mut out: Vec<CohortCount> = counts
            .into_iter()
            .map(|((cohort, source, stage, reason), count)| CohortCount {
                cohort,
                source,
                stage: stage as u8,
                reason,
                count,
            })
            .collect();
        out.sort_by_key(|c| (c.cohort, c.source, c.stage, c.reason));
        out
    }

    /// The current count for one cell, for tests and the status page.
    pub fn get(&self, cohort: i64, source: SourceId, stage: Stage, loss: Option<Loss>) -> i64 {
        let key = (cohort, source, stage, loss.map_or(0, |l| l as u8));
        self.counts
            .lock()
            .expect("funnel lock")
            .get(&key)
            .copied()
            .unwrap_or(0)
    }

    /// The total across cohorts and sources, for tests and the status page.
    pub fn total(&self, stage: Stage, loss: Option<Loss>) -> i64 {
        let reason = loss.map_or(0, |l| l as u8);
        self.counts
            .lock()
            .expect("funnel lock")
            .iter()
            .filter(|((_, _, s, r), _)| *s == stage && *r == reason)
            .map(|(_, n)| n)
            .sum()
    }
}

impl Funnel for FunnelCounts {
    fn record(&self, cohort: i64, source: SourceId, stage: Stage, loss: Option<Loss>) {
        let key = (cohort, source, stage, loss.map_or(0, |l| l as u8));
        *self
            .counts
            .lock()
            .expect("funnel lock")
            .entry(key)
            .or_insert(0) += 1;
    }
}
