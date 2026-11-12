//! The API's view of the running process (`lodestar_api::ops::OpsApi`):
//! `/stats`, `/crawler`, `/sources` and `/network/check` over the
//! pipeline's [`Control`] and [`Monitor`] and the index's counters.
//!
//! Every number is read from the counters `/metrics` and the status page
//! read (`metrics::render`): nothing is counted a second time.

use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use lodestar_api::dto::jobs::PurgePreview;
use lodestar_api::ops::{
    BoxFuture, CompanionNeed, CompanionNeedState, ConsentClass, CrawlerRuntime, CredentialField,
    CredentialKind, DhtStatus, EgressCounts, EgressLive, EgressRoute, EgressSetting, EgressState,
    ExternalAddress, FetchStatus, NetworkCheck, OpsApi, OpsError, PortMapping, ReachReason,
    RiskLevel, SamplingBlocked, Secret, SocksDns, Source, SourceAgents, SourceEgress, SourceHealth,
    SourceKind, SourceNeed, SourcePatch, SourcePriority, SourceSchedule, SourceState,
    StatsResponse, parse_proxy, parse_window, window_text,
};
use lodestar_dht::node::{NodeHandle, StatsSnapshot};
use lodestar_pipeline::Monitor;
use lodestar_pipeline::control::{
    self, Control, REACH_MIN_SECS, REACH_WINDOW_SECS, SourceChange, SourceError, SourceView,
};
use lodestar_pipeline::requires::State as CompanionState;
use lodestar_sources::consent::{self as consent_rules, Need, SourceCredentials};
use lodestar_sources::consent::{CredentialKind as SpecKind, CredentialSpec};
use lodestar_sources::{Consent, Kind, Network, Priority, ProxySpec, Risk, RouteState, Window};
use lodestar_store::SourceId;

use crate::http::SearchState;
use crate::settings::crawler::Crawler;

/// What a source's `PATCH` changes now, and what waits for a later slice
/// (purge, SRC-14b). Credentials have their own route,
/// `PATCH /sources/{id}/credentials`.
const EDITABLE: [&str; 5] = ["agents", "egress", "enabled", "priority", "schedule"];
const NOT_YET_EDITABLE: [&str; 1] = ["purge"];

/// [`EDITABLE`], plus `config` for a source whose settings can be written,
/// in order.
fn editable(config: bool) -> Vec<String> {
    let mut out: Vec<String> = EDITABLE.map(String::from).to_vec();
    if config {
        out.push("config".into());
        out.sort_unstable();
    }
    out
}
// The API's input bound and the stored bound are one number.
const _: () = assert!(lodestar_api::ops::MAX_WINDOWS == control::MAX_WINDOWS);
const _: () = assert!(lodestar_api::ops::MAX_CONFIG_BYTES == control::MAX_CONFIG_BYTES);
/// How long the data directory's size is reused before it's measured
/// again (it walks the directory).
const SIZE_TTL: Duration = Duration::from_secs(30);

/// The data directory's last measured size, and when it was measured.
type SizeCache = Arc<Mutex<Option<(Instant, Option<u64>)>>>;
/// The DHT source's last stats report (`None` until its first), and its
/// node while it runs: a report outlives a node that stopped or never
/// started.
#[derive(Clone)]
pub struct DhtReport {
    pub stats: Arc<Mutex<Option<StatsSnapshot>>>,
    pub node: Arc<Mutex<Option<NodeHandle>>>,
}

impl DhtReport {
    /// The last report, only while the node runs.
    fn live(&self) -> Option<StatsSnapshot> {
        let running = self
            .node
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_some();
        running
            .then(|| *self.stats.lock().unwrap_or_else(|e| e.into_inner()))
            .flatten()
    }
}

pub struct Ops {
    pub control: Control,
    /// The registry's crawler section, applied by `sync_crawler`.
    crawler: tokio::sync::watch::Receiver<Crawler>,
    pub monitor: Monitor,
    pub search: Option<SearchState>,
    pub data_dir: PathBuf,
    size: SizeCache,
    /// `None` when the DHT is off.
    dht: Option<DhtReport>,
    /// The per-source credential store, over the install's key file.
    pub credentials: SourceCredentials,
}

impl std::fmt::Debug for Ops {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Ops")
            .field("data_dir", &self.data_dir)
            .finish_non_exhaustive()
    }
}

impl Ops {
    /// Source `v` as the API shows it, with its companion's state now.
    fn source(&self, v: SourceView) -> Source {
        let companion = self.monitor.companion_state(v.status.plugin);
        source(v, companion)
    }

    pub fn new(
        control: Control,
        crawler: tokio::sync::watch::Receiver<Crawler>,
        monitor: Monitor,
        search: Option<SearchState>,
        data_dir: PathBuf,
    ) -> Ops {
        let credentials = SourceCredentials::new(control.store(), data_dir.clone());
        Ops {
            credentials,
            control,
            crawler,
            monitor,
            search,
            data_dir,
            size: Arc::default(),
            dht: None,
        }
    }

    /// The DHT's reports, for `sampling_blocked`.
    pub fn with_dht(mut self, dht: Option<DhtReport>) -> Ops {
        self.dht = dht;
        self
    }

    /// The data directory's size, measured at most every [`SIZE_TTL`]
    /// (blocking: it walks the directory).
    fn data_bytes(&self) -> impl FnOnce() -> Option<u64> + Send + 'static {
        let (size, dir) = (self.size.clone(), self.data_dir.clone());
        move || {
            let mut cached = size.lock().expect("size lock");
            if let Some((at, bytes)) = *cached
                && at.elapsed() < SIZE_TTL
            {
                return bytes;
            }
            let bytes = dir_size(&dir);
            *cached = Some((Instant::now(), bytes));
            bytes
        }
    }

    /// Everything but `data_bytes` and `index_lag_s`, which
    /// [`OpsApi::stats`] reads off the async runtime.
    fn stats_now(&self) -> StatsResponse {
        let now = crate::unix_now();
        let get = |a: &std::sync::atomic::AtomicU64| a.load(Ordering::Relaxed);
        let (harvested, fetched) = self.control.rates();
        StatsResponse {
            data_bytes: None,
            // std can't ask for a volume's free space; the crate that can
            // (fs4) is a new dependency, which needs the owner's approval.
            disk_free_bytes: None,
            fetched_per_hour: fetched,
            harvested_per_hour: harvested,
            index_lag_s: None,
            paused: self.control.status().paused,
            queue_depth: get(&self.monitor.pool_stats().pending),
            sampling_blocked: self
                .dht
                .as_ref()
                .and_then(DhtReport::live)
                .and_then(sampling_blocked),
            searchable_torrents: self.search.as_ref().map(|s| s.index.index().num_docs()),
            stored_torrents: self.monitor.torrents(),
            updated_at: now,
            uptime_s: now.saturating_sub(self.monitor.started_at()).max(0) as u64,
        }
    }
}

/// What `sampling_blocked` says for a running node's report (CR-0d): its
/// suppression and why while it lasts, nothing once sampling runs.
fn sampling_blocked(s: StatsSnapshot) -> Option<SamplingBlocked> {
    (s.sample_suppression_seconds > 0).then(|| SamplingBlocked {
        reason: s.sample_suppression_reason.as_str().into(),
        remaining_s: s.sample_suppression_seconds,
    })
}

/// How far the index trails the store: 0 when everything stored is
/// searchable (`watermark` ≥ `max_id`); while it's behind, the seconds
/// since the index last committed, so a stalled index shows a growing
/// lag. `None` when it's behind and hasn't committed since the start
/// (say, a backlog being indexed after a restart).
fn index_lag(now: u64, max_id: i64, watermark: i64, index_commit: u64) -> Option<u64> {
    if watermark >= max_id {
        Some(0)
    } else if index_commit == 0 {
        None
    } else {
        Some(now.saturating_sub(index_commit))
    }
}

/// Bytes in the files under `dir`, not following links; `None` if it
/// can't be read.
pub(crate) fn dir_size(dir: &Path) -> Option<u64> {
    let mut total = 0u64;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for entry in std::fs::read_dir(&d).ok()? {
            let Ok(entry) = entry else { continue };
            let Ok(meta) = entry.metadata() else { continue };
            if meta.is_dir() {
                stack.push(entry.path());
            } else if meta.is_file() {
                total += meta.len();
            }
        }
    }
    Some(total)
}

fn consent(c: Consent) -> ConsentClass {
    match c {
        Consent::None => ConsentClass::None,
        Consent::External => ConsentClass::External,
        Consent::Account => ConsentClass::Account,
        Consent::Restricted => ConsentClass::Restricted,
        Consent::Overlay => ConsentClass::Overlay,
    }
}

fn consent_back(c: ConsentClass) -> Consent {
    match c {
        ConsentClass::None => Consent::None,
        ConsentClass::External => Consent::External,
        ConsentClass::Account => Consent::Account,
        ConsentClass::Restricted => Consent::Restricted,
        ConsentClass::Overlay => Consent::Overlay,
    }
}

fn priority(p: Priority) -> SourcePriority {
    match p {
        Priority::Low => SourcePriority::Low,
        Priority::Normal => SourcePriority::Normal,
        Priority::High => SourcePriority::High,
        Priority::Max => SourcePriority::Max,
    }
}

fn priority_back(p: SourcePriority) -> Priority {
    match p {
        SourcePriority::Low => Priority::Low,
        SourcePriority::Normal => Priority::Normal,
        SourcePriority::High => Priority::High,
        SourcePriority::Max => Priority::Max,
    }
}

fn windows(w: &[Window]) -> Vec<String> {
    w.iter().map(|w| window_text(w.start, w.end)).collect()
}

fn egress_setting(e: &control::EgressSetting) -> EgressSetting {
    match e {
        control::EgressSetting::Direct => EgressSetting {
            route: EgressRoute::Direct,
            proxy: None,
            dns: None,
        },
        control::EgressSetting::Socks5(p) => EgressSetting {
            route: EgressRoute::Socks5,
            proxy: Some(match p.host.parse::<std::net::Ipv6Addr>() {
                Ok(_) => format!("[{}]:{}", p.host, p.port),
                Err(_) => format!("{}:{}", p.host, p.port),
            }),
            dns: p.dns.map(|d| match d {
                lodestar_sources::SocksDns::Proxy => SocksDns::Proxy,
                lodestar_sources::SocksDns::Local => SocksDns::Local,
            }),
        },
    }
}

/// The route's checked setting back as the pipeline's (the API refused
/// the interface and bind-address routes, and checked the proxy).
fn egress_back(e: EgressSetting) -> Result<control::EgressSetting, OpsError> {
    let invalid = |m: &str| OpsError::Invalid {
        code: "invalid_egress",
        field: "egress",
        message: m.to_string(),
    };
    match e.route {
        EgressRoute::Direct => Ok(control::EgressSetting::Direct),
        EgressRoute::Socks5 => {
            let (host, port) = e
                .proxy
                .as_deref()
                .and_then(parse_proxy)
                .ok_or_else(|| invalid("Give the SOCKS5 proxy as host:port."))?;
            Ok(control::EgressSetting::Socks5(ProxySpec {
                host,
                port,
                dns: e.dns.map(|d| match d {
                    SocksDns::Proxy => lodestar_sources::SocksDns::Proxy,
                    SocksDns::Local => lodestar_sources::SocksDns::Local,
                }),
            }))
        }
        EgressRoute::Interface | EgressRoute::BindAddress => Err(OpsError::RouteUnavailable),
    }
}

fn egress_live(l: &lodestar_pipeline::EgressLive) -> EgressLive {
    let c = l.status.counters;
    let (state, reason) = match l.status.state {
        _ if l.status.route == lodestar_sources::RouteKind::Direct => (EgressState::Direct, None),
        RouteState::Unchecked => (EgressState::Unchecked, None),
        RouteState::Up => (EgressState::Up, None),
        RouteState::Down(r) => (EgressState::Down, Some(r.to_string())),
    };
    EgressLive {
        counts: EgressCounts {
            answered: c.answered,
            kill_switch_aborts: c.kill_switch_aborts,
            kill_switch_refusals: c.kill_switch_refusals,
            policy_refusals: c.policy_refusals,
            transport_failures: c.transport_failures,
            trips: c.trips,
        },
        notice: l.status.notice.map(String::from),
        reason,
        state,
    }
}

/// A plugin's declared credentials, as the API lists them: the fields,
/// never a value.
fn credential_spec(spec: &[CredentialSpec]) -> Vec<CredentialField> {
    spec.iter()
        .map(|c| CredentialField {
            kind: match c.kind {
                SpecKind::Secret => CredentialKind::Secret,
                SpecKind::Username => CredentialKind::Username,
            },
            label: c.label.to_string(),
            name: c.name.to_string(),
            required: c.required,
        })
        .collect()
}

/// How deep [`config_schema`] looks into a settings schema. Deeper than
/// this counts as a secret (fail closed); every plugin's schema today is at
/// most four levels deep.
const SCHEMA_DEPTH: usize = 16;

/// A plugin's compiled-in settings schema as the API lists it: without
/// its secrets, at any depth. Secrets are written through
/// `PATCH /sources/{id}/credentials` (`credential_spec`), never as
/// settings, so a form generated from the schema never asks for one. It's
/// the second guard: `sources-plugins`' `credential_spec` test refuses a
/// write-only field or a credential-named property anywhere in a plugin's
/// schema. See [`strip_secrets`] for what goes. A schema that doesn't
/// parse (a plugin bug, which that test catches), or whose root goes,
/// reads as [`lodestar_sources::EMPTY_SCHEMA`]: a form with no fields.
fn config_schema(schema: &str, spec: &[CredentialSpec]) -> serde_json::Value {
    let empty =
        || serde_json::from_str(lodestar_sources::EMPTY_SCHEMA).expect("the empty schema parses");
    let Ok(mut v) = serde_json::from_str::<serde_json::Value>(schema) else {
        return empty();
    };
    let names: Vec<&str> = spec.iter().map(|c| c.name).collect();
    if strip_secrets(&mut v, &names, 0) {
        return empty();
    }
    v
}

/// Strips the secrets out of the subschema `v`; `true` when `v` itself
/// must go. A subschema goes when it is `"writeOnly"`, or when it's nested
/// deeper than [`SCHEMA_DEPTH`] (fail closed). Below it:
/// - a property (in `properties`, `$defs` or `definitions`) that goes, or
///   a property named in `names`, is removed with its `required` entry;
/// - an entry of `allOf`, `anyOf`, `oneOf` or `prefixItems` that goes is
///   removed;
/// - when `items`, `additionalProperties`, `not` or `contains` goes, its
///   values are secrets, so `v` goes too.
fn strip_secrets(v: &mut serde_json::Value, names: &[&str], depth: usize) -> bool {
    use serde_json::Value;
    if depth > SCHEMA_DEPTH {
        return true;
    }
    let Value::Object(o) = v else { return false };
    if o.get("writeOnly") == Some(&Value::Bool(true)) {
        return true;
    }
    let mut gone: Vec<String> = Vec::new();
    for key in ["properties", "$defs", "definitions"] {
        if let Some(Value::Object(map)) = o.get_mut(key) {
            map.retain(|k, c| {
                let named = key == "properties" && names.contains(&k.as_str());
                let keep = !named && !strip_secrets(c, names, depth + 1);
                if !keep && key == "properties" {
                    gone.push(k.clone());
                }
                keep
            });
        }
    }
    for key in ["allOf", "anyOf", "oneOf", "prefixItems"] {
        if let Some(Value::Array(list)) = o.get_mut(key) {
            list.retain_mut(|c| !strip_secrets(c, names, depth + 1));
        }
    }
    for key in ["items", "additionalProperties", "not", "contains"] {
        if let Some(c) = o.get_mut(key)
            && strip_secrets(c, names, depth + 1)
        {
            return true;
        }
    }
    if let Some(Value::Array(required)) = o.get_mut("required") {
        required.retain(|r| {
            r.as_str()
                .is_none_or(|r| !gone.iter().any(|g| g == r) && !names.contains(&r))
        });
    }
    false
}

/// The companion a source waits for, for the Sources screen's chip
/// (CMP-10); `None` when it needs none or has it.
fn companion_need(state: CompanionState) -> Option<CompanionNeed> {
    let (companion, state) = match state {
        CompanionState::Ok => return None,
        CompanionState::Needs(c) => (c, CompanionNeedState::Missing),
        CompanionState::Unhealthy(c) => (c, CompanionNeedState::Unhealthy),
    };
    Some(CompanionNeed {
        companion: companion.to_string(),
        state,
    })
}

fn source(v: SourceView, companion: CompanionState) -> Source {
    let m = &v.status.manifest;
    let state = match v.state {
        lodestar_store::SourceState::Enabled => SourceState::Enabled,
        lodestar_store::SourceState::Paused => SourceState::Paused,
        lodestar_store::SourceState::Disabled => SourceState::Disabled,
        lodestar_store::SourceState::Purged => SourceState::Purged,
    };
    let enabled = state == SourceState::Enabled;
    let policy = &v.status.policy;
    Source {
        agents: SourceAgents {
            count: v.settings.agents.unwrap_or(1),
            enforced: false,
            max: policy.max_agents.max(1),
        },
        config_schema: config_schema(policy.config_schema, policy.credentials),
        consent: consent(m.consent),
        consent_required: consent_rules::needs_ack(m.consent),
        consent_version: v.consent_version.clone(),
        credential_spec: credential_spec(policy.credentials),
        credentials: v.credentials.clone(),
        editable: editable(v.status.config_editable),
        egress: SourceEgress {
            live: v.status.egress.as_ref().map(egress_live),
            setting: v.settings.egress.as_ref().ok().map(egress_setting),
            warnings: v.egress_warnings.iter().map(|w| w.to_string()).collect(),
        },
        enabled,
        // Exactly what an acknowledgement covers.
        endpoints: consent_rules::addresses(m),
        always_adult: m.always_adult,
        finds_private: m.finds_private,
        health: SourceHealth {
            harvested_24h: v.harvested_24h,
            last_error: v.status.last_error.clone(),
            last_run: v.last_run,
            running: v.status.running,
            stored_24h: v.stored_24h,
        },
        id: v.status.id.get(),
        kinds: m
            .kinds
            .iter()
            .map(|k| match k {
                Kind::Discovery => SourceKind::Discovery,
                Kind::Importer => SourceKind::Importer,
                Kind::PeerFinder => SourceKind::PeerFinder,
                Kind::Enrichment => SourceKind::Enrichment,
                Kind::Network => SourceKind::Network,
                Kind::Output => SourceKind::Output,
            })
            .collect(),
        name: m.name.to_string(),
        networks: m
            .networks
            .iter()
            .map(|n| {
                match n {
                    Network::Bt => "bt",
                    Network::BtI2p => "bt_i2p",
                    Network::Ed2k => "ed2k",
                    Network::Ipfs => "ipfs",
                    Network::Usenet => "usenet",
                }
                .to_string()
            })
            .collect(),
        needs: v.need.map(|n| match n {
            Need::Endpoints => SourceNeed::EndpointsAck,
            Need::Credentials => SourceNeed::Credentials,
            Need::Consent => SourceNeed::Consent,
        }),
        companion_need: companion_need(companion),
        not_yet_editable: NOT_YET_EDITABLE.map(String::from).to_vec(),
        password_required: consent_rules::needs_password(m.consent),
        plugin: m.id.to_string(),
        purge_pending: v.purge_pending,
        priority: priority(v.priority),
        // On: it didn't run at this start, or it was stopped since (turned
        // off and on again). Off: only while it's still running, as a stop
        // that's already happened leaves nothing pending.
        restart_required: if enabled {
            !v.status.enabled || v.stopped
        } else {
            v.status.running
        } || v.config_pending,
        risk: match m.risk {
            Risk::Low => RiskLevel::Low,
            Risk::Medium => RiskLevel::Medium,
            Risk::High => RiskLevel::High,
        },
        schedule: SourceSchedule {
            default: windows(policy.schedule.0),
            enforced: false,
            windows: v
                .settings
                .schedule
                .as_deref()
                .map_or_else(|| windows(policy.schedule.0), windows),
        },
        state,
        version: v.version,
    }
}

fn source_error(e: SourceError) -> OpsError {
    match e {
        SourceError::Unknown => OpsError::UnknownSource,
        SourceError::ConsentRequired { class, version } => OpsError::ConsentRequired {
            class: consent(class),
            version,
        },
        SourceError::PasswordRequired => OpsError::PasswordRequired,
        SourceError::CredentialsRequired => OpsError::CredentialsRequired,
        SourceError::Credential(r) => OpsError::BadCredential(r.to_string()),
        SourceError::Sealing => OpsError::Internal("the credential key couldn't be read".into()),
        // One code per field, whichever layer refused it.
        SourceError::Invalid { field, reason } => OpsError::Invalid {
            code: match field.split('.').next() {
                Some("agents") => "invalid_agents",
                Some("schedule") => "invalid_schedule",
                Some("egress") => "invalid_egress",
                Some("config") => "invalid_config",
                _ => "invalid_setting",
            },
            field,
            message: format!("The setting wasn't saved: {reason}."),
        },
        SourceError::Store(e) => internal(e),
    }
}

fn internal(e: impl std::fmt::Display) -> OpsError {
    OpsError::Internal(e.to_string())
}

impl OpsApi for Ops {
    fn crawler(&self) -> CrawlerRuntime {
        let s = self.control.status();
        CrawlerRuntime {
            since: s.since,
            dht: s.dht.map(|d| DhtStatus {
                bootstrapped: d.bootstrapped,
                outstanding: d.outstanding,
                routing_nodes: d.routing_nodes,
            }),
            fetch: FetchStatus {
                in_flight: s.fetching,
                lookups: s.lookups,
                queued: s.queued,
            },
        }
    }

    fn sync_crawler(&self) -> BoxFuture<'_, ()> {
        Box::pin(async move {
            self.control
                .sync(|| {
                    let c = self.crawler.borrow();
                    (c.paused, c.intensity.into())
                })
                .await;
        })
    }

    fn stats(&self) -> BoxFuture<'_, StatsResponse> {
        Box::pin(async move {
            let data_bytes = tokio::task::spawn_blocking(self.data_bytes())
                .await
                .ok()
                .flatten();
            let index_lag_s = match &self.search {
                Some(s) => {
                    let control = self.control.clone();
                    let max_id = tokio::task::spawn_blocking(move || control.max_torrent_id())
                        .await
                        .ok()
                        .and_then(Result::ok);
                    max_id.and_then(|max_id| {
                        index_lag(
                            crate::unix_now().max(0) as u64,
                            max_id,
                            s.stats.watermark.load(Ordering::Relaxed),
                            s.stats.last_commit.load(Ordering::Relaxed),
                        )
                    })
                }
                None => None,
            };
            StatsResponse {
                data_bytes,
                index_lag_s,
                ..self.stats_now()
            }
        })
    }

    fn sources(&self) -> BoxFuture<'_, Result<Vec<Source>, OpsError>> {
        Box::pin(async move {
            let control = self.control.clone();
            let views = tokio::task::spawn_blocking(move || control.sources())
                .await
                .map_err(internal)?
                .map_err(internal)?;
            Ok(views.into_iter().map(|v| self.source(v)).collect())
        })
    }

    fn patch_source(
        &self,
        id: u8,
        patch: SourcePatch,
        password_confirmed: bool,
    ) -> BoxFuture<'_, Result<Source, OpsError>> {
        Box::pin(async move {
            let id = SourceId::new(id).ok_or(OpsError::UnknownSource)?;
            let change = SourceChange {
                enabled: patch.enabled,
                priority: patch.priority.map(priority_back),
                consent: patch.consent.map(consent_back),
                consent_version: patch.consent_version,
                password_confirmed,
                egress: patch.egress.map(egress_back).transpose()?,
                schedule: patch
                    .schedule
                    .map(|w| {
                        w.iter()
                            .map(|t| parse_window(t).map(|(a, b)| Window::new(a, b)))
                            .collect::<Option<Vec<_>>>()
                            .ok_or(OpsError::Invalid {
                                code: "invalid_schedule",
                                field: "schedule",
                                message: "Give each window as HH:MM-HH:MM.".into(),
                            })
                    })
                    .transpose()?,
                agents: patch.agents,
                config: patch.config.map(|v| v.to_string()),
            };
            self.control
                .update_source(id, change)
                .await
                .map(|v| self.source(v))
                .map_err(source_error)
        })
    }

    fn patch_source_credentials(
        &self,
        id: u8,
        changes: Vec<(String, Option<Secret>)>,
    ) -> BoxFuture<'_, Result<Source, OpsError>> {
        Box::pin(async move {
            let id = SourceId::new(id).ok_or(OpsError::UnknownSource)?;
            self.control
                .update_credentials(id, &self.credentials, changes)
                .await
                .map(|v| self.source(v))
                .map_err(source_error)
        })
    }

    fn begin_purge(
        &self,
        id: u8,
        password_confirmed: bool,
    ) -> BoxFuture<'_, Result<(String, u64), OpsError>> {
        Box::pin(async move {
            let source = SourceId::new(id).ok_or(OpsError::UnknownSource)?;
            let keys = self
                .control
                .begin_purge(source, password_confirmed)
                .await
                .map_err(source_error)?;
            let plugin = self
                .control
                .sources()
                .map_err(internal)?
                .into_iter()
                .find(|v| v.status.id == source)
                .map(|v| v.status.manifest.id.to_string())
                .ok_or(OpsError::UnknownSource)?;
            Ok((plugin, keys))
        })
    }

    fn purge_step(
        &self,
        id: u8,
        from: lodestar_store::unverified::PurgeCursor,
    ) -> BoxFuture<'_, Result<lodestar_store::unverified::PurgeStep, OpsError>> {
        Box::pin(async move {
            let source = SourceId::new(id).ok_or(OpsError::UnknownSource)?;
            self.control
                .purge_step(source, from)
                .await
                .map_err(source_error)
        })
    }

    /// Counted on a read-only connection of its own when the store is a
    /// file, so the scan never holds the shared store's lock (the Sources
    /// routes, the credential lookups); in memory (tests), on the shared
    /// one.
    fn purge_preview(&self, id: u8) -> BoxFuture<'_, Result<PurgePreview, OpsError>> {
        Box::pin(async move {
            let source = SourceId::new(id).ok_or(OpsError::UnknownSource)?;
            let control = self.control.clone();
            let view = tokio::task::spawn_blocking(move || control.sources())
                .await
                .map_err(internal)?
                .map_err(internal)?
                .into_iter()
                .find(|v| v.status.id == source)
                .ok_or(OpsError::UnknownSource)?;
            let shared = self.control.store();
            let counts = tokio::task::spawn_blocking(move || {
                let reopen = shared.lock().expect("store lock").reopen_args();
                match reopen {
                    Some((path, config)) => {
                        lodestar_store::Store::open_reader(&path, config)?.purge_preview(source)
                    }
                    None => shared.lock().expect("store lock").purge_preview(source),
                }
            })
            .await
            .map_err(internal)?
            .map_err(|e| match e {
                lodestar_store::Error::NotFound(_) => OpsError::UnknownSource,
                e => internal(e),
            })?;
            Ok(PurgePreview {
                claims_deleted: counts.claims_deleted,
                kept: counts.torrents_kept,
                plugin: view.status.manifest.id.to_string(),
                queue_deleted: counts.queue_deleted,
                // Off and stopped: nothing it finds can add to the counts.
                // Paused isn't off: its pending keys are still fetched,
                // and a purge refuses it.
                settled: matches!(
                    view.state,
                    lodestar_store::SourceState::Disabled | lodestar_store::SourceState::Purged
                ) && !view.status.running,
                source_id: id,
                state_deleted: counts.state_deleted,
                torrents_deleted: counts.torrents_deleted,
                unverified_deleted: counts.unverified_deleted,
                unverified_kept: counts.unverified_kept,
            })
        })
    }

    fn finish_purge(&self, id: u8) -> BoxFuture<'_, Result<(u64, u64), OpsError>> {
        Box::pin(async move {
            let source = SourceId::new(id).ok_or(OpsError::UnknownSource)?;
            self.control
                .finish_purge(source)
                .await
                .map_err(source_error)
        })
    }

    fn network(&self) -> NetworkCheck {
        network(self.control.network())
    }

    fn restart_network_check(&self) -> Result<NetworkCheck, OpsError> {
        self.control
            .restart_network_check()
            .map(network)
            .map_err(|retry_after_secs| OpsError::RateLimited { retry_after_secs })
    }
}

fn network(n: control::NetworkCheck) -> NetworkCheck {
    let r = n.reach;
    NetworkCheck {
        checked_at: crate::unix_now(),
        external: ExternalAddress {
            agreeing: n.external.agreeing as u32,
            ip: n.external.ip.map(|ip| ip.to_string()),
            port: n.external.port,
            voters: n.external.voters as u32,
        },
        inbound_queries: r.inbound,
        mapping: PortMapping::Unavailable,
        measured_s: r.measured_secs,
        min_window_s: REACH_MIN_SECS,
        reachable: r.reachable,
        reason: match r.reason {
            control::ReachReason::DhtOff => ReachReason::DhtOff,
            control::ReachReason::Bootstrapping => ReachReason::Bootstrapping,
            control::ReachReason::Measuring => ReachReason::Measuring,
            control::ReachReason::UnsolicitedQueries => ReachReason::UnsolicitedQueries,
            control::ReachReason::FewUnsolicitedSources => ReachReason::FewUnsolicitedSources,
            control::ReachReason::NoUnsolicitedQueries => ReachReason::NoUnsolicitedQueries,
        },
        unsolicited_queries: r.unsolicited,
        unsolicited_sources: r.sources,
        window_s: REACH_WINDOW_SECS,
        window_started_at: n.window_started_at,
    }
}

/// Where the first-run log tells you to open the UI: the bind address if
/// it names one, else "this host's address" (a wildcard bind isn't an
/// address a browser can open).
pub fn ui_address(bind: std::net::SocketAddr) -> String {
    if bind.ip().is_unspecified() {
        format!("http://<this host's address>:{}/", bind.port())
    } else {
        format!("http://{bind}/")
    }
}

#[cfg(test)]
mod tests;

/// `Source.companion_need` from the pipeline's state, and its wire shape
/// (CMP-10): what CMP-9b's chip reads.
#[cfg(test)]
mod companion_need_tests {
    use super::*;

    #[test]
    fn each_state_maps_to_its_field_and_json() {
        assert_eq!(companion_need(CompanionState::Ok), None);
        let need = |s| serde_json::to_value(companion_need(s)).unwrap();
        assert_eq!(
            need(CompanionState::Needs("prowlarr")),
            serde_json::json!({"companion": "prowlarr", "state": "missing"})
        );
        assert_eq!(
            need(CompanionState::Unhealthy("qbittorrent")),
            serde_json::json!({"companion": "qbittorrent", "state": "unhealthy"})
        );
        assert_eq!(need(CompanionState::Ok), serde_json::Value::Null);
        let back: CompanionNeed =
            serde_json::from_str(r#"{"companion":"prowlarr","state":"unhealthy"}"#).unwrap();
        assert_eq!(back.state, CompanionNeedState::Unhealthy);
    }
}

/// What `GET /sources` lists of each plugin's declared credentials and
/// settings schema (X-sources-api-spec), over every plugin in the build.
#[cfg(test)]
mod spec_tests {
    use std::sync::Arc;

    use lodestar_sources::BoxFuture;
    use lodestar_sources::enrich::AdultEnricher;
    use lodestar_sources::{Policy, SourcePlugin};
    use lodestar_sources_plugins::full_scrape::{LookupError, PrivateTrackers, SystemClock};
    use lodestar_sources_plugins::{
        StaticCredentials, academic, archive_org, arr, catalogue, clients, full_scrape, nyaa,
        prowlarr, stashdb, torrent_cache, tpdb,
    };
    use serde_json::{Value, json};

    use lodestar_sources::consent::{CredentialKind, CredentialSpec};

    use super::{config_schema, credential_spec};

    /// Every host private: the full scrape never runs here, only its
    /// policy is read.
    struct AllPrivate;

    impl PrivateTrackers for AllPrivate {
        fn is_private<'a>(&'a self, _: &'a str) -> BoxFuture<'a, Result<bool, LookupError>> {
            Box::pin(async { Ok(true) })
        }
    }

    fn policies() -> Vec<(&'static str, Policy)> {
        let creds = || Arc::new(StaticCredentials::default());
        let (cache, _queue, _verified) =
            torrent_cache::TorrentCache::new(torrent_cache::Config::new::<&str>(&[]).unwrap());
        let scrape = full_scrape::FullScrape::unconfigured(
            Arc::new(AllPrivate),
            Arc::new(SystemClock::default()),
        );
        vec![
            (academic::ID, *academic::Academic::new().policy()),
            (archive_org::ID, *archive_org::ArchiveOrg::new().policy()),
            (arr::ID, *arr::ArrWebhooks::new().policy()),
            (clients::ID, *clients::Clients::unconfigured().policy()),
            (full_scrape::ID, *scrape.policy()),
            (nyaa::ID, *nyaa::Nyaa::new().policy()),
            (nyaa::SUKEBEI_ID, *nyaa::Sukebei::new().policy()),
            (
                prowlarr::ID,
                *prowlarr::Prowlarr::unconfigured(creds()).policy(),
            ),
            (
                stashdb::ID,
                *AdultEnricher::policy(&stashdb::StashDb::new(creds())),
            ),
            (torrent_cache::ID, *cache.policy()),
            (tpdb::ID, *AdultEnricher::policy(&tpdb::Tpdb::new(creds()))),
        ]
    }

    #[test]
    fn every_plugin_is_listed_here() {
        let mut here: Vec<&str> = policies().into_iter().map(|(id, _)| id).collect();
        let mut all: Vec<&str> = catalogue::manifests().map(|m| m.id).collect();
        here.sort_unstable();
        all.sort_unstable();
        assert_eq!(here, all);
    }

    /// Each schema is listed as an object schema with no secret in it: a
    /// write-only or credential-named property is left out, and so is its
    /// `required` entry. Everything else is the plugin's own schema.
    #[test]
    fn each_schema_is_listed_without_its_secrets() {
        for (id, p) in policies() {
            let own: Value = serde_json::from_str(p.config_schema).unwrap();
            let listed = config_schema(p.config_schema, p.credentials);
            assert_eq!(listed["type"], "object", "{id}");
            let props = listed["properties"].as_object().unwrap();
            for (k, v) in props {
                assert_ne!(v["writeOnly"], true, "{id}: {k}");
                assert!(!p.credentials.iter().any(|c| c.name == k), "{id}: {k}");
                assert_eq!(v, &own["properties"][k], "{id}: {k} as declared");
            }
            for r in listed["required"].as_array().into_iter().flatten() {
                assert!(props.contains_key(r.as_str().unwrap()), "{id}: {r}");
            }
            for key in ["$schema", "type", "additionalProperties"] {
                assert_eq!(listed[key], own[key], "{id}: {key}");
            }
        }
    }

    #[test]
    fn the_api_key_plugins_list_it_as_a_credential() {
        for (id, p) in policies() {
            let listed = serde_json::to_value(credential_spec(p.credentials)).unwrap();
            let want = if [prowlarr::ID, stashdb::ID, tpdb::ID].contains(&id) {
                json!([{ "kind": "secret", "label": "API key", "name": "api_key", "required": true }])
            } else {
                json!([])
            };
            assert_eq!(listed, want, "{id}");
            // No settings form asks for it.
            let schema = config_schema(p.config_schema, p.credentials);
            assert!(schema["properties"].get("api_key").is_none(), "{id}");
            let own: Value = serde_json::from_str(p.config_schema).unwrap();
            assert!(own["properties"].get("api_key").is_none(), "{id}: its own");
        }
        // Prowlarr keeps its address and manager.
        let p = *prowlarr::Prowlarr::unconfigured(Arc::new(StaticCredentials::default())).policy();
        let schema = config_schema(p.config_schema, p.credentials);
        assert_eq!(schema["required"], json!(["url", "manager"]));
    }

    /// The second guard behind the plugins' own test: a secret a schema
    /// lists, marked write-only or named as a credential, never reaches
    /// the form.
    #[test]
    fn a_secret_in_a_schema_is_left_out() {
        let schema = r#"{"type":"object","properties":{
            "url":{"type":"string"},
            "api_key":{"type":"string"},
            "token":{"type":"string","writeOnly":true},
            "note":{"type":"string","writeOnly":false}
        },"required":["url","api_key","token"],"additionalProperties":false}"#;
        let spec = [CredentialSpec {
            name: "api_key",
            label: "API key",
            kind: CredentialKind::Secret,
            required: true,
        }];
        let listed = config_schema(schema, &spec);
        assert_eq!(
            listed,
            json!({
                "type": "object",
                "properties": { "url": { "type": "string" },
                                "note": { "type": "string", "writeOnly": false } },
                "required": ["url"],
                "additionalProperties": false
            })
        );
        // Without a spec, a write-only field still goes.
        let listed = config_schema(schema, &[]);
        assert!(listed["properties"].get("token").is_none());
        assert!(listed["properties"].get("api_key").is_some());
    }

    /// A write-only field nested anywhere goes too: inside an array's
    /// `items`, an object's `properties`, an `allOf` branch, or behind
    /// `additionalProperties`. What holds no secret stays as it was.
    #[test]
    fn a_nested_secret_is_left_out() {
        let schema = r#"{"type":"object","properties":{
            "caches":{"type":"array","items":{"type":"object","properties":{
                "url":{"type":"string"},
                "token":{"type":"string","writeOnly":true}
            },"required":["url","token"]}},
            "list":{"type":"array","items":{"type":"string","writeOnly":true}},
            "extra":{"type":"object","additionalProperties":{"type":"string","writeOnly":true}},
            "either":{"anyOf":[{"type":"string"},{"type":"string","writeOnly":true}]},
            "plain":{"type":"string"}
        },"allOf":[{"properties":{"api_key":{"type":"string"}}}],
        "required":["caches","list","plain"],"additionalProperties":false}"#;
        let spec = [CredentialSpec {
            name: "api_key",
            label: "API key",
            kind: CredentialKind::Secret,
            required: true,
        }];
        let listed = config_schema(schema, &spec);
        let props = &listed["properties"];
        let item = &props["caches"]["items"];
        assert_eq!(item["properties"], json!({ "url": { "type": "string" } }));
        assert_eq!(item["required"], json!(["url"]));
        // A property whose values are secrets goes, and so does its
        // `required` entry.
        assert!(props.get("list").is_none(), "{listed}");
        assert!(props.get("extra").is_none(), "{listed}");
        assert_eq!(props["either"]["anyOf"], json!([{ "type": "string" }]));
        assert_eq!(props["plain"], json!({ "type": "string" }));
        assert_eq!(listed["allOf"], json!([{ "properties": {} }]));
        assert_eq!(listed["required"], json!(["caches", "plain"]));
        assert!(!listed.to_string().contains("writeOnly"), "{listed}");
    }

    /// Past the depth bound, a secret is assumed: the form gets no fields.
    #[test]
    fn a_schema_too_deep_to_check_reads_as_empty() {
        let mut deep = String::from(r#"{"type":"string"}"#);
        for _ in 0..40 {
            deep = format!(r#"{{"type":"array","items":{deep}}}"#);
        }
        let schema = format!(r#"{{"type":"object","properties":{{"deep":{deep}}}}}"#);
        let listed = config_schema(&schema, &[]);
        assert!(listed["properties"].get("deep").is_none(), "{listed}");
    }

    #[test]
    fn a_broken_schema_reads_as_the_empty_one() {
        assert_eq!(
            config_schema("{not json", &[]),
            serde_json::from_str::<Value>(lodestar_sources::EMPTY_SCHEMA).unwrap()
        );
    }
}
