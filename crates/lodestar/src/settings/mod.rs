//! The runtime settings registry (architecture §10.10, §11): typed
//! sections the app can show and change, layered as defaults <
//! `config.toml` < saved in the app (the database) < `LODESTAR_*`
//! environment variables.
//!
//! - A field an environment variable sets is read-only here: a change is
//!   refused, naming the variable, and a saved value never overrides it.
//! - A change is a batch of section patches. It's checked whole (each
//!   section's revision, every field, the confirmations), saved in one
//!   transaction under one lock, and published on each changed section's
//!   `watch` channel. Subsystems that can apply it live follow the channel
//!   ([`log`], [`crawler::follow_intensity`]); fields that can't are
//!   `restart_required`, and the section lists the ones whose saved value
//!   waits for a restart.
//! - Each section has a revision, saved beside its values, that goes up
//!   with every save that changes it; a patch's `if_revision` refuses a
//!   save over a newer one ([`SettingsError::Stale`]).
//! - `null` for a field resets it: its saved value is removed, and the
//!   file's value or the default applies again.
//! - A preview runs the same checks and saves nothing ([`Registry`]'s
//!   `preview`); export and import ([`portable`]) move saved values as
//!   TOML through the same checks.
//! - Nothing here reaches filtering state (rules, the floor, packs,
//!   subscriptions: the filters API's, §20.7), and no field moves files
//!   at runtime. A secret field (`writeOnly`), or a list field whose items
//!   hold secrets ([`Field::secret_keys`]), is sealed at rest with the
//!   integrations [`SecretStore`] and never returned, exported or put in
//!   an error message.
//!
//! See `README.md` for how a later slice adds a section.

pub mod content;
pub mod crawler;
pub mod general;
pub mod insights;
pub mod integrations;
pub mod log;
pub mod logging;
pub mod network;
pub mod notifications;
mod portable;
pub mod preferences;
pub mod search;
pub mod storage;
pub mod trackers;
pub mod transport;
pub mod updates;

use std::any::Any;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};

use lodestar_api::settings::{
    FieldPreview, IF_REVISION, Imported, PreviewEstimate, PreviewWarning, Saved, SavedClient,
    SectionPreview, SectionValues, SettingField, SettingSource, SettingValue, SettingsApi,
    SettingsBatch, SettingsError, SettingsFailure, SettingsImport, SettingsIndex, SettingsPreview,
    SettingsSection,
};
use lodestar_integrations::{SealedSecret, Secret, SecretStore};
use lodestar_store::Store;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Map, Value};
use tokio::sync::watch;

use crate::config::Config;
use crate::logging::events;
use lodestar_log::log_event;

/// Every key the registry saves starts with this, then
/// `<section>.<field>`, or `<section>#revision` for the section's
/// revision.
pub const KEY_PREFIX: &str = "settings.";

/// Section names the API's own routes take (`/settings/preview`…).
const RESERVED_SECTIONS: &[&str] = &["preview", "test", "export", "import"];

/// A field's "automatic" value (`x-auto`): the form shows an Automatic
/// switch instead of the value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Auto {
    Int(i64),
    Str(&'static str),
}

/// How a change to a field is confirmed in the UI (§10.10.7, `x-danger`).
/// The whole vocabulary, though today's sections use only `Disruptive` and
/// `Privacy`.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Danger {
    /// Save opens a dialog naming the effect (the HTTP address, the DHT).
    Disruptive,
    /// It records what identifies torrents or people.
    Privacy,
    /// It can't be undone.
    Destructive,
    /// It needs the password again.
    Security,
}

impl Danger {
    fn name(self) -> &'static str {
        match self {
            Danger::Disruptive => "disruptive",
            Danger::Privacy => "privacy",
            Danger::Destructive => "destructive",
            Danger::Security => "security",
        }
    }
}

/// One field's metadata. Its label and help are the message keys
/// `settings.<section>.<field>.label` and `.help`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Field {
    /// The key in the section's serialized values.
    pub name: &'static str,
    /// Where `config.toml` sets it, as a dotted path (`dht.bind`).
    pub file: Option<&'static str>,
    /// The environment variable that overrides it.
    pub env: Option<&'static str>,
    /// Takes effect only at the next start.
    pub restart_required: bool,
    /// Never editable in the app.
    pub read_only: bool,
    /// Inclusive bounds of a number field (also put in its schema).
    pub range: Option<(i64, i64)>,
    /// The form group it's shown in (`x-group`); its heading is the
    /// message `settings.group.<group>`.
    pub group: &'static str,
    /// Behind the screen's Advanced disclosure.
    pub advanced: bool,
    /// The unit shown in the control (`seconds`, `mebibytes`,
    /// `connections`).
    pub unit: Option<&'static str>,
    /// A widget other than the schema's default (`radio`, `slider`,
    /// `theme`, `shortcuts`, `language`, `time-zone`).
    pub widget: Option<&'static str>,
    /// A string format the form checks (`socket-addr`, `log-filter`,
    /// `time-range`).
    pub format: Option<&'static str>,
    /// The value that means "automatic".
    pub auto: Option<Auto>,
    /// The preview has estimates or warnings for it ([`Section::preview`]).
    pub preview: bool,
    /// Search synonyms: the message `settings.<section>.<field>.keywords`.
    pub keywords: bool,
    pub danger: Option<Danger>,
    /// A credential: never returned, exported or echoed (`writeOnly`).
    pub secret: bool,
    /// For a list of objects with an `id` key: the keys of each item that
    /// are credentials (a download client's `password`). Treated like a
    /// secret field: sealed at rest, never returned, exported or echoed;
    /// an item sent back without one keeps the saved one ([`keep_secrets`]).
    pub secret_keys: &'static [&'static str],
}

impl Field {
    pub const fn new(name: &'static str) -> Field {
        Field {
            name,
            file: None,
            env: None,
            restart_required: false,
            read_only: false,
            range: None,
            group: "",
            advanced: false,
            unit: None,
            widget: None,
            format: None,
            auto: None,
            preview: false,
            keywords: false,
            danger: None,
            secret: false,
            secret_keys: &[],
        }
    }

    /// Whether its saved value holds a credential, whole or in its items:
    /// sealed at rest, never shown, exported or quoted.
    pub const fn holds_secret(&self) -> bool {
        self.secret || !self.secret_keys.is_empty()
    }

    pub const fn file(self, path: &'static str) -> Field {
        Field {
            file: Some(path),
            ..self
        }
    }

    pub const fn env(self, var: &'static str) -> Field {
        Field {
            env: Some(var),
            ..self
        }
    }

    pub const fn restart(self) -> Field {
        Field {
            restart_required: true,
            ..self
        }
    }

    pub const fn read_only(self) -> Field {
        Field {
            read_only: true,
            ..self
        }
    }

    pub const fn range(self, min: i64, max: i64) -> Field {
        Field {
            range: Some((min, max)),
            ..self
        }
    }

    pub const fn group(self, group: &'static str) -> Field {
        Field { group, ..self }
    }

    pub const fn advanced(self) -> Field {
        Field {
            advanced: true,
            ..self
        }
    }

    pub const fn unit(self, unit: &'static str) -> Field {
        Field {
            unit: Some(unit),
            ..self
        }
    }

    pub const fn widget(self, widget: &'static str) -> Field {
        Field {
            widget: Some(widget),
            ..self
        }
    }

    pub const fn format(self, format: &'static str) -> Field {
        Field {
            format: Some(format),
            ..self
        }
    }

    pub const fn auto(self, auto: Auto) -> Field {
        Field {
            auto: Some(auto),
            ..self
        }
    }

    pub const fn preview(self) -> Field {
        Field {
            preview: true,
            ..self
        }
    }

    pub const fn keywords(self) -> Field {
        Field {
            keywords: true,
            ..self
        }
    }

    pub const fn danger(self, danger: Danger) -> Field {
        Field {
            danger: Some(danger),
            ..self
        }
    }

    /// A credential: sealed at rest, `writeOnly`, never returned.
    pub const fn secret(self) -> Field {
        Field {
            secret: true,
            ..self
        }
    }

    /// The credential keys of each item of a list field.
    pub const fn secret_keys(self, keys: &'static [&'static str]) -> Field {
        Field {
            secret_keys: keys,
            ..self
        }
    }
}

/// A credential in a section's struct: a plain string on the wire (the
/// registry keeps it out of every answer), but `***` in `Debug`, so a
/// section printed whole never shows it.
#[derive(Clone, Default, PartialEq, Eq, Serialize, serde::Deserialize)]
#[serde(transparent)]
pub struct SecretText(pub String);

impl std::fmt::Debug for SecretText {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(if self.0.is_empty() { "\"\"" } else { "***" })
    }
}

impl SecretText {
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// The integrations crate's [`Secret`], for the one place that sends it.
    pub fn secret(&self) -> Secret {
        Secret::new(self.0.clone())
    }
}

/// What a client sees of a field's value: nothing of a secret, and each
/// item of a list without its secret keys.
fn redact(field: &Field, value: &Value) -> Value {
    if field.secret {
        return Value::Null;
    }
    let mut v = value.clone();
    if let Value::Array(items) = &mut v {
        for item in items.iter_mut().filter_map(Value::as_object_mut) {
            for k in field.secret_keys {
                item.remove(*k);
            }
        }
    }
    v
}

/// Which item secrets a list field has set, as `<item id>.<key>`.
fn secrets_set(field: &Field, value: &Value) -> Vec<String> {
    let Value::Array(items) = value else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for item in items.iter().filter_map(Value::as_object) {
        let id = item.get("id").and_then(Value::as_str).unwrap_or_default();
        for k in field.secret_keys {
            if item
                .get(*k)
                .is_some_and(|v| v.as_str().is_some_and(|s| !s.is_empty()))
            {
                out.push(format!("{id}.{k}"));
            }
        }
    }
    out
}

/// Where an endpoint value points, for telling whether a kept secret
/// would follow it elsewhere: a URL's scheme, host and port; any other
/// value as it is (an SMTP host name, a port).
fn endpoint(url: bool, v: Option<&Value>) -> Option<String> {
    let v = v?;
    let Some(s) = v.as_str() else {
        return Some(v.to_string());
    };
    let s = s.trim();
    if url
        && let Ok(u) = reqwest::Url::parse(s)
        && matches!(u.scheme(), "http" | "https")
        && let Some(host) = u.host_str()
    {
        return Some(format!(
            "{}://{}:{}",
            u.scheme(),
            host.to_ascii_lowercase(),
            u.port_or_known_default().unwrap_or_default()
        ));
    }
    // A host name, an address (`fd00::25`), a port or a kind: the value
    // itself, never parsed as a URL.
    Some(s.to_ascii_lowercase())
}

/// How an endpoint field is compared ([`Section::SECRET_ENDPOINTS`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Endpoint {
    /// An `http(s)` URL, by its scheme, host and port.
    Url(&'static str),
    /// Any other value (an SMTP host, a port, a security mode), whole.
    Value(&'static str),
}

impl Endpoint {
    fn field(self) -> &'static str {
        match self {
            Endpoint::Url(f) | Endpoint::Value(f) => f,
        }
    }

    fn key(self, v: Option<&Value>) -> Option<String> {
        endpoint(matches!(self, Endpoint::Url(_)), v)
    }
}

/// A patched list's items with the secrets they don't re-send: an item
/// whose `id` matches one in `old` keeps that item's secret when the
/// patch leaves the key out or sends `null`, as long as its `kind` and its
/// `url`'s origin are unchanged (a secret never follows an item to another
/// server unless it's sent again); `""` removes it.
fn keep_secrets(field: &Field, old: Option<&Value>, new: &mut Value) {
    if field.secret_keys.is_empty() {
        return;
    }
    let old_items: Vec<&Map<String, Value>> = match old {
        Some(Value::Array(a)) => a.iter().filter_map(Value::as_object).collect(),
        _ => Vec::new(),
    };
    let Value::Array(items) = new else { return };
    for item in items.iter_mut().filter_map(Value::as_object_mut) {
        let prev = item.get("id").and_then(Value::as_str).and_then(|id| {
            old_items
                .iter()
                .find(|o| o.get("id").and_then(Value::as_str) == Some(id))
        });
        for k in field.secret_keys {
            match item.get(*k) {
                None | Some(Value::Null) => match prev
                    .filter(|p| {
                        Endpoint::Value("kind").key(p.get("kind"))
                            == Endpoint::Value("kind").key(item.get("kind"))
                            && Endpoint::Url("url").key(p.get("url"))
                                == Endpoint::Url("url").key(item.get("url"))
                    })
                    .and_then(|p| p.get(*k))
                {
                    Some(v) => {
                        item.insert(k.to_string(), v.clone());
                    }
                    None => {
                        item.remove(*k);
                    }
                },
                Some(Value::String(s)) if s.is_empty() => {
                    item.remove(*k);
                }
                Some(_) => {}
            }
        }
    }
}

/// How a sealed value is saved: a JSON string with this prefix, then the
/// [`SealedSecret`] in hex.
const SEALED: &str = "sealed:";

/// The record name a field's sealed value is bound to, so it can't be
/// moved onto another field.
fn seal_context(section: &str, field: &str) -> String {
    format!("{KEY_PREFIX}{section}.{field}")
}

/// The record name a list item's sealed secret is bound to, so it can't
/// be moved onto another item or key.
fn item_context(section: &str, field: &str, id: &str, key: &str) -> String {
    format!("{KEY_PREFIX}{section}.{field}.{id}.{key}")
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok())
        .collect()
}

/// The install's [`SecretStore`] (`secrets.key` in the data directory),
/// opened the first time a secret is saved or read, so a registry with
/// nothing secret never creates the key file.
#[derive(Default)]
struct Sealer {
    dir: Option<PathBuf>,
    store: Mutex<Option<Arc<SecretStore>>>,
}

impl std::fmt::Debug for Sealer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Sealer")
    }
}

impl Sealer {
    fn store(&self) -> Result<Arc<SecretStore>, String> {
        let mut slot = self.store.lock().expect("sealer lock");
        if let Some(s) = &*slot {
            return Ok(Arc::clone(s));
        }
        let dir = self.dir.as_deref().ok_or("no data directory")?;
        let s = Arc::new(SecretStore::open_or_create(dir).map_err(|e| e.to_string())?);
        *slot = Some(Arc::clone(&s));
        Ok(s)
    }

    /// `text` sealed for `context`, as `sealed:<hex>`.
    fn seal_text(&self, context: &str, text: &str) -> Result<String, String> {
        let sealed = self
            .store()?
            .seal(context, &Secret::new(text))
            .map_err(|e| e.to_string())?;
        Ok(format!("{SEALED}{}", hex(sealed.as_bytes())))
    }

    /// `sealed:<hex>` opened for `context`. The error is the reason a
    /// start-up log line gives: the key file couldn't be read
    /// (`key_unavailable`), or the value doesn't open with it
    /// (`not_sealed`).
    fn open_text(&self, context: &str, s: &str) -> Result<Secret, &'static str> {
        let sealed = s
            .strip_prefix(SEALED)
            .and_then(unhex)
            .and_then(|b| SealedSecret::from_bytes(b).ok())
            .ok_or("not_sealed")?;
        let store = self.store().map_err(|_| "key_unavailable")?;
        store.open(context, &sealed).map_err(|_| "not_sealed")
    }

    /// A secret-holding field's value as saved JSON text: a secret field
    /// sealed whole, as a JSON string; a list with each item's secret keys
    /// sealed in place, the rest as plain JSON (so losing the key loses
    /// only the secrets).
    fn to_saved(&self, section: &str, field: &Field, v: &Value) -> Result<String, String> {
        if field.secret {
            let sealed = self.seal_text(&seal_context(section, field.name), &v.to_string())?;
            return Ok(Value::String(sealed).to_string());
        }
        let mut v = v.clone();
        if let Value::Array(items) = &mut v {
            for item in items.iter_mut().filter_map(Value::as_object_mut) {
                let id = item
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                for k in field.secret_keys {
                    if let Some(Value::String(s)) = item.get(*k) {
                        let ctx = item_context(section, field.name, &id, k);
                        let sealed = self.seal_text(&ctx, s)?;
                        item.insert(k.to_string(), Value::String(sealed));
                    }
                }
            }
        }
        Ok(v.to_string())
    }

    /// Saved JSON text back to the value, and the reason to log when some
    /// or all of it didn't open. A secret field that doesn't open is
    /// `None`; a list item whose secret doesn't open loses only that key.
    fn read_saved(
        &self,
        section: &str,
        field: &Field,
        text: &str,
    ) -> (Option<Value>, Option<&'static str>) {
        let Ok(mut v) = serde_json::from_str::<Value>(text) else {
            return (None, Some("not_json"));
        };
        if field.secret {
            let Value::String(s) = &v else {
                return (None, Some("not_sealed"));
            };
            return match self.open_text(&seal_context(section, field.name), s) {
                Ok(plain) => match serde_json::from_str(plain.expose()) {
                    Ok(v) => (Some(v), None),
                    Err(_) => (None, Some("not_sealed")),
                },
                Err(why) => (None, Some(why)),
            };
        }
        let mut lost = None;
        if let Value::Array(items) = &mut v {
            for item in items.iter_mut().filter_map(Value::as_object_mut) {
                let id = item
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                for k in field.secret_keys {
                    let Some(stored) = item.remove(*k) else {
                        continue;
                    };
                    let ctx = item_context(section, field.name, &id, k);
                    match stored.as_str().map(|s| self.open_text(&ctx, s)) {
                        Some(Ok(plain)) => {
                            item.insert(k.to_string(), Value::String(plain.expose().to_string()));
                        }
                        Some(Err(why)) => lost = Some(why),
                        None => lost = Some("not_sealed"),
                    }
                }
            }
        }
        (Some(v), lost)
    }
}

/// What the preview says about one field's new value.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Preview {
    pub estimates: Vec<PreviewEstimate>,
    pub warnings: Vec<PreviewWarning>,
}

impl Preview {
    pub fn estimate(mut self, name: &str, value: impl Into<Value>) -> Preview {
        self.estimates.push(PreviewEstimate {
            name: name.to_string(),
            value: value.into(),
        });
        self
    }

    pub fn warn(mut self, code: &str, message: &str) -> Preview {
        self.warnings.push(PreviewWarning {
            code: code.to_string(),
            message: message.to_string(),
        });
        self
    }
}

/// A settings section: one typed struct, its fields' metadata, and how
/// it maps onto [`Config`].
pub trait Section:
    Clone
    + PartialEq
    + std::fmt::Debug
    + Serialize
    + DeserializeOwned
    + utoipa::ToSchema
    + Send
    + Sync
    + 'static
{
    /// Its name in `/settings/{section}` and in saved keys.
    const SECTION: &'static str;
    /// One entry per field of the struct, in display order.
    const FIELDS: &'static [Field];
    /// Its values as `config` has them (defaults, file, environment).
    fn from_config(config: &Config) -> Self;
    /// Writes `field`'s value into `config`, which the services read at
    /// start. Only fields saved in the app are applied: the file's and the
    /// environment's are in `config` already, exactly as written.
    fn apply(&self, field: &str, config: &mut Config);
    /// Checks of `field` beyond its type and bounds; the message is for
    /// people.
    fn check(&self, _field: &str) -> Result<(), String> {
        Ok(())
    }
    /// Each secret field with the fields that say where it's sent (the
    /// Torznab key and its URL). A patch that moves one of them elsewhere
    /// without sending the secret again resets the secret.
    const SECRET_ENDPOINTS: &'static [(&'static str, &'static [Endpoint])] = &[];
    /// Keys a patch may carry beside the fields: confirmations that
    /// [`Section::confirm`] checks and that are never saved.
    const CONFIRMATIONS: &'static [&'static str] = &[];
    /// Checks a whole patch for the confirmations it needs (turning on
    /// adult content needs `confirm_adult`, [`content`]), after its fields
    /// passed their own checks. A refusal changes nothing.
    fn confirm(_patch: &Map<String, Value>) -> Result<(), SettingsError> {
        Ok(())
    }
    /// Estimates and warnings for `field` as these values have it, for
    /// fields marked [`Field::preview`].
    fn preview(&self, _field: &str) -> Preview {
        Preview::default()
    }
}

/// A section's saved values and revision.
#[derive(Debug, Default)]
struct State {
    saved: Map<String, Value>,
    revision: u64,
}

/// A section's state, behind the registry's lock. Its `Debug` names the
/// section only: the saved values may hold secrets.
struct Entry<T: Section> {
    /// Each field's value from defaults, file and environment.
    base: Map<String, Value>,
    /// And where it came from (`Default`, `File` or `Env`).
    base_source: BTreeMap<&'static str, SettingSource>,
    /// Values saved in the app, and the revision.
    state: Mutex<State>,
    /// The values the services started with.
    running: Map<String, Value>,
    tx: watch::Sender<T>,
}

impl<T: Section> std::fmt::Debug for Entry<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Entry")
            .field("section", &T::SECTION)
            .finish_non_exhaustive()
    }
}

fn object<T: Serialize>(value: &T) -> Map<String, Value> {
    match serde_json::to_value(value) {
        Ok(Value::Object(m)) => m,
        _ => panic!("a settings section serializes to an object"),
    }
}

fn from_object<T: DeserializeOwned>(m: &Map<String, Value>) -> Result<T, serde_json::Error> {
    T::deserialize(Value::Object(m.clone()))
}

fn key(section: &str, field: &str) -> String {
    format!("{KEY_PREFIX}{section}.{field}")
}

/// Where a section's revision is saved. `#` keeps it out of the
/// section's `settings.<section>.` prefix.
fn revision_key(section: &str) -> String {
    format!("{KEY_PREFIX}{section}#revision")
}

/// Why a value doesn't deserialize, for people. A secret's message never
/// quotes the value.
fn not_valid(field: &Field, e: impl std::fmt::Display) -> String {
    if field.holds_secret() {
        "Not a valid value.".to_string()
    } else {
        format!("Not a valid value: {e}.")
    }
}

fn find<T: Section>(name: &str) -> Option<&'static Field> {
    T::FIELDS.iter().find(|f| f.name == name)
}

impl<T: Section> Entry<T> {
    fn new(
        config: &Config,
        saved_rows: Vec<(String, String)>,
        revision: u64,
        sealer: &Sealer,
    ) -> Entry<T> {
        let layers = &config.layers;
        let base = object(&T::from_config(config));
        let base_source = T::FIELDS
            .iter()
            .map(|f| {
                let source = if f.env.is_some_and(|v| layers.in_env(v)) {
                    SettingSource::Env
                } else if f.file.is_some_and(|p| layers.in_file(p)) {
                    SettingSource::File
                } else {
                    SettingSource::Default
                };
                (f.name, source)
            })
            .collect();
        let mut entry = Entry {
            base,
            base_source,
            state: Mutex::default(),
            running: Map::new(),
            tx: watch::Sender::new(T::from_config(config)),
        };
        // Saved values a newer or older build, or a hand edit, left
        // invalid are skipped, never fatal.
        let prefix = key(T::SECTION, "");
        let mut saved = Map::new();
        for (k, text) in saved_rows {
            let name = &k[prefix.len()..];
            let Some(field) = find::<T>(name) else {
                // Its name came from the store: only the section is named.
                log_event!(
                    events::SETTINGS_SAVED_IGNORED,
                    section = T::SECTION,
                    reason = "no_such_setting"
                );
                continue;
            };
            let ignored = |reason: &'static str| {
                log_event!(
                    events::SETTINGS_SAVED_IGNORED,
                    section = T::SECTION,
                    field = field.name,
                    reason = reason
                )
            };
            if field.read_only {
                ignored("read_only");
                continue;
            }
            let value = if field.holds_secret() {
                // A key file lost or replaced, or a value never sealed: a
                // secret is skipped like any other invalid value; a list
                // keeps its items without the secrets that didn't open.
                let (value, lost) = sealer.read_saved(T::SECTION, field, &text);
                if let Some(why) = lost {
                    ignored(why);
                }
                match value {
                    Some(v) => v,
                    None => continue,
                }
            } else {
                match serde_json::from_str::<Value>(&text) {
                    Ok(v) => v,
                    Err(_) => {
                        ignored("not_json");
                        continue;
                    }
                }
            };
            let mut trial = entry.effective_map(&saved);
            trial.insert(field.name.to_string(), value.clone());
            match validate::<T>(&trial, field) {
                Ok(_) => {
                    // Never the value: say only that the file's is shadowed.
                    if entry.base_source[field.name] == SettingSource::File
                        && entry.base.get(field.name) != Some(&value)
                    {
                        log_event!(
                            events::SETTINGS_OVERRIDES_FILE,
                            section = T::SECTION,
                            field = field.name
                        );
                    }
                    saved.insert(field.name.to_string(), value);
                }
                // The message can quote the value: never logged.
                Err(_) => ignored("invalid"),
            }
        }
        let effective = entry.effective_map(&saved);
        entry.running = effective.clone();
        entry.tx = watch::Sender::new(from_object(&effective).expect("checked above"));
        *entry.state.get_mut().expect("settings lock") = State { saved, revision };
        entry
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().expect("settings lock")
    }

    /// Whether the environment sets `field`, which then always wins.
    fn env_set(&self, field: &Field) -> bool {
        self.base_source.get(field.name) == Some(&SettingSource::Env)
    }

    /// `base` with the saved values over it, except where the
    /// environment sets the field.
    fn effective_map(&self, saved: &Map<String, Value>) -> Map<String, Value> {
        let mut m = self.base.clone();
        for f in T::FIELDS {
            if let Some(v) = saved.get(f.name)
                && !self.env_set(f)
            {
                m.insert(f.name.to_string(), v.clone());
            }
        }
        m
    }

    /// Where `field`'s effective value comes from.
    fn source(&self, state: &State, field: &Field) -> SettingSource {
        if !self.env_set(field) && state.saved.contains_key(field.name) {
            SettingSource::Db
        } else {
            self.base_source[field.name]
        }
    }

    fn values_of(&self, state: &State) -> SectionValues {
        let effective = self.effective_map(&state.saved);
        let mut fields = BTreeMap::new();
        let mut restart_required = Vec::new();
        for f in T::FIELDS {
            let env = self.env_set(f);
            let value = effective.get(f.name).cloned().unwrap_or(Value::Null);
            if f.restart_required && self.running.get(f.name) != Some(&value) {
                restart_required.push(f.name.to_string());
            }
            fields.insert(
                f.name.to_string(),
                SettingValue {
                    env: if env { f.env.map(str::to_string) } else { None },
                    read_only: env || f.read_only,
                    restart_required: f.restart_required,
                    secrets_set: secrets_set(f, &value),
                    source: self.source(state, f),
                    // A secret is never sent back; `source` says whether
                    // one is set, and `secrets_set` which items have one.
                    value: redact(f, &value),
                },
            );
        }
        SectionValues {
            fields,
            restart_required,
            revision: state.revision,
            section: T::SECTION.to_string(),
        }
    }

    /// Checks `patch` against the current values; returns each field's
    /// new saved value, normalized (`None`: reset, its saved value
    /// removed), the section they make, and whether the saved values
    /// change.
    fn plan(&self, patch: &Map<String, Value>) -> Result<(Changes, T, bool), SettingsError> {
        let state = self.lock();
        let saved = &state.saved;
        let mut next = saved.clone();
        let mut fields = Vec::new();
        for (name, value) in patch {
            if name == IF_REVISION || T::CONFIRMATIONS.contains(&name.as_str()) {
                continue;
            }
            let Some(field) = find::<T>(name) else {
                return Err(SettingsError::UnknownField(name.clone()));
            };
            if field.read_only {
                return Err(SettingsError::ReadOnly(name.clone()));
            }
            if self.env_set(field) {
                return Err(SettingsError::SetByEnv {
                    field: name.clone(),
                    var: field.env.unwrap_or_default().to_string(),
                });
            }
            fields.push(field);
            // `null` resets the field to the file's value or the default;
            // so does an empty secret, which would otherwise show as set.
            if value.is_null() || (field.secret && value.as_str() == Some("")) {
                next.remove(name);
                continue;
            }
            // List items keep the secrets they don't send again.
            let mut value = value.clone();
            keep_secrets(field, self.effective_map(saved).get(name), &mut value);
            // Each field's type and bounds alone first, so an error names
            // the field at fault.
            let mut trial = self.effective_map(saved);
            trial.insert(name.clone(), value.clone());
            typed::<T>(&trial, field).map_err(|message| SettingsError::Invalid {
                field: name.clone(),
                message,
            })?;
            next.insert(name.clone(), value.clone());
        }
        // A saved secret never follows its endpoint to another server
        // unless the patch sends it again: it's reset instead.
        let current = self.effective_map(saved);
        let moved = self.effective_map(&next);
        for (secret, endpoints) in T::SECRET_ENDPOINTS {
            let Some(f) = find::<T>(secret) else { continue };
            if patch.contains_key(*secret) || !next.contains_key(*secret) {
                continue;
            }
            if endpoints.iter().any(|e| {
                let f = e.field();
                patch.contains_key(f) && e.key(current.get(f)) != e.key(moved.get(f))
            }) {
                next.remove(*secret);
                fields.push(f);
            }
        }
        let merged = self.effective_map(&next);
        let section: T = from_object(&merged).map_err(|e| {
            let field = fields.first().copied().unwrap_or(&T::FIELDS[0]);
            // serde may quote any value of the patch: none, if one is a
            // secret.
            let message = if fields.iter().any(|f| f.holds_secret()) {
                "Not a valid value.".to_string()
            } else {
                not_valid(field, e)
            };
            SettingsError::Invalid {
                field: field.name.to_string(),
                message,
            }
        })?;
        // Each patched field's own check again, on the section the whole
        // patch makes: a check that compares fields sees the new values of
        // all of them, and an error still names the field at fault (the
        // first in display order).
        let mut ordered = fields.clone();
        ordered.sort_by_key(|f| T::FIELDS.iter().position(|x| x.name == f.name));
        for f in &ordered {
            section
                .check(f.name)
                .map_err(|message| SettingsError::Invalid {
                    field: f.name.to_string(),
                    message,
                })?;
        }
        // The confirmations last, as B17 has them: after every field's own
        // checks.
        T::confirm(patch)?;
        // Saved as the section serializes them (e.g. a normalized address).
        let normalized = object(&section);
        let out: Changes = fields
            .iter()
            .map(|f| {
                let v = next
                    .contains_key(f.name)
                    .then(|| normalized.get(f.name).cloned().unwrap_or(Value::Null));
                (f.name.to_string(), v)
            })
            .collect();
        let mut after = saved.clone();
        apply_changes(&mut after, &out);
        let changed = after != *saved;
        Ok((out, section, changed))
    }
}

fn apply_changes(saved: &mut Map<String, Value>, changes: &Changes) {
    for (name, value) in changes {
        match value {
            Some(v) => saved.insert(name.clone(), v.clone()),
            None => saved.remove(name),
        };
    }
}

/// Checks `field` of the section `m` makes: its type, its bounds and the
/// section's own check.
fn validate<T: Section>(m: &Map<String, Value>, field: &Field) -> Result<T, String> {
    let section = typed::<T>(m, field)?;
    section.check(field.name)?;
    Ok(section)
}

/// Checks `field` of the section `m` makes: its type and bounds.
fn typed<T: Section>(m: &Map<String, Value>, field: &Field) -> Result<T, String> {
    let section: T = from_object(m).map_err(|e| not_valid(field, e))?;
    if let (Some((min, max)), Some(v)) = (field.range, m.get(field.name))
        && !v.is_null()
    {
        let n = v
            .as_i64()
            .ok_or_else(|| format!("Must be a whole number from {min} to {max}."))?;
        if n < min || n > max {
            return Err(format!("Must be from {min} to {max}."));
        }
    }
    Ok(section)
}

/// A section with its type erased, for the registry's list.
trait Erased: Send + Sync + std::fmt::Debug {
    fn name(&self) -> &'static str;
    fn info(&self) -> SettingsSection;
    fn values(&self) -> SectionValues;
    /// `Some(values now)` when `patch`'s `if_revision` is older than the
    /// section.
    fn stale(&self, patch: &Map<String, Value>) -> Result<Option<SectionValues>, SettingsError>;
    fn plan(&self, patch: &Map<String, Value>) -> Result<Planned, SettingsError>;
    fn preview(&self, planned: &Planned) -> SectionPreview;
    fn commit(&self, planned: Planned) -> SectionValues;
    fn apply(&self, config: &mut Config);
    /// Each field with its effective value, its source and whether the
    /// environment sets it.
    fn current(&self) -> Vec<(&'static Field, Value, SettingSource, bool)>;
    /// A field's name as declared (`'static`, for a log line).
    fn field(&self, name: &str) -> Option<&'static str>;
    /// Whether the field's saved value is sealed.
    fn holds_secret(&self, name: &str) -> bool;
    fn field_meta(&self, name: &str) -> Option<&'static Field>;
    /// A saved secret (`SettingsApi::saved_secret`).
    fn saved_secret(&self, key: &str, endpoints: &[(&str, Value)]) -> Option<String>;
    fn as_any(&self) -> &dyn Any;
}

/// Each patched field's new saved value; `None` removes it.
type Changes = Vec<(String, Option<Value>)>;

/// A checked patch, waiting to be saved.
struct Planned {
    fields: Changes,
    section: Box<dyn Any + Send>,
    /// The saved values change, so the revision goes up.
    changed: bool,
    /// The revision once saved.
    revision: u64,
}

impl<T: Section> Erased for Entry<T> {
    fn name(&self) -> &'static str {
        T::SECTION
    }

    fn info(&self) -> SettingsSection {
        SettingsSection {
            fields: T::FIELDS
                .iter()
                .map(|f| SettingField {
                    env: f.env.map(str::to_string),
                    help_key: format!("settings.{}.{}.help", T::SECTION, f.name),
                    label_key: format!("settings.{}.{}.label", T::SECTION, f.name),
                    name: f.name.to_string(),
                    read_only: f.read_only,
                    restart_required: f.restart_required,
                })
                .collect(),
            help_key: format!("settings.{}.help", T::SECTION),
            label_key: format!("settings.{}.label", T::SECTION),
            schema: schema::<T>(),
            section: T::SECTION.to_string(),
        }
    }

    fn values(&self) -> SectionValues {
        self.values_of(&self.lock())
    }

    fn stale(&self, patch: &Map<String, Value>) -> Result<Option<SectionValues>, SettingsError> {
        let Some(want) = patch.get(IF_REVISION) else {
            return Ok(None);
        };
        let want = want.as_u64().ok_or_else(|| SettingsError::Invalid {
            field: IF_REVISION.to_string(),
            message: "Must be the section's revision, a whole number from GET /settings/{section}."
                .to_string(),
        })?;
        let state = self.lock();
        Ok((want != state.revision).then(|| self.values_of(&state)))
    }

    fn plan(&self, patch: &Map<String, Value>) -> Result<Planned, SettingsError> {
        let (fields, section, changed) = Entry::plan(self, patch)?;
        let revision = self.lock().revision.saturating_add(u64::from(changed));
        Ok(Planned {
            fields,
            section: Box::new(section),
            changed,
            revision,
        })
    }

    fn preview(&self, planned: &Planned) -> SectionPreview {
        let section = planned
            .section
            .downcast_ref::<T>()
            .expect("planned by this entry");
        let values = object(section);
        let fields = planned
            .fields
            .iter()
            .filter_map(|(name, _)| {
                let f = find::<T>(name)?;
                let p = if f.preview {
                    section.preview(f.name)
                } else {
                    Preview::default()
                };
                let value = redact(f, values.get(f.name).unwrap_or(&Value::Null));
                Some((
                    name.clone(),
                    FieldPreview {
                        estimates: p.estimates,
                        restart_required: f.restart_required,
                        value,
                        warnings: p.warnings,
                    },
                ))
            })
            .collect();
        SectionPreview {
            fields,
            revision: self.lock().revision,
        }
    }

    fn commit(&self, planned: Planned) -> SectionValues {
        let mut state = self.lock();
        apply_changes(&mut state.saved, &planned.fields);
        state.revision = planned.revision;
        let section = *planned
            .section
            .downcast::<T>()
            .expect("planned by this entry");
        let values = self.values_of(&state);
        drop(state);
        // Only a real change wakes the subscribers.
        self.tx.send_if_modified(|current| {
            let changed = *current != section;
            *current = section;
            changed
        });
        values
    }

    fn apply(&self, config: &mut Config) {
        let state = self.lock();
        let current = self.tx.borrow();
        for f in T::FIELDS {
            if state.saved.contains_key(f.name) && !self.env_set(f) && !f.read_only {
                current.apply(f.name, config);
            }
        }
    }

    fn current(&self) -> Vec<(&'static Field, Value, SettingSource, bool)> {
        let state = self.lock();
        let effective = self.effective_map(&state.saved);
        T::FIELDS
            .iter()
            .map(|f| {
                let v = effective.get(f.name).cloned().unwrap_or(Value::Null);
                (f, v, self.source(&state, f), self.env_set(f))
            })
            .collect()
    }

    fn field(&self, name: &str) -> Option<&'static str> {
        find::<T>(name).map(|f| f.name)
    }

    fn holds_secret(&self, name: &str) -> bool {
        find::<T>(name).is_some_and(Field::holds_secret)
    }

    fn field_meta(&self, name: &str) -> Option<&'static Field> {
        find::<T>(name)
    }

    fn saved_secret(&self, key: &str, endpoints: &[(&str, Value)]) -> Option<String> {
        let state = self.lock();
        let current = self.effective_map(&state.saved);
        // Every field that says where the secret goes must be given, and
        // point where the saved one does.
        let same = |bound: &[Endpoint], saved: &dyn Fn(&str) -> Option<Value>| {
            bound.iter().all(|b| {
                let n = b.field();
                endpoints
                    .iter()
                    .any(|(e, v)| *e == n && b.key(Some(v)) == b.key(saved(n).as_ref()))
            })
        };
        if let Some((field, rest)) = key.split_once('.') {
            // `<field>.<item id>.<key>`: an item's secret goes where its
            // `url` and `kind` say, as `keep_secrets` holds it.
            let (id, k) = rest.rsplit_once('.')?;
            let f = find::<T>(field)?;
            if !f.secret_keys.contains(&k) {
                return None;
            }
            let item = current
                .get(field)?
                .as_array()?
                .iter()
                .filter_map(Value::as_object)
                .find(|i| i.get("id").and_then(Value::as_str) == Some(id))?;
            if !same(&[Endpoint::Url("url"), Endpoint::Value("kind")], &|n| {
                item.get(n).cloned()
            }) {
                return None;
            }
            return item.get(k)?.as_str().map(str::to_string);
        }
        let f = find::<T>(key)?;
        if !f.secret || self.env_set(f) || !state.saved.contains_key(key) {
            return None;
        }
        let bound = T::SECRET_ENDPOINTS
            .iter()
            .find(|(s, _)| *s == key)
            .map_or(&[][..], |(_, e)| *e);
        if !same(bound, &|n| current.get(n).cloned()) {
            return None;
        }
        current.get(key)?.as_str().map(str::to_string)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// `T`'s JSON schema, inline, with each number field's bounds, `readOnly`
/// on the fields the app never changes, `writeOnly` on secrets, and the
/// form's hints (`x-…`, `README.md`).
fn schema<T: Section>() -> Value {
    let mut schema =
        serde_json::to_value(<T as utoipa::PartialSchema>::schema()).expect("a schema serializes");
    if let Some(props) = schema.get_mut("properties").and_then(Value::as_object_mut) {
        for (order, f) in T::FIELDS.iter().enumerate() {
            let Some(Value::Object(p)) = props.get_mut(f.name) else {
                continue;
            };
            if let Some((min, max)) = f.range {
                p.insert("minimum".into(), min.into());
                p.insert("maximum".into(), max.into());
            }
            if f.read_only {
                p.insert("readOnly".into(), true.into());
            }
            if f.secret {
                p.insert("writeOnly".into(), true.into());
            }
            // Each item's credential keys are write-only too.
            for k in f.secret_keys {
                if let Some(Value::Object(item)) = p.get_mut("items")
                    && let Some(Value::Object(ip)) =
                        item.get_mut("properties").and_then(|x| x.get_mut(*k))
                {
                    ip.insert("writeOnly".into(), true.into());
                }
            }
            if let Some(format) = f.format {
                p.insert("format".into(), format.into());
            }
            p.insert("x-group".into(), f.group.into());
            p.insert("x-order".into(), order.into());
            p.insert("x-advanced".into(), f.advanced.into());
            p.insert("x-preview".into(), f.preview.into());
            let optional = [
                ("x-unit", f.unit.map(Value::from)),
                ("x-widget", f.widget.map(Value::from)),
                (
                    "x-auto",
                    f.auto.map(|a| match a {
                        Auto::Int(n) => Value::from(n),
                        Auto::Str(s) => Value::from(s),
                    }),
                ),
                (
                    "x-keywords",
                    f.keywords
                        .then(|| format!("settings.{}.{}.keywords", T::SECTION, f.name).into()),
                ),
                ("x-danger", f.danger.map(|d| d.name().into())),
                ("x-config", f.file.map(Value::from)),
                ("x-env", f.env.map(Value::from)),
            ];
            for (k, v) in optional {
                if let Some(v) = v {
                    p.insert(k.into(), v);
                }
            }
        }
    }
    schema
}

/// The registry: every section, the connection it saves through, and a
/// lock that makes each batch's check, save and publish one step.
#[derive(Debug)]
pub struct Registry {
    store: Arc<Mutex<Store>>,
    sections: Vec<Box<dyn Erased>>,
    write: Mutex<()>,
    /// `POST /send`'s limits, one set per install.
    send_limits: lodestar_api::settings::SendLimits,
    /// Seals and opens secret fields' saved values.
    sealer: Sealer,
}

/// A checked batch: each patched section (by its index in display order)
/// and its plan.
type Plans = Vec<(usize, Planned)>;

impl Registry {
    /// Builds every section from `config` (defaults, file, environment)
    /// and what `store` has saved.
    pub fn load(
        config: &Config,
        store: Arc<Mutex<Store>>,
    ) -> Result<Registry, lodestar_store::Error> {
        let mut registry = Registry::empty(store);
        registry.move_log_level()?;
        // One line per section, in display order (README.md).
        registry.register::<general::General>(config)?;
        registry.register::<network::Network>(config)?;
        registry.register::<crawler::Crawler>(config)?;
        registry.register::<transport::Transport>(config)?;
        registry.register::<storage::Storage>(config)?;
        registry.register::<search::Search>(config)?;
        registry.register::<trackers::Trackers>(config)?;
        registry.register::<content::Content>(config)?;
        registry.register::<integrations::Integrations>(config)?;
        registry.register::<notifications::Notifications>(config)?;
        registry.register::<insights::Insights>(config)?;
        registry.register::<updates::Updates>(config)?;
        registry.register::<preferences::Preferences>(config)?;
        registry.register::<logging::Logging>(config)?;
        Ok(registry)
    }

    /// An earlier build saved the log filter as `general.log_level`; it's
    /// the `logging` section's `verbosity`, `components` and `libraries`
    /// now (§12.1's aliases). Moved once, in one transaction, before the
    /// sections load. A value saved under the new keys already wins.
    fn move_log_level(&self) -> Result<(), lodestar_store::Error> {
        let old = key("general", "log_level");
        let mut store = self.store.lock().expect("store lock");
        let Some(text) = store.setting_text(&old)? else {
            return Ok(());
        };
        let saved: Vec<(String, String)> = store.settings_with_prefix(&key("logging", ""))?;
        let moved: Vec<(String, String)> = logging::move_log_level(&text)
            .into_iter()
            .map(|(field, value)| (key("logging", field), value))
            .filter(|(k, _)| !saved.iter().any(|(s, _)| s == k))
            .collect();
        let set: Vec<(&str, &str)> = moved
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();
        store.update_settings_text(&set, &[old.as_str()])?;
        drop(store);
        lodestar_log::log_event!(crate::logging::events::SETTINGS_LOG_LEVEL_MOVED);
        Ok(())
    }

    fn empty(store: Arc<Mutex<Store>>) -> Registry {
        Registry {
            store,
            sections: Vec::new(),
            write: Mutex::new(()),
            send_limits: lodestar_api::settings::SendLimits::new(),
            sealer: Sealer::default(),
        }
    }

    fn register<T: Section>(&mut self, config: &Config) -> Result<(), lodestar_store::Error> {
        // Secret fields are sealed with the install's key, in the data
        // directory (context `settings.<section>.<field>`).
        if self.sealer.dir.is_none() {
            self.sealer.dir = Some(config.data_dir.clone());
        }
        assert!(
            self.sections.iter().all(|s| s.name() != T::SECTION),
            "settings section {} registered twice",
            T::SECTION
        );
        assert!(
            !RESERVED_SECTIONS.contains(&T::SECTION),
            "settings section {} would hide the route /settings/{}",
            T::SECTION,
            T::SECTION
        );
        assert!(
            T::FIELDS
                .iter()
                .all(|f| f.name != IF_REVISION && !T::CONFIRMATIONS.contains(&f.name)),
            "settings section {}: a field can't be named like a patch key",
            T::SECTION
        );
        let mut store = self.store.lock().expect("store lock");
        let prefix = key(T::SECTION, "");
        let (retired, rows): (Vec<_>, Vec<_>) = store
            .settings_with_prefix(&prefix)?
            .into_iter()
            .partition(|(k, _)| {
                let name = &k[prefix.len()..];
                T::FIELDS.iter().any(|f| f.name == name && f.read_only)
            });
        // A field that has since become read-only (an earlier build saved
        // it): removed, so it doesn't come back at every start.
        if !retired.is_empty() {
            let keys: Vec<&str> = retired.iter().map(|(k, _)| k.as_str()).collect();
            store.update_settings_text(&[], &keys)?;
            for k in keys {
                if let Some(f) = find::<T>(&k[prefix.len()..]) {
                    log_event!(
                        events::SETTINGS_RETIRED,
                        section = T::SECTION,
                        field = f.name
                    );
                }
            }
        }
        let revision = match store.setting_text(&revision_key(T::SECTION)) {
            Ok(Some(text)) => text.parse().unwrap_or_else(|_| {
                log_event!(events::SETTINGS_REVISION_RESET, section = T::SECTION);
                0
            }),
            Ok(None) => 0,
            Err(e) => return Err(e),
        };
        drop(store);
        self.sections.push(Box::new(Entry::<T>::new(
            config,
            rows,
            revision,
            &self.sealer,
        )));
        Ok(())
    }

    fn entry<T: Section>(&self) -> &Entry<T> {
        self.sections
            .iter()
            .find_map(|s| s.as_any().downcast_ref::<Entry<T>>())
            .unwrap_or_else(|| panic!("settings section {} isn't registered", T::SECTION))
    }

    /// Follows `T`'s effective values; it changes when a patch does.
    pub fn subscribe<T: Section>(&self) -> watch::Receiver<T> {
        self.entry::<T>().tx.subscribe()
    }

    /// `T`'s effective values now.
    #[cfg(test)]
    pub fn get<T: Section>(&self) -> T {
        self.entry::<T>().tx.borrow().clone()
    }

    /// `config` with every saved value applied: what the services start
    /// with.
    pub fn effective_config(&self, config: &Config) -> Config {
        let mut out = config.clone();
        for s in &self.sections {
            s.apply(&mut out);
        }
        out
    }

    fn find(&self, section: &str) -> Option<&dyn Erased> {
        self.sections
            .iter()
            .find(|s| s.name() == section)
            .map(|s| s.as_ref())
    }

    /// Checks a whole batch, in display order: every revision first (all
    /// stale sections are reported together), then each section's fields
    /// and confirmations. The caller holds the write lock.
    fn plan(&self, batch: &SettingsBatch) -> Result<Plans, SettingsFailure> {
        let mut patched = Vec::new();
        for (name, patch) in &batch.0 {
            let i = self
                .sections
                .iter()
                .position(|s| s.name() == name)
                .ok_or_else(|| SettingsFailure::new(name, SettingsError::UnknownSection))?;
            patched.push((i, &patch.0));
        }
        patched.sort_by_key(|(i, _)| *i);
        let mut stale = Vec::new();
        for (i, patch) in &patched {
            let s = &self.sections[*i];
            if let Some(values) = s
                .stale(patch)
                .map_err(|e| SettingsFailure::new(s.name(), e))?
            {
                stale.push(values);
            }
        }
        if let Some(first) = stale.first() {
            return Err(SettingsFailure::new(
                first.section.clone(),
                SettingsError::Stale(stale),
            ));
        }
        patched
            .into_iter()
            .map(|(i, patch)| {
                let s = &self.sections[i];
                s.plan(patch)
                    .map(|p| (i, p))
                    .map_err(|e| SettingsFailure::new(s.name(), e))
            })
            .collect()
    }

    /// Saves checked plans in one transaction, then applies them.
    fn commit(&self, plans: Plans) -> Result<Saved, SettingsFailure> {
        let mut set = Vec::new();
        let mut remove = Vec::new();
        for (i, planned) in &plans {
            if !planned.changed {
                continue;
            }
            let s = &self.sections[*i];
            let section = s.name();
            for (name, value) in &planned.fields {
                match value {
                    Some(v) if s.holds_secret(name) => {
                        let field = s.field_meta(name).expect("a planned field");
                        // Never the error's detail: it can name the key
                        // file's path.
                        let text = self.sealer.to_saved(section, field, v).map_err(|_| {
                            SettingsFailure::new(
                                section,
                                {
                                    if let Some(field) = s.field(name) {
                                        log_event!(
                                            events::SETTINGS_SEAL_FAILED,
                                            section = section,
                                            field = field
                                        );
                                    }
                                    SettingsError::Store(
                                        "Couldn't seal a secret: check the data directory's secrets.key."
                                            .into(),
                                    )
                                },
                            )
                        })?;
                        set.push((key(section, name), text));
                    }
                    Some(v) => set.push((key(section, name), v.to_string())),
                    None => remove.push(key(section, name)),
                }
            }
            set.push((revision_key(section), planned.revision.to_string()));
        }
        if !set.is_empty() || !remove.is_empty() {
            let set: Vec<(&str, &str)> =
                set.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
            let remove: Vec<&str> = remove.iter().map(String::as_str).collect();
            // Saves, resets and revisions together, in one transaction.
            self.store
                .lock()
                .expect("store lock")
                .update_settings_text(&set, &remove)
                .map_err(|e| SettingsFailure::from(SettingsError::Store(e.to_string())))?;
        }
        let mut saved = Saved {
            sections: Vec::new(),
            changed: Vec::new(),
        };
        for (i, planned) in plans {
            let s = &self.sections[i];
            if planned.changed {
                saved.changed.push((s.name().to_string(), planned.revision));
                // Field names only, never values (§12.1).
                for (name, _) in &planned.fields {
                    if let Some(field) = s.field(name) {
                        log_event!(
                            events::SETTINGS_SAVED,
                            section = s.name(),
                            field = field,
                            revision = lodestar_log::Count(planned.revision)
                        );
                    }
                }
            }
            saved.sections.push(s.commit(planned));
        }
        Ok(saved)
    }
}

impl SettingsApi for Registry {
    fn index(&self) -> SettingsIndex {
        SettingsIndex {
            sections: self.sections.iter().map(|s| s.info()).collect(),
        }
    }

    fn section(&self, section: &str) -> Option<SectionValues> {
        self.find(section).map(|s| s.values())
    }

    fn save(&self, batch: SettingsBatch) -> Result<Saved, SettingsFailure> {
        let _write = self.write.lock().expect("settings write lock");
        let plans = self.plan(&batch)?;
        self.commit(plans)
    }

    fn preview(&self, batch: SettingsBatch) -> Result<SettingsPreview, SettingsFailure> {
        let _write = self.write.lock().expect("settings write lock");
        let plans = self.plan(&batch)?;
        Ok(SettingsPreview {
            sections: plans
                .iter()
                .map(|(i, p)| {
                    let s = &self.sections[*i];
                    (s.name().to_string(), s.preview(p))
                })
                .collect(),
        })
    }

    fn export(&self, preferences: bool) -> String {
        // Never half of a batch that's being saved.
        let _write = self.write.lock().expect("settings write lock");
        portable::export(self, preferences)
    }

    fn import(&self, import: SettingsImport, dry_run: bool) -> Result<Imported, SettingsFailure> {
        portable::import(self, import, dry_run)
    }

    fn saved_secret(
        &self,
        section: &str,
        key: &str,
        endpoints: &[(&str, Value)],
    ) -> Option<String> {
        self.find(section)?.saved_secret(key, endpoints)
    }

    fn send_limits(&self) -> &lodestar_api::settings::SendLimits {
        &self.send_limits
    }

    fn download_client(&self, id: Option<&str>) -> Option<SavedClient> {
        let entry = self
            .find(integrations::Integrations::SECTION)?
            .as_any()
            .downcast_ref::<Entry<integrations::Integrations>>()?;
        let section = entry.tx.borrow();
        let c = match id {
            Some(id) => section.client(id),
            None => section.default_client(),
        }?;
        Some(SavedClient {
            id: c.id.clone(),
            kind: c.kind.api(),
            url: c.url.clone(),
            username: c.username.clone(),
            password: c.password.as_ref().map(|p| p.0.clone()),
            category: c.category.clone(),
            save_path: c.save_path.clone(),
            paused: c.paused,
        })
    }
}

#[cfg(test)]
mod api_tests;
#[cfg(test)]
mod logging_tests;
#[cfg(test)]
mod screen_tests;
#[cfg(test)]
mod tests;
