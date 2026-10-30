//! `/updates` (architecture §12.2, §10.10.14): the update engine's state,
//! its policy, the releases a signed check found, the notices and the
//! history, for the Updates screen.
//!
//! Release notes are signed text from the release manifest; the UI
//! renders them as plain text or sanitised Markdown, never as HTML.
//! Nothing here is a URL with a query, key material or a signature.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// Why this process has no update engine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
#[schema(example = "windows_helper")]
pub enum UpdatesUnavailable {
    /// A fixture: served offline, never checking.
    Fixture,
    /// The update client couldn't be built.
    Network,
    /// Windows: checking and installing come with the Windows helper
    /// (E-M6), so the screen says that instead of offering updates.
    WindowsHelper,
}

impl UpdatesUnavailable {
    /// Its wire name, as the error message and `capability.reason` use it.
    pub fn code(self) -> &'static str {
        match self {
            UpdatesUnavailable::Fixture => "fixture",
            UpdatesUnavailable::Network => "network",
            UpdatesUnavailable::WindowsHelper => "windows_helper",
        }
    }
}

/// The check's state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum CheckState {
    /// Not checked since start (or updates are off).
    Idle,
    Checking,
    /// The last check succeeded.
    Ok,
    /// The last check failed (`error` says why).
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct CheckStatus {
    #[schema(inline)]
    pub state: CheckState,
    /// Unix seconds of the last completed check.
    #[schema(required = true)]
    pub checked_at: Option<i64>,
    /// When the next scheduled check runs; null when updates are off.
    #[schema(required = true)]
    pub next_check_at: Option<i64>,
    /// The last check's error code (`no_release_source`, `network`,
    /// `redirect_refused`, `bad_signature`, …).
    #[schema(required = true)]
    pub error: Option<String>,
}

/// The version running now.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct Running {
    pub version: String,
    /// The update channel this installation follows.
    pub channel: String,
    /// The container image's (the supervisor's) own version, when it
    /// differs: "Image 0.5.2 · running 0.5.3 (updated in place)".
    #[schema(required = true)]
    pub image_version: Option<String>,
    /// It runs an installed update rather than the image's own code.
    pub overlay: bool,
    /// Its signing key was revoked after it was installed.
    pub revoked_key: bool,
    /// Its release has been withdrawn: Roll back is offered, never done
    /// silently.
    pub withdrawn: bool,
}

/// Whether this installation can install updates itself.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct Capability {
    pub can_install: bool,
    /// Why not: `not_supervised`, `dev_build`, `noexec`, `not_writable`,
    /// `package_managed`, `image_mode`, `trust_damaged`,
    /// `channel_unavailable` or `not_built_yet`; or, when `unavailable`
    /// is set, the same reason (`windows_helper`, …). Install
    /// automatically then behaves as Check and notify.
    #[schema(required = true)]
    pub reason: Option<String>,
}

/// The policy in effect, as the settings and the clock make it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct Policy {
    /// `off`, `notify`, `auto` or `image`.
    pub mode: String,
    /// `fixes`, `features` or `all`.
    pub level: String,
    /// Whether security fixes install automatically (the effective
    /// "Always install security fixes").
    pub security_fixes: bool,
    pub wait_days: u32,
    pub window: String,
    /// The zone the window is in: an IANA name, or `UTC` when none was
    /// found on the host.
    pub window_zone: String,
    /// When the window next opens (or now, while it's open).
    #[schema(required = true)]
    pub next_window_at: Option<i64>,
    /// Updates are paused until then: the saved pause only, never a
    /// pause that failed to save.
    #[schema(required = true)]
    pub paused_until: Option<i64>,
    /// A pause failed to save, so automatic installs are held in this
    /// process only, until a pause or resume is saved or Lodestar
    /// restarts. Releases then show `blocked_by` `held`.
    pub held_in_process: bool,
    pub allow_reindex: bool,
}

/// One reason a release doesn't install automatically.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct Blocked {
    /// `mode`, `level`, `manual_steps`, `reindex`, `wait`, `paused`,
    /// `held`, `not_current`, `upgrade_path` or `capability`.
    pub code: String,
    /// The time it lifts (`wait`, `paused`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub until: Option<i64>,
    /// The release to install first (`upgrade_path`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub via: Option<String>,
    /// The capability reason (`capability`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// A release newer than the running version.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ReleaseInfo {
    pub version: String,
    /// Unix seconds.
    pub date: i64,
    /// `fix`, `feature` or `major`.
    pub level: String,
    pub security: bool,
    pub manual_steps: bool,
    pub migrates: bool,
    pub reindex: bool,
    /// Bytes a reindex needs, estimated.
    pub reindex_bytes: u64,
    /// Signed text; render as plain text or sanitised Markdown.
    pub notes: String,
    #[schema(required = true)]
    pub notes_url: Option<String>,
    /// It installs automatically (at `installs_at`).
    pub eligible: bool,
    pub blocked_by: Vec<Blocked>,
    #[schema(required = true)]
    pub installs_at: Option<i64>,
}

/// A version in probation after an install or a rollback.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct PendingInstall {
    pub version: String,
    /// `install` or `restored`.
    pub kind: String,
    pub attempts: u32,
}

/// Whether Roll back is offered (§10.10.7's Danger zone).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct RollbackInfo {
    pub available: bool,
    /// `image` or a version.
    #[schema(required = true)]
    pub to_version: Option<String>,
    /// Torrents a rollback would lose, when the store would be restored;
    /// null when it can't be counted.
    #[schema(required = true)]
    pub lost_torrents: Option<u64>,
}

/// One update in `history.json`, newest last.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct HistoryEntry {
    pub version: String,
    pub from: String,
    #[schema(required = true)]
    pub level: Option<String>,
    pub at: i64,
    /// `installed`, `rolled_back`, `failed` or `skipped`.
    pub result: String,
    #[schema(required = true)]
    pub reason: Option<String>,
    #[schema(required = true)]
    pub lost_torrents: Option<u64>,
}

/// `GET /updates`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct UpdatesStatus {
    /// Why updates aren't available at all; null otherwise. When it's
    /// set, only `running.version` and this field mean anything: the
    /// policy shows the defaults, and there are no releases, history or
    /// rollback.
    #[schema(required = true)]
    pub unavailable: Option<UpdatesUnavailable>,
    pub status: CheckStatus,
    pub running: Running,
    pub capability: Capability,
    pub policy: Policy,
    /// Newer releases, newest first.
    pub releases: Vec<ReleaseInfo>,
    #[schema(required = true)]
    pub pending: Option<PendingInstall>,
    pub rollback: RollbackInfo,
    /// `revoked_key`, `clock_behind`, `clock_ahead`, `trust_damaged`,
    /// `pending_unwritable`, `bad_overlay`, `rolled_back`,
    /// `manifest_lapsed`, `channel_unavailable`, `check_not_saved` (the
    /// last check's times couldn't be saved and won't survive a
    /// restart), …
    pub notices: Vec<String>,
    pub history: Vec<HistoryEntry>,
}

/// `POST /updates/skip`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SkipRequest {
    pub version: String,
}

/// `POST /updates/pause`: 1, 7 or 30 days; 0 resumes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct PauseRequest {
    pub days: u32,
}
