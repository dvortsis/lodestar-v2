//! `/settings` (architecture §10.10, §11): the runtime settings registry.
//!
//! Each section's own JSON schema, for generating its form, is served
//! by `GET /settings` rather than written into this document: sections
//! are registered by the server binary, which the API crate can't see.
//! The envelope around them is typed here, with the batch patch,
//! preview, connection test, export and import bodies (§10.10.9).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use utoipa::{IntoParams, ToSchema};

/// Where a setting's effective value comes from, lowest to highest:
/// the built-in default, `config.toml`, a change saved in the app, or a
/// `LODESTAR_*` environment variable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum SettingSource {
    Default,
    File,
    Db,
    Env,
}

/// One field of a section, for its form.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct SettingField {
    /// The environment variable that overrides it; null when none does.
    #[schema(required = true)]
    pub env: Option<String>,
    /// Message key of the help text beside the field.
    pub help_key: String,
    /// Message key of the field's label.
    pub label_key: String,
    pub name: String,
    /// Never editable in the app (e.g. the data directory).
    pub read_only: bool,
    /// A change takes effect at the next restart.
    pub restart_required: bool,
}

/// One section: its fields and the JSON schema of its values.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct SettingsSection {
    pub fields: Vec<SettingField>,
    pub help_key: String,
    pub label_key: String,
    /// A JSON Schema (2020-12) object for the section's values, with
    /// each number field's bounds as `minimum`/`maximum`.
    #[schema(value_type = Object)]
    pub schema: Value,
    pub section: String,
}

/// `GET /settings`: every section, in display order.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[schema(example = json!({
    "sections": [{
        "fields": [{
            "env": "LODESTAR_INTENSITY",
            "help_key": "settings.crawler.intensity.help",
            "label_key": "settings.crawler.intensity.label",
            "name": "intensity",
            "read_only": false,
            "restart_required": false
        }],
        "help_key": "settings.crawler.help",
        "label_key": "settings.crawler.label",
        "schema": {
            "type": "object",
            "additionalProperties": false,
            "properties": { "intensity": { "type": "string", "enum": ["low", "balanced", "aggressive"] } }
        },
        "section": "crawler"
    }]
}))]
pub struct SettingsIndex {
    pub sections: Vec<SettingsSection>,
}

/// One field's effective value.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct SettingValue {
    /// The variable that set it when `source` is `env`; null otherwise.
    #[schema(required = true)]
    pub env: Option<String>,
    /// Can't be changed in the app: set by the environment, or never
    /// editable here.
    pub read_only: bool,
    /// A change takes effect at the next restart.
    pub restart_required: bool,
    /// For a list whose items hold credentials (a download client's
    /// password): which are set, as `<item id>.<key>`. The values are
    /// never sent. Left out when empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub secrets_set: Vec<String>,
    pub source: SettingSource,
    /// Any JSON value, of the type the section's schema gives the field.
    pub value: Value,
}

/// `GET /settings/{section}` and `PATCH /settings/{section}`: the
/// section's effective values.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[schema(example = json!({
    "fields": {
        "intensity": {
            "env": null,
            "read_only": false,
            "restart_required": false,
            "source": "db",
            "value": "low"
        },
        "timeout_secs": {
            "env": null,
            "read_only": false,
            "restart_required": true,
            "source": "db",
            "value": 12
        }
    },
    "restart_required": ["timeout_secs"],
    "revision": 4,
    "section": "crawler"
}))]
pub struct SectionValues {
    pub fields: BTreeMap<String, SettingValue>,
    /// Fields whose saved value differs from the running one: they take
    /// effect at the next restart.
    pub restart_required: Vec<String>,
    /// Goes up by one with every save that changes the section. Send it
    /// back as `if_revision` to refuse a save over someone else's.
    pub revision: u64,
    pub section: String,
}

/// `PATCH /settings/{section}`: some of the section's fields and their
/// new values, as its schema in `GET /settings` describes. Fields left
/// out keep their values; an unknown field is refused. `null` resets a
/// field: its saved value is removed, and the `config.toml` value or the
/// default applies again.
///
/// Two keys aren't fields and are never saved: `if_revision` (the
/// section's `revision` the edit started from; a save over a newer one is
/// refused with `409 stale_revision`) and a section's confirmations
/// (`content`'s `confirm_adult`).
#[derive(Clone, Default, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(transparent)]
#[schema(value_type = Object, example = json!({ "if_revision": 3, "intensity": "low", "timeout_secs": 12 }))]
pub struct SettingsPatch(pub Map<String, Value>);

/// The keys only: a patch can carry a secret field's new value.
impl std::fmt::Debug for SettingsPatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_set().entries(self.0.keys()).finish()
    }
}

/// `PATCH /settings` and `POST /settings/preview`: patches for several
/// sections, `{section: {field: value}}`, each as `PATCH
/// /settings/{section}` takes it. Saved all together or not at all.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(transparent)]
#[schema(value_type = Object, example = json!({
    "crawler": { "if_revision": 3, "intensity": "low" },
    "transport": { "if_revision": 0, "utp": true }
}))]
pub struct SettingsBatch(pub BTreeMap<String, SettingsPatch>);

/// `PATCH /settings`: every patched section's new values.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct SettingsBatchValues {
    pub sections: BTreeMap<String, SectionValues>,
}

/// A consequence the preview can put a number on, for the UI to format.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[schema(example = json!({ "name": "sockets", "value": 400 }))]
pub struct PreviewEstimate {
    /// What it measures: `sockets` (concurrent metadata fetches), `url`
    /// (where the UI will be after a restart), `memory_mib`.
    pub name: String,
    pub value: Value,
}

/// Something worth knowing before saving; it doesn't stop the save.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[schema(example = json!({ "code": "search_stops", "message": "Search stops after the restart, until you turn it back on." }))]
pub struct PreviewWarning {
    /// A stable snake_case identifier.
    pub code: String,
    /// A sentence for people.
    pub message: String,
}

/// One patched field, as it would be saved.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct FieldPreview {
    pub estimates: Vec<PreviewEstimate>,
    /// It would apply at the next restart; otherwise at once.
    pub restart_required: bool,
    /// The value it would have (null for a secret).
    pub value: Value,
    pub warnings: Vec<PreviewWarning>,
}

/// One section's patched fields.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct SectionPreview {
    pub fields: BTreeMap<String, FieldPreview>,
    /// The section's revision now.
    pub revision: u64,
}

/// `POST /settings/preview`: what a `PATCH /settings` with the same body
/// would do, without saving it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[schema(example = json!({
    "sections": {
        "crawler": {
            "fields": {
                "intensity": {
                    "estimates": [{ "name": "sockets", "value": 100 }],
                    "restart_required": false,
                    "value": "low",
                    "warnings": []
                }
            },
            "revision": 3
        }
    }
}))]
pub struct SettingsPreview {
    pub sections: BTreeMap<String, SectionPreview>,
}

/// A refused settings change: the API's error envelope, plus the
/// `section` at fault and, for `409 stale_revision`, the current values
/// of every section that changed meanwhile.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[schema(example = json!({
    "error": {
        "code": "invalid_setting",
        "field": "timeout_secs",
        "message": "Must be from 1 to 120.",
        "section": "crawler"
    }
}))]
pub struct SettingsErrorEnvelope {
    /// On `409 stale_revision`: each stale section's values now, to merge
    /// the edit with.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub current: Option<BTreeMap<String, SectionValues>>,
    pub error: SettingsErrorBody,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct SettingsErrorBody {
    /// A stable snake_case identifier to branch on.
    #[schema(example = "invalid_setting")]
    pub code: String,
    /// The field at fault.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub field: Option<String>,
    /// A sentence for people. It never repeats a secret's value.
    pub message: String,
    /// The section at fault.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub section: Option<String>,
}

/// A download client a connection test can reach.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum DownloadClientKind {
    Qbittorrent,
    Transmission,
    Deluge,
    Rtorrent,
    Aria2,
}

/// A download client's settings as the form has them, unsaved.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct DownloadClientDraft {
    pub client: DownloadClientKind,
    /// The saved client the form edits. Without `password`, its saved
    /// password is used, as long as `client` and the address's server are
    /// the saved ones.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub id: Option<String>,
    /// The password (qBittorrent, Deluge, Transmission and rTorrent with
    /// authentication) or the RPC secret (Aria2).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false, write_only)]
    pub password: Option<String>,
    /// The client's address, as the form has it.
    #[schema(example = "http://192.168.1.5:8080/")]
    pub url: String,
    /// The user name (qBittorrent; Transmission and rTorrent with
    /// authentication).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub username: Option<String>,
}

impl std::fmt::Debug for DownloadClientDraft {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // No URL (it can carry credentials), no password.
        f.debug_struct("DownloadClientDraft")
            .field("client", &self.client)
            .finish_non_exhaustive()
    }
}

/// A Torznab endpoint as the form has it, unsaved.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct TorznabDraft {
    /// Without it, the saved key is used if `url` is on the saved
    /// endpoint's server.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false, write_only)]
    pub api_key: Option<String>,
    #[schema(example = "http://192.168.1.5:9696/1/api")]
    pub url: String,
}

/// An ntfy channel as the form has it. A secret left out is the saved
/// one (the token only while the topic URL's server is the saved one).
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct NtfyDraft {
    #[serde(default)]
    pub allow_private: bool,
    /// 1–5; 0 leaves ntfy's default.
    #[serde(default)]
    pub priority: u8,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false, write_only)]
    pub token: Option<String>,
    /// The topic URL; without it, the saved one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false, write_only)]
    pub url: Option<String>,
}

/// A Discord channel as the form has it.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct DiscordDraft {
    /// The webhook URL; without it, the saved one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false, write_only)]
    pub url: Option<String>,
}

/// A webhook channel as the form has it.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct WebhookDraft {
    #[serde(default)]
    pub allow_private: bool,
    /// Where the JSON is posted; without it, the saved one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false, write_only)]
    pub url: Option<String>,
}

/// How an email channel's SMTP session is protected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum EmailSecurity {
    Tls,
    #[default]
    Starttls,
    None,
}

/// An email channel as the form has it.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct EmailDraft {
    #[serde(default)]
    pub allow_private: bool,
    pub from: String,
    pub host: String,
    /// Without it (and with `username`), the saved password, as long as
    /// `host`, `port` and `security` are the saved ones.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false, write_only)]
    pub password: Option<String>,
    /// 0 is the security mode's usual port.
    #[serde(default)]
    pub port: u16,
    #[serde(default)]
    pub security: EmailSecurity,
    pub to: Vec<String>,
    /// Empty to send without logging in.
    #[serde(default)]
    pub username: String,
}

macro_rules! no_secrets_debug {
    ($($t:ident),*) => {$(
        impl std::fmt::Debug for $t {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                // No address (it can carry a token), no secret.
                f.debug_struct(stringify!($t)).finish_non_exhaustive()
            }
        }
    )*};
}
no_secrets_debug!(
    TorznabDraft,
    NtfyDraft,
    DiscordDraft,
    WebhookDraft,
    EmailDraft
);

/// `POST /settings/test`: what to test, with the values on screen.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SettingsTest {
    /// Log in to a download client and ask its version.
    DownloadClient(DownloadClientDraft),
    /// Ask a Torznab endpoint for its capabilities.
    Torznab(TorznabDraft),
    /// Send one test message to ntfy.
    Ntfy(NtfyDraft),
    /// Send one test message to Discord.
    Discord(DiscordDraft),
    /// Send one test email.
    Email(EmailDraft),
    /// Post one test payload to the webhook.
    Webhook(WebhookDraft),
}

/// `POST /send`: torrents to add to a saved download client.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ClientSend {
    /// The saved client's id; without it, the default client.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub client: Option<String>,
    /// 1 to 100 info hashes (40 hex characters).
    pub hashes: Vec<String>,
}

/// What happened to one torrent of a send.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ClientSendOutcome {
    /// The client took it.
    Sent,
    /// It was in the client already, which is where you want it.
    Duplicate,
    /// Not sent: unknown here, or hidden from you (`code` says which).
    Refused,
    /// The client didn't take it (`code` and `message` say why).
    Failed,
}

/// One torrent's result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ClientSendItem {
    /// When refused: `not_found` (unknown, or withheld from you, which
    /// look the same: hidden by a filter, or Adult while it's hidden),
    /// `private`, `same_torrent` or `invalid`. When failed: the connection
    /// test's codes, or `timeout_unknown` (some may have gone in). Null
    /// otherwise.
    #[schema(required = true)]
    pub code: Option<String>,
    pub hash: String,
    /// A sentence for people; never the client's address or a credential.
    #[schema(required = true)]
    pub message: Option<String>,
    #[schema(inline)]
    pub outcome: ClientSendOutcome,
}

/// `POST /send`: the result per torrent, in the order asked.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ClientSendResult {
    /// The client's id.
    pub client: String,
    pub results: Vec<ClientSendItem>,
    /// How many went in (sent or already there).
    pub sent: usize,
}

/// `GET /usage`: the usage data Lodestar keeps.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct UsageStats {
    /// Logged searches (only while query logging is on).
    pub queries: u64,
}

/// `POST /usage/clear`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct UsageClear {
    /// Must be true: the dialog that showed the count was confirmed
    /// (§10.10.7, a destructive action).
    #[serde(default)]
    pub confirm: bool,
}

/// `POST /usage/clear`: what was deleted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct UsageCleared {
    /// Logged searches deleted.
    pub queries: u64,
}

/// Why a connection test failed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[schema(example = json!({ "code": "bad_credentials", "message": "The client refused the user name or password." }))]
pub struct SettingsTestFailure {
    /// `bad_credentials`, `banned`, `origin_refused`, `session_refused`,
    /// `address_refused`, `unreachable`, `rejected`, `timeout`, `blocked`,
    /// `invalid`, `protocol` or `unsupported`.
    pub code: String,
    /// A sentence for people; never the URL or a credential.
    pub message: String,
}

/// `POST /settings/test`: the result, shown beside the button. Nothing is
/// saved either way.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[schema(example = json!({ "api_version": "2.9.3", "error": null, "ok": true, "version": "v4.6.3" }))]
pub struct SettingsTestResult {
    /// The client's API version, when it has one apart from its version.
    #[schema(required = true)]
    pub api_version: Option<String>,
    /// Why it failed; null when `ok`.
    #[schema(required = true)]
    pub error: Option<SettingsTestFailure>,
    pub ok: bool,
    /// The client's version, as it reports it; null unless `ok`.
    #[schema(required = true)]
    pub version: Option<String>,
}

/// `GET /settings/export` parameters.
#[derive(Debug, Clone, Default, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ExportQuery {
    /// Include the `preferences` section (off by default).
    pub preferences: Option<bool>,
}

/// `POST /settings/import` parameters.
#[derive(Debug, Clone, Default, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ImportQuery {
    /// Only report what the import would change (the diff); save nothing.
    pub dry_run: Option<bool>,
}

/// `POST /settings/import`: a file from `GET /settings/export`.
#[derive(Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SettingsImport {
    /// The 18+ confirmation, needed when the file turns on an adult
    /// setting (as `PATCH /settings/content` needs it).
    #[serde(default)]
    pub confirm_adult: bool,
    /// The sections' revisions the dry run reported: the import is refused
    /// with `409 stale_revision` if any changed since.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub revisions: Option<BTreeMap<String, u64>>,
    /// The file's text (TOML), at most 64 KiB.
    pub toml: String,
}

impl std::fmt::Debug for SettingsImport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SettingsImport")
            .field("confirm_adult", &self.confirm_adult)
            .field("revisions", &self.revisions)
            .field("toml_bytes", &self.toml.len())
            .finish()
    }
}

/// One setting an import changes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct ImportChange {
    pub field: String,
    /// The value now.
    pub from: Value,
    /// Where the value now comes from: to undo, send it back, or `null`
    /// when this isn't `db`.
    pub from_source: SettingSource,
    /// It applies at the next restart.
    pub restart_required: bool,
    pub section: String,
    pub to: Value,
}

/// A key in the file that an import leaves alone.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ImportSkip {
    /// The variable that sets it, for `set_by_environment`; null otherwise.
    #[schema(required = true)]
    pub env: Option<String>,
    /// `section.field`, or the key as the file has it.
    pub key: String,
    /// `unknown` (no such setting), `read_only`, `secret` (never imported)
    /// or `set_by_environment`.
    pub reason: String,
}

/// `POST /settings/import`: the diff, and whether it was applied.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[schema(example = json!({
    "applied": false,
    "changes": [{
        "field": "intensity",
        "from": "balanced",
        "from_source": "default",
        "restart_required": false,
        "section": "crawler",
        "to": "low"
    }],
    "revisions": { "crawler": 3 },
    "skipped": [{ "env": "LODESTAR_DHT_PORT", "key": "network.dht_port", "reason": "set_by_environment" }],
    "unchanged": 2
}))]
pub struct ImportReport {
    /// Saved (false on a dry run, or when nothing changes).
    pub applied: bool,
    pub changes: Vec<ImportChange>,
    /// Every section's revision after the import: pass them back with
    /// the real import to refuse it if anything changed after a dry run.
    pub revisions: BTreeMap<String, u64>,
    pub skipped: Vec<ImportSkip>,
    /// Settings in the file that already have its value.
    pub unchanged: u32,
}
