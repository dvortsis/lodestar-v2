//! Logging (architecture §12.1, §10.10.12; owner's decision §2.11): what
//! gets a line, what a line may contain, and where lines go. Every field
//! applies at once except the file's folder.
//!
//! - **Events:** a verbosity preset, per-event overrides (only those that
//!   differ from the preset are kept), a timed verbosity ("Detailed for 30
//!   minutes"), per-component levels (Advanced) and the raw `tracing`
//!   filter for the third-party crates (Libraries).
//! - **Data kinds:** a preset (Private, Hashes, Full) with its deadline,
//!   per-kind overrides with theirs, and the two opt-ins (Peer activity,
//!   Filter change details), which no preset touches.
//! - **Timers are deadlines** (Unix seconds): `0` off, `-1` until turned
//!   off, any other value on while it's ahead. A kind turns off when its
//!   deadline passes with no save, a restart doesn't extend it, and the
//!   runtime logs the required switch event. An opt-in or a timed verbosity
//!   can't be `-1` or more than 24 hours ahead.
//! - **Aliases:** `verbosity`, `components` and `libraries` are what
//!   `config.toml`'s `log` and `LODESTAR_LOG` set ([`alias`]); set by the
//!   environment, all three are read-only.
//! - A saved `general.log_level` from an earlier build moves here once
//!   ([`move_log_level`]).

use std::collections::BTreeMap;

use lodestar_log::{Component, Kind, Level};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use super::{Danger, Field, Preview, Section};
use crate::config::Config;
use crate::logging;

/// The longest `libraries` filter accepted.
const MAX_FILTER: usize = 1024;
/// How far ahead a time-limited switch may be: 24 hours, with five
/// minutes for the browser's round trip.
pub const MAX_AHEAD_SECS: i64 = 24 * 3600 + 300;
/// The deadline that means "until turned off".
pub const UNTIL_OFF: i64 = -1;
/// What `libraries` is by default: third-party crates capped at warnings.
pub const LIBRARIES: &str = "warn";

/// The verbosity preset (§12.1): every event at or above its level.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum Verbosity {
    Errors,
    Warnings,
    Normal,
    Detailed,
    Everything,
}

impl Verbosity {
    pub const fn level(self) -> Level {
        match self {
            Verbosity::Errors => Level::Error,
            Verbosity::Warnings => Level::Warn,
            Verbosity::Normal => Level::Info,
            Verbosity::Detailed => Level::Debug,
            Verbosity::Everything => Level::Trace,
        }
    }

    pub const fn of(level: Level) -> Verbosity {
        match level {
            Level::Error => Verbosity::Errors,
            Level::Warn => Verbosity::Warnings,
            Level::Info => Verbosity::Normal,
            Level::Debug => Verbosity::Detailed,
            Level::Trace => Verbosity::Everything,
        }
    }

    pub const fn name(self) -> &'static str {
        match self {
            Verbosity::Errors => "errors",
            Verbosity::Warnings => "warnings",
            Verbosity::Normal => "normal",
            Verbosity::Detailed => "detailed",
            Verbosity::Everything => "everything",
        }
    }
}

/// The data preset (§12.1). Custom is what per-kind overrides make.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum DataPreset {
    /// Client addresses and sizes only.
    Private,
    /// Private plus info hashes.
    Hashes,
    /// Every kind but the opt-ins.
    Full,
}

impl DataPreset {
    pub const fn name(self) -> &'static str {
        match self {
            DataPreset::Private => "private",
            DataPreset::Hashes => "hashes",
            DataPreset::Full => "full",
        }
    }

    /// Whether the preset turns `kind` on (identifying ones only while its
    /// deadline is ahead).
    pub fn has(self, kind: Kind) -> bool {
        match self {
            _ if kind.opt_in() => false,
            _ if kind.default_on() => true,
            DataPreset::Private => false,
            DataPreset::Hashes => kind == Kind::Hash,
            DataPreset::Full => true,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum ConsoleFormat {
    Text,
    Json,
}

/// What the console may contain: Private kinds, or the same as the file
/// (never the opt-ins: Lodestar can't delete what `docker logs` keeps).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum ConsoleDetail {
    Private,
    File,
}

/// An output's minimum level.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum OutputLevel {
    Error,
    Warn,
    Info,
    Debug,
    Trace,
}

impl OutputLevel {
    pub const fn level(self) -> Level {
        match self {
            OutputLevel::Error => Level::Error,
            OutputLevel::Warn => Level::Warn,
            OutputLevel::Info => Level::Info,
            OutputLevel::Debug => Level::Debug,
            OutputLevel::Trace => Level::Trace,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct Logging {
    // Enums are undocumented here: a field's own description would wrap
    // the inlined enum in a `oneOf`.
    #[schema(inline)]
    pub verbosity: Verbosity,
    /// Events turned on or off against the preset, by id. Required events
    /// can't be turned off; `filters.change_detail` follows its opt-in.
    pub events: BTreeMap<String, bool>,
    #[schema(inline)]
    pub temporary_verbosity: Verbosity,
    /// Until when `temporary_verbosity` applies (Unix seconds; 0: none).
    pub temporary_until: i64,
    /// Per-component levels (`dht`: `debug`), over the verbosity.
    pub components: BTreeMap<String, String>,
    /// The `tracing` filter for third-party crates (Libraries).
    pub libraries: String,
    #[schema(inline)]
    pub data: DataPreset,
    /// Until when the preset's identifying kinds are on (0 off, -1 until
    /// turned off, else Unix seconds).
    pub data_until: i64,
    /// Kinds switched against the preset, each with its deadline (0 off,
    /// -1 until turned off, else Unix seconds).
    pub kinds: BTreeMap<String, i64>,
    /// The peer activity opt-in's deadline (Unix seconds; 0: off).
    pub peer_activity_until: i64,
    /// The filter change details opt-in's deadline (Unix seconds; 0: off).
    pub change_details_until: i64,
    #[schema(inline)]
    pub console_format: ConsoleFormat,
    #[schema(inline)]
    pub console_detail: ConsoleDetail,
    #[schema(inline)]
    pub console_level: OutputLevel,
    /// Writes `lodestar.jsonl` in `file_folder`.
    pub file_enabled: bool,
    /// Where the log files go; applies after a restart.
    pub file_folder: String,
    #[schema(inline)]
    pub file_level: OutputLevel,
    /// The size a file is rotated at, in MiB.
    pub file_size_mib: u32,
    /// Rotated (compressed) files kept.
    pub file_count: u32,
    /// Rotated files older than this are deleted, in days.
    pub file_max_age_days: u32,
    /// The logs folder never goes over this, in MiB.
    pub file_total_mib: u32,
    /// Errors also go to the Windows Event Log (Windows service only).
    pub event_log_errors: bool,
}

impl Default for Logging {
    fn default() -> Self {
        Logging::from_config(&Config::default())
    }
}

/// What `log` / `LODESTAR_LOG` sets: the verbosity, component levels and
/// the Libraries filter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Alias {
    pub verbosity: Verbosity,
    pub components: BTreeMap<String, String>,
    pub libraries: String,
    /// `lodestar_*` targets that aren't a component (ignored).
    pub unknown: Vec<String>,
}

/// Maps a `log` value onto the section (§12.1, B-log1):
/// - a bare level (`error`…`trace`) is the verbosity;
/// - in a filter string, the default level is the verbosity, `lodestar_*`
///   directives are component levels, and everything else is the
///   Libraries filter.
///
/// `None` when it isn't a filter at all.
pub fn alias(log: &str) -> Option<Alias> {
    super::log::parse(log).ok()?;
    let mut out = Alias {
        verbosity: Verbosity::Normal,
        components: BTreeMap::new(),
        libraries: String::new(),
        unknown: Vec::new(),
    };
    let mut libraries = Vec::new();
    for directive in log.split(',').map(str::trim).filter(|d| !d.is_empty()) {
        let (target, level) = match directive.rsplit_once('=') {
            Some((t, l)) => (Some(t.trim()), l.trim()),
            // A bare first-party target means all its levels.
            None if logging::first_party(directive) && Level::parse(directive).is_none() => {
                (Some(directive), "trace")
            }
            None => (None, directive),
        };
        match (target, Level::parse(level)) {
            (None, Some(l)) => out.verbosity = Verbosity::of(l),
            (Some(t), Some(l)) if logging::first_party(t) => match logging::component_of(t) {
                Some(c) => {
                    out.components
                        .insert(c.name().to_string(), l.name().to_string());
                }
                None => out.unknown.push(t.to_string()),
            },
            _ => libraries.push(directive),
        }
    }
    out.libraries = match libraries.is_empty() {
        true => LIBRARIES.to_string(),
        false => libraries.join(","),
    };
    Some(out)
}

/// The saved `general.log_level` (JSON text) as this section's saved
/// values, also JSON text: the one-time move at `Registry::load`.
pub fn move_log_level(saved: &str) -> Vec<(&'static str, String)> {
    let Ok(serde_json::Value::String(log)) = serde_json::from_str(saved) else {
        return Vec::new();
    };
    let Some(a) = alias(&log) else {
        return Vec::new();
    };
    let json = |v: serde_json::Value| v.to_string();
    vec![
        ("verbosity", json(a.verbosity.name().into())),
        (
            "components",
            json(serde_json::to_value(&a.components).expect("a map")),
        ),
        ("libraries", json(a.libraries.into())),
    ]
}

/// Whether a deadline is ahead at `now`.
pub fn ahead(until: i64, now: i64) -> bool {
    until == UNTIL_OFF || until > now
}

fn check_deadline(until: i64, can_be_until_off: bool) -> Result<(), String> {
    let now = logging::unix_now();
    match until {
        0 => Ok(()),
        UNTIL_OFF if can_be_until_off => Ok(()),
        UNTIL_OFF => {
            Err("Can be on for 24 hours at most; there's no \"until I turn it off\".".into())
        }
        u if u < 0 => Err("Must be 0 (off), -1 (until turned off) or a time.".into()),
        u if u > now + MAX_AHEAD_SECS => Err("Can be on for 24 hours at most.".into()),
        _ => Ok(()),
    }
}

impl Section for Logging {
    const SECTION: &'static str = "logging";
    const FIELDS: &'static [Field] = &[
        Field::new("verbosity")
            .file("log")
            .env("LODESTAR_LOG")
            .group("events")
            .widget("radio")
            .preview()
            .keywords(),
        Field::new("events")
            .group("events")
            .widget("log-events")
            .preview(),
        Field::new("temporary_verbosity")
            .group("events")
            .widget("radio")
            .preview(),
        Field::new("temporary_until")
            .group("events")
            .widget("duration")
            .preview(),
        Field::new("components")
            .file("log")
            .env("LODESTAR_LOG")
            .group("events")
            .advanced()
            .widget("log-components")
            .preview(),
        Field::new("libraries")
            .file("log")
            .env("LODESTAR_LOG")
            .group("events")
            .advanced()
            .format("log-filter")
            .preview(),
        Field::new("data")
            .group("data")
            .widget("radio")
            .danger(Danger::Privacy)
            .preview()
            .keywords(),
        Field::new("data_until")
            .group("data")
            .widget("duration")
            .danger(Danger::Privacy)
            .preview(),
        Field::new("kinds")
            .group("data")
            .widget("log-kinds")
            .danger(Danger::Privacy)
            .preview(),
        Field::new("peer_activity_until")
            .group("opt_ins")
            .widget("duration")
            .danger(Danger::Privacy)
            .preview(),
        Field::new("change_details_until")
            .group("opt_ins")
            .widget("duration")
            .danger(Danger::Privacy)
            .preview(),
        Field::new("console_format")
            .group("console")
            .widget("radio"),
        Field::new("console_detail")
            .group("console")
            .widget("radio")
            .danger(Danger::Privacy)
            .preview(),
        Field::new("console_level").group("console").advanced(),
        Field::new("file_enabled").group("file").preview(),
        Field::new("file_folder").restart().group("file").advanced(),
        Field::new("file_level").group("file").advanced(),
        Field::new("file_size_mib")
            .range(1, 1024)
            .group("file")
            .advanced()
            .unit("mebibytes")
            .preview(),
        Field::new("file_count")
            .range(1, 100)
            .group("file")
            .advanced()
            .unit("files")
            .preview(),
        Field::new("file_max_age_days")
            .range(1, 365)
            .group("file")
            .advanced()
            .unit("days")
            .preview(),
        Field::new("file_total_mib")
            .range(16, 65536)
            .group("file")
            .advanced()
            .unit("mebibytes")
            .preview(),
        Field::new("event_log_errors").group("event_log").advanced(),
    ];

    fn from_config(config: &Config) -> Self {
        // A `log` that isn't a filter starts at the defaults, as the
        // subscriber did ("info").
        let a = alias(&config.log).unwrap_or(Alias {
            verbosity: Verbosity::Normal,
            components: BTreeMap::new(),
            libraries: LIBRARIES.to_string(),
            unknown: Vec::new(),
        });
        Logging {
            verbosity: a.verbosity,
            events: BTreeMap::new(),
            temporary_verbosity: Verbosity::Detailed,
            temporary_until: 0,
            components: a.components,
            libraries: a.libraries,
            data: DataPreset::Private,
            data_until: 0,
            kinds: BTreeMap::new(),
            peer_activity_until: 0,
            change_details_until: 0,
            console_format: ConsoleFormat::Text,
            console_detail: ConsoleDetail::Private,
            console_level: OutputLevel::Trace,
            file_enabled: true,
            file_folder: config.data_dir.join("logs").display().to_string(),
            file_level: OutputLevel::Trace,
            file_size_mib: 16,
            file_count: 7,
            file_max_age_days: 14,
            file_total_mib: 256,
            event_log_errors: true,
        }
    }

    fn apply(&self, _field: &str, _config: &mut Config) {
        // Nothing the services start with: `log::follow` applies every
        // field (and opens the file in `file_folder`) from the channel.
    }

    fn check(&self, field: &str) -> Result<(), String> {
        match field {
            "events" => {
                for (id, on) in &self.events {
                    let e = logging::find(id).ok_or_else(|| format!("No such event: {id:?}."))?;
                    if e.required && !on {
                        return Err(format!("{id} is required and can't be turned off."));
                    }
                    if e.opt_in.is_some() {
                        return Err(format!(
                            "{id} is on only while its opt-in is; turn on the opt-in instead."
                        ));
                    }
                }
                Ok(())
            }
            "components" => {
                for (name, level) in &self.components {
                    match Component::parse(name) {
                        Some(Component::Libraries) => {
                            return Err("Libraries take the libraries filter.".into());
                        }
                        Some(_) => {}
                        None => return Err(format!("No such component: {name:?}.")),
                    }
                    if Level::parse(level).is_none() {
                        return Err(format!(
                            "{name}: must be error, warn, info, debug or trace."
                        ));
                    }
                }
                Ok(())
            }
            "libraries" => {
                if self.libraries.len() > MAX_FILTER {
                    return Err(format!("Must be at most {MAX_FILTER} characters."));
                }
                super::log::parse(&self.libraries)?;
                for d in self.libraries.split(',') {
                    // `target=level`, or a bare `target` (all its levels).
                    let target = d.rsplit_once('=').map_or(d, |(t, _)| t).trim();
                    let target = target.split('[').next().unwrap_or(target).trim();
                    if !target.is_empty()
                        && Level::parse(target).is_none()
                        && logging::first_party(target)
                    {
                        return Err(format!(
                            "{target} is Lodestar's own; set its component's level instead."
                        ));
                    }
                }
                Ok(())
            }
            "temporary_until" | "peer_activity_until" | "change_details_until" => {
                let until = match field {
                    "temporary_until" => self.temporary_until,
                    "peer_activity_until" => self.peer_activity_until,
                    _ => self.change_details_until,
                };
                check_deadline(until, false)
            }
            "data_until" => {
                if self.data != DataPreset::Private && self.data_until == 0 {
                    return Err("Choose how long: 1 hour, 24 hours or until turned off.".into());
                }
                check_deadline(self.data_until, true)
            }
            "data" => match self.data != DataPreset::Private && self.data_until == 0 {
                true => Err("Choose how long: 1 hour, 24 hours or until turned off.".into()),
                false => Ok(()),
            },
            "kinds" => {
                for (name, until) in &self.kinds {
                    let kind = Kind::parse(name)
                        .filter(|k| !k.opt_in())
                        .ok_or_else(|| format!("No such data kind: {name:?}."))?;
                    if !kind.identifying() && !matches!(*until, 0 | UNTIL_OFF) {
                        return Err(format!("{name}: must be 0 (off) or -1 (on)."));
                    }
                    check_deadline(*until, true).map_err(|e| format!("{name}: {e}"))?;
                }
                Ok(())
            }
            "file_size_mib" | "file_total_mib" => match self.file_total_mib < self.file_size_mib {
                true => Err("The total cap must be at least one file's size.".into()),
                false => Ok(()),
            },
            "file_folder" => match self.file_folder.trim().is_empty() {
                true => Err("Must be a folder.".into()),
                false if self.file_folder.len() > 4096 => {
                    Err("Must be at most 4096 characters.".into())
                }
                false => Ok(()),
            },
            _ => Ok(()),
        }
    }

    fn preview(&self, _field: &str) -> Preview {
        logging::preview(self, logging::unix_now())
    }
}
