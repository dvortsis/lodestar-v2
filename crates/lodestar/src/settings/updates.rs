//! Updates (§12.2, owner §2.13): off until chosen; then check and notify,
//! install automatically, or leave it to the container image. All live:
//! the update engine follows the section.
//!
//! - `security_fixes` is a sentinel (no field may be nullable): `auto` is
//!   on under `auto` and off under `notify`, and it never applies under
//!   `image` or `off`. `GET /updates` gives the effective value.
//! - `image` is offered only where the deployment is Docker or Unraid.
//! - The window is `HH:MM-HH:MM` in the host's local time, wrapping
//!   midnight if its end is earlier; a start equal to its end is refused.
//! - Notify me (`notify_*`) is stored; delivery waits for the
//!   notification channels.

use lodestar_api::system::DeploymentKind;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use super::{Auto, Danger, Field, Section};
use crate::config::{Config, SecurityFixes, UpdateChannel, UpdateLevel, UpdateMode};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    Off,
    Notify,
    Auto,
    Image,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    Fixes,
    Features,
    All,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum Security {
    Auto,
    On,
    Off,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum Channel {
    Stable,
    Beta,
    Nightly,
}

impl Channel {
    /// The update crate's channel (the engine's, Unix only).
    #[cfg_attr(not(unix), allow(dead_code))]
    pub fn update(self) -> lodestar_update::Channel {
        match self {
            Channel::Stable => lodestar_update::Channel::Stable,
            Channel::Beta => lodestar_update::Channel::Beta,
            Channel::Nightly => lodestar_update::Channel::Nightly,
        }
    }
}

/// The section. Enum fields are undocumented here so their schema stays
/// inline (no `oneOf` wrapper).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct Updates {
    #[schema(inline)]
    pub mode: Mode,
    #[schema(inline)]
    pub level: Level,
    #[schema(inline)]
    pub security_fixes: Security,
    #[schema(inline)]
    pub channel: Channel,
    /// Days a release waits after its date before it installs
    /// automatically (0–14). Security fixes don't wait.
    pub wait_days: u32,
    /// When automatic installs happen, local time, `HH:MM-HH:MM`.
    pub window: String,
    /// Install releases that rebuild the search index.
    pub allow_reindex: bool,
    /// Notify me before an automatic install. Stored; delivery comes
    /// with the notification channels.
    pub notify_before: bool,
    /// Notify me after an update.
    pub notify_after: bool,
    /// Notify me when an update fails or is rolled back.
    pub notify_failure: bool,
}

impl Default for Updates {
    fn default() -> Self {
        Updates::from_config(&Config::default())
    }
}

#[cfg(any(unix, test))]
impl Updates {
    /// Whether security fixes install automatically, as `security_fixes`
    /// and `mode` together have it.
    pub fn security_effective(&self) -> bool {
        match (self.mode, self.security_fixes) {
            (Mode::Off | Mode::Image, _) => false,
            (_, Security::On) => true,
            (_, Security::Off) => false,
            (m, Security::Auto) => m == Mode::Auto,
        }
    }
}

impl Section for Updates {
    const SECTION: &'static str = "updates";
    const FIELDS: &'static [Field] = &[
        Field::new("mode")
            .file("updates.mode")
            .env("LODESTAR_UPDATES")
            .group("updates")
            .widget("radio")
            .keywords(),
        Field::new("level")
            .file("updates.level")
            .env("LODESTAR_UPDATES_LEVEL")
            .group("updates")
            .widget("radio"),
        Field::new("security_fixes")
            .file("updates.security_fixes")
            .env("LODESTAR_UPDATES_SECURITY_FIXES")
            .group("updates")
            .auto(Auto::Str("auto")),
        Field::new("wait_days")
            .file("updates.wait_days")
            .env("LODESTAR_UPDATES_WAIT_DAYS")
            .range(0, 14)
            .group("schedule")
            .unit("days"),
        Field::new("window")
            .file("updates.window")
            .env("LODESTAR_UPDATES_WINDOW")
            .group("schedule")
            .format("time-range"),
        Field::new("allow_reindex")
            .file("updates.allow_reindex")
            .env("LODESTAR_UPDATES_ALLOW_REINDEX")
            .group("schedule")
            .advanced(),
        Field::new("channel")
            .file("updates.channel")
            .env("LODESTAR_UPDATES_CHANNEL")
            .group("updates")
            .advanced()
            .danger(Danger::Disruptive),
        Field::new("notify_before")
            .file("updates.notify_before")
            .env("LODESTAR_UPDATES_NOTIFY_BEFORE")
            .group("notify"),
        Field::new("notify_after")
            .file("updates.notify_after")
            .env("LODESTAR_UPDATES_NOTIFY_AFTER")
            .group("notify"),
        Field::new("notify_failure")
            .file("updates.notify_failure")
            .env("LODESTAR_UPDATES_NOTIFY_FAILURE")
            .group("notify"),
    ];

    fn from_config(config: &Config) -> Self {
        let u = &config.updates;
        Updates {
            mode: match u.mode {
                UpdateMode::Off => Mode::Off,
                UpdateMode::Notify => Mode::Notify,
                UpdateMode::Auto => Mode::Auto,
                UpdateMode::Image => Mode::Image,
            },
            level: match u.level {
                UpdateLevel::Fixes => Level::Fixes,
                UpdateLevel::Features => Level::Features,
                UpdateLevel::All => Level::All,
            },
            security_fixes: match u.security_fixes {
                SecurityFixes::Auto => Security::Auto,
                SecurityFixes::On => Security::On,
                SecurityFixes::Off => Security::Off,
            },
            channel: match u.channel {
                UpdateChannel::Stable => Channel::Stable,
                UpdateChannel::Beta => Channel::Beta,
                UpdateChannel::Nightly => Channel::Nightly,
            },
            wait_days: u.wait_days,
            window: u.window.clone(),
            allow_reindex: u.allow_reindex,
            notify_before: u.notify_before,
            notify_after: u.notify_after,
            notify_failure: u.notify_failure,
        }
    }

    fn apply(&self, field: &str, config: &mut Config) {
        let u = &mut config.updates;
        match field {
            "mode" => {
                u.mode = match self.mode {
                    Mode::Off => UpdateMode::Off,
                    Mode::Notify => UpdateMode::Notify,
                    Mode::Auto => UpdateMode::Auto,
                    Mode::Image => UpdateMode::Image,
                }
            }
            "level" => {
                u.level = match self.level {
                    Level::Fixes => UpdateLevel::Fixes,
                    Level::Features => UpdateLevel::Features,
                    Level::All => UpdateLevel::All,
                }
            }
            "security_fixes" => {
                u.security_fixes = match self.security_fixes {
                    Security::Auto => SecurityFixes::Auto,
                    Security::On => SecurityFixes::On,
                    Security::Off => SecurityFixes::Off,
                }
            }
            "channel" => {
                u.channel = match self.channel {
                    Channel::Stable => UpdateChannel::Stable,
                    Channel::Beta => UpdateChannel::Beta,
                    Channel::Nightly => UpdateChannel::Nightly,
                }
            }
            "wait_days" => u.wait_days = self.wait_days,
            "window" => u.window = self.window.clone(),
            "allow_reindex" => u.allow_reindex = self.allow_reindex,
            "notify_before" => u.notify_before = self.notify_before,
            "notify_after" => u.notify_after = self.notify_after,
            "notify_failure" => u.notify_failure = self.notify_failure,
            _ => {}
        }
    }

    fn check(&self, field: &str) -> Result<(), String> {
        self.check_on(field, crate::system::info::deployment_here().kind)
    }
}

impl Updates {
    /// [`Section::check`] for an installation of `kind`.
    fn check_on(&self, field: &str, kind: DeploymentKind) -> Result<(), String> {
        match field {
            "window" => crate::update::window::Window::parse(&self.window).map(|_| ()),
            "mode" if self.mode == Mode::Image => image_ok(kind),
            _ => Ok(()),
        }
    }
}

/// `image` means the container image updates Lodestar, so it's only for
/// a container.
pub(crate) fn image_ok(kind: DeploymentKind) -> Result<(), String> {
    match kind {
        DeploymentKind::Docker | DeploymentKind::Unraid => Ok(()),
        _ => Err(
            "\"image\" is for Docker and Unraid, where the container image updates \
                  Lodestar"
                .into(),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn off_by_default() {
        let u = Updates::default();
        assert_eq!(
            (u.mode, u.level, u.security_fixes, u.channel),
            (Mode::Off, Level::Fixes, Security::Auto, Channel::Stable)
        );
        assert_eq!((u.wait_days, u.window.as_str()), (3, "03:00-05:00"));
        assert!(u.allow_reindex && !u.notify_before && u.notify_after && u.notify_failure);
    }

    #[test]
    fn security_fixes_follow_the_mode() {
        let with = |mode, security_fixes| Updates {
            mode,
            security_fixes,
            ..Updates::default()
        };
        assert!(with(Mode::Auto, Security::Auto).security_effective());
        assert!(!with(Mode::Notify, Security::Auto).security_effective());
        assert!(with(Mode::Notify, Security::On).security_effective());
        assert!(!with(Mode::Auto, Security::Off).security_effective());
        for m in [Mode::Off, Mode::Image] {
            assert!(!with(m, Security::On).security_effective(), "{m:?}");
        }
    }

    #[test]
    fn image_is_only_for_containers_and_windows_are_checked() {
        assert!(image_ok(DeploymentKind::Docker).is_ok());
        assert!(image_ok(DeploymentKind::Unraid).is_ok());
        for k in [
            DeploymentKind::Systemd,
            DeploymentKind::Tarball,
            DeploymentKind::WindowsService,
            DeploymentKind::Unknown,
        ] {
            assert!(image_ok(k).is_err(), "{k:?}");
        }
        let image = Updates {
            mode: Mode::Image,
            ..Updates::default()
        };
        assert!(image.check_on("mode", DeploymentKind::Systemd).is_err());
        assert!(image.check_on("mode", DeploymentKind::Unraid).is_ok());
        assert!(
            Updates::default()
                .check_on("mode", DeploymentKind::Systemd)
                .is_ok()
        );
        let bad = Updates {
            window: "05:00-05:00".into(),
            ..Updates::default()
        };
        assert!(bad.check("window").is_err());
        assert!(Updates::default().check("window").is_ok());
    }

    mod registry {
        use std::path::Path;
        use std::sync::{Arc, Mutex};

        use lodestar_api::settings::{SettingSource, SettingsApi, SettingsError, SettingsPatch};
        use lodestar_store::{Store, StoreConfig};
        use serde_json::{Value, json};
        use tokio::sync::watch;

        use super::super::{Mode, Updates};
        use crate::config::Config;
        use crate::settings::Registry;

        fn registry(dir: &Path, env: &'static [(&'static str, &'static str)]) -> Registry {
            let config = Config::load(None, |k: &str| {
                if k == "LODESTAR_DATA_DIR" {
                    return Some(dir.display().to_string());
                }
                env.iter()
                    .find(|(n, _)| *n == k)
                    .map(|(_, v)| v.to_string())
            })
            .unwrap();
            let store = Store::open(dir.join("t.db"), StoreConfig::default()).unwrap();
            Registry::load(&config, Arc::new(Mutex::new(store))).unwrap()
        }

        fn patch(v: Value) -> SettingsPatch {
            SettingsPatch(v.as_object().unwrap().clone())
        }

        #[test]
        fn the_environment_locks_a_field_and_null_resets_one() {
            let dir = tempfile::tempdir().unwrap();
            let r = registry(dir.path(), &[("LODESTAR_UPDATES_CHANNEL", "beta")]);
            let values = r.section("updates").unwrap();
            let f = &values.fields["channel"];
            assert_eq!(
                (&f.value, f.source, f.read_only),
                (&json!("beta"), SettingSource::Env, true)
            );
            assert!(matches!(
                r.patch("updates", patch(json!({ "channel": "stable" }))),
                Err(SettingsError::SetByEnv { ref var, .. }) if var == "LODESTAR_UPDATES_CHANNEL"
            ));
            // Saved, followed live, then reset with null.
            let live: watch::Receiver<Updates> = r.subscribe();
            r.patch(
                "updates",
                patch(json!({ "mode": "notify", "wait_days": 9 })),
            )
            .unwrap();
            assert_eq!(
                (live.borrow().mode, live.borrow().wait_days),
                (Mode::Notify, 9)
            );
            r.patch("updates", patch(json!({ "mode": null, "wait_days": null })))
                .unwrap();
            assert_eq!(
                (live.borrow().mode, live.borrow().wait_days),
                (Mode::Off, 3)
            );
            assert_eq!(
                r.section("updates").unwrap().fields["mode"].source,
                SettingSource::Default
            );
            // Out of range and badly formed values are refused.
            for bad in [
                json!({ "wait_days": 15 }),
                json!({ "window": "4-5" }),
                json!({ "mode": "on" }),
            ] {
                assert!(r.patch("updates", patch(bad.clone())).is_err(), "{bad}");
            }
        }
    }
}
