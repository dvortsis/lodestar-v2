//! Time for the cluster protocol: one clock for leases, heartbeats, clock
//! offsets and certificate checks, so tests can move it by hand.

use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rustls::pki_types::UnixTime;
use rustls::time_provider::TimeProvider;

/// Milliseconds since the Unix epoch.
pub trait Clock: Send + Sync + fmt::Debug {
    fn now_ms(&self) -> i64;

    fn now_secs(&self) -> i64 {
        self.now_ms().div_euclid(1000)
    }
}

/// The system clock.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now_ms(&self) -> i64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
    }
}

/// A clock that only moves when told to.
#[derive(Debug)]
pub struct ManualClock(AtomicI64);

impl ManualClock {
    pub fn new(ms: i64) -> ManualClock {
        ManualClock(AtomicI64::new(ms))
    }

    /// Starts at the system clock's current time.
    pub fn starting_now() -> ManualClock {
        ManualClock::new(SystemClock.now_ms())
    }

    pub fn set_ms(&self, ms: i64) {
        self.0.store(ms, Ordering::SeqCst);
    }

    pub fn advance(&self, by: Duration) {
        let ms = i64::try_from(by.as_millis()).unwrap_or(i64::MAX);
        self.0.fetch_add(ms, Ordering::SeqCst);
    }
}

impl Clock for ManualClock {
    fn now_ms(&self) -> i64 {
        self.0.load(Ordering::SeqCst)
    }
}

/// Another clock, a fixed number of milliseconds off: a worker whose
/// clock is wrong.
#[derive(Debug)]
pub struct OffsetClock {
    pub base: Arc<dyn Clock>,
    pub offset_ms: i64,
}

impl Clock for OffsetClock {
    fn now_ms(&self) -> i64 {
        self.base.now_ms().saturating_add(self.offset_ms)
    }
}

/// A [`Clock`] as rustls' certificate-validity time.
#[derive(Debug, Clone)]
pub struct ClockTime(pub Arc<dyn Clock>);

impl TimeProvider for ClockTime {
    fn current_time(&self) -> Option<UnixTime> {
        let secs = u64::try_from(self.0.now_secs()).ok()?;
        Some(UnixTime::since_unix_epoch(Duration::from_secs(secs)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manual_clocks_move_only_when_told() {
        let clock = Arc::new(ManualClock::new(5_000));
        assert_eq!((clock.now_ms(), clock.now_secs()), (5_000, 5));
        clock.advance(Duration::from_millis(1_500));
        assert_eq!(clock.now_secs(), 6);
        let skewed = OffsetClock {
            base: clock.clone(),
            offset_ms: -7_000,
        };
        assert_eq!(skewed.now_ms(), -500);
        assert_eq!(skewed.now_secs(), -1, "rounds down below zero");
        let time = ClockTime(clock.clone()).current_time().unwrap();
        assert_eq!(time.as_secs(), 6);
        assert!(ClockTime(Arc::new(skewed)).current_time().is_none());
    }

    #[test]
    fn the_system_clock_is_after_2020() {
        assert!(SystemClock.now_secs() > 1_577_836_800);
    }
}
