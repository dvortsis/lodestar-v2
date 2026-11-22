//! `/updates` for `serve --fixture --fixture-variant updates-on`
//! (X-fixture-updates-on): a fixed, offline answer, so the Updates screen's
//! "on" states (a release available, automatic installs paused) run against
//! the real server without anything that reaches out.
//!
//! - **Nothing reaches out:** there's no release source, download source or
//!   client here. The release is a fixed record in this file, never fetched.
//! - **Nothing is verified or installed:** no key and no verifier is
//!   involved. `capability.reason` is `fixture`, so install is never offered.
//!   Check, install, skip, rollback and clearing history answer the existing
//!   `409 updates_unavailable` (`fixture`), as a default fixture's do.
//! - **Pausing works:** `pause` holds `paused_until` in memory, so the
//!   paused state is reachable by clicking. A restart forgets it.
//! - **The policy is the saved settings**, as the engine's is: the variant's
//!   default mode is Notify (`fixture.rs`), and a saved change shows here.
//!
//! [`crate::update::start`] uses it only while [`super::updates_on`] says
//! so: a fixture of this variant, which plain `serve` never is.

use std::sync::Mutex;

use lodestar_api::ops::BoxFuture;
use lodestar_api::updates::{
    Blocked, Capability, CheckState, CheckStatus, Policy, ReleaseInfo, RollbackInfo, Running,
    UpdatesApi, UpdatesError, UpdatesStatus, UpdatesUnavailable,
};
use lodestar_update::doc::{Level as ReleaseLevel, Release};
use lodestar_update::semver::Version;
use tokio::sync::watch;

use crate::settings::updates::{Mode, Updates};
use crate::update::policy;

/// The sample release's age at the fixture's anchor: two days old.
const RELEASE_AGE: i64 = 2 * 86_400;
/// The sample release's notes.
const NOTES: &str = "A sample release: this server runs on sample data and installs nothing.";

/// The fixture's update state.
pub struct Sample {
    /// The running version, as the engine reports it.
    version: String,
    /// When the fixture was built: the last check's time.
    anchor: i64,
    settings: watch::Receiver<Updates>,
    paused_until: Mutex<Option<i64>>,
    clock: fn() -> i64,
}

impl std::fmt::Debug for Sample {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Sample")
            .field("version", &self.version)
            .field("anchor", &self.anchor)
            .finish_non_exhaustive()
    }
}

impl Sample {
    pub fn new(version: String, anchor: i64, settings: watch::Receiver<Updates>) -> Sample {
        Sample::with_clock(version, anchor, settings, crate::logging::unix_now)
    }

    fn with_clock(
        version: String,
        anchor: i64,
        settings: watch::Receiver<Updates>,
        clock: fn() -> i64,
    ) -> Sample {
        Sample {
            version,
            anchor,
            settings,
            paused_until: Mutex::new(None),
            clock,
        }
    }

    /// `/system`'s Updates row: on unless the mode is Off, checked at the anchor.
    pub fn summary(&self) -> (bool, Option<i64>) {
        (self.settings.borrow().mode != Mode::Off, Some(self.anchor))
    }

    /// The running version, or 0.0.0 if it doesn't parse.
    fn running(&self) -> Version {
        Version::parse(&self.version).unwrap_or_else(|_| Version::new(0, 0, 0))
    }

    /// The one release the fixture offers: the next minor version after this one.
    fn release_version(&self) -> String {
        let v = self.running();
        Version::new(v.major, v.minor + 1, 0).to_string()
    }

    /// The sample release, as a manifest would carry it. Never fetched,
    /// never verified, and with no artifacts: nothing here can install.
    fn release(&self) -> Release {
        Release {
            version: Version::new(self.running().major, self.running().minor + 1, 0),
            date: u64::try_from(self.anchor - RELEASE_AGE).unwrap_or(0),
            level: ReleaseLevel::Feature,
            security: false,
            manual_steps: false,
            min_from: None,
            withdrawn: false,
            migrates: false,
            reindex: false,
            reindex_bytes: 0,
            notes: NOTES.into(),
            notes_url: None,
            artifacts: vec![],
        }
    }

    /// Why the sample release doesn't install automatically: the real
    /// policy's reasons for these settings (mode, level, wait, pause), and
    /// always `capability` (`fixture`), so it's never eligible.
    fn blocked_by(&self, settings: &Updates, now: i64, paused: Option<i64>) -> Vec<Blocked> {
        let release = [self.release()];
        let running = self.running();
        let plan = policy::plan(&policy::Inputs {
            settings,
            releases: &release,
            current: true,
            running: &running,
            skipped: &[],
            paused_until: paused,
            held_in_process: false,
            now,
            window_from: &|t| t,
            incapable: None,
        });
        let mut out: Vec<Blocked> = plan
            .releases
            .iter()
            .flat_map(|a| &a.blocked_by)
            .filter_map(|b| serde_json::to_value(b).ok())
            .filter_map(|v| serde_json::from_value(v).ok())
            .collect();
        out.push(Blocked {
            code: "capability".into(),
            until: None,
            via: None,
            reason: Some(UpdatesUnavailable::Fixture.code().into()),
        });
        out
    }

    fn paused(&self) -> Option<i64> {
        let now = (self.clock)();
        self.paused_until
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .filter(|&p| p > now)
    }

    fn status(&self) -> UpdatesStatus {
        let s = self.settings.borrow().clone();
        let paused = self.paused();
        let fixture = UpdatesUnavailable::Fixture.code().to_string();
        UpdatesStatus {
            unavailable: None,
            status: CheckStatus {
                state: CheckState::Ok,
                checked_at: Some(self.anchor),
                next_check_at: None,
                error: None,
            },
            running: Running {
                version: self.version.clone(),
                channel: s.channel.update().name().into(),
                image_version: None,
                overlay: false,
                revoked_key: false,
                withdrawn: false,
            },
            capability: Capability {
                can_install: false,
                reason: Some(fixture.clone()),
            },
            policy: Policy {
                mode: wire(&s.mode),
                level: wire(&s.level),
                security_fixes: s.security_effective(),
                wait_days: s.wait_days,
                window: s.window.clone(),
                window_zone: "UTC".into(),
                next_window_at: None,
                paused_until: paused,
                held_in_process: false,
                allow_reindex: s.allow_reindex,
            },
            releases: vec![ReleaseInfo {
                version: self.release_version(),
                date: self.anchor - RELEASE_AGE,
                // The release's level word (`fix`, `feature`, `major`), as
                // the engine sends it: not the policy's `features`.
                level: "feature".into(),
                security: false,
                manual_steps: false,
                migrates: false,
                reindex: false,
                reindex_bytes: 0,
                notes: NOTES.into(),
                notes_url: None,
                eligible: false,
                blocked_by: self.blocked_by(&s, (self.clock)(), paused),
                installs_at: None,
            }],
            pending: None,
            installing: None,
            rollback: RollbackInfo {
                available: false,
                reason: Some(fixture),
                to_version: None,
                lost_torrents: None,
                restores_store: None,
            },
            notices: vec![],
            history: vec![],
        }
    }

    fn refused<T: Send + 'static>(&self) -> BoxFuture<'_, Result<T, UpdatesError>> {
        Box::pin(async { Err(UpdatesError::Unavailable(UpdatesUnavailable::Fixture)) })
    }
}

/// A settings enum's wire name (`notify`, `fixes`, …), as the engine sends it.
fn wire<T: serde::Serialize>(v: &T) -> String {
    serde_json::to_value(v)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_default()
}

impl UpdatesApi for Sample {
    fn status(&self) -> BoxFuture<'_, UpdatesStatus> {
        let s = Sample::status(self);
        Box::pin(async move { s })
    }

    fn check(&self) -> BoxFuture<'_, Result<UpdatesStatus, UpdatesError>> {
        self.refused()
    }

    fn install(
        &self,
        _version: String,
        _when: lodestar_api::updates::InstallWhen,
        _confirm_manual: bool,
    ) -> BoxFuture<'_, Result<UpdatesStatus, UpdatesError>> {
        self.refused()
    }

    fn skip(&self, _version: String) -> BoxFuture<'_, Result<UpdatesStatus, UpdatesError>> {
        self.refused()
    }

    /// Held in memory: `/updates/pause` already allows only 0, 1, 7 or 30.
    fn pause(&self, days: u32) -> BoxFuture<'_, Result<UpdatesStatus, UpdatesError>> {
        let until = (days > 0).then(|| (self.clock)() + i64::from(days) * 86_400);
        *self.paused_until.lock().unwrap_or_else(|e| e.into_inner()) = until;
        let s = Sample::status(self);
        Box::pin(async move { Ok(s) })
    }

    fn clear_history(&self) -> BoxFuture<'_, Result<UpdatesStatus, UpdatesError>> {
        self.refused()
    }

    fn rollback(&self) -> BoxFuture<'_, Result<UpdatesStatus, UpdatesError>> {
        self.refused()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ANCHOR: i64 = 1_800_000_000;
    fn now() -> i64 {
        ANCHOR + 3600
    }

    fn sample(mode: Mode) -> (Sample, watch::Sender<Updates>) {
        let (tx, rx) = watch::channel(Updates {
            mode,
            ..Updates::default()
        });
        (Sample::with_clock("0.5.3".into(), ANCHOR, rx, now), tx)
    }

    /// The variant's answer: on, checked at the anchor, one release
    /// available, nothing installable, no rollback, and every write but
    /// pause the default fixture's `409 updates_unavailable` (`fixture`).
    #[tokio::test]
    async fn the_sample_is_on_offers_a_release_and_installs_nothing() {
        let (s, _tx) = sample(Mode::Notify);
        let json = serde_json::to_value(UpdatesApi::status(&s).await).unwrap();
        assert_eq!(json["unavailable"], serde_json::Value::Null);
        assert_eq!(json["status"]["state"], "ok");
        assert_eq!(json["status"]["checked_at"], ANCHOR);
        assert_eq!(json["status"]["error"], serde_json::Value::Null);
        assert_eq!(json["policy"]["mode"], "notify");
        assert_eq!(json["policy"]["level"], "fixes");
        assert_eq!(json["capability"]["can_install"], false);
        assert_eq!(json["capability"]["reason"], "fixture");
        assert_eq!(json["rollback"]["available"], false);
        assert_eq!(json["rollback"]["reason"], "fixture");
        let releases = json["releases"].as_array().unwrap();
        assert_eq!(releases.len(), 1);
        assert_eq!(releases[0]["version"], "0.6.0");
        assert_eq!(releases[0]["date"], ANCHOR - RELEASE_AGE);
        assert_eq!(releases[0]["level"], "feature", "a release level word");
        assert_eq!(releases[0]["eligible"], false);
        assert_eq!(releases[0]["notes_url"], serde_json::Value::Null);
        // The real policy's reasons for the defaults (Notify, Fixes, a
        // 3-day wait after a 2-day-old release), then the fixture's.
        let wait = ANCHOR - RELEASE_AGE + 3 * 86_400;
        assert_eq!(
            releases[0]["blocked_by"],
            serde_json::json!([
                {"code": "mode"},
                {"code": "level"},
                {"code": "wait", "until": wait},
                {"code": "capability", "reason": "fixture"},
            ])
        );
        assert_eq!(json["pending"], serde_json::Value::Null);
        assert_eq!(json["installing"], serde_json::Value::Null);
        let fixture = UpdatesError::Unavailable(UpdatesUnavailable::Fixture);
        assert_eq!(s.check().await.unwrap_err(), fixture);
        assert_eq!(
            s.install(
                "0.6.0".into(),
                lodestar_api::updates::InstallWhen::Now,
                true
            )
            .await
            .unwrap_err(),
            fixture
        );
        assert_eq!(s.skip("0.6.0".into()).await.unwrap_err(), fixture);
        assert_eq!(s.rollback().await.unwrap_err(), fixture);
        assert_eq!(s.clear_history().await.unwrap_err(), fixture);
    }

    /// Pausing shows in the policy and blocks the release; 0 resumes. The
    /// settings would install it (Auto, every level, no wait), so only the
    /// pause and the fixture's capability block it.
    #[tokio::test]
    async fn pausing_and_resuming_show_without_timing() {
        let (s, tx) = sample(Mode::Auto);
        tx.send_modify(|u| {
            u.level = crate::settings::updates::Level::All;
            u.wait_days = 0;
        });
        let fixture = serde_json::json!({"code": "capability", "reason": "fixture"});
        let paused = serde_json::to_value(s.pause(7).await.unwrap()).unwrap();
        let until = now() + 7 * 86_400;
        assert_eq!(paused["policy"]["paused_until"], until);
        assert_eq!(
            paused["releases"][0]["blocked_by"],
            serde_json::json!([{"code": "paused", "until": until}, fixture])
        );
        assert_eq!(paused["releases"][0]["eligible"], false);
        let again = serde_json::to_value(UpdatesApi::status(&s).await).unwrap();
        assert_eq!(again["policy"]["paused_until"], until);
        let resumed = serde_json::to_value(s.pause(0).await.unwrap()).unwrap();
        assert_eq!(resumed["policy"]["paused_until"], serde_json::Value::Null);
        assert_eq!(
            resumed["releases"][0]["blocked_by"],
            serde_json::json!([fixture])
        );
        assert_eq!(resumed["releases"][0]["eligible"], false);
    }

    /// The policy follows the saved settings, and so does `/system`'s row.
    #[tokio::test]
    async fn the_policy_and_the_system_row_follow_the_settings() {
        let (s, tx) = sample(Mode::Notify);
        assert_eq!(s.summary(), (true, Some(ANCHOR)));
        tx.send_modify(|u| u.mode = Mode::Off);
        assert_eq!(s.summary(), (false, Some(ANCHOR)));
        let json = serde_json::to_value(UpdatesApi::status(&s).await).unwrap();
        assert_eq!(json["policy"]["mode"], "off");
    }

    /// An unparseable running version still gives a release, and never panics.
    #[test]
    fn the_release_follows_the_running_version() {
        let (_tx, rx) = watch::channel(Updates::default());
        let s = Sample::with_clock("1.9.4-nightly.3".into(), ANCHOR, rx.clone(), now);
        assert_eq!(s.release_version(), "1.10.0");
        let s = Sample::with_clock("not a version".into(), ANCHOR, rx, now);
        assert_eq!(s.release_version(), "0.1.0");
    }
}
