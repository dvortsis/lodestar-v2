//! The runtime settings registry (architecture §10.10, §11): typed
//! sections the app can show and change, layered as defaults <
//! `config.toml` < saved in the app (the database) < `LODESTAR_*`
//! environment variables.
//!
//! - A field an environment variable sets is read-only here: a change is
//!   refused, naming the variable, and a saved value never overrides it.
//! - A change is checked whole, saved in one transaction, and published
//!   on the section's `watch` channel. Subsystems that can apply it live
//!   follow the channel ([`log`], [`crawler::follow_intensity`]); fields
//!   that can't are `restart_required`, and the section lists the ones
//!   whose saved value waits for a restart.
//! - `null` for a field resets it: its saved value is removed, and the
//!   file's value or the default applies again.
//! - Nothing here reaches filtering state (rules, the floor, packs,
//!   subscriptions: the filters API's, §20.7) or secrets (the
//!   integrations `SecretStore`), and no field moves files at runtime.
//!
//! See `README.md` for how a later slice adds a section.

pub mod crawler;
pub mod general;
pub mod log;
pub mod network;
pub mod search;
pub mod storage;
pub mod transport;

use std::any::Any;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use lodestar_api::settings::{
    SectionValues, SettingField, SettingSource, SettingValue, SettingsApi, SettingsError,
    SettingsIndex, SettingsPatch, SettingsSection,
};
use lodestar_store::Store;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Map, Value};
use tokio::sync::watch;

use crate::config::Config;

/// Every key the registry saves starts with this, then
/// `<section>.<field>`.
pub const KEY_PREFIX: &str = "settings.";

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
}

/// A section's state, behind the registry's lock.
#[derive(Debug)]
struct Entry<T: Section> {
    /// Each field's value from defaults, file and environment.
    base: Map<String, Value>,
    /// And where it came from (`Default`, `File` or `Env`).
    base_source: BTreeMap<&'static str, SettingSource>,
    /// Values saved in the app.
    saved: Mutex<Map<String, Value>>,
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

impl<T: Section> Entry<T> {
    fn new(config: &Config, saved_rows: Vec<(String, String)>) -> Entry<T> {
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
            saved: Mutex::new(Map::new()),
            running: Map::new(),
            tx: watch::Sender::new(T::from_config(config)),
        };
        // Saved values a newer or older build, or a hand edit, left
        // invalid are skipped, never fatal.
        let prefix = key(T::SECTION, "");
        let mut saved = Map::new();
        for (k, text) in saved_rows {
            let name = &k[prefix.len()..];
            let Some(field) = T::FIELDS.iter().find(|f| f.name == name) else {
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
                    tracing::warn!("settings: ignoring saved {k}: {e}");
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
        *entry.saved.get_mut().expect("settings lock") = saved;
        entry
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

    fn values(&self) -> SectionValues {
        let saved = self.saved.lock().expect("settings lock");
        let effective = self.effective_map(&saved);
        let mut fields = BTreeMap::new();
        let mut restart_required = Vec::new();
        for f in T::FIELDS {
            let env = self.env_set(f);
            let source = if !env && saved.contains_key(f.name) {
                SettingSource::Db
            } else {
                self.base_source[f.name]
            };
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
                    source,
                    value,
                },
            );
        }
        SectionValues {
            fields,
            restart_required,
            section: T::SECTION.to_string(),
        }
    }

    /// Checks `patch` against the current values; returns each field's
    /// new saved value, normalized (`None`: reset, its saved value
    /// removed), and the section they make.
    fn plan(&self, patch: &Map<String, Value>) -> Result<(Changes, T), SettingsError> {
        let saved = self.saved.lock().expect("settings lock");
        let mut next = saved.clone();
        let mut fields = Vec::new();
        for (name, value) in patch {
            let Some(field) = T::FIELDS.iter().find(|f| f.name == name) else {
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
            let mut trial = self.effective_map(&saved);
            trial.insert(name.clone(), value.clone());
            validate::<T>(&trial, field).map_err(|message| SettingsError::Invalid {
                field: name.clone(),
                message,
            })?;
            next.insert(name.clone(), value.clone());
        }
        let merged = self.effective_map(&next);
        let section: T = from_object(&merged).map_err(|e| SettingsError::Invalid {
            field: patch.keys().next().cloned().unwrap_or_default(),
            message: format!("Not a valid value: {e}."),
        })?;
        // Saved as the section serializes them (e.g. a normalized address).
        let normalized = object(&section);
        let out = fields
            .iter()
            .map(|f| {
                let v = next
                    .contains_key(f.name)
                    .then(|| normalized.get(f.name).cloned().unwrap_or(Value::Null));
                (f.name.to_string(), v)
            })
            .collect();
        Ok((out, section))
    }
}

/// Checks `field` of the section `m` makes: its type, its bounds and the
/// section's own check.
fn validate<T: Section>(m: &Map<String, Value>, field: &Field) -> Result<T, String> {
    let section: T = from_object(m).map_err(|e| format!("Not a valid value: {e}."))?;
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
    fn plan(&self, patch: &Map<String, Value>) -> Result<Planned, SettingsError>;
    fn commit(&self, planned: Planned);
    fn apply(&self, config: &mut Config);
    fn as_any(&self) -> &dyn Any;
}

/// Each patched field's new saved value; `None` removes it.
type Changes = Vec<(String, Option<Value>)>;

/// A checked patch, waiting to be saved.
struct Planned {
    fields: Changes,
    section: Box<dyn Any + Send>,
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
        Entry::values(self)
    }

    fn plan(&self, patch: &Map<String, Value>) -> Result<Planned, SettingsError> {
        let (fields, section) = Entry::plan(self, patch)?;
        Ok(Planned {
            fields,
            section: Box::new(section),
        })
    }

    fn commit(&self, planned: Planned) {
        let mut saved = self.saved.lock().expect("settings lock");
        for (name, value) in planned.fields {
            match value {
                Some(v) => saved.insert(name, v),
                None => saved.remove(&name),
            };
        }
        let section = *planned
            .section
            .downcast::<T>()
            .expect("planned by this entry");
        drop(saved);
        // Only a real change wakes the subscribers.
        self.tx.send_if_modified(|current| {
            let changed = *current != section;
            *current = section;
            changed
        });
    }

    fn apply(&self, config: &mut Config) {
        let saved = self.saved.lock().expect("settings lock");
        let current = self.tx.borrow();
        for f in T::FIELDS {
            if saved.contains_key(f.name) && !self.env_set(f) && !f.read_only {
                current.apply(f.name, config);
            }
        }
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// `T`'s JSON schema, inline, with each number field's bounds and
/// `readOnly` on the fields the app never changes.
fn schema<T: Section>() -> Value {
    let mut schema =
        serde_json::to_value(<T as utoipa::PartialSchema>::schema()).expect("a schema serializes");
    if let Some(props) = schema.get_mut("properties").and_then(Value::as_object_mut) {
        for f in T::FIELDS {
            if let (Some((min, max)), Some(Value::Object(p))) = (f.range, props.get_mut(f.name)) {
                p.insert("minimum".into(), min.into());
                p.insert("maximum".into(), max.into());
            }
            if let (true, Some(Value::Object(p))) = (f.read_only, props.get_mut(f.name)) {
                p.insert("readOnly".into(), true.into());
            }
        }
    }
    schema
}

/// The registry: every section, the connection it saves through, and a
/// lock that makes each patch's check, save and publish one step.
#[derive(Debug)]
pub struct Registry {
    store: Arc<Mutex<Store>>,
    sections: Vec<Box<dyn Erased>>,
    write: Mutex<()>,
}

impl Registry {
    /// Builds every section from `config` (defaults, file, environment)
    /// and what `store` has saved.
    pub fn load(
        config: &Config,
        store: Arc<Mutex<Store>>,
    ) -> Result<Registry, lodestar_store::Error> {
        let mut registry = Registry {
            store,
            sections: Vec::new(),
            write: Mutex::new(()),
        };
        // One line per section, in display order (README.md).
        registry.register::<general::General>(config)?;
        registry.register::<network::Network>(config)?;
        registry.register::<crawler::Crawler>(config)?;
        registry.register::<transport::Transport>(config)?;
        registry.register::<storage::Storage>(config)?;
        registry.register::<search::Search>(config)?;
        Ok(registry)
    }

    fn register<T: Section>(&mut self, config: &Config) -> Result<(), lodestar_store::Error> {
        assert!(
            self.sections.iter().all(|s| s.name() != T::SECTION),
            "settings section {} registered twice",
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
        drop(store);
        self.sections.push(Box::new(Entry::<T>::new(config, rows)));
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

    fn patch(&self, section: &str, patch: SettingsPatch) -> Result<SectionValues, SettingsError> {
        let entry = self.find(section).ok_or(SettingsError::UnknownSection)?;
        let _write = self.write.lock().expect("settings write lock");
        let planned = entry.plan(&patch.0)?;
        let mut set = Vec::new();
        let mut remove = Vec::new();
        for (name, value) in &planned.fields {
            match value {
                Some(v) => set.push((key(section, name), v.to_string())),
                None => remove.push(key(section, name)),
            }
        }
        let set: Vec<(&str, &str)> = set.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
        let remove: Vec<&str> = remove.iter().map(String::as_str).collect();
        // Saves and resets together, in one transaction.
        self.store
            .lock()
            .expect("store lock")
            .update_settings_text(&set, &remove)
            .map_err(|e| SettingsError::Store(e.to_string()))?;
        entry.commit(planned);
        Ok(entry.values())
    }
}

#[cfg(test)]
mod api_tests;
#[cfg(test)]
mod tests;
