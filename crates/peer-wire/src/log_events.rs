//! The metadata fetch's log events (architecture §12.1). A failed fetch
//! names the torrent (`Torrent`) and the peer (`Peer`): side by side they
//! are peer activity, shown only with that opt-in and never on the console.
//! At the fetch pool's peak (20,000 failures a second) a line each would
//! cost 23% of a core and hundreds of GiB a day, so `fetch.failed` is
//! `Trace` and a summary every minute is on by default instead (B-log1b's
//! `default_on` rule).
//!
//! Counting never blocks a fetch: the summary's counters are atomics, and
//! whichever failed fetch first finds a minute gone writes it. With no
//! failures there's no summary, so a quiet spell is folded into the next
//! one; `seconds` says how long it covers.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use crate::FailureKind;

lodestar_log::catalogue! {
    "lodestar-peer-wire";
    /// One transport's try at a torrent's metadata from a peer failed:
    /// `error` is the kind, `attempt` the transport (a uTP failure can
    /// still end in a TCP success).
    FETCH_FAILED {
        id: "fetch.failed",
        component: Fetch,
        level: Trace,
        default_on: false,
        message: "Metadata fetch failed",
        fields: { torrent: Hash, peer: Peer, error: Error, attempt: Label },
    }
    /// The fetches that failed since the last summary (`seconds` ago), by
    /// their final kind; a blocklisted address isn't counted.
    FAILED_SUMMARY {
        id: "fetch.failed_summary",
        component: Fetch,
        level: Info,
        message: "Metadata fetches that failed since the last summary",
        fields: { seconds: Size, unreachable: Size, timed_out: Size, refused: Size, encryption_failed: Size, invalid: Size, oversize: Size },
    }
}

/// How often the summary is written, at most.
pub const SUMMARY_EVERY_SECS: u64 = 60;

const KINDS: usize = 6;

/// Failures since the last summary, by kind, and when that was (seconds
/// since the first failure, plus one: 0 before it). A monotonic clock, so
/// a stepped wall clock can't stretch or stall the window. Aligned to its
/// own cache lines, away from other statics the fetch workers touch.
#[repr(align(128))]
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

/// Counts a failed fetch, and writes the summary when a minute has gone
/// by.
pub(crate) fn count(k: FailureKind) {
    static START: OnceLock<Instant> = OnceLock::new();
    let now = START.get_or_init(Instant::now).elapsed().as_secs() + 1;
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
        // Taken whether or not the summary is on, so turning it on later
        // starts from this minute.
        let n: [u64; KINDS] = std::array::from_fn(|i| self.failed[i].swap(0, Ordering::Relaxed));
        lodestar_log::log_event!(
            FAILED_SUMMARY,
            seconds = now - last,
            unreachable = n[0],
            timed_out = n[1],
            refused = n[2],
            encryption_failed = n[3],
            invalid = n[4],
            oversize = n[5]
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
