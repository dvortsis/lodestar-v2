//! What installs, and when (§12.2 "Policy", owner §2.13): a pure
//! function of the settings, the verified manifest, the running version,
//! `skipped`, the pause, the clock and the window, table-tested.
//!
//! - **Candidates:** not withdrawn, newer than the running version, not
//!   in `skipped`.
//! - **Automatic:** under `auto`, a release within `level`, without
//!   manual steps, allowed to reindex if it does, past its wait, while not
//!   paused, from a current manifest. **Security fixes** skip the level
//!   and the wait, and under `notify` they're the one automatic install
//!   (when "Always install security fixes" is in effect). Never under
//!   `off` or `image`, and never with manual steps.
//! - **Every automatic install waits for the window**, security fixes
//!   included; Install now skips the window and the wait, but never
//!   installs a withdrawn, skipped or unreachable release.
//! - **The upgrade path:** a release whose `min_from` is above the
//!   running version can't be reached directly; the newest release the
//!   running version meets goes first.
//! - **Capability:** when this installation can't update itself (the
//!   reason), automatic installs behave as Check and notify.

use lodestar_update::doc::{Level as ReleaseLevel, Release};
use lodestar_update::semver::Version;
use serde::Serialize;

use crate::settings::updates::{Level, Mode, Updates};

/// Why this installation can't install updates itself (§10.10.14's
/// "can't update itself" reasons), as the API reports them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Reason {
    /// Not the supervisor's child (an unsupervised `serve --child`).
    NotSupervised,
    /// A development build has no release version to update from.
    DevBuild,
    /// `<data>/bin` is on a `noexec` mount.
    Noexec,
    /// `<data>/bin` can't be written.
    NotWritable,
    /// A package manager owns this installation.
    PackageManaged,
    /// `mode` is `image`: the container image updates it.
    ImageMode,
    /// `trust.json` doesn't verify.
    TrustDamaged,
    /// No key is known for the channel (Nightly, until it exists).
    ChannelUnavailable,
    /// Installing arrives in part 5 of B-update.
    NotBuiltYet,
}

impl Reason {
    pub fn code(self) -> &'static str {
        match self {
            Reason::NotSupervised => "not_supervised",
            Reason::DevBuild => "dev_build",
            Reason::Noexec => "noexec",
            Reason::NotWritable => "not_writable",
            Reason::PackageManaged => "package_managed",
            Reason::ImageMode => "image_mode",
            Reason::TrustDamaged => "trust_damaged",
            Reason::ChannelUnavailable => "channel_unavailable",
            Reason::NotBuiltYet => "not_built_yet",
        }
    }
}

/// Why a candidate doesn't install automatically.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "code", rename_all = "snake_case")]
pub enum Block {
    /// The mode installs nothing automatically (`off`, `image`, or
    /// `notify` for anything but a security fix).
    Mode,
    /// Its level is above `level`.
    Level,
    /// It has manual steps: never automatic.
    ManualSteps,
    /// It rebuilds the index and `allow_reindex` is off.
    Reindex,
    /// It's waiting out `wait_days` after its date.
    Wait { until: i64 },
    /// Updates are paused.
    Paused { until: i64 },
    /// The manifest isn't current (expired, or not refreshed).
    NotCurrent,
    /// The running version is below its `min_from`: `via` goes first.
    UpgradePath { via: Option<String> },
    /// This installation can't install itself.
    Capability { reason: Reason },
}

/// One candidate as the policy sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Assessed {
    pub version: Version,
    pub security: bool,
    /// It installs automatically (at `installs_at`).
    pub eligible: bool,
    pub blocked_by: Vec<Block>,
    /// When an eligible release installs: its wait and any pause over,
    /// at the window's next opening.
    pub installs_at: Option<i64>,
}

/// What the policy decides.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Plan {
    /// Candidates, newest first.
    pub releases: Vec<Assessed>,
    /// The automatic install to make, and when: the soonest eligible
    /// release (the newest if several are due together).
    pub next: Option<(Version, i64)>,
}

/// Everything the policy reads.
pub struct Inputs<'a> {
    pub settings: &'a Updates,
    pub releases: &'a [Release],
    /// The manifest (or its freshness) hasn't expired.
    pub current: bool,
    pub running: &'a Version,
    /// Versions in `skipped`, whatever the reason.
    pub skipped: &'a [String],
    pub paused_until: Option<i64>,
    pub now: i64,
    /// The first moment at or after a time when the window is open.
    pub window_from: &'a dyn Fn(i64) -> i64,
    /// Why this installation can't install itself, if it can't.
    pub incapable: Option<Reason>,
}

/// The candidates and the next automatic install.
pub fn plan(i: &Inputs<'_>) -> Plan {
    let mut candidates: Vec<&Release> = i
        .releases
        .iter()
        .filter(|r| !r.withdrawn && r.version > *i.running)
        .filter(|r| !skipped(i, &r.version))
        .collect();
    candidates.sort_by(|a, b| b.version.cmp(&a.version));
    let reachable = |r: &Release| r.min_from.as_ref().is_none_or(|m| *i.running >= *m);
    let releases: Vec<Assessed> = candidates
        .iter()
        .map(|r| {
            // For the upgrade path: the newest reachable release below
            // this one, which installs first.
            let via = candidates
                .iter()
                .filter(|c| c.version < r.version && reachable(c))
                .map(|c| c.version.to_string())
                .next();
            assess(i, r, reachable(r), via)
        })
        .collect();
    let next = releases
        .iter()
        .filter_map(|a| a.installs_at.map(|at| (a.version.clone(), at)))
        .min_by(|(va, a), (vb, b)| a.cmp(b).then(vb.cmp(va)));
    Plan { releases, next }
}

/// Whether `skipped` names `version` (compared as versions, so another
/// spelling of the same one still counts).
fn skipped(i: &Inputs<'_>, version: &Version) -> bool {
    i.skipped
        .iter()
        .any(|s| Version::parse(s).is_ok_and(|v| v == *version))
}

/// Why this installation can't install: the caller's reason, or `image`
/// mode (the container image updates it), which the policy knows itself.
fn incapable(i: &Inputs<'_>) -> Option<Reason> {
    i.incapable
        .or((i.settings.mode == Mode::Image).then_some(Reason::ImageMode))
}

fn assess(i: &Inputs<'_>, r: &Release, reachable: bool, via: Option<String>) -> Assessed {
    let s = i.settings;
    let security = r.security && s.security_effective();
    let mut blocks = Vec::new();
    match s.mode {
        Mode::Auto => {}
        Mode::Notify if security => {}
        _ => blocks.push(Block::Mode),
    }
    if !security && !level_allows(s.level, r.level) {
        blocks.push(Block::Level);
    }
    if r.manual_steps {
        blocks.push(Block::ManualSteps);
    }
    if r.reindex && !s.allow_reindex {
        blocks.push(Block::Reindex);
    }
    let wait_until = if security {
        i.now
    } else {
        i64::try_from(r.date)
            .unwrap_or(i64::MAX)
            .saturating_add(i64::from(s.wait_days) * 86_400)
    };
    if wait_until > i.now {
        blocks.push(Block::Wait { until: wait_until });
    }
    let paused = i.paused_until.filter(|&p| p > i.now);
    if let Some(until) = paused {
        blocks.push(Block::Paused { until });
    }
    if !i.current {
        blocks.push(Block::NotCurrent);
    }
    if !reachable {
        blocks.push(Block::UpgradePath { via });
    }
    if let Some(reason) = incapable(i) {
        blocks.push(Block::Capability { reason });
    }
    // Waiting (out the wait or a pause) still installs, later; anything
    // else means it doesn't install automatically at all.
    let waits_only = blocks
        .iter()
        .all(|b| matches!(b, Block::Wait { .. } | Block::Paused { .. }));
    let installs_at = waits_only.then(|| {
        let from = wait_until.max(paused.unwrap_or(i.now)).max(i.now);
        (i.window_from)(from)
    });
    Assessed {
        version: r.version.clone(),
        security: r.security,
        eligible: waits_only,
        blocked_by: blocks,
        installs_at,
    }
}

/// fixes ≤ features ≤ all.
fn level_allows(setting: Level, release: ReleaseLevel) -> bool {
    match release {
        ReleaseLevel::Fix => true,
        ReleaseLevel::Feature => setting >= Level::Features,
        ReleaseLevel::Major => setting >= Level::All,
    }
}

/// Why Install now refuses `version`, if it does: it must be a
/// candidate (not withdrawn, newer, not skipped), reachable from the
/// running version, from a current manifest, on an installation that
/// can install (not in `image` mode). The window, the wait, the level and
/// a pause don't apply. Confirming a release's manual steps is the
/// route's check (`confirm_manual`), not this one's.
#[cfg_attr(not(test), allow(dead_code))] // `POST /updates/install` (part 5).
pub fn install_now(i: &Inputs<'_>, version: &Version) -> Result<(), Refusal> {
    let Some(r) = i.releases.iter().find(|r| r.version == *version) else {
        return Err(Refusal::UnknownVersion);
    };
    if skipped(i, version) {
        return Err(Refusal::Skipped);
    }
    if r.withdrawn || r.version <= *i.running {
        return Err(Refusal::NotCandidate);
    }
    if let Some(reason) = incapable(i) {
        return Err(Refusal::Incapable(reason));
    }
    if !i.current {
        return Err(Refusal::NotCurrent);
    }
    if r.min_from.as_ref().is_some_and(|m| *i.running < *m) {
        return Err(Refusal::UpgradePath);
    }
    Ok(())
}

/// Why Install now refuses.
#[cfg_attr(not(test), allow(dead_code))] // `POST /updates/install` (part 5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    UnknownVersion,
    Skipped,
    NotCandidate,
    Incapable(Reason),
    NotCurrent,
    UpgradePath,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::updates::Security;

    const DAY: i64 = 86_400;
    const NOW: i64 = 1_800_000_000;

    fn release(v: &str, date: i64, level: ReleaseLevel) -> Release {
        Release {
            version: Version::parse(v).unwrap(),
            date: date as u64,
            level,
            security: false,
            manual_steps: false,
            min_from: None,
            withdrawn: false,
            migrates: false,
            reindex: false,
            reindex_bytes: 0,
            notes: String::new(),
            notes_url: None,
            artifacts: vec![],
        }
    }

    /// The window opens on the hour at 03:00 UTC, an hour long.
    fn window(t: i64) -> i64 {
        let day = t.div_euclid(DAY) * DAY;
        let open = day + 3 * 3600;
        if t < open {
            open
        } else if t < open + 3600 {
            t
        } else {
            open + DAY
        }
    }

    struct Case {
        settings: Updates,
        releases: Vec<Release>,
        current: bool,
        skipped: Vec<String>,
        paused_until: Option<i64>,
        incapable: Option<Reason>,
    }

    impl Case {
        fn new(mode: Mode, releases: Vec<Release>) -> Case {
            Case {
                settings: Updates {
                    mode,
                    ..Updates::default()
                },
                releases,
                current: true,
                skipped: vec![],
                paused_until: None,
                incapable: None,
            }
        }

        fn plan(&self) -> Plan {
            plan(&self.inputs())
        }

        fn inputs(&self) -> Inputs<'_> {
            static RUNNING: std::sync::OnceLock<Version> = std::sync::OnceLock::new();
            Inputs {
                settings: &self.settings,
                releases: &self.releases,
                current: self.current,
                running: RUNNING.get_or_init(|| Version::new(0, 5, 0)),
                skipped: &self.skipped,
                paused_until: self.paused_until,
                now: NOW,
                window_from: &window,
                incapable: self.incapable,
            }
        }
    }

    fn codes(a: &Assessed) -> Vec<&'static str> {
        a.blocked_by
            .iter()
            .map(|b| match b {
                Block::Mode => "mode",
                Block::Level => "level",
                Block::ManualSteps => "manual_steps",
                Block::Reindex => "reindex",
                Block::Wait { .. } => "wait",
                Block::Paused { .. } => "paused",
                Block::NotCurrent => "not_current",
                Block::UpgradePath { .. } => "upgrade_path",
                Block::Capability { .. } => "capability",
            })
            .collect()
    }

    #[test]
    fn candidates_are_newer_not_withdrawn_and_not_skipped() {
        let mut withdrawn = release("0.7.0", NOW - 10 * DAY, ReleaseLevel::Fix);
        withdrawn.withdrawn = true;
        let mut c = Case::new(
            Mode::Auto,
            vec![
                release("0.4.0", NOW - 10 * DAY, ReleaseLevel::Fix),
                release("0.5.0", NOW - 10 * DAY, ReleaseLevel::Fix),
                release("0.5.1", NOW - 10 * DAY, ReleaseLevel::Fix),
                release("0.5.2", NOW - 10 * DAY, ReleaseLevel::Fix),
                withdrawn,
            ],
        );
        c.skipped = vec!["0.5.2".into()];
        let p = c.plan();
        let versions: Vec<String> = p.releases.iter().map(|a| a.version.to_string()).collect();
        assert_eq!(versions, ["0.5.1"]);
    }

    /// mode × level: what installs automatically, past its wait.
    #[test]
    fn the_mode_and_level_table() {
        use ReleaseLevel::*;
        let old = NOW - 10 * DAY;
        for (mode, level, release_level, eligible) in [
            (Mode::Auto, Level::Fixes, Fix, true),
            (Mode::Auto, Level::Fixes, Feature, false),
            (Mode::Auto, Level::Features, Feature, true),
            (Mode::Auto, Level::Features, Major, false),
            (Mode::Auto, Level::All, Major, true),
            (Mode::Notify, Level::All, Fix, false),
            (Mode::Off, Level::All, Fix, false),
            (Mode::Image, Level::All, Fix, false),
        ] {
            let mut c = Case::new(mode, vec![release("0.6.0", old, release_level)]);
            c.settings.level = level;
            let a = &c.plan().releases[0];
            assert_eq!(a.eligible, eligible, "{mode:?} {level:?} {release_level:?}");
            assert_eq!(a.installs_at.is_some(), eligible);
        }
    }

    /// Security fixes skip the level and the wait, and under notify
    /// they're the one automatic install; never under off or image.
    #[test]
    fn security_fixes() {
        let mut fix = release("0.6.0", NOW, ReleaseLevel::Major);
        fix.security = true;
        for (mode, security, eligible) in [
            (Mode::Auto, Security::Auto, true),
            (Mode::Notify, Security::Auto, false),
            (Mode::Notify, Security::On, true),
            (Mode::Auto, Security::Off, false),
            (Mode::Off, Security::On, false),
            (Mode::Image, Security::On, false),
        ] {
            let mut c = Case::new(mode, vec![fix.clone()]);
            c.settings.security_fixes = security;
            let a = &c.plan().releases[0];
            assert_eq!(
                a.eligible,
                eligible,
                "{mode:?} {security:?}: {:?}",
                codes(a)
            );
            if eligible {
                // No wait, but the window still holds (§2.13).
                assert_eq!(a.installs_at, Some(window(NOW)));
            }
        }
        // Manual steps are never automatic, security or not.
        let mut manual = fix.clone();
        manual.manual_steps = true;
        let c = Case::new(Mode::Auto, vec![manual]);
        assert_eq!(codes(&c.plan().releases[0]), ["manual_steps"]);
    }

    #[test]
    fn the_wait_and_the_pause_delay_the_window() {
        let fresh = release("0.6.0", NOW - DAY, ReleaseLevel::Fix);
        let c = Case::new(Mode::Auto, vec![fresh.clone()]);
        let a = &c.plan().releases[0];
        let until = NOW - DAY + 3 * DAY;
        assert_eq!(a.blocked_by, [Block::Wait { until }]);
        assert!(a.eligible);
        assert_eq!(a.installs_at, Some(window(until)));
        let mut c = Case::new(Mode::Auto, vec![fresh]);
        c.paused_until = Some(NOW + 7 * DAY);
        let a = &c.plan().releases[0];
        assert_eq!(codes(a), ["wait", "paused"]);
        assert_eq!(a.installs_at, Some(window(NOW + 7 * DAY)));
        // A pause that's over doesn't count.
        let mut c = Case::new(
            Mode::Auto,
            vec![release("0.6.0", NOW - 10 * DAY, ReleaseLevel::Fix)],
        );
        c.paused_until = Some(NOW - 1);
        assert!(c.plan().releases[0].blocked_by.is_empty());
    }

    #[test]
    fn manual_steps_reindex_currency_and_capability_block() {
        let old = NOW - 10 * DAY;
        let mut reindex = release("0.6.0", old, ReleaseLevel::Fix);
        reindex.reindex = true;
        let mut c = Case::new(Mode::Auto, vec![reindex]);
        assert!(c.plan().releases[0].eligible, "allowed by default");
        c.settings.allow_reindex = false;
        assert_eq!(codes(&c.plan().releases[0]), ["reindex"]);

        let mut c = Case::new(Mode::Auto, vec![release("0.6.0", old, ReleaseLevel::Fix)]);
        c.current = false;
        assert_eq!(codes(&c.plan().releases[0]), ["not_current"]);
        c.current = true;
        c.incapable = Some(Reason::Noexec);
        let a = &c.plan().releases[0];
        assert_eq!(
            a.blocked_by,
            [Block::Capability {
                reason: Reason::Noexec
            }]
        );
        assert!(c.plan().next.is_none(), "it behaves as check and notify");
    }

    #[test]
    fn the_upgrade_path_goes_through_the_newest_reachable_release() {
        let old = NOW - 10 * DAY;
        let mut far = release("0.9.0", old, ReleaseLevel::Fix);
        far.min_from = Some(Version::new(0, 6, 0));
        let c = Case::new(
            Mode::Auto,
            vec![
                far,
                release("0.6.0", old, ReleaseLevel::Fix),
                release("0.5.5", old, ReleaseLevel::Fix),
            ],
        );
        let p = c.plan();
        assert_eq!(
            p.releases[0].blocked_by,
            [Block::UpgradePath {
                via: Some("0.6.0".into())
            }]
        );
        assert_eq!(
            p.next.as_ref().map(|(v, _)| v.to_string()),
            Some("0.6.0".into())
        );
        assert_eq!(
            install_now(&c.inputs(), &Version::new(0, 9, 0)),
            Err(Refusal::UpgradePath)
        );
        assert_eq!(install_now(&c.inputs(), &Version::new(0, 6, 0)), Ok(()));
    }

    /// Install now skips the window, the wait, the level and a pause, but
    /// never a skipped (failed) or withdrawn version.
    #[test]
    fn install_now_skips_the_schedule_but_not_the_rules() {
        let mut withdrawn = release("0.7.0", NOW, ReleaseLevel::Major);
        withdrawn.withdrawn = true;
        let mut c = Case::new(
            Mode::Notify,
            vec![
                release("0.6.0", NOW, ReleaseLevel::Major),
                release("0.6.1", NOW, ReleaseLevel::Fix),
                withdrawn,
            ],
        );
        c.paused_until = Some(NOW + DAY);
        c.skipped = vec!["0.6.1".into()];
        let i = c.inputs();
        assert_eq!(install_now(&i, &Version::new(0, 6, 0)), Ok(()));
        assert_eq!(
            install_now(&i, &Version::new(0, 6, 1)),
            Err(Refusal::Skipped)
        );
        assert_eq!(
            install_now(&i, &Version::new(0, 7, 0)),
            Err(Refusal::NotCandidate)
        );
        assert_eq!(
            install_now(&i, &Version::new(0, 4, 0)),
            Err(Refusal::UnknownVersion)
        );
        let mut c2 = Case::new(Mode::Notify, vec![release("0.6.0", NOW, ReleaseLevel::Fix)]);
        c2.incapable = Some(Reason::NotSupervised);
        assert_eq!(
            install_now(&c2.inputs(), &Version::new(0, 6, 0)),
            Err(Refusal::Incapable(Reason::NotSupervised))
        );
    }

    /// `image` mode never installs, even when the caller found no reason.
    #[test]
    fn image_mode_installs_nothing_by_itself() {
        let c = Case::new(
            Mode::Image,
            vec![release("0.6.0", NOW - 10 * DAY, ReleaseLevel::Fix)],
        );
        assert!(
            c.plan().releases[0]
                .blocked_by
                .contains(&Block::Capability {
                    reason: Reason::ImageMode
                })
        );
        assert_eq!(
            install_now(&c.inputs(), &Version::new(0, 6, 0)),
            Err(Refusal::Incapable(Reason::ImageMode))
        );
    }

    #[test]
    fn the_next_install_is_the_soonest_newest_first() {
        let old = NOW - 10 * DAY;
        let c = Case::new(
            Mode::Auto,
            vec![
                release("0.6.0", old, ReleaseLevel::Fix),
                release("0.6.1", old, ReleaseLevel::Fix),
                release("0.6.2", NOW, ReleaseLevel::Fix),
            ],
        );
        let (v, at) = c.plan().next.unwrap();
        assert_eq!((v.to_string(), at), ("0.6.1".to_string(), window(NOW)));
    }
}
