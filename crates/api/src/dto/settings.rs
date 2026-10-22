//! `/settings` (architecture §10.10, §11): the runtime settings registry.
//!
//! Each section's own JSON schema, for generating its form, is served
//! by `GET /settings` rather than written into this document: sections
//! are registered by the server binary, which the API crate can't see.
//! The envelope around them is typed here.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use utoipa::ToSchema;

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
    "section": "crawler"
}))]
pub struct SectionValues {
    pub fields: BTreeMap<String, SettingValue>,
    /// Fields whose saved value differs from the running one: they take
    /// effect at the next restart.
    pub restart_required: Vec<String>,
    pub section: String,
}

/// `PATCH /settings/{section}`: some of the section's fields and their
/// new values, as its schema in `GET /settings` describes. Fields left
/// out keep their values; an unknown field is refused.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(transparent)]
#[schema(value_type = Object, example = json!({ "intensity": "low", "timeout_secs": 12 }))]
pub struct SettingsPatch(pub Map<String, Value>);
