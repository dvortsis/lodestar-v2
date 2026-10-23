//! Debug-only test overrides (`LODESTAR_UPDATE_TEST`, JSON), for
//! `tests/supervisor.rs` on the real binary. Every accessor returns `None`
//! unless `cfg(debug_assertions)`, so none of this reaches a release
//! build:
//!
//! - `trust_root`: TEST ONLY public keys replacing the embedded ones;
//! - `version`: a pretend version for the supervisor's own build;
//! - `target`: the target overlays are verified for;
//! - `probation_ms`, `stall_ms`, `migration_cap_ms`, `grace_ms`: short
//!   probation clocks;
//! - `fault`, a comma-separated list: `journal:<k>` exits right after
//!   journal step k (a crash), `journal:slow` makes each step take 2 s,
//!   and `child:<what>@<v>` for the child running version v: `crash` on
//!   start, `hang` without confirming, fail its `selftest`, `stall` (start
//!   migrating, then stop beating), `migrate=<s>` (migrate for s seconds
//!   while beating), `restart` (ask for a supervised restart),
//!   `switch=<version>` (ask to switch), `rollback` (ask to roll back),
//!   `orphan` (leave a grandchild), `unrecorded` (start migrating, then
//!   fail to write `migration.json`);
//! - `beat_ms`: the migration heartbeat's period.

use std::time::Duration;

use serde::Deserialize;

use super::pending::Limits;

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Overrides {
    pub trust_root: Option<TrustRoot>,
    pub version: Option<String>,
    pub target: Option<String>,
    pub probation_ms: Option<u64>,
    pub stall_ms: Option<u64>,
    pub migration_cap_ms: Option<u64>,
    pub grace_ms: Option<u64>,
    pub beat_ms: Option<u64>,
    pub fault: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrustRoot {
    pub recovery: String,
    pub release: Option<String>,
    pub nightly: Option<String>,
    pub freshness: Option<String>,
    /// Key statements this "build" embeds.
    #[serde(default)]
    pub statements: Vec<lodestar_update::SignedStatement>,
}

pub const ENV: &str = "LODESTAR_UPDATE_TEST";

#[cfg(debug_assertions)]
fn get() -> Option<&'static Overrides> {
    static O: std::sync::OnceLock<Option<Overrides>> = std::sync::OnceLock::new();
    O.get_or_init(|| {
        let text = std::env::var(ENV).ok()?;
        match serde_json::from_str(&text) {
            Ok(o) => Some(o),
            Err(e) => {
                eprintln!("{ENV}: {e}");
                None
            }
        }
    })
    .as_ref()
}

#[cfg(not(debug_assertions))]
fn get() -> Option<&'static Overrides> {
    None
}

pub fn trust_root() -> Option<&'static TrustRoot> {
    get()?.trust_root.as_ref()
}

pub fn version() -> Option<&'static str> {
    get()?.version.as_deref()
}

pub fn target() -> Option<lodestar_update::Target> {
    lodestar_update::Target::from_triple(get()?.target.as_deref()?)
}

/// Probation's clocks, shortened where asked.
pub fn limits() -> Limits {
    let mut l = Limits::default();
    if let Some(o) = get() {
        let ms = Duration::from_millis;
        l.deadline = o.probation_ms.map_or(l.deadline, ms);
        l.stall = o.stall_ms.map_or(l.stall, ms);
        l.migration_cap = o.migration_cap_ms.map_or(l.migration_cap, ms);
        l.grace = o.grace_ms.map_or(l.grace, ms);
    }
    l
}

/// The migration heartbeat's period (10 s).
pub fn beat() -> Duration {
    Duration::from_millis(get().and_then(|o| o.beat_ms).unwrap_or(10_000))
}

/// The faults asked for (`fault` is a comma-separated list).
fn faults() -> impl Iterator<Item = &'static str> {
    get()
        .and_then(|o| o.fault.as_deref())
        .unwrap_or("")
        .split(',')
        .filter(|f| !f.is_empty())
}

/// `journal:<k>`: exits at once after step k, as a crash would.
pub fn fault(kind: &str, step: u8) {
    let want = format!("{kind}:{step}");
    if faults().any(|f| f == want) {
        eprintln!("supervisor: test fault {kind}:{step}");
        std::process::exit(86);
    }
}

/// `journal:slow`: each journal step takes 2 s (for a stop mid-rollback).
pub fn slow(kind: &str) {
    let want = format!("{kind}:slow");
    if faults().any(|f| f == want) {
        std::thread::sleep(Duration::from_secs(2));
    }
}

/// `child:<what>@<version>` for the child running `version`.
pub fn child_fault(version: &str) -> Option<&'static str> {
    faults().find_map(|f| {
        let (what, v) = f.strip_prefix("child:")?.split_once('@')?;
        (v == version).then_some(what)
    })
}
