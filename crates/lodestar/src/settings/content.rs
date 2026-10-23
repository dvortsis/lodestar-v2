//! Content: "Show adult content" (§20.9), live, plus the per-output
//! adult settings their slices will wire.
//!
//! Adult is a media category whose visibility is a preference, not a
//! filter: it changes what's shown, never what's stored, indexed or
//! filtered, so it doesn't ask for the filters password (§20.11). It's
//! off by default, and the app turns it on only with an 18+
//! confirmation: a patch that sets any adult field to `true` must carry
//! `"confirm_adult": true`, which is checked and never saved. Turning
//! one off needs nothing. `config.toml` (`[content] show_adult`) and
//! `LODESTAR_SHOW_ADULT` set it without one; the environment makes it
//! read-only here, like every other field.

use lodestar_api::AdultVisibility;
use lodestar_api::settings::SettingsError;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use tokio::sync::watch;
use utoipa::ToSchema;

use super::{Field, Section};
use crate::config::Config;

/// The patch key that confirms the user is 18 or older.
pub const CONFIRM_ADULT: &str = "confirm_adult";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct Content {
    /// Show adult content in search results and counts. Off by default;
    /// turning it on needs `"confirm_adult": true` in the same change
    /// (the 18+ confirmation). Applies at once.
    pub show_adult: bool,
    /// Adult results in Torznab feeds. Not used yet.
    pub torznab_adult: bool,
    /// Adult results in the Stremio add-on. Not used yet.
    pub stremio_adult: bool,
    /// Adult torrents in exports. Not used yet.
    pub export_adult: bool,
}

impl Default for Content {
    fn default() -> Self {
        Content::from_config(&Config::default())
    }
}

impl Section for Content {
    const SECTION: &'static str = "content";
    const FIELDS: &'static [Field] = &[
        Field::new("show_adult")
            .file("content.show_adult")
            .env("LODESTAR_SHOW_ADULT")
            .group("adult")
            .keywords(),
        // Reserved for the output slices (Torznab, Stremio, export), off
        // by default; nothing reads them yet.
        Field::new("torznab_adult").group("adult").advanced(),
        Field::new("stremio_adult").group("adult").advanced(),
        Field::new("export_adult").group("adult").advanced(),
    ];
    const CONFIRMATIONS: &'static [&'static str] = &[CONFIRM_ADULT];

    fn from_config(config: &Config) -> Self {
        Content {
            show_adult: config.content.show_adult,
            torznab_adult: false,
            stremio_adult: false,
            export_adult: false,
        }
    }

    fn apply(&self, field: &str, config: &mut Config) {
        if field == "show_adult" {
            config.content.show_adult = self.show_adult;
        }
    }

    fn confirm(patch: &Map<String, Value>) -> Result<(), SettingsError> {
        let confirmed = patch.get(CONFIRM_ADULT) == Some(&Value::Bool(true));
        let turned_on = Self::FIELDS
            .iter()
            .find(|f| patch.get(f.name) == Some(&Value::Bool(true)));
        match turned_on {
            Some(f) if !confirmed => Err(SettingsError::NeedsConfirmation {
                field: f.name.to_string(),
                code: "adult_confirmation_required",
                message: "Showing adult content needs your confirmation that you're 18 or older: \
                          send \"confirm_adult\": true with it."
                    .to_string(),
            }),
            _ => Ok(()),
        }
    }
}

/// What the API reads "Show adult content" through: the section's
/// current value at each request, so a change applies to the next one.
pub fn follow(rx: watch::Receiver<Content>) -> AdultVisibility {
    AdultVisibility::follow(move || rx.borrow().show_adult)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn confirm(v: Value) -> Result<(), SettingsError> {
        Content::confirm(v.as_object().unwrap())
    }

    #[test]
    fn hidden_by_default() {
        assert_eq!(
            Content::default(),
            Content {
                show_adult: false,
                torznab_adult: false,
                stremio_adult: false,
                export_adult: false,
            }
        );
    }

    #[test]
    fn turning_adult_on_needs_the_confirmation() {
        let refused = |v: Value, field: &str| match confirm(v.clone()) {
            Err(SettingsError::NeedsConfirmation { field: f, code, .. }) => {
                assert_eq!(
                    (f.as_str(), code),
                    (field, "adult_confirmation_required"),
                    "{v}"
                );
            }
            other => panic!("{v}: {other:?}"),
        };
        refused(json!({ "show_adult": true }), "show_adult");
        refused(
            json!({ "show_adult": true, "confirm_adult": false }),
            "show_adult",
        );
        refused(
            json!({ "show_adult": true, "confirm_adult": "true" }),
            "show_adult",
        );
        refused(json!({ "export_adult": true }), "export_adult");
        assert!(confirm(json!({ "show_adult": true, "confirm_adult": true })).is_ok());
        assert!(confirm(json!({ "torznab_adult": true, "confirm_adult": true })).is_ok());
    }

    #[test]
    fn turning_adult_off_or_resetting_needs_nothing() {
        assert!(confirm(json!({ "show_adult": false })).is_ok());
        assert!(confirm(json!({ "show_adult": null })).is_ok());
        assert!(confirm(json!({})).is_ok());
        // A wrong type isn't "on": the field's own check refuses it.
        assert!(confirm(json!({ "show_adult": "yes" })).is_ok());
    }

    mod registry {
        use std::path::Path;
        use std::sync::{Arc, Mutex};

        use lodestar_api::settings::{SettingSource, SettingsApi, SettingsError, SettingsPatch};
        use lodestar_store::{Store, StoreConfig};
        use serde_json::{Value, json};

        use super::super::follow;
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

        fn saved(r: &Registry) -> Vec<(String, String)> {
            r.store
                .lock()
                .unwrap()
                .settings_with_prefix("settings.content.")
                .unwrap()
        }

        #[test]
        fn an_unconfirmed_patch_saves_nothing() {
            let dir = tempfile::tempdir().unwrap();
            let r = registry(dir.path(), &[]);
            let shown = follow(r.subscribe());
            // Not even the other fields of the same patch.
            let refused = r.patch(
                "content",
                patch(json!({ "export_adult": false, "show_adult": true })),
            );
            assert!(matches!(
                refused,
                Err(SettingsError::NeedsConfirmation { ref field, .. }) if field == "show_adult"
            ));
            assert!(saved(&r).is_empty());
            assert!(!shown.shown());
            // A confirmation alone changes nothing either.
            r.patch("content", patch(json!({ "confirm_adult": true })))
                .unwrap();
            assert!(saved(&r).is_empty());
        }

        #[test]
        fn the_api_follows_the_setting_live_and_it_survives_a_restart() {
            let dir = tempfile::tempdir().unwrap();
            let r = registry(dir.path(), &[]);
            let shown = follow(r.subscribe());
            r.patch(
                "content",
                patch(json!({ "show_adult": true, "confirm_adult": true })),
            )
            .unwrap();
            assert!(shown.shown());
            assert_eq!(
                saved(&r),
                [(
                    "settings.content.show_adult".to_string(),
                    "true".to_string()
                )]
            );
            drop(r);
            let r = registry(dir.path(), &[]);
            let shown = follow(r.subscribe());
            assert!(shown.shown());
            r.patch("content", patch(json!({ "show_adult": false })))
                .unwrap();
            assert!(!shown.shown());
            // `null` resets to the default: hidden.
            r.patch(
                "content",
                patch(json!({ "show_adult": true, "confirm_adult": true })),
            )
            .unwrap();
            r.patch("content", patch(json!({ "show_adult": null })))
                .unwrap();
            assert!(!shown.shown());
            assert!(saved(&r).is_empty());
        }

        #[test]
        fn the_environment_sets_it_read_only() {
            let dir = tempfile::tempdir().unwrap();
            let r = registry(dir.path(), &[("LODESTAR_SHOW_ADULT", "true")]);
            let values = r.section("content").unwrap();
            let f = &values.fields["show_adult"];
            assert_eq!(
                (&f.value, f.source, f.read_only, f.env.as_deref()),
                (
                    &json!(true),
                    SettingSource::Env,
                    true,
                    Some("LODESTAR_SHOW_ADULT")
                )
            );
            assert!(follow(r.subscribe()).shown());
            for body in [
                json!({ "show_adult": false }),
                json!({ "show_adult": true, "confirm_adult": true }),
            ] {
                assert!(matches!(
                    r.patch("content", patch(body)),
                    Err(SettingsError::SetByEnv { ref var, .. }) if var == "LODESTAR_SHOW_ADULT"
                ));
            }
            assert!(saved(&r).is_empty());
            let bad = Config::load(None, |k: &str| {
                (k == "LODESTAR_SHOW_ADULT").then(|| "nope".to_string())
            });
            assert!(bad.is_err());
        }

        #[test]
        fn the_reserved_output_settings_are_off_and_unused() {
            let dir = tempfile::tempdir().unwrap();
            let r = registry(dir.path(), &[]);
            let index = r.index();
            let content = index
                .sections
                .iter()
                .find(|s| s.section == "content")
                .unwrap();
            for f in ["torznab_adult", "stremio_adult", "export_adult"] {
                assert_eq!(r.section("content").unwrap().fields[f].value, json!(false));
                let help = content.schema["properties"][f]["description"]
                    .as_str()
                    .unwrap();
                assert!(help.contains("Not used yet"), "{f}: {help}");
            }
        }
    }
}
