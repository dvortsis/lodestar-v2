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

/// What enabling a source still needs (§18.7).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SourceNeed {
    /// An `external` source: show the endpoint list, then enable with
    /// `consent` and `consent_version`.
    EndpointsAck,
    /// An `account` source: store its credentials first
    /// (`PATCH /sources/{id}/credentials`).
    Credentials,
    /// A `restricted` or `overlay` source: show the consent screen, then
    /// enable with `consent`, `consent_version` and the admin `password`
    /// (which enabling it always needs).
    Consent,
}

/// How a source's traffic leaves (§18.4, §18.7).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum EgressRoute {
    /// The system's routing table (a VPN that routes the whole container
    /// still applies).
    Direct,
    /// Through a SOCKS5 proxy. Its kill switch is always on: when the
    /// proxy can't be reached, the source's requests are refused, never
    /// sent directly.
    Socks5,
    /// Through a network interface by name. Not offered yet: the server
    /// refuses it with `409 route_unavailable`, because DNS lookups would
    /// leave outside the interface.
    Interface,
    /// From a local address. Not offered yet, as for `interface`.
    BindAddress,
}

/// Who resolves names on a SOCKS5 route.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum SocksDns {
    /// The proxy (`socks5h`): no lookup leaves outside the tunnel. The
    /// proxy decides what a name reaches, so a public name that resolves
    /// to an address on the proxy's own network reaches that network.
    Proxy,
    /// Lodestar, through its address checks (`socks5`): lookups leave
    /// outside the tunnel, but every address is checked before the proxy
    /// is asked.
    Local,
}

/// A source's egress setting.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
#[schema(example = json!({ "route": "socks5", "proxy": "10.8.0.1:1080" }))]
pub struct EgressSetting {
    #[schema(inline)]
    pub route: EgressRoute,
    /// For `socks5`: the proxy as `host:port` (a name, an IPv4 address or
    /// a bracketed IPv6 address). A name is resolved when the source
    /// starts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub proxy: Option<String>,
    /// For `socks5`: who resolves names. Left out, it's `proxy` for a
    /// source that only reaches its built-in sites, so lookups stay inside
    /// the tunnel, and `local` for a source whose sites you enter, so a
    /// public name can't reach devices on the proxy's network.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false, inline)]
    pub dns: Option<SocksDns>,
}

/// The live state of a source's route.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum EgressState {
    /// Direct: there's nothing to check.
    Direct,
    /// Not checked yet.
    Unchecked,
    Up,
    /// The kill switch is on: requests are refused.
    Down,
}

/// Counts since the source started (`source_egress_*` in
/// `docs/metrics.md`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
pub struct EgressCounts {
    /// Requests that got an HTTP answer.
    pub answered: u64,
    /// Requests abandoned in flight because the route went down.
    pub kill_switch_aborts: u64,
    /// Requests refused before sending because the route was down.
    pub kill_switch_refusals: u64,
    /// Requests refused by the address checks or the host list.
    pub policy_refusals: u64,
    /// Requests that failed to connect, timed out or were too large.
    pub transport_failures: u64,
    /// Times the route went down.
    pub trips: u64,
}

/// A running source's route, as its kill switch sees it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct EgressLive {
    #[schema(inline)]
    pub counts: EgressCounts,
    /// What you should know about this route's privacy; null when
    /// nothing.
    #[schema(required = true)]
    pub notice: Option<String>,
    /// Why it's down; null when it isn't.
    #[schema(required = true)]
    pub reason: Option<String>,
    #[schema(inline)]
    pub state: EgressState,
}

/// A source's egress: what's set, how the running source's route is
/// doing, and warnings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct SourceEgress {
    /// The running source's route; null when it isn't running (a change
    /// applies at the next start).
    #[schema(required = true, inline)]
    pub live: Option<EgressLive>,
    /// What's saved; null when it can't be read (saved by a newer
    /// version, or damaged). An unreadable setting is never taken as
    /// direct: the source's requests are refused until you set its route
    /// again.
    #[schema(required = true, inline)]
    pub setting: Option<EgressSetting>,
    /// Things to fix or know, such as `HTTPS_PROXY` being set while the
    /// source connects directly (source plugins never use it).
    pub warnings: Vec<String>,
}

/// A source's concurrency, in the plugin's own unit (§18.4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct SourceAgents {
    pub count: u32,
    /// Whether the running source follows `count`. Always false in this
    /// version: it's saved and shown, and the scheduler applies it in a
    /// later version. Label it "not active yet".
    pub enforced: bool,
    /// The most the plugin allows; 1 means it can't be changed.
    pub max: u32,
}

/// When a source may run (§18.4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct SourceSchedule {
    /// The plugin's own windows, which apply when you haven't set any.
    pub default: Vec<String>,
    /// Whether the running source keeps to `windows`. Always false in
    /// this version: they're saved and shown, and the scheduler applies
    /// them in a later version. Label them "not active yet".
    pub enforced: bool,
    /// The daily windows, as `HH:MM-HH:MM` in the server's local time (an
    /// end at or before the start wraps past midnight). Empty: any time.
    pub windows: Vec<String>,
}

/// One source plugin of this build.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct Source {
    #[schema(inline)]
    pub agents: SourceAgents,
    #[schema(inline)]
    pub consent: ConsentClass,
    /// Its class is enabled by acknowledging what's shown (`external`,
    /// `restricted`, `overlay`): enabling it sends `"consent"` naming the
    /// class and `"consent_version"`, unless that version is already
    /// acknowledged.
    pub consent_required: bool,
    /// The version an acknowledgement must name. It changes when the
    /// class's consent text or the source's endpoints change, and then
    /// the source asks again. Empty for `none` and `account`.
    pub consent_version: String,
    /// The names of its stored credentials; their values are never
    /// returned.
    pub credentials: Vec<String>,
    /// Fields `PATCH /sources/{id}` changes now.
    pub editable: Vec<String>,
    #[schema(inline)]
    pub egress: SourceEgress,
    /// Its stored state is `enabled`.
    pub enabled: bool,
    /// The hosts it contacts, as `protocol://host`: what an `external`
    /// source's acknowledgement covers.
    pub endpoints: Vec<String>,
    /// What it finds reflects your own setup (download clients, indexers,
    /// Sonarr and Radarr): kept private, and never named to an API key or
    /// feed token as a source of anything (§18.7).
    pub finds_private: bool,
    pub health: SourceHealth,
    pub id: u8,
    pub kinds: Vec<SourceKind>,
    pub name: String,
    /// The networks it reaches, such as `bt`, `bt_i2p`, `ed2k`,
    /// `ipfs` or `usenet` (§18.6).
    pub networks: Vec<String>,
    /// Fields `PATCH` refuses: `purge`, which runs as a job through
    /// `POST /sources/{id}/purge`.
    pub not_yet_editable: Vec<String>,
    /// What enabling it still needs; null when enabling is a toggle.
    #[schema(required = true, inline)]
    pub needs: Option<SourceNeed>,
    /// Enabling it asks for the admin password, every time
    /// (`restricted`, `overlay`).
    pub password_required: bool,
    /// The manifest id.
    pub plugin: String,
    #[schema(inline)]
    pub priority: SourcePriority,
    /// A purge started and didn't finish (cancelled, failed, or cut short
    /// by a restart): what it removed stays removed, and the source stays
    /// off until a purge run again finishes it.
    pub purge_pending: bool,
    /// Its stored state differs from what's running: it starts or stops
    /// at the next restart.
    pub restart_required: bool,
    #[schema(inline)]
    pub risk: RiskLevel,
    #[schema(inline)]
    pub schedule: SourceSchedule,
    #[schema(inline)]
    pub state: SourceState,
    pub version: String,
}

/// `GET /sources`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[schema(example = json!({
    "sources": [{
        "agents": { "count": 1, "enforced": false, "max": 1 },
        "consent": "none",
        "consent_required": false,
        "consent_version": "",
        "credentials": [],
        "editable": ["agents", "egress", "enabled", "priority", "schedule"],
        "egress": {
            "live": { "counts": { "answered": 0, "kill_switch_aborts": 0, "kill_switch_refusals": 0,
                "policy_refusals": 0, "transport_failures": 0, "trips": 0 },
                "notice": null, "reason": null, "state": "direct" },
            "setting": { "route": "direct" },
            "warnings": []
        },
        "enabled": true,
        "endpoints": [],
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
        "needs": null,
        "networks": ["bt"],
        "not_yet_editable": ["purge"],
        "password_required": false,
        "plugin": "dht",
        "priority": "normal",
        "purge_pending": false,
        "restart_required": false,
        "risk": "low",
        "schedule": { "default": [], "enforced": false, "windows": [] },
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
#[derive(Clone, Default, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
#[schema(example = json!({ "enabled": true, "priority": "high" }))]
pub struct SourcePatch {
    /// The consent class you acknowledge; needed to enable a source whose
    /// `needs` is `endpoints_ack` or `consent`, and must be its class.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false, inline)]
    pub consent: Option<ConsentClass>,
    /// The `consent_version` you were shown, with `consent`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub consent_version: Option<String>,
    /// The admin password, to enable a `restricted` or `overlay` source
    /// (`428 confirm_required` without it). Asked every time; never
    /// stored, logged or serialized.
    #[serde(default, skip_serializing)]
    #[schema(nullable = false, write_only)]
    pub password: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub enabled: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false, inline)]
    pub priority: Option<SourcePriority>,
    /// Its concurrency, 1 to its `agents.max`. Applies at the next start.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub agents: Option<u32>,
    /// How its traffic leaves. Applies at the next start. Changing it
    /// needs the admin `password` (`428 confirm_required`): a route is
    /// what keeps a source's traffic on your VPN. `interface` and
    /// `bind_address` are refused (`409 route_unavailable`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false, inline)]
    pub egress: Option<EgressSetting>,
    /// Not editable yet: refused with `409 not_editable`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(value_type = Value, nullable = false)]
    pub purge: Option<Value>,
    /// Daily windows as `HH:MM-HH:MM`, at most 8; `[]` runs it any time.
    /// Applies at the next start.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub schedule: Option<Vec<String>>,
}

impl std::fmt::Debug for SourcePatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SourcePatch")
            .field("consent", &self.consent)
            .field("consent_version", &self.consent_version)
            .field("password", &self.password.as_ref().map(|_| "…"))
            .field("enabled", &self.enabled)
            .field("priority", &self.priority)
            .field("agents", &self.agents)
            .field("egress", &self.egress)
            .field("schedule", &self.schedule)
            .finish_non_exhaustive()
    }
}

/// `PATCH /sources/{id}/credentials`: credential names to values (a
/// string stores it, sealed; `null` removes it). Names are 1–64 of
/// `a-z`, `0-9` and `_`, or `egress.socks5.username` and
/// `egress.socks5.password` for the source's SOCKS5 proxy; values are
/// 1–4096 bytes; a source holds at most 16. Values are never returned,
/// logged or exported.
#[derive(Clone, Default, PartialEq, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
#[schema(example = json!({ "credentials": { "api_key": "…", "old_token": null } }))]
pub struct CredentialsPatch {
    #[schema(value_type = std::collections::BTreeMap<String, Option<String>>, write_only)]
    pub credentials: std::collections::BTreeMap<String, Option<String>>,
}

impl std::fmt::Debug for CredentialsPatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CredentialsPatch")
            .field("names", &self.credentials.keys().collect::<Vec<_>>())
            .finish()
    }
}

impl SourcePatch {
    /// The first field given that can't be changed yet.
    pub fn not_yet_editable(&self) -> Option<&'static str> {
        self.purge.is_some().then_some("purge")
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
