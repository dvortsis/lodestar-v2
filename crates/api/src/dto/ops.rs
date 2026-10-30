//! `/stats`, `/crawler`, `/sources` and `/network/check` (architecture
//! §9, §10.9's global bar, §10.10's network check, §18.8): the process's
//! summary numbers and its controls. Field meanings and units are in
//! `docs/metrics.md` §7.6; a value that can't be measured is `null`,
//! never 0.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use utoipa::ToSchema;

/// How hard the crawler works (§5), as `/settings/crawler` has it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum CrawlerIntensity {
    Low,
    Balanced,
    Aggressive,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum CrawlerState {
    Running,
    Paused,
}

/// The DHT node's state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct DhtStatus {
    /// It has a routing table to crawl from.
    pub bootstrapped: bool,
    /// Queries awaiting an answer.
    pub outstanding: u64,
    /// Nodes across its routing tables.
    pub routing_nodes: u64,
}

/// The metadata-fetch pool's state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct FetchStatus {
    /// Fetches running now.
    pub in_flight: u64,
    /// Peer lookups out.
    pub lookups: u64,
    /// Keys waiting for a fetch slot or for peers.
    pub queued: u64,
}

/// `GET /crawler`, and the answer to every crawler control.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[schema(example = json!({
    "dht": { "bootstrapped": true, "outstanding": 212, "routing_nodes": 3104 },
    "fetch": { "in_flight": 388, "lookups": 120, "queued": 15022 },
    "intensity": "balanced",
    "intensity_env": null,
    "paused": false,
    "since": 1792677600,
    "state": "running"
}))]
pub struct CrawlerStatus {
    /// Null when the DHT is off or hasn't started yet.
    #[schema(required = true)]
    pub dht: Option<DhtStatus>,
    pub fetch: FetchStatus,
    #[schema(inline)]
    pub intensity: CrawlerIntensity,
    /// The environment variable that sets the intensity, which then
    /// can't be changed here; null when it can.
    #[schema(required = true)]
    pub intensity_env: Option<String>,
    pub paused: bool,
    /// Unix seconds of the last pause or resume; the start when running
    /// since then.
    pub since: i64,
    #[schema(inline)]
    pub state: CrawlerState,
}

/// `PATCH /crawler`: the global bar's intensity control. The same
/// setting as `crawler.intensity` in `/settings/crawler`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
#[schema(example = json!({ "intensity": "low" }))]
pub struct CrawlerPatch {
    #[schema(inline)]
    pub intensity: CrawlerIntensity,
}

/// `GET /stats`: what the global bar and status pill show
/// (`docs/metrics.md` §7.6).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[schema(example = json!({
    "data_bytes": 9126805504u64,
    "disk_free_bytes": null,
    "fetched_per_hour": 1260.0,
    "harvested_per_hour": 184320.0,
    "index_lag_s": 2,
    "paused": false,
    "queue_depth": 15410,
    "sampling_blocked": { "reason": "first_start", "remaining_s": 20940 },
    "searchable_torrents": 2410388,
    "stored_torrents": 2410391,
    "updated_at": 1792681200,
    "uptime_s": 86400
}))]
pub struct StatsResponse {
    /// Bytes the data directory holds (database, index, state).
    #[schema(required = true)]
    pub data_bytes: Option<u64>,
    /// Bytes free on the data directory's volume; null where this build
    /// can't measure it.
    #[schema(required = true)]
    pub disk_free_bytes: Option<u64>,
    /// Info dicts fetched per hour, over the last 5 minutes; null in the
    /// first minute.
    #[schema(required = true)]
    pub fetched_per_hour: Option<f64>,
    /// Keys the sources reported per hour, over the last 5 minutes; null
    /// in the first minute.
    #[schema(required = true)]
    pub harvested_per_hour: Option<f64>,
    /// Seconds the index trails the store's last commit; null when search
    /// is off or either hasn't committed since the start.
    #[schema(required = true)]
    pub index_lag_s: Option<u64>,
    pub paused: bool,
    /// Keys queued or in flight in the fetch pool.
    pub queue_depth: u64,
    /// Why the DHT isn't sampling yet; null when it is, before its first
    /// report, and when it is off or not running.
    #[schema(required = true)]
    pub sampling_blocked: Option<SamplingBlocked>,
    /// Documents in the search index; null when search is off.
    #[schema(required = true)]
    pub searchable_torrents: Option<u64>,
    pub stored_torrents: u64,
    /// Unix seconds these numbers were read.
    pub updated_at: i64,
    pub uptime_s: u64,
}

/// Why the DHT holds back its samples after a start (CR-0d): an unclean
/// restart, the restart proof switched off, or a restart record it can't
/// write.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct SamplingBlocked {
    /// `dht_sample_suppression_reason`'s label (`docs/metrics.md`), such
    /// as `off`, `first_start`, `dirty`, `locked` or `invalidate_failed`.
    pub reason: String,
    /// Seconds until sampling may begin (`dht_sample_suppression_seconds`).
    /// For `invalidate_failed`, and `locked` while the lock is busy, it's a
    /// lower bound: sampling also waits until the restart record can be
    /// written.
    pub remaining_s: u64,
}

/// A source's stored lifecycle state (§18.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum SourceState {
    Enabled,
    Paused,
    Disabled,
    Purged,
}

/// What enabling a source requires (§18.7).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum ConsentClass {
    None,
    External,
    Account,
    Restricted,
    Overlay,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum RiskLevel {
    Low,
    Medium,
    High,
}

/// A source's share of the fetch pool (§18.4: weights 1 / 3 / 6 / 10).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum SourcePriority {
    Low,
    Normal,
    High,
    Max,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SourceKind {
    Discovery,
    Importer,
    PeerFinder,
    Enrichment,
    Network,
    Output,
}

/// How a source is doing.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct SourceHealth {
    /// Keys it was first to report in the last 24 hours.
    pub harvested_24h: i64,
    /// Why it last stopped with an error; null when it didn't.
    #[schema(required = true)]
    pub last_error: Option<String>,
    /// Unix seconds it last started; null when it hasn't run since the
    /// process started.
    #[schema(required = true)]
    pub last_run: Option<i64>,
    pub running: bool,
    /// Of `harvested_24h`, the ones stored.
    pub stored_24h: i64,
}

/// One source plugin of this build.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct Source {
    #[schema(inline)]
    pub consent: ConsentClass,
    /// Enabling it needs `"consent"` naming its class.
    pub consent_required: bool,
    /// Fields `PATCH /sources/{id}` changes now.
    pub editable: Vec<String>,
    /// Its stored state is `enabled`.
    pub enabled: bool,
    pub health: SourceHealth,
    pub id: u8,
    pub kinds: Vec<SourceKind>,
    pub name: String,
    /// Fields planned for the v0.6 sources epic (agents, egress, purge,
    /// schedule): listed so the UI can show them, refused by `PATCH`.
    pub not_yet_editable: Vec<String>,
    /// The manifest id.
    pub plugin: String,
    #[schema(inline)]
    pub priority: SourcePriority,
    /// Its stored state differs from what's running: it starts or stops
    /// at the next restart.
    pub restart_required: bool,
    #[schema(inline)]
    pub risk: RiskLevel,
    #[schema(inline)]
    pub state: SourceState,
    pub version: String,
}

/// `GET /sources`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[schema(example = json!({
    "sources": [{
        "consent": "none",
        "consent_required": false,
        "editable": ["enabled", "priority"],
        "enabled": true,
        "health": {
            "harvested_24h": 1840221,
            "last_error": null,
            "last_run": 1792677600,
            "running": true,
            "stored_24h": 30115
        },
        "id": 0,
        "kinds": ["discovery", "peer_finder"],
        "name": "Mainline DHT",
        "not_yet_editable": ["agents", "egress", "purge", "schedule"],
        "plugin": "dht",
        "priority": "normal",
        "restart_required": false,
        "risk": "low",
        "state": "enabled",
        "version": "0.0.0"
    }]
}))]
pub struct SourceList {
    pub sources: Vec<Source>,
}

/// `PATCH /sources/{id}`. Fields left out keep their values. A new
/// priority applies at once; enabling or disabling takes effect at the
/// next start (`restart_required`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
#[schema(example = json!({ "enabled": true, "priority": "high" }))]
pub struct SourcePatch {
    /// The consent class you agree to; needed to enable a source whose
    /// class is `restricted` or `overlay`, and must be that class.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false, inline)]
    pub consent: Option<ConsentClass>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub enabled: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false, inline)]
    pub priority: Option<SourcePriority>,
    /// Not editable before v0.6: refused with `409 not_editable`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(value_type = Value, nullable = false)]
    pub agents: Option<Value>,
    /// Not editable before v0.6: refused with `409 not_editable`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(value_type = Value, nullable = false)]
    pub egress: Option<Value>,
    /// Not editable before v0.6: refused with `409 not_editable`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(value_type = Value, nullable = false)]
    pub purge: Option<Value>,
    /// Not editable before v0.6: refused with `409 not_editable`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(value_type = Value, nullable = false)]
    pub schedule: Option<Value>,
}

impl SourcePatch {
    /// The first field given that can't be changed yet.
    pub fn not_yet_editable(&self) -> Option<&'static str> {
        [
            ("agents", &self.agents),
            ("egress", &self.egress),
            ("purge", &self.purge),
            ("schedule", &self.schedule),
        ]
        .into_iter()
        .find(|(_, v)| v.is_some())
        .map(|(name, _)| name)
    }
}

/// Why `reachable` reads as it does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ReachReason {
    /// The DHT is off.
    DhtOff,
    /// The node has no routing table yet.
    Bootstrapping,
    /// Less than `min_window_s` since bootstrap or a restart of the check.
    Measuring,
    /// Nodes we never queried are reaching us from at least 3 networks
    /// (/24 or /64): the port is open.
    UnsolicitedQueries,
    /// Some did, but from fewer than 3 networks: too little to call the
    /// port open (a NAT can let a neighbour's query through).
    FewUnsolicitedSources,
    /// None did in the window: the port looks closed to inbound packets.
    NoUnsolicitedQueries,
}

/// UPnP / NAT-PMP port mapping (§5). This build has neither.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum PortMapping {
    Unavailable,
}

/// Our address as other DHT nodes see it: the BEP 42 `ip` key of their
/// replies, one vote per network (/24 or /64), the last 32 networks,
/// IPv4 and IPv6 tallied apart (the IPv4 consensus wins when both have
/// one).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ExternalAddress {
    /// Voting networks naming the leading address.
    pub agreeing: u32,
    /// The consensus address: at least 5 voters, and more than half of
    /// them, agree; null until they do.
    #[schema(required = true)]
    pub ip: Option<String>,
    /// The port most of those voters saw; null without a consensus.
    #[schema(required = true)]
    pub port: Option<u16>,
    pub voters: u32,
}

/// `GET /network/check` and `POST /network/check`: the first-run
/// wizard's network check and the dashboard's "reachable" flag. Every
/// probe is ordinary DHT traffic; no outside service is asked (§18.7).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[schema(example = json!({
    "checked_at": 1792681200,
    "external": { "agreeing": 14, "ip": "203.0.113.9", "port": 6881, "voters": 16 },
    "inbound_queries": 412,
    "mapping": "unavailable",
    "measured_s": 590,
    "min_window_s": 120,
    "reachable": true,
    "reason": "unsolicited_queries",
    "unsolicited_queries": 37,
    "unsolicited_sources": 21,
    "window_s": 600,
    "window_started_at": 1792677610
}))]
pub struct NetworkCheck {
    pub checked_at: i64,
    pub external: ExternalAddress,
    /// DHT queries received in the window; null until measuring.
    #[schema(required = true)]
    pub inbound_queries: Option<u64>,
    #[schema(inline)]
    pub mapping: PortMapping,
    /// Seconds the evidence covers.
    pub measured_s: u64,
    /// `reachable` stays null until the window has run this long.
    pub min_window_s: u64,
    /// Whether unsolicited inbound DHT queries arrive on the DHT port;
    /// null is unknown.
    #[schema(required = true)]
    pub reachable: Option<bool>,
    #[schema(inline)]
    pub reason: ReachReason,
    /// Of `inbound_queries`, the ones from nodes we hadn't sent anything to lately.
    #[schema(required = true)]
    pub unsolicited_queries: Option<u64>,
    /// The distinct networks (/24 or /64) those came from, counting at
    /// most the 64 most recent; null until measuring. `reachable` needs
    /// at least 3.
    #[schema(required = true)]
    pub unsolicited_sources: Option<u64>,
    /// The longest stretch of evidence looked at.
    pub window_s: u64,
    /// Unix seconds the window started (bootstrap or a restart); null
    /// before bootstrap.
    #[schema(required = true)]
    pub window_started_at: Option<i64>,
}
