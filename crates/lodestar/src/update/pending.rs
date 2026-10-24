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
//! - Every duration is the supervisor's own monotonic measure ([`Clock`]);
//!   the child's timestamps only show that a beat changed.

use std::io;
use std::time::Duration;

use lodestar_update::semver::Version;
use serde::{Deserialize, Serialize};

use super::layout::Bin;

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
    /// `failed` or `manual` (written by the supervisor: never installed
    /// again), or `user` (the updater's).
    pub reason: String,
}

/// `halted.json`: a restored version failed too, or a failed version
/// can't be rolled back; nothing more is tried.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Halted {
    pub failed: String,
    pub previous: String,
    pub at: u64,
    /// Why a rollback was refused; `None` when the restored version failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refused: Option<String>,
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

/// This attempt as the supervisor measured it, on its own monotonic
/// clock (never the child's timestamps, so a wall-clock jump can't fail
/// or extend an attempt).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Clock {
    /// What earlier attempts spent starting.
    pub used: Duration,
    /// What this attempt spent starting (not migrating) so far.
    pub starting: Duration,
    /// Since the heartbeat last changed (or migrating was first seen).
    pub since_beat: Duration,
    /// Since this attempt was first seen migrating.
    pub migrating: Duration,
}

/// One check of a running child in probation (`run_id` is this attempt's).
pub fn judge(
    pending_exists: bool,
    probation: Option<&Probation>,
    run_id: &str,
    clock: &Clock,
    limits: &Limits,
) -> Verdict {
    if !pending_exists {
        return Verdict::Passed;
    }
    let ours = probation.filter(|p| p.run_id == run_id);
    if ours.is_some_and(|p| p.phase == Phase::Migrating) {
        if clock.since_beat >= limits.stall {
            return Verdict::Fail("the migration stopped making progress");
        }
        if clock.migrating >= limits.migration_cap {
            return Verdict::Fail("the migration ran past its time limit");
        }
        return Verdict::Wait;
    }
    if clock.used.saturating_add(clock.starting) >= limits.deadline {
        return Verdict::Fail("it didn't pass its self-test in time");
    }
    Verdict::Wait
}

/// `pending`, refused (as damage) in a format this build doesn't know.
pub fn read(bin: &Bin) -> io::Result<Option<Pending>> {
    match bin.read::<Pending>("pending")? {
        Some(p) if p.format != PENDING_FORMAT => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "pending has format {}, which this build can't read",
                p.format
            ),
        )),
        other => Ok(other),
    }
}

/// The most `skipped` keeps (oldest dropped first).
pub const MAX_SKIPPED: usize = 64;

/// Whether the supervisor refuses `version` (it failed, or was rolled
/// back by hand). An unreadable `skipped` refuses everything.
pub fn is_skipped(bin: &Bin, version: &str) -> io::Result<bool> {
    let skipped: Vec<Skip> = bin.read("skipped")?.unwrap_or_default();
    Ok(skipped
        .iter()
        .any(|s| s.version == version && (s.reason == "failed" || s.reason == "manual")))
}

/// Adds `version` to `skipped` (a supervisor entry is never replaced by
/// a `user` one). An unreadable `skipped` is an error, not a fresh list,
/// so no earlier entry is lost.
pub fn skip(bin: &Bin, version: &str, reason: &str) -> io::Result<()> {
    let mut skipped: Vec<Skip> = bin.read("skipped")?.unwrap_or_default();
    let refusing = |s: &Skip| s.reason == "failed" || s.reason == "manual";
    if skipped
        .iter()
        .any(|s| s.version == version && (s.reason == reason || refusing(s)))
    {
        return Ok(());
    }
    skipped.retain(|s| s.version != version);
    skipped.push(Skip {
        version: version.to_string(),
        reason: reason.to_string(),
    });
    trim(&mut skipped);
    bin.write("skipped", &skipped)
}

/// Keeps at most [`MAX_SKIPPED`]: the oldest `user` skips go first, so a
/// version the supervisor refused (it failed, or needs manual steps)
/// stays refused.
fn trim(skipped: &mut Vec<Skip>) {
    let mut over = skipped.len().saturating_sub(MAX_SKIPPED);
    // Never the entry just added (the last).
    let newest = skipped.len().saturating_sub(1);
    let mut i = 0;
    skipped.retain(|s| {
        let drop = over > 0 && i < newest && s.reason == "user";
        over -= usize::from(drop);
        i += 1;
        !drop
    });
    let over = skipped.len().saturating_sub(MAX_SKIPPED);
    skipped.drain(..over);
}

/// A newer image makes the entries at or below its version moot: they
/// go. Anything unreadable is left as it is.
pub fn prune_skipped(bin: &Bin, image: &Version) {
    let Ok(Some(mut skipped)) = bin.read::<Vec<Skip>>("skipped") else {
        return;
    };
    let before = skipped.len();
    skipped.retain(|s| super::layout::parse_version(&s.version).is_none_or(|v| v > *image));
    if skipped.len() != before {
        let _ = bin.write("skipped", &skipped);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(phase: Phase) -> Probation {
        Probation {
            run_id: "r".into(),
            version: String::new(),
            phase,
            at: 0,
            since: 0,
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

    fn secs(s: u64) -> Duration {
        Duration::from_secs(s)
    }

    #[test]
    fn a_long_migration_that_keeps_beating_passes_the_deadline() {
        let mig = p(Phase::Migrating);
        // An hour in, a beat ten seconds ago: wait, whatever starting time
        // has accrued, since migrating doesn't count.
        let beating = Clock {
            used: secs(299),
            starting: secs(299),
            since_beat: secs(10),
            migrating: secs(3600),
        };
        assert_eq!(judge(true, Some(&mig), "r", &beating, &L), Verdict::Wait);
        // No beat for five minutes: fail.
        let stalled = Clock {
            since_beat: secs(300),
            ..beating
        };
        assert!(matches!(
            judge(true, Some(&mig), "r", &stalled, &L),
            Verdict::Fail(_)
        ));
        // Past the cap, even beating: fail.
        let capped = Clock {
            migrating: secs(6 * 3600),
            ..beating
        };
        assert!(matches!(
            judge(true, Some(&mig), "r", &capped, &L),
            Verdict::Fail(_)
        ));
    }

    #[test]
    fn the_deadline_adds_up_across_attempts() {
        let s = p(Phase::Starting);
        let at = |used, starting| Clock {
            used: Duration::from_millis(used),
            starting: Duration::from_millis(starting),
            ..Clock::default()
        };
        assert_eq!(
            judge(true, Some(&s), "r", &at(200_000, 99_000), &L),
            Verdict::Wait
        );
        assert!(matches!(
            judge(true, Some(&s), "r", &at(200_000, 100_000), &L),
            Verdict::Fail(_)
        ));
        // No heartbeat at all (a child hanging before it writes one) counts
        // as starting; another run's file is ignored.
        assert!(matches!(
            judge(true, None, "r", &at(0, 300_000), &L),
            Verdict::Fail(_)
        ));
        let other = Probation {
            run_id: "x".into(),
            ..p(Phase::Migrating)
        };
        assert!(matches!(
            judge(true, Some(&other), "r", &at(0, 300_000), &L),
            Verdict::Fail(_)
        ));
        assert_eq!(
            judge(false, None, "r", &at(0, 900_000), &L),
            Verdict::Passed
        );
    }

    /// At the cap, the oldest `user` skip goes before any version the
    /// supervisor refused.
    #[test]
    fn the_cap_evicts_user_skips_first() {
        let data = tempfile::tempdir().unwrap();
        let bin = Bin::open(data.path()).unwrap();
        skip(&bin, "0.1.0", "failed").unwrap();
        skip(&bin, "0.2.0", "user").unwrap();
        for i in 3..=MAX_SKIPPED {
            skip(&bin, &format!("0.{i}.0"), "failed").unwrap();
        }
        skip(&bin, "0.99.0", "failed").unwrap();
        let all: Vec<Skip> = bin.read("skipped").unwrap().unwrap();
        assert_eq!(all.len(), MAX_SKIPPED);
        assert!(
            all.iter().all(|s| s.reason == "failed"),
            "the user skip went"
        );
        assert_eq!(all[0].version, "0.1.0", "the oldest refusal stays");
    }

    #[test]
    fn skipped_is_capped_fails_closed_and_prunes_below_a_newer_image() {
        let data = tempfile::tempdir().unwrap();
        let bin = Bin::open(data.path()).unwrap();
        for i in 0..(MAX_SKIPPED + 5) {
            skip(&bin, &format!("0.{i}.0"), "failed").unwrap();
        }
        let all: Vec<Skip> = bin.read("skipped").unwrap().unwrap();
        assert_eq!(all.len(), MAX_SKIPPED);
        assert_eq!(all[0].version, "0.5.0", "the oldest went first");
        assert!(is_skipped(&bin, "0.68.0").unwrap());
        skip(&bin, "0.68.0", "user").unwrap();
        assert!(
            is_skipped(&bin, "0.68.0").unwrap(),
            "a user skip doesn't replace it"
        );
        skip(&bin, "0.69.0", "user").unwrap();
        assert!(!is_skipped(&bin, "0.69.0").unwrap());
        prune_skipped(&bin, &Version::parse("0.60.0").unwrap());
        let left: Vec<Skip> = bin.read("skipped").unwrap().unwrap();
        assert!(
            left.iter()
                .all(|s| Version::parse(&s.version).unwrap().minor > 60)
        );
        std::fs::write(bin.path("skipped"), "not json").unwrap();
        assert!(is_skipped(&bin, "0.61.0").is_err());
        assert!(skip(&bin, "0.61.0", "failed").is_err());
        assert_eq!(std::fs::read(bin.path("skipped")).unwrap(), b"not json");
    }

    #[test]
    fn pending_in_another_format_is_damage() {
        let data = tempfile::tempdir().unwrap();
        let bin = Bin::open(data.path()).unwrap();
        let mut pd = Pending {
            format: PENDING_FORMAT,
            version: "0.6.0".into(),
            previous: IMAGE.into(),
            kind: Kind::Install,
            attempts: 0,
            used_ms: 0,
            index_before: vec![],
        };
        bin.write("pending", &pd).unwrap();
        assert_eq!(read(&bin).unwrap(), Some(pd.clone()));
        pd.format = 2;
        bin.write("pending", &pd).unwrap();
        assert_eq!(read(&bin).unwrap_err().kind(), io::ErrorKind::InvalidData);
    }
}
