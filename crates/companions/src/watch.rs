//! Health for companions Lodestar doesn't supervise (plan
//! 2026-11-06-v06-companions CMP-5g, Q7, Q12; architecture §18.10): the
//! ones you added by detection or a URL ([`crate::guided::Record`]).
//!
//! - **Cadence (Q7):** every companion is checked when it's added, then
//!   every 30 s, through CMP-8a's [`Health`] adapter and the guarded
//!   client only, on the target its record names. Never a scan (Q12).
//! - **Debounced (Q7's "3 failures in a row"):** a companion seen healthy
//!   stays healthy until three checks in a row fail, so one slow answer
//!   doesn't stop the plugins that need it (CMP-10 restarts a plugin on
//!   every flip it's sent). The first check's result counts at once: with
//!   nothing seen yet there's nothing to keep.
//! - **What it reports:** [`Seen::Healthy`], [`Seen::Unhealthy`] (it
//!   answered, but not as a healthy companion), or [`Seen::Missing`]
//!   (nothing answered), which moves its dependents to "Needs X".
//!
//! Nothing a companion sends is kept: an adapter hands back a status from
//! a closed set. The loop runs until its list's sender is dropped or its
//! task is aborted.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::watch;
use tokio::task::JoinHandle;

use crate::adapter::{GuardedHttp, Health, HealthStatus, Target};
use crate::inputs::CompanionId;

/// How often each companion is checked (Q7).
pub const EVERY: Duration = Duration::from_secs(30);
/// Failed checks in a row before a healthy companion counts as down (Q7).
pub const FAILURES: u32 = 3;

/// What the loop last made of a companion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Seen {
    Healthy,
    /// It answered, but not as a healthy companion does.
    Unhealthy,
    /// Nothing answered.
    Missing,
}

/// One companion's debounce.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Tracker {
    seen: Option<Seen>,
    failures: u32,
}

impl Default for Tracker {
    fn default() -> Tracker {
        Tracker::new()
    }
}

impl Tracker {
    pub const fn new() -> Tracker {
        Tracker {
            seen: None,
            failures: 0,
        }
    }

    /// Takes one check's result and returns what the companion now counts
    /// as.
    pub fn observe(&mut self, status: HealthStatus) -> Seen {
        let now = match status {
            HealthStatus::Healthy => {
                self.failures = 0;
                Seen::Healthy
            }
            failed => {
                self.failures = self.failures.saturating_add(1);
                let down = match failed {
                    HealthStatus::Unreachable => Seen::Missing,
                    _ => Seen::Unhealthy,
                };
                match self.seen {
                    Some(Seen::Healthy) if self.failures < FAILURES => Seen::Healthy,
                    _ => down,
                }
            }
        };
        self.seen = Some(now);
        now
    }
}

/// The health adapter for a companion, if it has one (`detect::health` in
/// shipped builds; a fake in tests).
pub type Checker = Arc<dyn Fn(&CompanionId) -> Option<Box<dyn Health>> + Send + Sync>;

/// What the loop knows: each watched companion and what it's seen as.
pub type Watched = BTreeMap<CompanionId, Seen>;

/// Starts the loop over the companions `list` names, checking each every
/// `every` (shipped builds pass [`EVERY`]). The receiver starts empty; a
/// companion appears once its first check has run, and leaves when it
/// leaves the list. A companion with no health adapter is never listed.
pub fn spawn(
    http: Arc<dyn GuardedHttp>,
    checker: Checker,
    mut list: watch::Receiver<Vec<(CompanionId, Target)>>,
    every: Duration,
) -> (watch::Receiver<Watched>, JoinHandle<()>) {
    let (tx, rx) = watch::channel(Watched::new());
    let task = tokio::spawn(async move {
        let mut trackers: BTreeMap<CompanionId, (Target, Tracker)> = BTreeMap::new();
        let mut tick = tokio::time::interval(every);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            let wanted = list.borrow_and_update().clone();
            // A changed target starts its debounce afresh.
            trackers.retain(|id, (t, _)| wanted.iter().any(|(w, wt)| w == id && wt == t));
            for (id, t) in &wanted {
                trackers
                    .entry(id.clone())
                    .or_insert_with(|| (t.clone(), Tracker::new()));
            }
            let mut seen = Watched::new();
            for (id, (target, tracker)) in trackers.iter_mut() {
                let Some(health) = checker(id) else {
                    continue;
                };
                let status = health.check(http.as_ref(), target).await;
                seen.insert(id.clone(), tracker.observe(status));
            }
            tx.send_if_modified(|w| {
                let changed = *w != seen;
                *w = seen;
                changed
            });
            tokio::select! {
                _ = tick.tick() => {}
                r = list.changed() => if r.is_err() { return },
            }
        }
    });
    (rx, task)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_healthy_companion_survives_two_failures_then_goes_down() {
        let mut t = Tracker::new();
        assert_eq!(t.observe(HealthStatus::Healthy), Seen::Healthy);
        assert_eq!(t.observe(HealthStatus::Unreachable), Seen::Healthy);
        assert_eq!(t.observe(HealthStatus::Unhealthy), Seen::Healthy);
        assert_eq!(t.observe(HealthStatus::Unreachable), Seen::Missing);
        assert_eq!(t.observe(HealthStatus::Unreachable), Seen::Missing);
        // One answer brings it back.
        assert_eq!(t.observe(HealthStatus::Healthy), Seen::Healthy);
        assert_eq!(t.observe(HealthStatus::Unhealthy), Seen::Healthy);
        assert_eq!(t.observe(HealthStatus::Healthy), Seen::Healthy);
    }

    #[test]
    fn the_first_check_counts_at_once() {
        let mut t = Tracker::new();
        assert_eq!(t.observe(HealthStatus::Unreachable), Seen::Missing);
        let mut t = Tracker::new();
        assert_eq!(t.observe(HealthStatus::Unhealthy), Seen::Unhealthy);
        // Down, the kind follows the latest answer.
        assert_eq!(t.observe(HealthStatus::Unreachable), Seen::Missing);
    }
}
