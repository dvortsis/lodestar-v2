//! `/system` (architecture §10.10.6, §10.10.9, §10.10.13): what About,
//! the Overview and General show about the running build and this
//! installation, the restart, and the HTTP address's keep-or-revert
//! timer. One `GET /system` feeds all three screens.
//!
//! A value this platform or build can't measure is `null`, never 0.
//! Nothing here is a secret, a URL credential or a torrent detail:
//! connections are counts and states, never addresses.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::dto::events::RebuildEvent;
use crate::dto::settings::SettingSource;

/// The release channel this build came from. `dev` is a local build.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum Channel {
    Stable,
    Beta,
    Nightly,
    Dev,
}

/// How the build was made.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct BuildInfo {
    /// The full commit hash; null when the build didn't know it.
    #[schema(required = true)]
    pub commit: Option<String>,
    /// Unix seconds: `SOURCE_DATE_EPOCH`, else the commit's date; null
    /// when neither was known (never the time of the build, so builds
    /// stay reproducible).
    #[schema(required = true)]
    pub date: Option<u64>,
    /// `rustc -V` of the compiler that built it.
    #[schema(required = true)]
    pub rustc: Option<String>,
    /// The target triple, e.g. `x86_64-unknown-linux-musl`.
    pub target: String,
    /// The embedded UI's build id (SvelteKit's `_app/version.json`); null
    /// when the binary embeds the stub page.
    #[schema(required = true)]
    pub ui_build_id: Option<String>,
}

/// The data formats a bug report needs, and why an upgrade might reindex.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct DataFormats {
    /// The search index's version, `v{schema}-t{tokenizer}`.
    pub index: String,
    /// A background index rebuild's progress; null when none runs.
    #[schema(required = true)]
    pub rebuild: Option<RebuildEvent>,
    /// The store's schema version (the last migration applied); null if
    /// the store couldn't be read.
    #[schema(required = true)]
    pub store_schema: Option<i64>,
}

/// How Lodestar is installed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum DeploymentKind {
    Docker,
    Unraid,
    WindowsService,
    WindowsPortable,
    Systemd,
    Tarball,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct Deployment {
    /// The image's digest, when the image sets it.
    #[schema(required = true)]
    pub image_digest: Option<String>,
    /// The image's tag, when the image sets it.
    #[schema(required = true)]
    pub image_tag: Option<String>,
    #[schema(inline)]
    pub kind: DeploymentKind,
}

/// The machine Lodestar runs on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct HostInfo {
    /// `x86_64`, `aarch64`, …
    pub arch: String,
    /// CPU cores this process may use.
    #[schema(required = true)]
    pub cpus: Option<u64>,
    /// Memory this process may use: the machine's, or a container's
    /// limit when lower.
    #[schema(required = true)]
    pub memory_bytes: Option<u64>,
    /// `linux`, `macos`, `windows`, …
    pub os: String,
    /// The IANA time zone, e.g. `Europe/Berlin`.
    #[schema(required = true)]
    pub time_zone: Option<String>,
}

/// The data directory and what it holds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct DataInfo {
    /// Bytes in the cold store; null when there is none.
    #[schema(required = true)]
    pub cold_bytes: Option<u64>,
    /// Bytes in the database, its WAL and shared memory.
    #[schema(required = true)]
    pub database_bytes: Option<u64>,
    /// The data directory, as configured.
    pub dir: String,
    /// What's wrong with its filesystem and the safe alternative (FUSE,
    /// a network share, Unraid's user share); null when nothing is.
    #[schema(required = true)]
    pub filesystem_warning: Option<String>,
    /// Bytes free on its volume.
    #[schema(required = true)]
    pub free_bytes: Option<u64>,
    /// Bytes in the search index, every version included.
    #[schema(required = true)]
    pub index_bytes: Option<u64>,
    /// Bytes in the log files; null when nothing is logged to files.
    #[schema(required = true)]
    pub logs_bytes: Option<u64>,
}

/// Something Lodestar connects to (§10.10.13 "No telemetry, ever").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ConnectionKind {
    Dht,
    Sources,
    BlocklistSubscriptions,
    Integrations,
    Updates,
    Companions,
    Workers,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum ConnectionState {
    On,
    Paused,
    Off,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct Connection {
    /// How many are enabled (sources, lists, integrations); null for the
    /// DHT, and for what this build doesn't have yet.
    #[schema(required = true)]
    pub count: Option<u64>,
    #[schema(inline)]
    pub kind: ConnectionKind,
    /// Unix seconds of the last contact, where it's recorded (a list's
    /// last refresh).
    #[schema(required = true)]
    pub last_checked_at: Option<u64>,
    /// The Settings screen that changes it (`/settings/<screen>`).
    pub settings: String,
    /// Null for what this build doesn't have yet (updates until the
    /// update engine, companions and workers until v0.6).
    #[schema(required = true, inline)]
    pub state: Option<ConnectionState>,
}

/// How this process restarts itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum RestartMethod {
    /// A graceful shutdown, then the same binary again in place with the
    /// same arguments and process id (Linux, macOS, Docker, Unraid,
    /// systemd).
    Reexec,
    /// It can't: restart it the way it was started.
    Manual,
}

/// Whether Restart now is offered (§10.10.6).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct RestartInfo {
    pub available: bool,
    /// Seconds this run took from start to listening: about how long a
    /// restart pauses crawling.
    pub estimated_seconds: u64,
    /// When it isn't available, how to restart instead.
    #[schema(required = true)]
    pub hint: Option<String>,
    #[schema(inline)]
    pub method: RestartMethod,
}

/// What a keep-or-revert timer guards.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum KeepOrRevertKind {
    HttpBind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum AddressState {
    /// Saved; the new address applies at the next restart.
    AwaitingRestart,
    /// Serving on the new address: confirm it before `deadline`, or the
    /// previous one comes back.
    AwaitingConfirmation,
    /// Nobody confirmed (or it didn't bind, or Revert was chosen): the
    /// previous address is saved again. When this process can't restart
    /// itself, it applies at the next restart (`restart_required`).
    Reverted,
}

/// A pending HTTP-address change (§10.10.6's lockout protection).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct AddressChange {
    /// Unix seconds when it reverts, while awaiting confirmation.
    #[schema(required = true)]
    pub deadline: Option<u64>,
    #[schema(inline)]
    pub kind: KeepOrRevertKind,
    /// Where the UI was before the change.
    pub previous_url: String,
    #[schema(inline)]
    pub state: AddressState,
    /// Where the UI is after it.
    pub url: String,
}

/// One setting changed from its default, by name only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ChangedSetting {
    /// `section.field`.
    pub name: String,
    #[schema(inline)]
    pub source: SettingSource,
}

/// The copyable diagnostics block's server half: never a value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct Diagnostics {
    /// Settings that aren't at their default, and where each comes from.
    pub changed: Vec<ChangedSetting>,
    /// The logging preset; null until file logging lands.
    #[schema(required = true)]
    pub logging: Option<String>,
    /// Errors logged recently; null until file logging lands.
    #[schema(required = true)]
    pub recent_errors: Option<u64>,
}

/// `GET /system`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[schema(example = json!({
    "address_change": null,
    "build": {
        "commit": "2e64daaddf4df35b41908529eed3de8102b64e30",
        "date": 1792677600,
        "rustc": "rustc 1.94.0 (4a4ef493e 2026-03-02)",
        "target": "x86_64-unknown-linux-musl",
        "ui_build_id": "1792677600123"
    },
    "channel": "stable",
    "connections": [
        { "count": null, "kind": "dht", "last_checked_at": null, "settings": "network", "state": "on" }
    ],
    "data": {
        "cold_bytes": null,
        "database_bytes": 6442450944u64,
        "dir": "/data",
        "filesystem_warning": null,
        "free_bytes": 107374182400u64,
        "index_bytes": 2684354560u64,
        "logs_bytes": null
    },
    "data_formats": { "index": "v3-t1", "rebuild": null, "store_schema": 14 },
    "deployment": { "image_digest": null, "image_tag": "0.5.2", "kind": "docker" },
    "diagnostics": {
        "changed": [{ "name": "crawler.intensity", "source": "db" }],
        "logging": null,
        "recent_errors": null
    },
    "host": { "arch": "x86_64", "cpus": 4, "memory_bytes": 8589934592u64, "os": "linux", "time_zone": "Europe/Berlin" },
    "repository": "https://github.com/dvortsis/lodestar-v2",
    "restart": { "available": true, "estimated_seconds": 4, "hint": null, "method": "reexec" },
    "restart_required": ["network.dht_port"],
    "started_at": 1792677600,
    "uptime_s": 86400,
    "version": "0.5.2"
}))]
pub struct SystemInfo {
    /// A pending HTTP-address change; null when there is none.
    #[schema(required = true)]
    pub address_change: Option<AddressChange>,
    pub build: BuildInfo,
    #[schema(inline)]
    pub channel: Channel,
    /// Everything Lodestar connects to, each with its state.
    pub connections: Vec<Connection>,
    pub data: DataInfo,
    pub data_formats: DataFormats,
    pub deployment: Deployment,
    pub diagnostics: Diagnostics,
    pub host: HostInfo,
    /// The source repository (`Cargo.toml`'s `repository`), so a fork
    /// links to itself.
    pub repository: String,
    pub restart: RestartInfo,
    /// Saved settings that apply at the next restart, as `section.field`.
    pub restart_required: Vec<String>,
    /// Unix seconds this run started.
    pub started_at: u64,
    pub uptime_s: u64,
    pub version: String,
}

/// `POST /system/restart`: accepted; the server restarts once this
/// answer has gone out.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[schema(example = json!({ "estimated_seconds": 4, "restarting": true }))]
pub struct RestartAccepted {
    /// About how long until it answers again.
    pub estimated_seconds: u64,
    pub restarting: bool,
}

/// `POST /system/address/keep` and `/revert`: the change as it is now.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[schema(example = json!({ "address_change": null, "restarting": false }))]
pub struct AddressOutcome {
    /// Null once kept (or a revert before the restart undid it).
    #[schema(required = true)]
    pub address_change: Option<AddressChange>,
    /// The server restarts on the previous address once this answer has
    /// gone out.
    pub restarting: bool,
}
