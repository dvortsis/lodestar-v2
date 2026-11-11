//! `lodestar settings get|set|reset` (architecture §10.10.6 "Recovery
//! without the UI"): the settings saved in the app, read and changed on
//! the **stopped** store.
//!
//! - **The same rules as the API.** The command loads the settings
//!   registry ([`Registry`]) over an in-memory copy of the store's
//!   `settings.` rows, saves through it exactly as `PATCH /settings` does
//!   (types, bounds, each section's checks, revisions, the adult
//!   confirmation), then writes the rows that changed back to the store in
//!   one transaction. There is no second copy of the rules.
//! - **Offline.** It takes the store lock ([`StoreLock`]) for the whole
//!   command and refuses (exit 3) while anything has the store open, or
//!   while an update rollback is in progress (§12.2).
//! - **What `set` refuses** (plan Q2, `docs/decisions/2026-11-11-PK-cli.md`):
//!   - a field that holds a secret (`writeOnly`, or a list whose items
//!     have one): the value would sit on the command line, in process
//!     listings and shell history. `get` says only whether one is set;
//!   - a field the UI confirms as `security` or `privacy` (`x-danger`), and
//!     turning on an adult field (the 18+ confirmation): those need the
//!     UI's own confirmation, which the command line doesn't offer;
//!
//!   Each exits 3 with what to do instead. An unknown, read-only or
//!   environment-set field, `null` (that's `reset`), or a value the API
//!   refuses, exits 2 with the API's message.
//! - **A secret the save resets is named.** Moving a secret's server
//!   (`notifications.email_host`, `integrations.torznab_url`) resets the
//!   saved secret, as the API does (`Section::SECRET_ENDPOINTS`); the
//!   command says so, since it can't set the secret back.
//! - **`reset`** removes saved values only, never `config.toml` or the
//!   environment; `reset --all` removes every one. Defaults are the safe
//!   values (adult and privacy opt-ins off), so reset refuses nothing the
//!   registry allows.
//! - **The HTTP address's keep-or-revert timer** (`system/address.rs`):
//!   `set` or `reset` of `network.http_bind`, and `reset --all`, settle a
//!   pending `system.keep_or_revert.http_bind` record the way the UI's
//!   Keep does, by removing it, in the same transaction. The operator at
//!   the machine chose the address, so the timer mustn't revert it at the
//!   next start.

use std::collections::{BTreeMap, BTreeSet};
use std::process::ExitCode;
use std::sync::{Arc, Mutex};

use clap::{Args, Subcommand};
use lodestar_api::error::ApiError;
use lodestar_api::settings::{
    SectionValues, SettingSource, SettingValue, SettingsApi, SettingsBatch, SettingsError,
    SettingsPatch,
};
use lodestar_store::rusqlite::{self, Connection};
use lodestar_store::{Store, StoreConfig};
use serde_json::{Map, Value};

use super::lock::StoreLock;
use super::util::{Failure, refuse_during_update_rollback};
use crate::config::Config;
use crate::settings::{KEY_PREFIX, Registry};

#[derive(Debug, Args, PartialEq, Eq)]
pub struct SettingsArgs {
    #[command(subcommand)]
    pub command: SettingsCommand,
}

#[derive(Debug, Subcommand, PartialEq, Eq)]
pub enum SettingsCommand {
    /// Show a setting's value and where it comes from; a section, or
    /// nothing, lists every field (secrets only as set or not set)
    Get {
        /// <section>.<field>, or a <section>
        #[arg(value_name = "SETTING")]
        setting: Option<String>,
    },
    /// Save a value, checked as the app checks it (server stopped).
    /// Secrets, privacy opt-ins and adult content are refused: use the app
    Set {
        /// <section>.<field>
        #[arg(value_name = "SETTING")]
        setting: String,
        /// The value as JSON (true, 30, "text", ["a", "b"]); anything that
        /// isn't JSON is taken as text
        #[arg(value_name = "VALUE", allow_hyphen_values = true)]
        value: String,
    },
    /// Remove a saved value, so config.toml's value or the default applies
    /// again (server stopped); --all removes every saved value
    Reset {
        /// <section>.<field>
        #[arg(value_name = "SETTING", required_unless_present = "all")]
        setting: Option<String>,
        /// Every saved value; config.toml and the environment stay as
        /// they are
        #[arg(long, conflicts_with = "setting")]
        all: bool,
    },
}

/// The keep-or-revert records (`system/address.rs`, `Kind::key`) and the
/// field each guards.
const KEEP_OR_REVERT: &[(&str, &str, &str)] =
    &[("network", "http_bind", "system.keep_or_revert.http_bind")];

/// Why the command stopped; each is an exit status.
#[derive(Debug, PartialEq, Eq)]
enum Stop {
    /// 3: refused for safety, nothing changed.
    Refused(String),
    /// 2: a bad setting name or value, nothing changed.
    Bad(String),
    /// 1: something went wrong.
    Failed(String),
}

impl From<Failure> for Stop {
    fn from(f: Failure) -> Stop {
        match f {
            Failure::Refused(m) => Stop::Refused(m),
            Failure::Failed(m) => Stop::Failed(m),
        }
    }
}

type Done<T = ()> = Result<T, Stop>;

pub fn run(config: &Config, args: SettingsArgs) -> ExitCode {
    let done = match args.command {
        SettingsCommand::Get { setting } => get(config, setting.as_deref()),
        SettingsCommand::Set { setting, value } => set(config, &setting, &value),
        SettingsCommand::Reset { setting, all } => match (setting, all) {
            (_, true) => reset_all(config),
            (Some(s), false) => reset(config, &s),
            (None, false) => Err(Stop::Bad("name a setting, or --all".into())),
        },
    };
    match done {
        Ok(()) => ExitCode::SUCCESS,
        Err(Stop::Refused(m)) => {
            eprintln!("lodestar settings: refused: {m}");
            ExitCode::from(3)
        }
        Err(Stop::Bad(m)) => {
            eprintln!("lodestar settings: {m}");
            ExitCode::from(2)
        }
        Err(Stop::Failed(m)) => {
            eprintln!("lodestar settings: {m}");
            ExitCode::FAILURE
        }
    }
}

/// What a field's schema says about it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Meta {
    section: String,
    name: String,
    /// `writeOnly`, whole or in its items.
    secret: bool,
    /// `x-danger`.
    danger: Option<String>,
}

/// The store, locked, and the registry over a copy of its settings.
struct Offline {
    lock: StoreLock,
    /// The store's `settings.` rows as they were read.
    rows: BTreeMap<String, String>,
    copy: Arc<Mutex<Store>>,
    registry: Registry,
}

impl Offline {
    fn open(config: &Config) -> Done<Offline> {
        refuse_during_update_rollback(&config.data_dir)?;
        let lock = StoreLock::acquire(&config.db_path())?;
        let rows = saved_rows(lock.conn())?;
        let copy = Arc::new(Mutex::new(copy_of(&rows)?));
        let registry = Registry::load(config, Arc::clone(&copy))
            .map_err(|e| Stop::Failed(format!("reading the settings: {e}")))?;
        Ok(Offline {
            lock,
            rows,
            copy,
            registry,
        })
    }

    /// Every field of every section, in display order.
    fn fields(&self) -> Vec<Meta> {
        self.registry
            .index()
            .sections
            .into_iter()
            .flat_map(|s| {
                let props = s.schema.get("properties").cloned().unwrap_or(Value::Null);
                s.fields
                    .into_iter()
                    .map(|f| meta(&s.section, &f.name, props.get(&f.name)))
                    .collect::<Vec<_>>()
            })
            .collect()
    }

    /// `<section>.<field>` as a known field.
    fn field(&self, setting: &str) -> Done<Meta> {
        let (section, name) = setting.split_once('.').ok_or_else(|| {
            Stop::Bad(format!(
                "name the setting as <section>.<field>, like network.http_bind \
                 (`lodestar settings get` lists them); got {setting:?}"
            ))
        })?;
        let fields = self.fields();
        if !fields.iter().any(|m| m.section == section) {
            return Err(Stop::Bad(format!(
                "there's no settings section {section:?} (`lodestar settings get` lists them)"
            )));
        }
        fields
            .into_iter()
            .find(|m| m.section == section && m.name == name)
            .ok_or_else(|| {
                Stop::Bad(format!(
                    "{section}: there's no setting {name:?} (`lodestar settings get {section}` \
                     lists them)"
                ))
            })
    }

    fn value(&self, m: &Meta) -> SettingValue {
        self.registry
            .section(&m.section)
            .and_then(|mut s| s.fields.remove(&m.name))
            .expect("a listed field has a value")
    }

    /// Saves `batch` through the registry, as `PATCH /settings` does.
    fn save(&self, batch: SettingsBatch, m: Option<&Meta>) -> Done<Vec<SectionValues>> {
        self.registry
            .save(batch)
            .map(|s| s.sections)
            .map_err(|f| refusal(f.section.as_deref(), f.error, m))
    }

    /// Writes the rows the registry changed back to the store, removes
    /// the keep-or-revert records in `settle`, and unlocks. Returns the
    /// records that were pending.
    fn finish(self, settle: &[&'static str]) -> Done<Vec<&'static str>> {
        let after: BTreeMap<String, String> = self
            .copy
            .lock()
            .expect("store lock")
            .settings_with_prefix(KEY_PREFIX)
            .map_err(|e| Stop::Failed(format!("reading the settings: {e}")))?
            .into_iter()
            .collect();
        let set: Vec<(&str, &str)> = after
            .iter()
            .filter(|(k, v)| self.rows.get(*k) != Some(*v))
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();
        let remove: Vec<&str> = self
            .rows
            .keys()
            .filter(|k| !after.contains_key(*k))
            .map(String::as_str)
            .collect();
        let settled = write(self.lock.conn(), &set, &remove, settle)
            .map_err(|e| Stop::Failed(format!("saving the settings: {e}")))?;
        self.lock.release()?;
        Ok(settled)
    }
}

/// The store's text rows under `settings.`. A row that isn't text stops
/// the command, as it would stop the server's registry.
fn saved_rows(conn: &Connection) -> Done<BTreeMap<String, String>> {
    let read = || -> rusqlite::Result<Vec<(String, rusqlite::types::Value)>> {
        let mut stmt = conn.prepare(
            "SELECT key, value FROM settings WHERE substr(key, 1, ?2) = ?1 ORDER BY key",
        )?;
        let rows = stmt.query_map(
            rusqlite::params![KEY_PREFIX, KEY_PREFIX.len() as i64],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        rows.collect()
    };
    let mut out = BTreeMap::new();
    for (key, value) in read().map_err(|e| Stop::Failed(format!("reading the settings: {e}")))? {
        match value {
            rusqlite::types::Value::Text(t) => {
                out.insert(key, t);
            }
            _ => {
                return Err(Stop::Failed(
                    "reading the settings: a saved setting isn't text (the store is damaged)"
                        .into(),
                ));
            }
        }
    }
    Ok(out)
}

/// An in-memory store holding just `rows`, for the registry.
fn copy_of(rows: &BTreeMap<String, String>) -> Done<Store> {
    let fail = |e: lodestar_store::Error| Stop::Failed(format!("reading the settings: {e}"));
    let mut store = Store::open_in_memory(StoreConfig::default()).map_err(fail)?;
    let existing: Vec<String> = store
        .settings_with_prefix(KEY_PREFIX)
        .map_err(fail)?
        .into_iter()
        .map(|(k, _)| k)
        .collect();
    let set: Vec<(&str, &str)> = rows.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    let remove: Vec<&str> = existing.iter().map(String::as_str).collect();
    store.update_settings_text(&set, &remove).map_err(fail)?;
    Ok(store)
}

/// One transaction: the changed rows, the removed ones, and the settled
/// records. Returns the records that existed.
fn write(
    conn: &Connection,
    set: &[(&str, &str)],
    remove: &[&str],
    settle: &[&'static str],
) -> rusqlite::Result<Vec<&'static str>> {
    let tx = conn.unchecked_transaction()?;
    for (k, v) in set {
        tx.execute(
            "INSERT INTO settings (key, value) VALUES (?1, ?2)
             ON CONFLICT (key) DO UPDATE SET value = excluded.value",
            [k, v],
        )?;
    }
    for k in remove {
        tx.execute("DELETE FROM settings WHERE key = ?1", [k])?;
    }
    let mut settled = Vec::new();
    for k in settle {
        if tx.execute("DELETE FROM settings WHERE key = ?1", [k])? > 0 {
            settled.push(*k);
        }
    }
    tx.commit()?;
    Ok(settled)
}

fn meta(section: &str, name: &str, prop: Option<&Value>) -> Meta {
    let write_only =
        |p: Option<&Value>| p.and_then(|p| p.get("writeOnly")) == Some(&Value::Bool(true));
    let item_secret = prop
        .and_then(|p| p.pointer("/items/properties"))
        .and_then(Value::as_object)
        .is_some_and(|props| props.values().any(|p| write_only(Some(p))));
    Meta {
        section: section.to_string(),
        name: name.to_string(),
        secret: write_only(prop) || item_secret,
        danger: prop
            .and_then(|p| p.get("x-danger"))
            .and_then(Value::as_str)
            .map(str::to_string),
    }
}

/// The API's refusal as the command's: the adult confirmation and a
/// failed save as themselves, anything else as a bad argument with the
/// API's own message.
fn refusal(section: Option<&str>, e: SettingsError, m: Option<&Meta>) -> Stop {
    let field = |f: &str| format!("{}.{f}", section.unwrap_or_default());
    match e {
        SettingsError::NeedsConfirmation {
            field: f,
            code: "adult_confirmation_required",
            ..
        } => Stop::Refused(format!(
            "{}: turning on adult content needs the confirmation that you're 18 or older, \
             which only the app asks for (Settings → Content). Nothing was changed.",
            field(&f)
        )),
        // Any other confirmation is the app's too.
        SettingsError::NeedsConfirmation {
            field: f, message, ..
        } => Stop::Refused(format!(
            "{}: {message} Only the app asks for it. Nothing was changed.",
            field(&f)
        )),
        // Logged by the registry; the detail can name a path.
        SettingsError::Store(_) => Stop::Failed("saving the settings failed".into()),
        e => {
            let at = match &e {
                SettingsError::UnknownField(f)
                | SettingsError::ReadOnly(f)
                | SettingsError::SetByEnv { field: f, .. }
                | SettingsError::Invalid { field: f, .. } => field(f),
                _ => m.map_or_else(
                    || section.unwrap_or_default().to_string(),
                    |m| format!("{}.{}", m.section, m.name),
                ),
            };
            let message = ApiError::from(e).error_body().message;
            Stop::Bad(format!("{at}: {message} Nothing was changed."))
        }
    }
}

/// The value on the command line: JSON, or else the text itself.
fn parse_value(text: &str) -> Value {
    serde_json::from_str(text).unwrap_or_else(|_| Value::String(text.to_string()))
}

fn source_name(v: &SettingValue) -> String {
    match v.source {
        SettingSource::Default => "default".into(),
        SettingSource::File => "config.toml".into(),
        SettingSource::Db => "saved".into(),
        SettingSource::Env => match &v.env {
            Some(var) => format!("environment, {var}"),
            None => "environment".into(),
        },
    }
}

/// A value as printed: a secret only as whether it's set.
fn shown(m: &Meta, v: &SettingValue) -> String {
    let secret_field = m.secret && v.secrets_set.is_empty() && !v.value.is_array();
    if secret_field {
        return if v.source == SettingSource::Default {
            "(not set)".into()
        } else {
            "(set)".into()
        };
    }
    let mut s = v.value.to_string();
    if !v.secrets_set.is_empty() {
        s.push_str(&format!(" (secrets set: {})", v.secrets_set.join(", ")));
    }
    s
}

fn get(config: &Config, setting: Option<&str>) -> Done {
    let offline = Offline::open(config)?;
    let fields = offline.fields();
    let one = setting.filter(|s| s.contains('.'));
    if let Some(s) = one {
        let m = offline.field(s)?;
        let v = offline.value(&m);
        println!("{}.{} = {}", m.section, m.name, shown(&m, &v));
        println!("  source: {}", source_name(&v));
        if v.source == SettingSource::Db {
            // What applies without the saved value: the file's or the default.
            let base = base_value(config, &m)?;
            println!(
                "  without the saved value: {} ({})",
                shown(&m, &base),
                source_name(&base)
            );
        }
        if v.read_only {
            println!("  read-only here");
        }
        if v.restart_required {
            println!("  a change applies at the next start");
        }
    } else {
        if let Some(section) = setting
            && !fields.iter().any(|m| m.section == section)
        {
            return Err(Stop::Bad(format!(
                "there's no settings section {section:?} (`lodestar settings get` lists them)"
            )));
        }
        for m in fields
            .iter()
            .filter(|m| setting.is_none_or(|s| m.section == s))
        {
            let v = offline.value(m);
            println!(
                "{}.{} = {}  [{}]",
                m.section,
                m.name,
                shown(m, &v),
                source_name(&v)
            );
        }
    }
    // Read only: nothing is written back.
    offline.lock.release()?;
    Ok(())
}

/// `m`'s value without anything saved.
fn base_value(config: &Config, m: &Meta) -> Done<SettingValue> {
    let store = copy_of(&BTreeMap::new())?;
    let registry = Registry::load(config, Arc::new(Mutex::new(store)))
        .map_err(|e| Stop::Failed(format!("reading the settings: {e}")))?;
    Ok(registry
        .section(&m.section)
        .and_then(|mut s| s.fields.remove(&m.name))
        .expect("a listed field has a value"))
}

/// The keep-or-revert records a change to `m` settles.
fn settles(m: &Meta) -> Vec<&'static str> {
    KEEP_OR_REVERT
        .iter()
        .filter(|(s, f, _)| m.section == *s && m.name == *f)
        .map(|(_, _, k)| *k)
        .collect()
}

fn batch(section: &str, patch: Map<String, Value>) -> SettingsBatch {
    SettingsBatch(BTreeMap::from([(
        section.to_string(),
        SettingsPatch(patch),
    )]))
}

fn set(config: &Config, setting: &str, text: &str) -> Done {
    let offline = Offline::open(config)?;
    let m = offline.field(setting)?;
    let at = format!("{}.{}", m.section, m.name);
    // Before the value is even parsed: a secret is never read, checked or
    // echoed.
    if m.secret {
        return Err(Stop::Refused(format!(
            "{at} holds a secret. A secret on the command line shows in process listings and \
             shell history, so it's set only in the app (Settings → {}). If you just typed one, \
             change it where it's used and clear it from your shell history. \
             `lodestar settings reset {at}` removes the saved one.",
            title(&m.section)
        )));
    }
    match m.danger.as_deref() {
        Some("security") => {
            return Err(Stop::Refused(format!(
                "{at} is a security setting: the app asks for your password before it changes. \
                 Change it in the app, or `lodestar settings reset {at}` to go back to the \
                 default."
            )));
        }
        Some("privacy") => {
            return Err(Stop::Refused(format!(
                "{at} records what identifies torrents or people: the app explains that and \
                 asks you to confirm before it changes. Change it in the app, or \
                 `lodestar settings reset {at}` to turn it back off."
            )));
        }
        _ => {}
    }
    let value = parse_value(text);
    if value.is_null() {
        return Err(Stop::Bad(format!(
            "{at}: null resets a setting; use `lodestar settings reset {at}`"
        )));
    }
    let before_section = offline
        .registry
        .section(&m.section)
        .expect("a listed section");
    let before = before_section.fields[&m.name].clone();
    let sections = offline.save(
        batch(&m.section, Map::from_iter([(m.name.clone(), value)])),
        Some(&m),
    )?;
    let mut after_section = sections
        .into_iter()
        .find(|s| s.section == m.section)
        .expect("the saved section");
    let after = after_section
        .fields
        .remove(&m.name)
        .expect("the saved field");
    // Secrets the save reset with it: their server moved.
    let dropped: Vec<String> = before_section
        .fields
        .iter()
        .filter(|(name, v)| {
            **name != m.name
                && v.source == SettingSource::Db
                && after_section
                    .fields
                    .get(*name)
                    .is_some_and(|a| a.source != SettingSource::Db)
        })
        .map(|(name, _)| format!("{}.{name}", m.section))
        .collect();
    let settled = offline.finish(&settles(&m))?;
    if before.source == SettingSource::Db && before.value == after.value {
        println!("{at} is already {}; nothing changed.", shown(&m, &after));
    } else {
        println!("Saved {at} = {}.", shown(&m, &after));
    }
    for d in &dropped {
        println!(
            "Also reset {d}: a saved secret isn't sent to another server. Enter it again in \
             the app (Settings → {}).",
            title(&m.section)
        );
    }
    report(&after, &settled);
    Ok(())
}

fn reset(config: &Config, setting: &str) -> Done {
    let offline = Offline::open(config)?;
    let m = offline.field(setting)?;
    let at = format!("{}.{}", m.section, m.name);
    let before = offline.value(&m);
    let sections = offline.save(
        batch(&m.section, Map::from_iter([(m.name.clone(), Value::Null)])),
        Some(&m),
    )?;
    let after = sections
        .into_iter()
        .find(|s| s.section == m.section)
        .and_then(|mut s| s.fields.remove(&m.name))
        .expect("the saved section");
    let settled = offline.finish(&settles(&m))?;
    if before.source == SettingSource::Db {
        println!(
            "Reset {at}: {} applies again ({}).",
            shown(&m, &after),
            source_name(&after)
        );
    } else {
        println!(
            "{at} wasn't saved; nothing changed. (A saved value this build can't read is \
             removed by `lodestar settings reset --all`.)"
        );
    }
    report(&after, &settled);
    Ok(())
}

fn reset_all(config: &Config) -> Done {
    let offline = Offline::open(config)?;
    // Every saved value the registry shows, reset through it, so each
    // section's revision goes up and its checks run.
    let mut patches: BTreeMap<String, Map<String, Value>> = BTreeMap::new();
    for m in offline.fields() {
        if offline.value(&m).source == SettingSource::Db {
            patches
                .entry(m.section.clone())
                .or_default()
                .insert(m.name.clone(), Value::Null);
        }
    }
    let count: usize = patches.values().map(Map::len).sum();
    let mut changed: BTreeSet<String> = patches.keys().cloned().collect();
    if !patches.is_empty() {
        let batch = SettingsBatch(
            patches
                .into_iter()
                .map(|(s, p)| (s, SettingsPatch(p)))
                .collect(),
        );
        offline.save(batch, None)?;
    }
    // Then what the registry doesn't show: a value the environment hides
    // (it would come back once the variable goes), or one this build has
    // no field for. Their sections' revisions go up too.
    let hidden = {
        let mut copy = offline.copy.lock().expect("store lock");
        let rows = copy
            .settings_with_prefix(KEY_PREFIX)
            .map_err(|e| Stop::Failed(format!("reading the settings: {e}")))?;
        let mut remove = Vec::new();
        let mut sections = BTreeSet::new();
        for (k, _) in &rows {
            let rest = &k[KEY_PREFIX.len()..];
            if let Some((section, _)) = rest.split_once('.')
                && !section.contains('#')
            {
                remove.push(k.clone());
                sections.insert(section.to_string());
            }
        }
        let mut set = Vec::new();
        for s in sections.difference(&changed) {
            let key = format!("{KEY_PREFIX}{s}#revision");
            let revision = rows
                .iter()
                .find(|(k, _)| *k == key)
                .and_then(|(_, v)| v.parse::<u64>().ok())
                .unwrap_or(0);
            set.push((key, revision.saturating_add(1).to_string()));
        }
        let set_ref: Vec<(&str, &str)> =
            set.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
        let remove_ref: Vec<&str> = remove.iter().map(String::as_str).collect();
        copy.update_settings_text(&set_ref, &remove_ref)
            .map_err(|e| Stop::Failed(format!("resetting the settings: {e}")))?;
        changed.extend(sections);
        remove.len()
    };
    let settle: Vec<&'static str> = KEEP_OR_REVERT.iter().map(|(_, _, k)| *k).collect();
    let settled = offline.finish(&settle)?;
    if count == 0 && hidden == 0 {
        println!("Nothing was saved in the app; nothing changed.");
    } else {
        println!(
            "Reset {} saved value{} in {} section{}. config.toml and the environment are \
             unchanged; they and the defaults apply at the next start.",
            count + hidden,
            if count + hidden == 1 { "" } else { "s" },
            changed.len(),
            if changed.len() == 1 { "" } else { "s" },
        );
    }
    if !settled.is_empty() {
        println!("The pending HTTP address change is settled: nothing reverts it at the start.");
    }
    Ok(())
}

/// What happens next, after a save or a reset.
fn report(after: &SettingValue, settled: &[&str]) {
    if after.restart_required {
        println!("It applies at the next start.");
    } else {
        println!("A running server would apply it live; it's read at the next start.");
    }
    if !settled.is_empty() {
        println!("The pending HTTP address change is settled: nothing reverts it at the start.");
    }
}

/// A section's name as the Settings screens title it (`Notifications`).
fn title(section: &str) -> String {
    let mut c = section.chars();
    c.next()
        .map(|f| f.to_uppercase().collect::<String>() + c.as_str())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::network::Network;
    use crate::system::address;

    fn config(dir: &std::path::Path) -> Config {
        let d = dir.display().to_string();
        Config::load(None, |k| (k == "LODESTAR_DATA_DIR").then(|| d.clone())).unwrap()
    }

    fn store(dir: &std::path::Path) -> Store {
        Store::open(dir.join("lodestar.db"), StoreConfig::default()).unwrap()
    }

    #[test]
    fn a_set_is_what_the_server_loads() {
        let dir = tempfile::tempdir().unwrap();
        drop(store(dir.path()));
        let c = config(dir.path());
        set(&c, "network.http_bind", "127.0.0.1:9123").unwrap();
        set(&c, "crawler.paused", "true").unwrap();
        // The server's own load, on the real store.
        let registry = Registry::load(&c, Arc::new(Mutex::new(store(dir.path())))).unwrap();
        assert_eq!(
            registry.get::<Network>().http_bind,
            "127.0.0.1:9123".parse().unwrap()
        );
        assert!(registry.get::<crate::settings::crawler::Crawler>().paused);
        let effective = registry.effective_config(&c);
        assert_eq!(effective.http.bind, "127.0.0.1:9123".parse().unwrap());
        // Each save went up one revision, as the API's does.
        let s = registry.section("network").unwrap();
        assert_eq!(s.revision, 1);
        // The server stopped.
        drop(registry);
        reset(&c, "network.http_bind").unwrap();
        let registry = Registry::load(&c, Arc::new(Mutex::new(store(dir.path())))).unwrap();
        assert_eq!(registry.get::<Network>().http_bind, c.http.bind);
        assert_eq!(registry.section("network").unwrap().revision, 2);
    }

    /// The key is the one `system/address.rs` reads: `health_bind` follows
    /// a record's `serving` until it's settled.
    #[test]
    fn settling_removes_the_record_the_timer_reads() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path());
        let record = address::Record {
            kind: address::Kind::HttpBind,
            previous: address::Previous::Reset,
            new: Value::String("127.0.0.1:9001".into()),
            state: address::Pending::AwaitingConfirmation,
            deadline: Some(1),
            serving: Some(Value::String("127.0.0.1:9002".into())),
        };
        s.connection()
            .execute(
                "INSERT INTO settings (key, value) VALUES (?1, ?2)",
                [
                    KEEP_OR_REVERT[0].2,
                    &serde_json::to_string(&record).unwrap(),
                ],
            )
            .unwrap();
        drop(s);
        let c = config(dir.path());
        assert_eq!(
            address::health_bind(&c),
            "127.0.0.1:9002".parse().unwrap(),
            "the test's record is the one address.rs reads"
        );
        set(&c, "network.http_bind", "127.0.0.1:9003").unwrap();
        assert_eq!(address::health_bind(&c), "127.0.0.1:9003".parse().unwrap());
        assert_eq!(
            store(dir.path()).setting_text(KEEP_OR_REVERT[0].2).unwrap(),
            None
        );
    }

    #[test]
    fn values_parse_as_json_or_text() {
        assert_eq!(parse_value("true"), Value::Bool(true));
        assert_eq!(parse_value("-1"), Value::from(-1));
        assert_eq!(parse_value("\"30\""), Value::from("30"));
        assert_eq!(parse_value("0.0.0.0:8080"), Value::from("0.0.0.0:8080"));
        assert_eq!(parse_value("[\"a\"]"), serde_json::json!(["a"]));
        assert_eq!(parse_value(""), Value::from(""));
    }

    /// Every secret and `x-danger` field `set` refuses, from the
    /// registry's own schema; the decisions file lists the same.
    #[test]
    fn the_refused_fields() {
        let dir = tempfile::tempdir().unwrap();
        drop(store(dir.path()));
        let c = config(dir.path());
        let offline = Offline::open(&c).unwrap();
        let fields = offline.fields();
        let names = |p: &dyn Fn(&Meta) -> bool| -> Vec<String> {
            fields
                .iter()
                .filter(|m| p(m))
                .map(|m| format!("{}.{}", m.section, m.name))
                .collect()
        };
        assert_eq!(
            names(&|m| m.secret),
            [
                "integrations.clients",
                "integrations.torznab_api_key",
                "notifications.ntfy_url",
                "notifications.ntfy_token",
                "notifications.discord_url",
                "notifications.email_password",
                "notifications.webhook_url",
            ]
        );
        assert_eq!(
            names(&|m| m.danger.as_deref() == Some("privacy")),
            [
                "insights.query_logging",
                "logging.data",
                "logging.data_until",
                "logging.kinds",
                "logging.peer_activity_until",
                "logging.change_details_until",
                "logging.console_detail",
            ]
        );
        assert!(names(&|m| m.danger.as_deref() == Some("security")).is_empty());
        offline.lock.release().unwrap();
    }

    #[test]
    fn titles() {
        assert_eq!(title("notifications"), "Notifications");
        assert_eq!(title(""), "");
    }
}
