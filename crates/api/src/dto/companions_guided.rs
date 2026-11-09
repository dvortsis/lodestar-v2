//! Guided and detected companions (plan
//! `docs/plan/2026-11-06-v06-companions.md` CMP-5g, Q12, Q19): what
//! detection found, the target you add, the key or temporary password you
//! paste, and the configuration you confirm.
//!
//! **What never appears here:** the value you paste ([`CompanionPasteRequest`]
//! hides it from `Debug`, and no response carries it), a credential, or a
//! path on this machine.

use std::fmt;

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use super::REDACTED;

/// Where a companion is: loopback or Docker's network on one of its
/// catalogue ports, or an address you typed (`http`/`https`, no user name
/// or password, query or fragment).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum CompanionTarget {
    Loopback {
        port: u16,
    },
    /// Its catalogue container name, on Docker's network (only when
    /// Lodestar runs in a container).
    Container {
        port: u16,
    },
    Url {
        url: String,
    },
}

/// A companion detection found.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct DetectedCompanion {
    pub id: String,
    pub target: CompanionTarget,
    /// The version it reports, when it reports one.
    #[schema(required = true)]
    pub version: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct DetectedCompanions {
    pub found: Vec<DetectedCompanion>,
}

/// `POST /companions/{id}/paste`: qBittorrent's temporary WebUI password
/// (Q19), sealed at once and never shown again.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CompanionPasteRequest {
    #[schema(example = json!("(pasted from qBittorrent's log)"))]
    pub value: String,
}

impl fmt::Debug for CompanionPasteRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CompanionPasteRequest")
            .field("value", &REDACTED)
            .finish()
    }
}

/// The settings a configuration changes, by name (never a value), shown
/// before you confirm.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct CompanionPlan {
    pub id: String,
    /// Setting names, such as `webui.password` or `downloads.save_path`.
    pub settings: Vec<String>,
}

/// `POST /companions/{id}/confirm`: the plan's settings, echoed exactly.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CompanionConfirmRequest {
    pub settings: Vec<String>,
}

/// `POST /companions/{id}/guided/forget`: `confirm` is the companion's id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CompanionForgetRequest {
    pub confirm: String,
}

/// What forgetting a companion you added deleted: Lodestar's record of it
/// and its stored credential, by `settings` key. The download client entry
/// in Integrations stays.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct CompanionForgotten {
    /// The deleted credential was one Lodestar generated, which is, or may
    /// be, the password qBittorrent uses now. Reveal it before forgetting
    /// if you'll need to sign in to qBittorrent yourself.
    pub credential_in_use: bool,
    pub deleted: Vec<String>,
    pub id: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pasted_value_never_reaches_debug() {
        let p = CompanionPasteRequest {
            value: "TempPass123".into(),
        };
        assert!(!format!("{p:?}").contains("TempPass123"));
    }

    #[test]
    fn targets_are_closed() {
        let t: CompanionTarget =
            serde_json::from_str(r#"{"kind":"loopback","port":8080}"#).unwrap();
        assert_eq!(t, CompanionTarget::Loopback { port: 8080 });
        for bad in [
            r#"{"kind":"lan","port":1}"#,
            r#"{"kind":"loopback","port":8080,"host":"x"}"#,
            r#"{"kind":"url"}"#,
        ] {
            assert!(
                serde_json::from_str::<CompanionTarget>(bad).is_err(),
                "{bad}"
            );
        }
    }
}
