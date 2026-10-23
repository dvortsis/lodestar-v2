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

/// What a field of a log line carries: a data kind, an error (its kind,
/// and its message only for plain error types), a label the code wrote, or
/// a field of a line the code didn't type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum LogFieldKind {
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
    Error,
    Label,
    Untyped,
}

/// A field's value as shown: text (`[hidden]`, `ref:…` or the value), a
/// number or a flag.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(untagged)]
pub enum LogValue {
    Flag(bool),
    Number(u64),
    Signed(i64),
    Text(String),
}

/// One field of a log line.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct LogField {
    pub kind: LogFieldKind,
    pub name: String,
    pub value: LogValue,
}

/// A fold summary: identical lines that came within the window after the
/// first one, counted rather than written.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct LogRepeated {
    pub count: u64,
    /// The id of the first line, when the output gave it one.
    #[schema(required = true)]
    pub first: Option<String>,
    /// The window, in seconds.
    pub seconds: u64,
}

/// One log line, as the live log, the history and the stream show it:
/// through the data kinds in force now.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct LogLine {
    pub component: LogComponent,
    /// The catalogued event's id; null for a line the code didn't type.
    #[schema(required = true)]
    pub event: Option<String>,
    /// In the order written.
    pub fields: Vec<LogField>,
    /// `<epoch>-<seq>`; null for a line an older build wrote.
    #[schema(required = true)]
    pub id: Option<String>,
    pub level: LogLevel,
    pub msg: String,
    /// The opt-in the line belongs to: when it turns off, the line goes.
    #[schema(required = true)]
    pub opt_in: Option<LogDataKind>,
    /// A summary of folded lines.
    #[schema(required = true)]
    pub repeated: Option<LogRepeated>,
    /// The `tracing` target of a line the code didn't type.
    #[schema(required = true)]
    pub target: Option<String>,
    /// A value was cut to fit the ring's per-line limit (16 KiB); the
    /// file has it whole.
    pub truncated: bool,
    /// RFC 3339, UTC, with milliseconds.
    pub ts: String,
}

/// `GET /logs`: a page of history, newest first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct LogHistory {
    pub lines: Vec<LogLine>,
    /// More may be older than this page (a page cut short by the read cap
    /// says so too).
    pub more: bool,
    /// Pass back as `cursor` for the next page; null at the end.
    #[schema(required = true)]
    pub next_cursor: Option<String>,
    /// Lines skipped because they were over 1 MiB or not the file's JSON
    /// shape.
    pub unreadable: u64,
}

/// Why the live log asks the client to catch up.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum LogResyncReason {
    /// The cursor is from before a restart.
    Restarted,
    /// The cursor's line has left the ring buffer.
    Expired,
    /// The cursor isn't one of ours, or is from the future.
    Malformed,
    /// The client fell behind: more than 256 matching lines waited.
    Lagged,
}

/// `event: resync`: continue from the newest lines; reload `GET /logs`
/// for what was skipped.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct LogResync {
    /// Lines that left the ring unseen (whether they matched is unknown);
    /// `lagged` only.
    #[schema(required = true)]
    pub evicted: Option<u64>,
    pub reason: LogResyncReason,
    /// Matching lines skipped, still readable through `GET /logs`;
    /// `lagged` only.
    #[schema(required = true)]
    pub skipped: Option<u64>,
}

/// `event: purge`: data kinds turned off. In lines up to `through`, set
/// those kinds' values to `[hidden]`, and drop lines whose `opt_in` is
/// listed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct LogPurge {
    pub kinds: Vec<LogDataKind>,
    /// The id of the newest line it applies to.
    pub through: String,
}

/// One frame of `GET /logs/stream`: `event:` names the variant, `data:`
/// holds its payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(tag = "event", content = "data", rename_all = "snake_case")]
pub enum LogFrame {
    Line(LogLine),
    Resync(LogResync),
    Purge(LogPurge),
}
