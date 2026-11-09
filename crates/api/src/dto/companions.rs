//! `/companions` (architecture §18.10, §12; plan
//! `docs/plan/2026-11-06-v06-companions.md` CMP-6): the companions Lodestar
//! can install, detect and supervise, what an install or removal would do,
//! their templates and the password-gated credential reveal (Q5).
//!
//! **What never appears here:** a credential outside [`RevealedCredential`]
//! (the reveal's own response), a path on this machine (a removal names
//! the companion's folder relative to the data folder), a download URL or
//! a peer address. Template responses carry back only what you typed into
//! the request, built into the template.

use std::fmt;

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use super::REDACTED;

/// The last health check the manager saw.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum CompanionHealth {
    Unknown,
    Healthy,
    Unhealthy,
}

/// A template a companion offers (CMP-7).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum TemplateKind {
    /// A `docker compose` service.
    Compose,
    /// A filled Unraid Community Applications template.
    Unraid,
}

/// One companion: the list's row and the detail.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct Companion {
    /// The version an install or update would fetch here; null when it's
    /// up to date or can't be installed here (`refusal` says why).
    #[schema(required = true)]
    pub available: Option<String>,
    /// The stored credentials, by field (`webui_password`, `api_key`):
    /// names only, never values. What `…/reveal` can show.
    pub credentials: Vec<String>,
    /// Null when Lodestar didn't install it.
    #[schema(required = true, inline)]
    pub health: Option<CompanionHealth>,
    /// The catalogue id, e.g. `qbittorrent`.
    pub id: String,
    /// The version Lodestar installed; null when it didn't.
    #[schema(required = true)]
    pub installed: Option<String>,
    /// The SPDX licence id, e.g. `GPL-2.0-or-later`.
    pub licence: String,
    /// Its display name, e.g. `qBittorrent`.
    pub name: String,
    /// Off and never installed unless you switch it on (FlareSolverr).
    pub opt_in: bool,
    /// Why it can't be installed or updated here, as a stable code
    /// (`native_in_container`, `awaiting_pin`, `unsupported_platform`,
    /// `guided_only`, …); null when it can, or when it's up to date.
    #[schema(required = true)]
    pub refusal: Option<String>,
    /// The templates it offers.
    #[schema(inline)]
    pub templates: Vec<TemplateKind>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct CompanionList {
    pub companions: Vec<Companion>,
}

/// What an install or update would fetch, shown before you confirm it.
/// The confirmation echoes `version` and `sha256` exactly.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct InstallOffer {
    pub id: String,
    /// The SPDX licence id.
    pub licence: String,
    pub name: String,
    /// The installed version this update replaces; null for an install.
    #[schema(required = true)]
    pub replaces: Option<String>,
    /// The pinned SHA-256 of the download, 64 lowercase hex digits.
    pub sha256: String,
    pub size_bytes: u64,
    pub version: String,
}

/// `POST /companions/{id}/install`: the offer's version and SHA-256,
/// echoed. Anything else is `409 confirmation_mismatch`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CompanionInstallRequest {
    pub sha256: String,
    pub version: String,
}

/// What a removal deletes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum RemovalItemKind {
    /// A folder, relative to the data folder, with everything in it.
    Folder,
    /// A credential's `settings` row, if it's there.
    Setting,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct RemovalItem {
    #[schema(inline)]
    pub kind: RemovalItemKind,
    /// `companions/<id>` for the folder; the row key for a setting.
    pub name: String,
}

/// Exactly what removing a companion deletes: shown first, and returned
/// again once it's done.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct RemovalPlan {
    pub deletes: Vec<RemovalItem>,
    pub id: String,
    /// Whether it has been removed (`POST`), or is only shown (`GET`).
    pub removed: bool,
}

/// `POST /companions/{id}/remove`: `confirm` is the companion's id, typed
/// or echoed. Anything else is `409 confirmation_mismatch`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct RemoveRequest {
    pub confirm: String,
}

/// A companion's recent output, redacted again on the way out.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct CompanionLogs {
    pub id: String,
    /// Oldest first.
    pub lines: Vec<String>,
}

/// How a templated companion's traffic leaves.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum TemplateRoute {
    /// Straight out, on Lodestar's network.
    #[default]
    Direct,
    /// Through a gluetun generated with it.
    Gluetun,
    /// Through the gluetun container you already run
    /// (`gluetun_container`).
    ExistingGluetun,
}

fn default_network() -> String {
    "lodestar".into()
}

fn default_id() -> String {
    "1000".into()
}

/// `POST /companions/{id}/template`: what the template is built from,
/// each value checked as `lodestar companion compose` checks it. Nothing
/// here is stored.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct TemplateRequest {
    /// The host folder for its settings.
    #[serde(default)]
    pub config_dir: Option<String>,
    /// The host folder it saves downloads in.
    #[serde(default)]
    pub downloads: Option<String>,
    /// The gluetun container's name, with `route: existing_gluetun`.
    #[serde(default)]
    pub gluetun_container: Option<String>,
    #[schema(inline)]
    pub kind: TemplateKind,
    /// The Docker network Lodestar is on; `lodestar` by default.
    #[serde(default = "default_network")]
    pub network: String,
    /// `1000` by default.
    #[serde(default = "default_id")]
    pub pgid: String,
    /// `1000` by default.
    #[serde(default = "default_id")]
    pub puid: String,
    #[serde(default)]
    #[schema(inline)]
    pub route: TemplateRoute,
}

/// One filled Unraid template.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct UnraidFile {
    /// The path on your Unraid server's web UI that opens the saved
    /// template in Add Container.
    pub add_container_path: String,
    /// Label and value, in the form's order, for filling it by hand.
    pub fields: Vec<UnraidField>,
    /// `my-<name>.xml`.
    pub file_name: String,
    pub xml: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct UnraidField {
    pub label: String,
    /// Empty for a VPN setting you fill in yourself.
    pub value: String,
}

/// A generated template, to copy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct CompanionTemplate {
    /// The compose document, for `kind: compose`; null otherwise.
    #[schema(required = true)]
    pub compose: Option<String>,
    pub id: String,
    #[schema(inline)]
    pub kind: TemplateKind,
    /// gluetun's template first when it's generated, then the
    /// companion's, for `kind: unraid`; empty otherwise.
    pub unraid: Vec<UnraidFile>,
}

/// `POST …/reveal`: the admin password, entered again (Q5). Asked every
/// time, never remembered.
#[derive(Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct RevealRequest {
    #[serde(default)]
    pub password: Option<String>,
}

impl fmt::Debug for RevealRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RevealRequest")
            .field("password", &self.password.as_ref().map(|_| REDACTED))
            .finish()
    }
}

/// One stored credential, in the clear: the reveal's response alone
/// carries it, with `Cache-Control: no-store`.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct RevealedCredential {
    /// `webui_password` or `api_key`.
    pub field: String,
    pub id: String,
    pub value: String,
}

impl fmt::Debug for RevealedCredential {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RevealedCredential")
            .field("field", &self.field)
            .field("id", &self.id)
            .field("value", &REDACTED)
            .finish()
    }
}

/// `202` for a restart or a regenerated credential: the change is under
/// way; the companion's row says when it's done.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct CompanionAccepted {
    pub id: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secrets_never_reach_debug() {
        let r = RevealRequest {
            password: Some("hunter22hunter".into()),
        };
        assert!(!format!("{r:?}").contains("hunter"));
        let c = RevealedCredential {
            field: "api_key".into(),
            id: "prowlarr".into(),
            value: "0123456789abcdef0123456789abcdef".into(),
        };
        assert!(!format!("{c:?}").contains("0123456789"));
    }

    #[test]
    fn template_defaults_match_the_cli() {
        let t: TemplateRequest = serde_json::from_str(r#"{"kind":"compose"}"#).unwrap();
        assert_eq!(
            (t.network.as_str(), t.puid.as_str(), t.pgid.as_str()),
            ("lodestar", "1000", "1000")
        );
        assert_eq!(t.route, TemplateRoute::Direct);
        assert!(serde_json::from_str::<TemplateRequest>(r#"{"kind":"compose","x":1}"#).is_err());
    }
}
