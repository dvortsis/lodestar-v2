//! The state files the supervisor keeps (`active`, `pending`, `skipped`,
//! `halted.json`), what the child reports (`probation.json`,
//! `<version>/migration.json`), and probation's accounting as a pure
//! function of them and the clock.
//!
//! - **Attempts:** one is charged in `pending` (fsynced) before each
//!   start. It's refunded only when the child exited after a stop the
//!   supervisor forwarded, or after a valid `EXIT_RESTART`. A hard kill
//!   in between leaves it charged.
//! - **Migration:** while `probation.json` says `migrating`, the child
//!   beats every 10 s. Beats that stop for 5 minutes fail the attempt, and
//!   so does a migration longer than [`Limits::migration_cap`].
//! - **The deadline:** `used_ms` adds up the time spent starting (not
//!   migrating) across starts; past 5 minutes in all, the attempt fails.

use std::time::Duration;

use serde::{Deserialize, Serialize};

/// `active`: the version to run, and the one before it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Active {
    pub version: String,
    /// A version, or `"image"` (the supervisor's own code).
    pub previous: String,
}

pub const IMAGE: &str = "image";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    /// A new version's first runs.
    Install,
    /// The previous version, started again after a rollback.
    Restored,
}

/// `pending`: a version in probation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Pending {
    pub format: u32,
    pub version: String,
    pub previous: String,
    pub kind: Kind,
    pub attempts: u32,
    /// Time spent starting (not migrating), over every attempt.
    pub used_ms: u64,
    /// `index/`'s entries before the switch: a rollback removes the rest.
    pub index_before: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Phase {
    Migrating,
    Starting,
}

/// `probation.json`, the child's heartbeat.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Probation {
    pub run_id: String,
    /// The version reporting (a baseline carries over between its
    /// attempts only).
    #[serde(default)]
    pub version: String,
    pub phase: Phase,
    /// The last beat (Unix seconds).
    pub at: u64,
    /// When the migration began, while migrating.
    #[serde(default)]
    pub since: u64,
    pub from: i64,
    pub to: i64,
    #[serde(default)]
    pub baseline_max_id: Option<i64>,
    /// Torrents stored since `baseline_max_id`, updated every minute.
    #[serde(default)]
    pub stored_since: Option<u64>,
}

/// `<version>/migration.json`: this version migrated the store.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Migration {
    pub from: i64,
    pub to: i64,
    pub started_at: u64,
    /// The `pre-v` snapshots that existed before this migration: a
    /// rollback never restores one of these (an earlier upgrade's, or a
    /// copy `lodestar rollback` kept), only the one this migration took.
    #[serde(default)]
    pub before: Vec<String>,
}

/// One `skipped` entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Skip {
    pub version: String,
    /// `failed` (written by the supervisor) or `user` (the updater's).
    pub reason: String,
}

/// `halted.json`: a restored version failed too; nothing more is tried.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Halted {
    pub failed: String,
    pub previous: String,
    pub at: u64,
}

pub const PENDING_FORMAT: u32 = 1;
/// Attempts before a rollback (or, for a restored version, a halt).
pub const ATTEMPTS: u32 = 3;

/// Probation's clocks (shortened by the debug-only test overrides).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// Total time starting, over every attempt.
    pub deadline: Duration,
    /// Beats that stop this long fail a migration.
    pub stall: Duration,
    /// The longest a migration may take (§2.19: a new number, 6 h).
    pub migration_cap: Duration,
    /// SIGTERM, then this long, then SIGKILL.
    pub grace: Duration,
}

impl Default for Limits {
    fn default() -> Limits {
        Limits {
            deadline: Duration::from_secs(5 * 60),
            stall: Duration::from_secs(5 * 60),
            migration_cap: Duration::from_secs(6 * 3600),
            grace: Duration::from_secs(30),
        }
    }
}

/// What probation says about a running child, checked every second.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// Keep waiting.
    Wait,
    /// The child cleared `pending`: it passed.
    Passed,
    /// Stop the child and count a failed attempt.
    Fail(&'static str),
}

/// One check of a running child in probation. `started_ms` is when this
/// attempt began (supervisor clock), `now_ms` now; `wall` the Unix time
/// for the child's beats; `used_ms` what earlier attempts spent starting;
/// `migrating_ms` how much of this attempt went to migrating so far.
pub fn judge(
    pending_exists: bool,
    probation: Option<&Probation>,
    run_id: &str,
    used_ms: u64,
    starting_ms: u64,
    wall: u64,
    limits: &Limits,
) -> Verdict {
    if !pending_exists {
        return Verdict::Passed;
    }
    let ours = probation.filter(|p| p.run_id == run_id);
    if let Some(p) = ours.filter(|p| p.phase == Phase::Migrating) {
        if wall.saturating_sub(p.at) >= limits.stall.as_secs() {
            return Verdict::Fail("the migration stopped making progress");
        }
        if wall.saturating_sub(p.since) >= limits.migration_cap.as_secs() {
            return Verdict::Fail("the migration ran past its time limit");
        }
        return Verdict::Wait;
    }
    if used_ms.saturating_add(starting_ms) >= limits.deadline.as_millis() as u64 {
        return Verdict::Fail("it didn't pass its self-test in time");
    }
    Verdict::Wait
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(phase: Phase, at: u64, since: u64) -> Probation {
        Probation {
            run_id: "r".into(),
            version: String::new(),
            phase,
            at,
            since,
            from: 9,
            to: 10,
            baseline_max_id: None,
            stored_since: None,
        }
    }

    const L: Limits = Limits {
        deadline: Duration::from_secs(300),
        stall: Duration::from_secs(300),
        migration_cap: Duration::from_secs(6 * 3600),
        grace: Duration::from_secs(30),
    };

    #[test]
    fn a_long_migration_that_keeps_beating_passes_the_deadline() {
        // An hour in, still beating ten seconds ago: wait, whatever
        // starting time has accrued, since migrating doesn't count.
        let mig = p(Phase::Migrating, 10_000 - 10, 10_000 - 3600);
        assert_eq!(
            judge(true, Some(&mig), "r", 0, 0, 10_000, &L),
            Verdict::Wait
        );
        // Beats stopped five minutes ago: fail.
        let stalled = p(Phase::Migrating, 10_000 - 300, 10_000 - 400);
        assert!(matches!(
            judge(true, Some(&stalled), "r", 0, 0, 10_000, &L),
            Verdict::Fail(_)
        ));
        // Past the cap, even beating: fail.
        let capped = p(Phase::Migrating, 30_000, 30_000 - 6 * 3600);
        assert!(matches!(
            judge(true, Some(&capped), "r", 0, 0, 30_000, &L),
            Verdict::Fail(_)
        ));
    }

    #[test]
    fn the_deadline_adds_up_across_attempts() {
        let s = p(Phase::Starting, 0, 0);
        assert_eq!(
            judge(true, Some(&s), "r", 200_000, 99_000, 0, &L),
            Verdict::Wait
        );
        assert!(matches!(
            judge(true, Some(&s), "r", 200_000, 100_000, 0, &L),
            Verdict::Fail(_)
        ));
        // No heartbeat at all (a child hanging before it writes one) counts
        // as starting; another run's file is ignored.
        assert!(matches!(
            judge(true, None, "r", 0, 300_000, 0, &L),
            Verdict::Fail(_)
        ));
        let other = Probation {
            run_id: "x".into(),
            ..p(Phase::Migrating, 0, 0)
        };
        assert!(matches!(
            judge(true, Some(&other), "r", 0, 300_000, 0, &L),
            Verdict::Fail(_)
        ));
        assert_eq!(judge(false, None, "r", 0, 900_000, 0, &L), Verdict::Passed);
    }
}
