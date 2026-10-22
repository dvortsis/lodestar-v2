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
//!   at runtime. A secret field (`writeOnly`) is never returned, exported
//!   or put in an error message.
//!
//! See `README.md` for how a later slice adds a section.

pub mod content;
pub mod crawler;
pub mod general;
pub mod log;
pub mod network;
mod portable;
pub mod preferences;
pub mod search;
pub mod storage;
pub mod transport;

use std::any::Any;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard};

use lodestar_api::settings::{
    FieldPreview, IF_REVISION, Imported, PreviewEstimate, PreviewWarning, Saved, SectionPreview,
    SectionValues, SettingField, SettingSource, SettingValue, SettingsApi, SettingsBatch,
    SettingsError, SettingsFailure, SettingsImport, SettingsIndex, SettingsPreview,
    SettingsSection,
};
use lodestar_store::Store;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Map, Value};
use tokio::sync::watch;

use crate::config::Config;

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
/// The whole vocabulary, though today's sections use only `Disruptive`.
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
    /// A string format the form checks (`socket-addr`, `log-filter`).
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
        }
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

    /// No section has a secret yet (credentials live in the integrations
    /// `SecretStore`); the tests hold the rules for the first one.
    #[allow(dead_code)]
    pub const fn secret(self) -> Field {
        Field {
            secret: true,
            ..self
        }
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

/// A section's state, behind the registry's lock.
#[derive(Debug)]
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
    if field.secret {
        "Not a valid value.".to_string()
    } else {
        format!("Not a valid value: {e}.")
    }
}

fn find<T: Section>(name: &str) -> Option<&'static Field> {
    T::FIELDS.iter().find(|f| f.name == name)
}

impl<T: Section> Entry<T> {
    fn new(config: &Config, saved_rows: Vec<(String, String)>, revision: u64) -> Entry<T> {
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
                tracing::warn!("settings: ignoring saved {k}: no such setting");
                continue;
            };
            if field.read_only {
                tracing::warn!("settings: ignoring saved {k}: it can't be set in the app");
                continue;
            }
            let value = match serde_json::from_str::<Value>(&text) {
                Ok(v) => v,
                Err(e) => {
                    tracing::warn!("settings: ignoring saved {k}: {}", not_valid(field, e));
                    continue;
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
                        tracing::info!(
                            "settings: {}.{} is saved in the app and overrides config.toml; \
                             set it to null in the app to reset",
                            T::SECTION,
                            field.name
                        );
                    }
                    saved.insert(field.name.to_string(), value);
                }
                Err(message) => tracing::warn!("settings: ignoring saved {k}: {message}"),
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
                    source: self.source(state, f),
                    // A secret is never sent back; `source` says whether
                    // one is set.
                    value: if f.secret { Value::Null } else { value },
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
            // `null` resets the field to the file's value or the default.
            if value.is_null() {
                next.remove(name);
                continue;
            }
            // Each field alone first, so an error names the field at fault.
            let mut trial = self.effective_map(saved);
            trial.insert(name.clone(), value.clone());
            validate::<T>(&trial, field).map_err(|message| SettingsError::Invalid {
                field: name.clone(),
                message,
            })?;
            next.insert(name.clone(), value.clone());
        }
        T::confirm(patch)?;
        let merged = self.effective_map(&next);
        let section: T = from_object(&merged).map_err(|e| {
            let field = fields.first().copied().unwrap_or(&T::FIELDS[0]);
            SettingsError::Invalid {
                field: field.name.to_string(),
                message: not_valid(field, e),
            }
        })?;
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
    section.check(field.name)?;
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
        let revision = self.lock().revision + u64::from(changed);
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
                let value = match f.secret {
                    true => Value::Null,
                    false => values.get(f.name).cloned().unwrap_or(Value::Null),
                };
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
        // One line per section, in display order (README.md).
        registry.register::<general::General>(config)?;
        registry.register::<network::Network>(config)?;
        registry.register::<crawler::Crawler>(config)?;
        registry.register::<transport::Transport>(config)?;
        registry.register::<storage::Storage>(config)?;
        registry.register::<search::Search>(config)?;
        registry.register::<content::Content>(config)?;
        registry.register::<preferences::Preferences>(config)?;
        Ok(registry)
    }

    fn empty(store: Arc<Mutex<Store>>) -> Registry {
        Registry {
            store,
            sections: Vec::new(),
            write: Mutex::new(()),
        }
    }

    fn register<T: Section>(&mut self, config: &Config) -> Result<(), lodestar_store::Error> {
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
                tracing::info!(
                    "settings: removed saved {}: it can only be set in config.toml or the environment now",
                    &k[KEY_PREFIX.len()..]
                );
            }
        }
        let revision = match store.setting_text(&revision_key(T::SECTION)) {
            Ok(Some(text)) => text.parse().unwrap_or_else(|_| {
                tracing::warn!(
                    "settings: the saved revision of {} isn't a number; starting again",
                    T::SECTION
                );
                0
            }),
            Ok(None) => 0,
            Err(e) => return Err(e),
        };
        drop(store);
        self.sections
            .push(Box::new(Entry::<T>::new(config, rows, revision)));
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
            let section = self.sections[*i].name();
            for (name, value) in &planned.fields {
                match value {
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
}

#[cfg(test)]
mod api_tests;
#[cfg(test)]
mod screen_tests;
#[cfg(test)]
mod tests;
