//! The metadata fetch's log events (architecture §12.1). A failed fetch
//! names the torrent (`Torrent`) and the peer (`Peer`): side by side they
//! are peer activity, shown only with that opt-in and never on the console.
//! At the fetch pool's peak (20,000 failures a second) a line each would
//! cost 23% of a core and hundreds of GiB a day, so `fetch.failed` is
//! `Trace` and a summary every minute is on by default instead (B-log1b's
//! `default_on` rule).
//!
//! Counting never blocks a fetch: the summary's counters are atomics, and
//! whichever failure first finds a minute gone writes it. With no failures
//! there's no summary.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::FailureKind;

lodestar_log::catalogue! {
    "lodestar-peer-wire";
    /// Fetching a torrent's metadata from a peer failed: `error` is the
    /// kind, `attempt` the transport tried.
    FETCH_FAILED {
        id: "fetch.failed",
        component: Fetch,
        level: Trace,
        default_on: false,
        message: "Metadata fetch failed",
        fields: { torrent: Hash, peer: Peer, error: Error, attempt: Label },
    }
    /// The fetches that failed in the last `seconds`, by kind.
    FAILED_SUMMARY {
        id: "fetch.failed_summary",
        component: Fetch,
        level: Info,
        message: "Metadata fetches that failed in the last minute",
        fields: { seconds: Size, unreachable: Size, timed_out: Size, refused: Size, encryption_failed: Size, invalid: Size, oversize: Size },
    }
}

/// How often the summary is written, at most.
pub const SUMMARY_EVERY_SECS: u64 = 60;

const KINDS: usize = 6;

/// Failures since the last summary, by kind, and when that was (Unix
/// seconds; 0 before the first failure).
struct Summary {
    failed: [AtomicU64; KINDS],
    since: AtomicU64,
}

static SUMMARY: Summary = Summary::new();

pub(crate) fn label(k: FailureKind) -> &'static str {
    match k {
        FailureKind::Unreachable => "unreachable",
        FailureKind::TimedOut => "timed_out",
        FailureKind::Refused => "refused",
        FailureKind::EncryptionFailed => "encryption_failed",
        FailureKind::Invalid => "invalid",
        FailureKind::Oversize => "oversize",
    }
}

fn index(k: FailureKind) -> usize {
    match k {
        FailureKind::Unreachable => 0,
        FailureKind::TimedOut => 1,
        FailureKind::Refused => 2,
        FailureKind::EncryptionFailed => 3,
        FailureKind::Invalid => 4,
        FailureKind::Oversize => 5,
    }
}

/// Counts a failure, and writes the summary when a minute has gone by.
pub(crate) fn count(k: FailureKind) {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    SUMMARY.count(k, now, SUMMARY_EVERY_SECS);
}

impl Summary {
    const fn new() -> Summary {
        Summary {
            failed: [const { AtomicU64::new(0) }; KINDS],
            since: AtomicU64::new(0),
        }
    }

    /// Counts a failure at `now`, and writes the summary if `every`
    /// seconds have passed since the last one: one caller wins the swap,
    /// the rest go on.
    fn count(&self, k: FailureKind, now: u64, every: u64) {
        self.failed[index(k)].fetch_add(1, Ordering::Relaxed);
        let last = self.since.load(Ordering::Relaxed);
        if last == 0 {
            let _ = self
                .since
                .compare_exchange(0, now, Ordering::Relaxed, Ordering::Relaxed);
            return;
        }
        if now.saturating_sub(last) < every
            || self
                .since
                .compare_exchange(last, now, Ordering::Relaxed, Ordering::Relaxed)
                .is_err()
        {
            return;
        }
        let n = |i: usize| self.failed[i].swap(0, Ordering::Relaxed);
        lodestar_log::log_event!(
            FAILED_SUMMARY,
            seconds = now - last,
            unreachable = n(0),
            timed_out = n(1),
            refused = n(2),
            encryption_failed = n(3),
            invalid = n(4),
            oversize = n(5)
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_summary_takes_the_counts_once_a_minute() {
        lodestar_log::check_ids(&[&CATALOGUE]).unwrap();
        let s = Summary::new();
        // The first failure starts the clock.
        s.count(FailureKind::TimedOut, 1_000, 60);
        assert_eq!(s.since.load(Ordering::Relaxed), 1_000);
        // Not a minute yet: counted, not written.
        s.count(FailureKind::TimedOut, 1_030, 60);
        assert_eq!(s.since.load(Ordering::Relaxed), 1_000);
        assert_eq!(s.failed[1].load(Ordering::Relaxed), 2);
        // A minute: written once, the counts taken.
        s.count(FailureKind::Refused, 1_061, 60);
        assert_eq!(s.since.load(Ordering::Relaxed), 1_061);
        assert!(s.failed.iter().all(|f| f.load(Ordering::Relaxed) == 0));
        for k in [
            FailureKind::Unreachable,
            FailureKind::TimedOut,
            FailureKind::Refused,
            FailureKind::EncryptionFailed,
            FailureKind::Invalid,
            FailureKind::Oversize,
        ] {
            assert!(!label(k).is_empty());
            assert!(index(k) < KINDS);
        }
    }
}
