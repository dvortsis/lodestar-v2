//! `/logs` (architecture §12.1, §10.10.12): the catalogue of log events
//! and data kinds the Logging screen renders from, so a new event needs no
//! UI change. B-log2 adds the history, the live stream and the files.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// A level, from the least to the most verbose.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum LogLevel {
    Error,
    Warn,
    Info,
    Debug,
    Trace,
}

/// Where an event comes from (§12.1's per-component levels; `system` is
/// the process's own: start, stop, settings, logging).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum LogComponent {
    System,
    Dht,
    Fetch,
    Pipeline,
    Search,
    Api,
    Sources,
    Filters,
    Integrations,
    Workers,
    Companions,
    Auth,
    Libraries,
}

/// A kind of data a line may contain (§12.1). `peer_activity` and
/// `filter_changes` are the opt-ins.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum LogDataKind {
    Hash,
    Name,
    Path,
    Url,
    Query,
    Client,
    Peer,
    Size,
    PeerActivity,
    FilterChanges,
}

/// An output (§12.1): each has its own level and data kinds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum LogOutputName {
    Console,
    File,
    Live,
    EventLog,
}

/// One catalogued event.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct LogEvent {
    pub component: LogComponent,
    /// On now.
    pub current: bool,
    /// On in the default preset (Normal). One that's off by default joins
    /// at Detailed.
    pub default_on: bool,
    /// The message key of one line of help.
    pub help_key: String,
    /// Stable: `fetch.failed`.
    pub id: String,
    /// The data kinds it can carry (the badges).
    pub kinds: Vec<LogDataKind>,
    /// The message key of its plain name ("Each failed metadata fetch").
    pub label_key: String,
    pub level: LogLevel,
    /// On only while this opt-in is.
    #[schema(required = true)]
    pub opt_in: Option<LogDataKind>,
    /// Lines per hour since start, from its counter, which counts while
    /// it's off too.
    pub rate_per_hour: u64,
    /// Can't be turned off.
    pub required: bool,
}

/// Whether a kind shows on each output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct LogKindOutputs {
    pub console: bool,
    pub event_log: bool,
    pub file: bool,
    pub live: bool,
}

/// One data kind and its switch.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct LogKind {
    /// On in the Private preset.
    pub default_on: bool,
    /// Identifies torrents or people: turning it on is time-limited.
    pub identifying: bool,
    pub kind: LogDataKind,
    /// Only by opt-in, for at most 24 hours, never by a preset, never on
    /// the console or the Event Log.
    pub opt_in: bool,
    pub outputs: LogKindOutputs,
    /// When it turns off: 0 off, -1 until turned off, else Unix seconds.
    pub until: i64,
}

/// A timed verbosity ("Detailed for 30 minutes").
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct LogTemporary {
    /// Unix seconds.
    pub until: i64,
    /// `detailed` or `everything`…
    pub verbosity: String,
}

/// The presets now.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct LogPresets {
    /// `private`, `hashes`, `full` or `custom`.
    pub data: String,
    #[schema(required = true)]
    pub temporary: Option<LogTemporary>,
    /// `errors`, `warnings`, `normal`, `detailed`, `everything` or
    /// `custom`.
    pub verbosity: String,
}

/// One output's state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct LogOutput {
    /// `private` or `file` (the console's data kinds).
    #[schema(required = true)]
    pub detail: Option<String>,
    pub enabled: bool,
    /// `text` or `json` (console); `jsonl` (file).
    #[schema(required = true)]
    pub format: Option<String>,
    /// Its minimum level.
    pub level: LogLevel,
    pub output: LogOutputName,
}

/// `GET /logs/catalog`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct LogCatalog {
    pub events: Vec<LogEvent>,
    pub kinds: Vec<LogKind>,
    /// The server's clock (Unix seconds): deadlines are set from it.
    pub now: i64,
    pub outputs: Vec<LogOutput>,
    pub presets: LogPresets,
}
