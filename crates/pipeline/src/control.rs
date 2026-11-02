//! Process control (architecture §9 `crawler` and `sources`, §10.9's
//! global bar, §10.10's network check): the crawler's pause, the events
//! Insights annotates, live source priorities and stored states, and the
//! sampled counters behind `/stats` rates and the reachability check.
//!
//! - **Pause** stops harvesting and metadata fetching: the DHT node stops
//!   sampling, lookups, scrapes and harvests but keeps answering other
//!   nodes (`NodeHandle::set_paused`), and the fetch pool starts no
//!   fetches or lookups (`PoolHandle::set_paused`). What's in flight
//!   finishes or times out; the writer and indexer go on. The paused
//!   state itself is the settings registry's (`crawler.paused`): after
//!   each change the binary calls [`Control::sync`], which applies the
//!   registry's value as it is then.
//! - **Events:** every pause, resume and intensity change is an `events`
//!   row ([`crate::event`]), written through the control connection.
//! - **Rates and reachability** come from one sampler task reading the
//!   same counters `/metrics` shows, every [`SAMPLE_EVERY`]; nothing is
//!   counted twice.
//! - **Reachable** means queries are arriving, from at least
//!   [`MIN_UNSOLICITED_SOURCES`] networks, from nodes we hadn't sent
//!   anything to lately (`Stats::queries_in_unsolicited`), which only a
//!   port open to unsolicited inbound packets allows. Every probe is the
//!   DHT's own traffic (§18.7); no outside service is asked.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use lodestar_dht::node::{ExternalVotes, Intensity, StatsSnapshot};
use lodestar_sources::consent::{self, Ack, CredentialRefusal, Need, Secret, SourceCredentials};
use lodestar_sources::{Consent, Priority, ProxySpec, SocksDns, SourcePlugin, Stage, Window};
use lodestar_store::{Event, SourceId, SourceSetting, SourceState, Store, WriteBatch};
use tokio::time::Instant;

use crate::{DhtSource, Error, Monitor, SourceStatus, event, unix_now};

/// Where a source's priority saved in the app lives in the `settings`
/// table: this, then the plugin's manifest id. The value is JSON text.
pub const PRIORITY_KEY: &str = "sources.priority.";
/// How often the counters are sampled.
pub const SAMPLE_EVERY: Duration = Duration::from_secs(10);
/// Rates are measured over the last this many seconds...
pub const RATE_WINDOW_SECS: u64 = 300;
/// ...once at least this many seconds have been sampled.
pub const RATE_MIN_SECS: u64 = 60;
/// Reachability looks at the last this many seconds of evidence...
pub const REACH_WINDOW_SECS: u64 = 600;
/// ...and says nothing until the window has run this long after
/// bootstrap (or a restart of the check).
pub const REACH_MIN_SECS: u64 = 120;
/// Distinct networks (/24 or /64) that must reach us unsolicited in the
/// window before we call the port open: one stray query (a neighbour
/// whose NAT mapping we can't see, a spoofed packet) isn't enough.
pub const MIN_UNSOLICITED_SOURCES: u64 = 3;
/// The check can be restarted at most this often.
pub const RESTART_EVERY_SECS: u64 = 30;
/// Samples kept: the longest window, plus one.
const KEEP: usize = (REACH_WINDOW_SECS / 10 + 2) as usize;

/// The priority saved in the app for `plugin`, if any (an unreadable
/// value is ignored with a warning).
pub fn saved_priority(store: &Store, plugin: &'static str) -> Result<Option<Priority>, Error> {
    let Some(text) = store.setting_text(&format!("{PRIORITY_KEY}{plugin}"))? else {
        return Ok(None);
    };
    let parsed = serde_json_str(&text).and_then(parse_priority);
    if parsed.is_none() {
        lodestar_log::log_event!(crate::log_events::PRIORITY_UNREADABLE, plugin = plugin);
    }
    Ok(parsed)
}

/// A source's egress as the user set it (`sources.egress.<plugin>`). The
/// network-interface and bind-address routes aren't offered: their DNS
/// lookups would leave outside them (SRC-5, until a bound resolver exists).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum EgressSetting {
    #[default]
    Direct,
    Socks5(ProxySpec),
}

impl EgressSetting {
    /// `direct`, or `socks5 <host> <port> <auto|proxy|local>`.
    pub fn encode(&self) -> String {
        match self {
            EgressSetting::Direct => "direct".into(),
            EgressSetting::Socks5(p) => {
                let dns = match p.dns {
                    None => "auto",
                    Some(SocksDns::Proxy) => "proxy",
                    Some(SocksDns::Local) => "local",
                };
                format!("socks5 {} {} {dns}", p.host, p.port)
            }
        }
    }

    pub fn decode(text: &str) -> Option<EgressSetting> {
        let mut parts = text.split(' ');
        let setting = match parts.next()? {
            "direct" => EgressSetting::Direct,
            "socks5" => {
                let host = parts.next()?;
                let port: u16 = parts.next()?.parse().ok().filter(|p| *p != 0)?;
                let dns = match parts.next()? {
                    "auto" => None,
                    "proxy" => Some(SocksDns::Proxy),
                    "local" => Some(SocksDns::Local),
                    _ => return None,
                };
                if !proxy_host_ok(host) {
                    return None;
                }
                EgressSetting::Socks5(ProxySpec {
                    host: host.to_owned(),
                    port,
                    dns,
                })
            }
            _ => return None,
        };
        parts.next().is_none().then_some(setting)
    }
}

/// A proxy host as the API passes it (`lodestar_api::ops::parse_proxy`'s
/// rules): an IP literal (no brackets) a proxy can be at
/// ([`lodestar_sources::egress::usable_proxy`]), or a DNS name of 1–63-byte labels
/// of letters, digits and hyphens (not at either end), at most 253 bytes,
/// whose last label isn't a number in disguise.
pub fn proxy_host_ok(host: &str) -> bool {
    if let Ok(ip) = host.parse::<std::net::IpAddr>() {
        return lodestar_sources::egress::usable_proxy(ip);
    }
    !host.is_empty()
        && host.len() <= 253
        && host.split('.').all(|l| {
            !l.is_empty()
                && l.len() <= 63
                && !l.starts_with('-')
                && !l.ends_with('-')
                && l.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
        })
        && !host.split('.').next_back().is_some_and(|l| {
            l.bytes().all(|b| b.is_ascii_digit()) || l.to_ascii_lowercase().starts_with("0x")
        })
}

/// Most windows a source's schedule holds.
pub const MAX_WINDOWS: usize = 8;

/// Windows as `start-end` minutes, comma-separated (`""`: any time).
pub fn encode_schedule(windows: &[Window]) -> String {
    windows
        .iter()
        .map(|w| format!("{}-{}", w.start, w.end))
        .collect::<Vec<_>>()
        .join(",")
}

pub fn decode_schedule(text: &str) -> Option<Vec<Window>> {
    if text.is_empty() {
        return Some(Vec::new());
    }
    let windows = text
        .split(',')
        .map(|w| {
            let (a, b) = w.split_once('-')?;
            let (a, b): (u16, u16) = (a.parse().ok()?, b.parse().ok()?);
            let day = 24 * 60;
            (a < day && b < day).then(|| Window::new(a, b))
        })
        .collect::<Option<Vec<_>>>()?;
    (windows.len() <= MAX_WINDOWS).then_some(windows)
}

/// A saved egress setting that doesn't decode (a rollback from a version
/// with a route this one can't read, a stricter parser, a damaged row).
/// It's never read as direct: the source's route stays down (fail closed,
/// runner #394 review).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Unreadable;

/// A source's saved egress: what's set (absent is direct), or unreadable.
pub type SavedEgress = Result<EgressSetting, Unreadable>;

/// A source's own settings beyond its state and priority (§18.4). The
/// schedule and agents are stored and reported only: scheduler v2
/// enforces them (runner, SRC-14a). An unreadable schedule or agents
/// value falls back to its default; an unreadable egress doesn't.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceSettings {
    pub egress: SavedEgress,
    /// `None`: the plugin's default windows.
    pub schedule: Option<Vec<Window>>,
    /// `None`: one agent.
    pub agents: Option<u32>,
}

impl Default for SourceSettings {
    fn default() -> SourceSettings {
        SourceSettings {
            egress: Ok(EgressSetting::Direct),
            schedule: None,
            agents: None,
        }
    }
}

/// `plugin`'s saved settings. A schedule or agents value that isn't
/// readable keeps its default; an egress that isn't is [`Unreadable`].
/// Each is logged when `warn` (once, at start: `GET /sources` reads them
/// on every call).
pub fn saved_settings(
    store: &Store,
    plugin: &'static str,
    warn: bool,
) -> Result<SourceSettings, Error> {
    let read =
        |setting| -> Result<Option<String>, Error> { Ok(store.source_setting(plugin, setting)?) };
    let decoded = |text: Option<String>, label: &'static str, ok: bool| {
        if text.is_some() && !ok && warn {
            lodestar_log::log_event!(
                crate::log_events::SOURCE_SETTING_UNREADABLE,
                plugin = plugin,
                setting = label
            );
        }
    };
    let egress = read(SourceSetting::Egress)?;
    let saved_egress_absent = egress.is_none();
    let egress_value = egress.as_deref().and_then(EgressSetting::decode);
    decoded(egress, "egress", egress_value.is_some());
    let schedule = read(SourceSetting::Schedule)?;
    let schedule_value = schedule.as_deref().and_then(decode_schedule);
    decoded(schedule, "schedule", schedule_value.is_some());
    let agents = read(SourceSetting::Agents)?;
    let agents_value = agents
        .as_deref()
        .and_then(|t| t.parse::<u32>().ok().filter(|n| *n >= 1));
    decoded(agents, "agents", agents_value.is_some());
    Ok(SourceSettings {
        egress: match (egress_value, saved_egress_absent) {
            (Some(e), _) => Ok(e),
            (None, true) => Ok(EgressSetting::Direct),
            (None, false) => Err(Unreadable),
        },
        schedule: schedule_value,
        agents: agents_value,
    })
}

/// `"high"` → `high`: the JSON string the value is saved as.
fn serde_json_str(text: &str) -> Option<&str> {
    text.strip_prefix('"')?.strip_suffix('"')
}

pub fn priority_name(p: Priority) -> &'static str {
    match p {
        Priority::Low => "low",
        Priority::Normal => "normal",
        Priority::High => "high",
        Priority::Max => "max",
    }
}

pub fn parse_priority(name: &str) -> Option<Priority> {
    Some(match name {
        "low" => Priority::Low,
        "normal" => Priority::Normal,
        "high" => Priority::High,
        "max" => Priority::Max,
        _ => return None,
    })
}

pub fn intensity_name(i: Intensity) -> &'static str {
    match i {
        Intensity::Low => "low",
        Intensity::Balanced => "balanced",
        Intensity::Aggressive => "aggressive",
    }
}

/// One sample of the counters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Sample {
    /// Seconds since the control started (monotonic).
    t: u64,
    /// Keys the sources reported (`intake_received_total`).
    received: u64,
    /// Info dicts fetched (`fetch_fetched_total`).
    fetched: u64,
    dht: Option<DhtSample>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DhtSample {
    queries_in: u64,
    unsolicited: u64,
    routing_nodes: u64,
}

impl From<&StatsSnapshot> for DhtSample {
    fn from(s: &StatsSnapshot) -> DhtSample {
        DhtSample {
            queries_in: s.queries_in,
            unsolicited: s.queries_in_unsolicited,
            routing_nodes: s.routing_nodes,
        }
    }
}

#[derive(Debug, Default)]
struct Samples {
    ring: VecDeque<Sample>,
    /// When the reachability window started (a restart of the check).
    window_from: u64,
    /// The first sample with a routing table, since `window_from`.
    bootstrapped_at: Option<u64>,
    last_restart: Option<u64>,
    external: ExternalVotes,
    /// When (sample time) each recent network last reached us
    /// unsolicited, from the node's snapshot.
    unsolicited_at: Vec<u64>,
    /// Rates look no further back than this (the last resume), so the
    /// paused stretch doesn't drag them down.
    rates_from: u64,
}

impl Samples {
    fn push(&mut self, s: Sample) {
        if self.bootstrapped_at.is_none() && s.dht.is_some_and(|d| d.routing_nodes > 0) {
            self.bootstrapped_at = Some(s.t);
        }
        self.ring.push_back(s);
        while self.ring.len() > KEEP {
            self.ring.pop_front();
        }
    }

    /// The change in `f` per hour over the last [`RATE_WINDOW_SECS`]
    /// (since the last resume, if later); `None` until [`RATE_MIN_SECS`]
    /// are covered.
    fn per_hour(&self, f: impl Fn(&Sample) -> u64) -> Option<f64> {
        let head = self.ring.back()?;
        let from = head.t.saturating_sub(RATE_WINDOW_SECS).max(self.rates_from);
        let base = self.ring.iter().find(|s| s.t >= from)?;
        let span = head.t - base.t;
        if span < RATE_MIN_SECS {
            return None;
        }
        Some(f(head).saturating_sub(f(base)) as f64 * 3600.0 / span as f64)
    }

    fn reachability(&self, dht_on: bool) -> Reachability {
        let unknown = |reason| Reachability {
            reachable: None,
            reason,
            inbound: None,
            unsolicited: None,
            sources: None,
            measured_secs: 0,
        };
        if !dht_on {
            return unknown(ReachReason::DhtOff);
        }
        let Some(start) = self.bootstrapped_at.map(|b| b.max(self.window_from)) else {
            return unknown(ReachReason::Bootstrapping);
        };
        let Some(head) = self.ring.back().filter(|h| h.dht.is_some()) else {
            return unknown(ReachReason::Bootstrapping);
        };
        let from = start.max(head.t.saturating_sub(REACH_WINDOW_SECS));
        let Some((base, b)) = self
            .ring
            .iter()
            .find_map(|s| s.dht.filter(|_| s.t >= from).map(|d| (s.t, d)))
        else {
            return unknown(ReachReason::Bootstrapping);
        };
        let h = head.dht.expect("filtered above");
        let inbound = h.queries_in.saturating_sub(b.queries_in);
        let unsolicited = h.unsolicited.saturating_sub(b.unsolicited);
        let measured_secs = head.t - base;
        let sources = self.unsolicited_at.iter().filter(|t| **t >= from).count() as u64;
        let (reachable, reason) = if head.t - start < REACH_MIN_SECS {
            (None, ReachReason::Measuring)
        } else if sources >= MIN_UNSOLICITED_SOURCES {
            (Some(true), ReachReason::UnsolicitedQueries)
        } else if sources > 0 {
            (Some(false), ReachReason::FewUnsolicitedSources)
        } else {
            (Some(false), ReachReason::NoUnsolicitedQueries)
        };
        Reachability {
            reachable,
            reason,
            inbound: Some(inbound),
            unsolicited: Some(unsolicited),
            sources: Some(sources),
            measured_secs,
        }
    }
}

/// Why reachability reads as it does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReachReason {
    /// The DHT is off, so there's nothing to measure.
    DhtOff,
    /// The node has no routing table yet.
    Bootstrapping,
    /// Not long enough since bootstrap or a restart of the check.
    Measuring,
    /// Nodes we never queried are reaching us, from at least
    /// [`MIN_UNSOLICITED_SOURCES`] networks.
    UnsolicitedQueries,
    /// Some did, but from too few networks to tell.
    FewUnsolicitedSources,
    /// None did in the window: the port looks closed to inbound packets.
    NoUnsolicitedQueries,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Reachability {
    /// `None` is unknown.
    pub reachable: Option<bool>,
    pub reason: ReachReason,
    /// Queries received in the window; `None` when not measuring.
    pub inbound: Option<u64>,
    /// Of those, from nodes we hadn't sent anything to lately.
    pub unsolicited: Option<u64>,
    /// Distinct networks (/24 or /64) those came from.
    pub sources: Option<u64>,
    /// Seconds the evidence covers.
    pub measured_secs: u64,
}

/// The network check: what other nodes see, and whether they reach us.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NetworkCheck {
    pub external: ExternalVotes,
    pub reach: Reachability,
    /// Unix seconds the current window started (bootstrap or restart).
    pub window_started_at: Option<i64>,
}

/// The crawler's state for `GET /crawler`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CrawlerStatus {
    pub paused: bool,
    /// Unix seconds of the last pause or resume (or the start).
    pub since: i64,
    /// `None` when the DHT is off or hasn't reported yet.
    pub dht: Option<DhtStatus>,
    /// Fetches running now.
    pub fetching: u64,
    /// Keys waiting for a fetch slot or for peers.
    pub queued: u64,
    /// Peer lookups out.
    pub lookups: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DhtStatus {
    pub bootstrapped: bool,
    pub routing_nodes: u64,
    pub outstanding: u64,
}

/// One source, for `GET /sources`.
#[derive(Debug, Clone, PartialEq)]
pub struct SourceView {
    pub status: SourceStatus,
    /// The stored state (§18.1); it decides whether the source runs at
    /// the next start.
    pub state: SourceState,
    pub version: String,
    /// Its priority now (live).
    pub priority: Priority,
    /// Keys it was first to report in the last 24 hours (discovery
    /// cohorts), and how many of those were stored.
    pub harvested_24h: i64,
    pub stored_24h: i64,
    /// When it last started (the pipeline's start), if it ran.
    pub last_run: Option<i64>,
    /// What enabling it still needs (§18.7); `None` when it's a toggle.
    pub need: Option<Need>,
    /// The version an acknowledgement must name ([`consent::version`]);
    /// empty for a class that's never acknowledged.
    pub consent_version: String,
    /// The names of its stored credentials. Never their values.
    pub credentials: Vec<String>,
    /// Its egress, schedule and agents as saved (they apply at the next
    /// start).
    pub settings: SourceSettings,
    /// Warnings about its egress, for the Sources screen.
    pub egress_warnings: Vec<&'static str>,
    /// A purge started and didn't finish (a crash, a cancel or an error):
    /// the source stays off until a purge run again finishes it.
    pub purge_pending: bool,
}

/// What a purge job has committed so far, for its progress (SRC-14b).
/// Each count is of rows already committed: a purge that stops partway
/// reports what it did, and the rest is done by running it again.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PurgeCounts {
    /// Keys the fetch pool dropped when the source stopped.
    pub keys_dropped: u64,
    pub unverified_deleted: u64,
    pub unverified_kept: u64,
    pub claims_deleted: u64,
    pub torrents_deleted: u64,
    pub torrents_kept: u64,
    /// Plugin state keys and persisted fetch-queue keys deleted.
    pub state_deleted: u64,
    /// Snapshots whose copy of the source's records was dropped.
    pub snapshots_scrubbed: u64,
    /// Snapshots that couldn't be read or written now; the hourly pass
    /// and any rollback scrub them later.
    pub snapshots_left: u64,
}

impl PurgeCounts {
    /// Adds one store step's counts; the number of rows it covered.
    pub fn add(&mut self, step: &lodestar_store::unverified::PurgeStep) -> u64 {
        let n = |v: usize| v as u64;
        self.unverified_deleted += n(step.unverified_deleted);
        self.unverified_kept += n(step.unverified_kept);
        self.claims_deleted += n(step.claims_deleted);
        self.torrents_deleted += n(step.torrents_deleted.len());
        self.torrents_kept += n(step.torrents_kept);
        self.state_deleted += n(step.state_deleted + step.queue_deleted);
        n(step.unverified_deleted
            + step.unverified_kept
            + step.claims_deleted
            + step.torrents_deleted.len()
            + step.torrents_kept
            + step.state_deleted
            + step.queue_deleted)
    }
}

/// `HTTPS_PROXY` or `ALL_PROXY` is set while a source connects directly.
pub const WARN_PROXY_ENV: &str = "HTTPS_PROXY or ALL_PROXY is set, but source plugins never use \
     it: this source connects directly. To send it through a proxy, choose SOCKS5.";
/// The proxy resolves names and is on the user's network: the trade, plainly
/// (runner, #394).
pub const WARN_LAN_PROXY_DNS: &str = "The proxy resolves names, so lookups stay inside the \
     tunnel. Because the proxy is on your network, a public name that resolves to one of your \
     devices would reach that device. Choosing Lodestar's DNS prevents that, but lookups then \
     leave outside the tunnel.";
/// The saved egress setting can't be read.
pub const WARN_SETTING_UNREADABLE: &str = "This source's saved network setting couldn't be \
     read, so its requests are refused rather than sent directly. Set its network route again.";

/// The warnings for a source's egress: `saved` as saved, `live` as it
/// runs, whether a proxy variable is set, and who resolves names on a
/// SOCKS5 route left on `auto` for this plugin ([`crate::egress::auto_dns`]).
/// The running route is judged only while it was built from what's saved:
/// once the user changes the setting, the warnings follow the new one.
pub fn egress_warnings(
    saved: &SavedEgress,
    live: Option<&crate::EgressLive>,
    proxy_env: bool,
    auto_dns: SocksDns,
) -> Vec<&'static str> {
    let mut out = Vec::new();
    let setting = match saved {
        Ok(s) => s,
        Err(Unreadable) => return vec![WARN_SETTING_UNREADABLE],
    };
    if proxy_env && *setting == EgressSetting::Direct {
        out.push(WARN_PROXY_ENV);
    }
    let current = live.filter(|l| l.setting == *saved);
    let lan_proxy_dns = match current.and_then(|l| l.route.as_ref()) {
        Some(lodestar_sources::Egress::Socks5 { proxy, dns }) => {
            *dns == SocksDns::Proxy && lodestar_sources::egress::on_lan(proxy.ip())
        }
        // Not running, or not on what's saved: judge the saved setting.
        // A name's address is known only once it has resolved.
        _ => match setting {
            EgressSetting::Socks5(ProxySpec { host, dns, .. }) => {
                dns.unwrap_or(auto_dns) == SocksDns::Proxy
                    && host.parse().is_ok_and(lodestar_sources::egress::on_lan)
            }
            EgressSetting::Direct => false,
        },
    };
    if lan_proxy_dns {
        out.push(WARN_LAN_PROXY_DNS);
    }
    out
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SourceChange {
    pub enabled: Option<bool>,
    pub priority: Option<Priority>,
    /// The consent class the user acknowledged, for enabling a source
    /// whose class needs it...
    pub consent: Option<Consent>,
    /// ...and the version of what they were shown ([`consent::version`]).
    pub consent_version: Option<String>,
    /// The caller checked the admin password for this change. Accepting
    /// Restricted or Overlay consent needs it (Q13), and so does any
    /// egress change.
    pub password_confirmed: bool,
    pub egress: Option<EgressSetting>,
    /// `Some(windows)` sets them (`[]`: any time).
    pub schedule: Option<Vec<Window>>,
    pub agents: Option<u32>,
}

#[derive(Debug)]
pub enum SourceError {
    Unknown,
    /// Enabling needs an acknowledgement of this class, at this version
    /// (a missing, wrong or stale one).
    ConsentRequired {
        class: Consent,
        version: String,
    },
    /// Accepting this class's consent needs the admin password.
    PasswordRequired,
    /// Enabling needs the source's credentials stored first.
    CredentialsRequired,
    /// A credential change was refused.
    Credential(CredentialRefusal),
    /// The credentials couldn't be sealed (the key file couldn't be
    /// read or created). Carries nothing secret.
    Sealing,
    /// A setting was refused: `field` names it, `reason` says why.
    Invalid {
        field: &'static str,
        reason: String,
    },
    Store(Error),
}

impl From<Error> for SourceError {
    fn from(e: Error) -> SourceError {
        SourceError::Store(e)
    }
}

impl From<lodestar_store::Error> for SourceError {
    fn from(e: lodestar_store::Error) -> SourceError {
        SourceError::Store(Error::Store(e))
    }
}

/// One credential change: a value to store, or `None` to remove it.
pub type CredentialChange = (String, Option<Secret>);

#[derive(Debug)]
struct State {
    paused: bool,
    since: i64,
    /// The last intensity recorded, so a repeat writes no event.
    intensity: Intensity,
    priorities: HashMap<SourceId, Priority>,
}

struct Inner {
    monitor: Monitor,
    dht: Option<Arc<DhtSource>>,
    store: Arc<Mutex<Store>>,
    epoch: Instant,
    /// Unix seconds at `epoch`.
    epoch_unix: i64,
    /// Serializes pause changes, so each is applied and recorded once.
    toggle: tokio::sync::Mutex<()>,
    state: Mutex<State>,
    samples: Mutex<Samples>,
}

/// A cloneable handle on the running pipeline's controls.
#[derive(Clone)]
pub struct Control {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for Control {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Control").finish_non_exhaustive()
    }
}

impl Control {
    /// Takes over a started pipeline, whose pool and DHT already have
    /// `paused` (`PoolConfig::paused`, the node's `Config::paused`), and
    /// starts the sampler. `store` is a connection for events and source
    /// settings, apart from the writer's.
    pub async fn start(
        monitor: Monitor,
        dht: Option<Arc<DhtSource>>,
        store: Arc<Mutex<Store>>,
        paused: bool,
        intensity: Intensity,
    ) -> Result<Control, Error> {
        // A DHT whose stored state kept it from starting is off.
        let dht = dht.filter(|d| {
            let id = d.manifest().id;
            monitor
                .sources()
                .iter()
                .any(|s| s.plugin == id && s.enabled)
        });
        // A pause that outlived a restart dates from the pause itself.
        let started_at = monitor.started_at();
        let since = {
            let store = store.clone();
            tokio::task::spawn_blocking(move || {
                let last = store
                    .lock()
                    .expect("store lock")
                    .last_event(&[event::PAUSE, event::RESUME])?;
                Ok::<_, Error>(
                    last.filter(|e| paused && e.kind == event::PAUSE)
                        .map_or(started_at, |e| e.at),
                )
            })
            .await
            .map_err(Error::Join)??
        };
        let priorities = monitor
            .sources()
            .iter()
            .map(|s| (s.id, s.priority))
            .collect();
        let control = Control {
            inner: Arc::new(Inner {
                monitor,
                dht,
                store,
                epoch: Instant::now(),
                epoch_unix: unix_now(),
                toggle: tokio::sync::Mutex::new(()),
                state: Mutex::new(State {
                    paused,
                    since,
                    intensity,
                    priorities,
                }),
                samples: Mutex::default(),
            }),
        };
        control.sample_now();
        tokio::spawn(sampler(Arc::downgrade(&control.inner)));
        Ok(control)
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.inner.state.lock().expect("control state lock")
    }

    fn now_t(&self) -> u64 {
        self.inner.epoch.elapsed().as_secs()
    }

    /// Applies the crawler settings as they are *now*: `current` is read
    /// under the same lock every change goes through, so after any number
    /// of concurrent saves the last one to finish applies the newest
    /// value, and the process never stays out of step with the registry.
    /// Each change is recorded once ([`crate::event`]).
    pub async fn sync(&self, current: impl FnOnce() -> (bool, Intensity)) {
        let _one = self.inner.toggle.lock().await;
        let (paused, intensity) = current();
        self.apply_paused(paused).await;
        self.apply_intensity(intensity).await;
    }

    /// Pauses or resumes harvesting and fetching, and records it.
    /// Idempotent: `false` (and nothing recorded) when it already was.
    pub async fn set_paused(&self, paused: bool) -> bool {
        let _one = self.inner.toggle.lock().await;
        self.apply_paused(paused).await
    }

    /// Records an intensity change (the node takes it from the settings
    /// registry). `false` when it's the one recorded last.
    pub async fn note_intensity(&self, intensity: Intensity) -> bool {
        let _one = self.inner.toggle.lock().await;
        self.apply_intensity(intensity).await
    }

    /// With `toggle` held.
    async fn apply_paused(&self, paused: bool) -> bool {
        if self.state().paused == paused {
            return false;
        }
        // Pausing stops the pool first, so it asks the DHT nothing more.
        // Resuming starts the DHT first: the node forwards the change ahead
        // of any later peer request on the same command channel, so the
        // pool's requeued lookups never reach a still-paused actor and wait
        // out a refusal back-off.
        if paused {
            self.inner.monitor.pool().set_paused(true).await;
        }
        if let Some(dht) = &self.inner.dht {
            dht.set_paused(paused);
        }
        if !paused {
            self.inner.monitor.pool().set_paused(false).await;
        }
        {
            let mut st = self.state();
            st.paused = paused;
            st.since = unix_now();
        }
        if !paused {
            let now = self.now_t();
            self.inner.samples.lock().expect("samples lock").rates_from = now;
        }
        lodestar_log::log_event!(
            crate::log_events::CRAWLER_TOGGLED,
            state = if paused { "paused" } else { "resumed" }
        );
        let kind = if paused { event::PAUSE } else { event::RESUME };
        // The pause is applied either way; a lost event costs Insights a
        // gap annotation, not the user their pause.
        if let Err(e) = self.record(kind, String::new()).await {
            lodestar_log::log_event!(
                crate::log_events::EVENT_UNRECORDED,
                what = "pause",
                error = crate::log_events::error(&e)
            );
        }
        true
    }

    /// With `toggle` held.
    async fn apply_intensity(&self, intensity: Intensity) -> bool {
        {
            let mut st = self.state();
            if st.intensity == intensity {
                return false;
            }
            st.intensity = intensity;
        }
        if let Err(e) = self
            .record(event::INTENSITY, intensity_name(intensity).into())
            .await
        {
            lodestar_log::log_event!(
                crate::log_events::EVENT_UNRECORDED,
                what = "intensity",
                error = crate::log_events::error(&e)
            );
        }
        true
    }

    async fn record(&self, kind: u32, detail: String) -> Result<(), Error> {
        let store = self.inner.store.clone();
        tokio::task::spawn_blocking(move || {
            store.lock().expect("store lock").write(&WriteBatch {
                events: &[Event {
                    at: unix_now(),
                    kind,
                    detail,
                }],
                ..WriteBatch::default()
            })
        })
        .await
        .map_err(Error::Join)??;
        Ok(())
    }

    pub fn status(&self) -> CrawlerStatus {
        let (paused, since) = {
            let st = self.state();
            (st.paused, st.since)
        };
        let p = self.inner.monitor.pool_stats();
        let get = |a: &std::sync::atomic::AtomicU64| a.load(std::sync::atomic::Ordering::Relaxed);
        CrawlerStatus {
            paused,
            since,
            dht: self
                .inner
                .dht
                .as_ref()
                .and_then(|d| d.stats())
                .map(|s| DhtStatus {
                    bootstrapped: s.routing_nodes > 0,
                    routing_nodes: s.routing_nodes,
                    outstanding: s.outstanding,
                }),
            fetching: get(&p.active),
            queued: get(&p.waiting),
            lookups: get(&p.lookups),
        }
    }

    /// Keys harvested and info dicts fetched per hour, over the last
    /// [`RATE_WINDOW_SECS`]; `None` until enough has been sampled, and
    /// while paused (a gap, not a zero: §10.9).
    pub fn rates(&self) -> (Option<f64>, Option<f64>) {
        if self.state().paused {
            return (None, None);
        }
        let s = self.inner.samples.lock().expect("samples lock");
        (s.per_hour(|x| x.received), s.per_hour(|x| x.fetched))
    }

    pub fn network(&self) -> NetworkCheck {
        let s = self.inner.samples.lock().expect("samples lock");
        let start = s.bootstrapped_at.map(|b| b.max(s.window_from));
        NetworkCheck {
            external: s.external,
            reach: s.reachability(self.inner.dht.is_some()),
            window_started_at: start.map(|t| self.inner.epoch_unix + t as i64),
        }
    }

    /// Starts the reachability window again (after changing a port
    /// forward, say). `Err(seconds)` to wait when asked too often.
    pub fn restart_network_check(&self) -> Result<NetworkCheck, u64> {
        let now = self.now_t();
        {
            let mut s = self.inner.samples.lock().expect("samples lock");
            if let Some(last) = s.last_restart
                && now < last + RESTART_EVERY_SECS
            {
                return Err(last + RESTART_EVERY_SECS - now);
            }
            s.last_restart = Some(now);
            // The ring stays: the rates read it too.
            s.window_from = now;
            s.bootstrapped_at = None;
        }
        self.sample_now();
        Ok(self.network())
    }

    /// Reads the counters into the ring.
    fn sample_now(&self) {
        sample(&self.inner);
    }

    /// The highest torrent id stored (blocking: one index probe).
    pub fn max_torrent_id(&self) -> Result<i64, Error> {
        Ok(self.inner.store.lock().expect("store lock").max_id()?)
    }

    /// Every source of this build with its stored state, priority and
    /// health (blocking: reads the store).
    pub fn sources(&self) -> Result<Vec<SourceView>, Error> {
        let statuses = self.inner.monitor.sources();
        let hour = unix_now().div_euclid(3600);
        let (rows, cohorts, gates) = {
            let store = self.inner.store.lock().expect("store lock");
            let gates = statuses
                .iter()
                .map(|s| {
                    let need = consent::need_in(&store, &s.manifest)?;
                    let names = store.source_secret_names(s.manifest.id)?;
                    let settings = saved_settings(&store, s.manifest.id, false)?;
                    let pending = store.source_purge_started(s.manifest.id)?.is_some();
                    Ok((s.id, (need, names, settings, pending)))
                })
                .collect::<Result<HashMap<_, _>, Error>>()?;
            (store.sources()?, store.cohorts(hour - 23, hour + 1)?, gates)
        };
        let priorities = self.state().priorities.clone();
        let started_at = self.inner.monitor.started_at();
        let proxy_env = crate::egress::proxy_env_set();
        Ok(statuses
            .into_iter()
            .map(|status| {
                let row = rows.iter().find(|r| r.id == status.id);
                let (need, credentials, settings, purge_pending) =
                    gates.get(&status.id).cloned().unwrap_or_default();
                let egress_warnings = egress_warnings(
                    &settings.egress,
                    status.egress.as_ref(),
                    proxy_env,
                    crate::egress::auto_dns(&status.manifest),
                );
                let count = |stage: Stage| -> i64 {
                    cohorts
                        .iter()
                        .filter(|c| c.source == status.id && c.stage == stage as u8)
                        .map(|c| c.count)
                        .sum()
                };
                SourceView {
                    state: row.map_or(SourceState::Enabled, |r| r.state),
                    version: row.map_or_else(String::new, |r| r.version.clone()),
                    priority: priorities
                        .get(&status.id)
                        .copied()
                        .unwrap_or(status.priority),
                    harvested_24h: count(Stage::Harvested),
                    stored_24h: count(Stage::Stored),
                    last_run: status.enabled.then_some(started_at),
                    need,
                    consent_version: consent::version(&status.manifest),
                    credentials,
                    settings,
                    egress_warnings,
                    purge_pending,
                    status,
                }
            })
            .collect())
    }

    /// Enables or disables a source and sets its priority. The priority
    /// applies at once; whether the source runs changes at the next
    /// start. Enabling checks the source's class condition (§18.7,
    /// [`consent::need`]) against the store under the store's lock, and
    /// writes a new acknowledgement and the state in one transaction:
    /// - External, Restricted and Overlay need `change.consent` and
    ///   `change.consent_version` naming the class and the current version
    ///   unless an acknowledgement of that version is stored; Restricted
    ///   and Overlay also need `change.password_confirmed`, every time
    ///   they're enabled.
    /// - Account needs credentials stored ([`Control::update_credentials`]).
    pub async fn update_source(
        &self,
        id: SourceId,
        change: SourceChange,
    ) -> Result<SourceView, SourceError> {
        let manifest = self.manifest(id)?;
        let priority = change.priority;
        let this = self.clone();
        tokio::task::spawn_blocking(move || {
            let mut store = this.inner.store.lock().expect("store lock");
            let current = store
                .sources()?
                .into_iter()
                .find(|r| r.id == id)
                .map_or(SourceState::Enabled, |r| r.state);
            let mut ack = None;
            let state = match change.enabled {
                Some(true) if current != SourceState::Enabled => {
                    // A purge under way finishes first (SRC-14b).
                    if store.source_purge_started(manifest.id)?.is_some() {
                        return Err(SourceError::Invalid {
                            field: "enabled",
                            reason: "a purge of this source didn't finish: purge it again \
                                     to finish, then enable it"
                                .into(),
                        });
                    }
                    match consent::need_in(&store, &manifest)? {
                        None => {}
                        Some(Need::Credentials) => return Err(SourceError::CredentialsRequired),
                        Some(Need::Endpoints | Need::Consent) => {
                            let wanted = consent::version(&manifest);
                            if change.consent != Some(manifest.consent)
                                || change.consent_version.as_deref() != Some(wanted.as_str())
                            {
                                return Err(SourceError::ConsentRequired {
                                    class: manifest.consent,
                                    version: wanted,
                                });
                            }
                            ack = Some(Ack::of(&manifest).encode());
                        }
                    }
                    // Enabling Restricted or Overlay asks for the password
                    // every time, not only when the consent is first
                    // accepted (§20.11: never remembered).
                    if consent::needs_password(manifest.consent) && !change.password_confirmed {
                        return Err(SourceError::PasswordRequired);
                    }
                    Some(SourceState::Enabled)
                }
                Some(false) if current == SourceState::Enabled => Some(SourceState::Disabled),
                _ => None,
            };
            // A route keeps a source's traffic on the VPN: changing it
            // needs the password, whatever the source's class.
            if change.egress.is_some() && !change.password_confirmed {
                return Err(SourceError::PasswordRequired);
            }
            let settings = settings_changes(&change, &this.policy(id)?)?;
            if let Some(p) = change.priority {
                let key = format!("{PRIORITY_KEY}{}", manifest.id);
                let value = format!("\"{}\"", priority_name(p));
                store.set_settings_text(&[(key.as_str(), value.as_str())])?;
            }
            if !settings.is_empty() {
                let changes: Vec<(SourceSetting, Option<&str>)> = settings
                    .iter()
                    .map(|(k, v)| (*k, Some(v.as_str())))
                    .collect();
                store.set_source_settings(id, &changes)?;
            }
            if let Some(state) = state {
                if state == SourceState::Enabled {
                    store.enable_source(id, ack.as_deref())?;
                } else {
                    store.set_source_state(id, state)?;
                }
                lodestar_log::log_event!(
                    crate::log_events::SOURCE_STATE_CHANGED,
                    plugin = manifest.id,
                    state = crate::log_events::state(state)
                );
            }
            Ok::<_, SourceError>(())
        })
        .await
        .map_err(|e| SourceError::Store(Error::Join(e)))??;
        if let Some(p) = priority {
            self.state().priorities.insert(id, p);
            self.inner.monitor.pool().set_weight(id, p.weight()).await;
        }
        self.view(id).await
    }

    /// Stores or removes a source's credentials, sealed by `sealer` under
    /// the source's own context. Removing what an enabled Account source
    /// needs disables it in the same write. Values are never returned,
    /// logged or put in an error.
    pub async fn update_credentials(
        &self,
        id: SourceId,
        sealer: &SourceCredentials,
        changes: Vec<CredentialChange>,
    ) -> Result<SourceView, SourceError> {
        let manifest = self.manifest(id)?;
        for (name, value) in &changes {
            consent::check_name(name).map_err(SourceError::Credential)?;
            if let Some(v) = value {
                consent::check_value(name, v).map_err(SourceError::Credential)?;
            }
        }
        let this = self.clone();
        let sealer = sealer.clone();
        tokio::task::spawn_blocking(move || {
            // Sealed before the store's lock is taken: it may create the
            // key file.
            let mut set = Vec::new();
            let mut remove = Vec::new();
            for (name, value) in &changes {
                match value {
                    Some(v) => set.push((
                        name.as_str(),
                        sealer
                            .seal(manifest.id, name, v)
                            .map_err(|_| SourceError::Sealing)?,
                    )),
                    None => remove.push(name.as_str()),
                }
            }
            let mut store = this.inner.store.lock().expect("store lock");
            let mut names: std::collections::BTreeSet<String> = store
                .source_secret_names(manifest.id)?
                .into_iter()
                .collect();
            for (n, _) in &set {
                names.insert((*n).to_string());
            }
            for n in &remove {
                names.remove(*n);
            }
            if names.len() > consent::MAX_CREDENTIALS {
                return Err(SourceError::Credential(CredentialRefusal::TooMany));
            }
            let current = store
                .sources()?
                .into_iter()
                .find(|r| r.id == id)
                .map(|r| r.state);
            let names: Vec<String> = names.into_iter().collect();
            let ack = store
                .source_consent(manifest.id)?
                .as_deref()
                .and_then(Ack::decode);
            let lapses = current == Some(SourceState::Enabled)
                && consent::need(&manifest, ack.as_ref(), &names).is_some();
            let state = lapses.then_some(SourceState::Disabled);
            let set: Vec<(&str, &[u8])> = set.iter().map(|(n, v)| (*n, v.as_slice())).collect();
            store.update_source_secrets(id, &set, &remove, state)?;
            if let Some(state) = state {
                lodestar_log::log_event!(
                    crate::log_events::SOURCE_STATE_CHANGED,
                    plugin = manifest.id,
                    state = crate::log_events::state(state)
                );
            }
            Ok::<_, SourceError>(())
        })
        .await
        .map_err(|e| SourceError::Store(Error::Join(e)))??;
        self.view(id).await
    }

    /// Starts purging source `id` (§18.1, SRC-14b), or starts it over: the
    /// first part of a purge job. Needs the admin password (the caller
    /// checked it: `password_confirmed`). Disables the source and records
    /// the purge in the store, so nothing it finds is written from here,
    /// then stops its plugin and drops its keys from the fetch pool.
    /// Returns the keys dropped.
    pub async fn begin_purge(
        &self,
        id: SourceId,
        password_confirmed: bool,
    ) -> Result<u64, SourceError> {
        let manifest = self.manifest(id)?;
        if !password_confirmed {
            return Err(SourceError::PasswordRequired);
        }
        let store = self.inner.store.clone();
        tokio::task::spawn_blocking(move || {
            store
                .lock()
                .expect("store lock")
                .begin_source_purge(id, unix_now())
        })
        .await
        .map_err(|e| SourceError::Store(Error::Join(e)))??;
        lodestar_log::log_event!(
            crate::log_events::SOURCE_STATE_CHANGED,
            plugin = manifest.id,
            state = crate::log_events::state(SourceState::Disabled)
        );
        Ok(self.inner.monitor.stop_source(id).await as u64)
    }

    /// One step of the purge: at most `MAX_PURGE_STEP` rows, in one
    /// transaction (`Store::purge_source`).
    pub async fn purge_step(
        &self,
        id: SourceId,
        from: lodestar_store::unverified::PurgeCursor,
    ) -> Result<lodestar_store::unverified::PurgeStep, SourceError> {
        let store = self.inner.store.clone();
        tokio::task::spawn_blocking(move || {
            store.lock().expect("store lock").purge_source(
                id,
                from,
                lodestar_store::unverified::MAX_PURGE_STEP,
            )
        })
        .await
        .map_err(|e| SourceError::Store(Error::Join(e)))?
        .map_err(SourceError::from)
    }

    /// Ends the purge once every step is done: the source Purged and its
    /// consent record, credentials and settings dropped, in one
    /// transaction, then every snapshot scrubbed of them (the snapshot
    /// purge's pass). Returns the snapshots scrubbed and those left for
    /// the hourly pass (or a rollback) to scrub.
    pub async fn finish_purge(&self, id: SourceId) -> Result<(u64, u64), SourceError> {
        let manifest = self.manifest(id)?;
        let store = self.inner.store.clone();
        let (scrubbed, left) = tokio::task::spawn_blocking(move || {
            let mut store = store.lock().expect("store lock");
            let now = unix_now();
            store.finish_source_purge(id, now)?;
            // Filter hits age out on the writer's own tick, not here.
            let report = store.purge_snapshots(now, i64::MIN);
            Ok::<_, SourceError>((report.sources as u64, report.warnings.len() as u64))
        })
        .await
        .map_err(|e| SourceError::Store(Error::Join(e)))??;
        lodestar_log::log_event!(
            crate::log_events::SOURCE_STATE_CHANGED,
            plugin = manifest.id,
            state = crate::log_events::state(SourceState::Purged)
        );
        Ok((scrubbed, left))
    }

    fn policy(&self, id: SourceId) -> Result<lodestar_sources::Policy, SourceError> {
        self.inner
            .monitor
            .sources()
            .into_iter()
            .find(|s| s.id == id)
            .map(|s| s.policy)
            .ok_or(SourceError::Unknown)
    }

    fn manifest(&self, id: SourceId) -> Result<lodestar_sources::Manifest, SourceError> {
        self.inner
            .monitor
            .sources()
            .into_iter()
            .find(|s| s.id == id)
            .map(|s| s.manifest)
            .ok_or(SourceError::Unknown)
    }

    async fn view(&self, id: SourceId) -> Result<SourceView, SourceError> {
        let this = self.clone();
        tokio::task::spawn_blocking(move || this.sources())
            .await
            .map_err(|e| SourceError::Store(Error::Join(e)))??
            .into_iter()
            .find(|v| v.status.id == id)
            .ok_or(SourceError::Unknown)
    }

    /// The store the control writes through (for the credential store).
    pub fn store(&self) -> Arc<Mutex<Store>> {
        self.inner.store.clone()
    }
}

/// The settings `change` writes, encoded and checked against the
/// plugin's `policy`.
fn settings_changes(
    change: &SourceChange,
    policy: &lodestar_sources::Policy,
) -> Result<Vec<(SourceSetting, String)>, SourceError> {
    let mut out = Vec::new();
    if let Some(e) = &change.egress {
        if let EgressSetting::Socks5(p) = e
            && (!proxy_host_ok(&p.host) || p.port == 0)
        {
            return Err(SourceError::Invalid {
                field: "egress.proxy",
                reason: "the proxy isn't a host and port".into(),
            });
        }
        out.push((SourceSetting::Egress, e.encode()));
    }
    if let Some(w) = &change.schedule {
        if w.len() > MAX_WINDOWS {
            return Err(SourceError::Invalid {
                field: "schedule",
                reason: format!("a schedule holds at most {MAX_WINDOWS} windows"),
            });
        }
        out.push((SourceSetting::Schedule, encode_schedule(w)));
    }
    if let Some(n) = change.agents {
        let max = policy.max_agents.max(1);
        if n == 0 || n > max {
            return Err(SourceError::Invalid {
                field: "agents",
                reason: if max == 1 {
                    "this source runs one agent; it can't be changed".into()
                } else {
                    format!("this source runs 1 to {max} agents")
                },
            });
        }
        out.push((SourceSetting::Agents, n.to_string()));
    }
    Ok(out)
}

fn sample(inner: &Inner) {
    let get = |a: &std::sync::atomic::AtomicU64| a.load(std::sync::atomic::Ordering::Relaxed);
    let s = Sample {
        t: inner.epoch.elapsed().as_secs(),
        received: get(&inner.monitor.intake_stats().received),
        fetched: get(&inner.monitor.pool_stats().fetched),
        dht: inner
            .dht
            .as_ref()
            .and_then(|d| d.stats())
            .map(|s| DhtSample::from(&s)),
    };
    inner.samples.lock().expect("samples lock").push(s);
}

/// Samples the counters until the control is dropped. The external
/// address comes from the node's snapshot, when it runs.
async fn sampler(inner: Weak<Inner>) {
    loop {
        tokio::time::sleep(SAMPLE_EVERY).await;
        let Some(inner) = inner.upgrade() else { break };
        sample(&inner);
        let node = inner
            .dht
            .as_ref()
            .and_then(|d| d.node_handle().lock().expect("dht handle lock").clone());
        if let Some(node) = node
            && let Some(snap) = node.snapshot().await
        {
            let t = inner.epoch.elapsed().as_secs();
            let mut s = inner.samples.lock().expect("samples lock");
            s.external = snap.external;
            s.unsolicited_at = snap
                .unsolicited_sources
                .iter()
                .map(|age| t.saturating_sub(age.as_secs()))
                .collect();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(t: u64, received: u64, dht: Option<(u64, u64, u64)>) -> Sample {
        Sample {
            t,
            received,
            fetched: received / 2,
            dht: dht.map(|(queries_in, unsolicited, routing_nodes)| DhtSample {
                queries_in,
                unsolicited,
                routing_nodes,
            }),
        }
    }

    #[test]
    fn rates_need_a_minute_and_use_the_last_five() {
        let mut ring = Samples::default();
        assert_eq!(ring.per_hour(|x| x.received), None, "nothing sampled");
        ring.push(s(0, 0, None));
        ring.push(s(50, 100, None));
        assert_eq!(ring.per_hour(|x| x.received), None, "under a minute");
        ring.push(s(60, 120, None));
        assert_eq!(ring.per_hour(|x| x.received), Some(7200.0));
        // Nothing new is a real zero, not a gap (a pause is: `rates`).
        let mut idle = Samples::default();
        idle.push(s(0, 5, None));
        idle.push(s(90, 5, None));
        assert_eq!(idle.per_hour(|x| x.received), Some(0.0));
        // Only the last 300 s count.
        for t in (70..=1000).step_by(10) {
            ring.push(s(t, 120 + (t - 60), None));
        }
        assert_eq!(ring.per_hour(|x| x.received), Some(3600.0));
        assert_eq!(ring.per_hour(|x| x.fetched), Some(1800.0));
        assert!(ring.ring.len() <= KEEP, "the ring is bounded");

        // After a resume the window starts there: no rate for a minute,
        // then one that the paused stretch doesn't drag down.
        let mut r = Samples::default();
        for t in (0..=300).step_by(10) {
            r.push(s(t, 100, None));
        }
        r.rates_from = 300;
        assert_eq!(r.per_hour(|x| x.received), None);
        for t in (310..=360).step_by(10) {
            r.push(s(t, 100 + (t - 300), None));
        }
        assert_eq!(r.per_hour(|x| x.received), Some(3600.0));
    }

    #[test]
    fn reachability_waits_for_the_window_then_decides() {
        let mut r = Samples::default();
        assert_eq!(r.reachability(false).reason, ReachReason::DhtOff);
        r.push(s(0, 0, Some((0, 0, 0))));
        assert_eq!(r.reachability(true).reason, ReachReason::Bootstrapping);
        r.push(s(10, 0, Some((3, 0, 8))));
        // Evidence before the window ends is shown, the verdict isn't.
        r.push(s(60, 0, Some((10, 2, 30))));
        let early = r.reachability(true);
        assert_eq!(
            (early.reachable, early.reason),
            (None, ReachReason::Measuring)
        );
        assert_eq!((early.inbound, early.unsolicited), (Some(7), Some(2)));
        r.push(s(130, 0, Some((20, 2, 30))));
        // Two networks aren't enough...
        r.unsolicited_at = vec![40, 60];
        let few = r.reachability(true);
        assert_eq!(
            (few.reachable, few.reason, few.sources),
            (Some(false), ReachReason::FewUnsolicitedSources, Some(2))
        );
        // ...and one from before the window doesn't count.
        r.unsolicited_at.push(5);
        assert_eq!(r.reachability(true).sources, Some(2));
        r.unsolicited_at.push(120);
        let open = r.reachability(true);
        assert_eq!(open.reachable, Some(true));
        assert_eq!(open.reason, ReachReason::UnsolicitedQueries);
        assert_eq!(open.measured_secs, 120);

        // Queries only from nodes we asked: closed.
        let mut c = Samples::default();
        c.push(s(0, 0, Some((0, 0, 5))));
        c.push(s(200, 0, Some((400, 0, 50))));
        let closed = c.reachability(true);
        assert_eq!(closed.reachable, Some(false));
        assert_eq!(closed.reason, ReachReason::NoUnsolicitedQueries);
        assert_eq!(closed.inbound, Some(400));

        // Old evidence ages out of the window.
        let mut a = Samples::default();
        a.push(s(0, 0, Some((0, 0, 5))));
        a.push(s(10, 0, Some((1, 1, 5))));
        a.unsolicited_at = vec![10, 10, 10];
        assert_eq!(a.reachability(true).reason, ReachReason::Measuring);
        for t in (20..=700).step_by(10) {
            a.push(s(t, 0, Some((1, 1, 5))));
        }
        assert_eq!(a.reachability(true).reachable, Some(false));

        // Restarting the check keeps the samples the rates read.
        let mut k = Samples::default();
        for t in (0..=120).step_by(10) {
            k.push(s(t, t * 10, Some((t, 0, 5))));
        }
        let before = k.per_hour(|x| x.received);
        k.window_from = 120;
        k.bootstrapped_at = None;
        k.push(s(130, 1300, Some((130, 0, 5))));
        assert!(before.is_some() && k.per_hour(|x| x.received).is_some());
        assert_eq!(k.reachability(true).reason, ReachReason::Measuring);
    }

    #[test]
    fn consent_and_names() {
        for p in [
            Priority::Low,
            Priority::Normal,
            Priority::High,
            Priority::Max,
        ] {
            assert_eq!(parse_priority(priority_name(p)), Some(p));
        }
        assert_eq!(parse_priority("urgent"), None);
        assert_eq!(
            serde_json_str("\"high\"").and_then(parse_priority),
            Some(Priority::High)
        );
        assert_eq!(serde_json_str("high"), None);
        assert_eq!(intensity_name(Intensity::Aggressive), "aggressive");
    }

    #[test]
    fn egress_settings_round_trip_and_refuse_junk() {
        let socks = |host: &str, dns| {
            EgressSetting::Socks5(ProxySpec {
                host: host.into(),
                port: 1080,
                dns,
            })
        };
        for e in [
            EgressSetting::Direct,
            socks("10.8.0.1", None),
            socks("fd00::1", Some(SocksDns::Proxy)),
            socks("gluetun", Some(SocksDns::Local)),
            // Mapped forms of usable addresses stay usable.
            socks("::ffff:127.0.0.1", None),
            socks("::ffff:192.168.1.10", None),
        ] {
            assert_eq!(EgressSetting::decode(&e.encode()), Some(e));
        }
        for bad in [
            "",
            "vpn",
            "direct extra",
            "socks5 h 0 auto",
            "socks5 h 1080",
            "socks5 h 1080 auto x",
            "socks5 h 1080 dns",
            "socks5 a/b 1080 auto",
            "socks5 u@h 1080 auto",
            "socks5 -h 1080 auto",
            "socks5 127.1 1080 auto",
            "socks5 0x7f000001 1080 auto",
            "socks5 0.0.0.0 1080 auto",
            "socks5 :: 1080 auto",
            "socks5 239.1.1.1 1080 auto",
            "socks5 ::ffff:0.0.0.0 1080 auto",
            "socks5 ::ffff:224.0.0.1 1080 auto",
            "interface wg0",
        ] {
            assert_eq!(EgressSetting::decode(bad), None, "{bad}");
        }
    }

    #[test]
    fn schedules_round_trip_within_bounds() {
        let w = vec![Window::new(60, 420), Window::new(1410, 15)];
        assert_eq!(decode_schedule(&encode_schedule(&w)), Some(w));
        assert_eq!(decode_schedule(""), Some(Vec::new()));
        assert_eq!(decode_schedule("60-1440"), None);
        assert_eq!(decode_schedule("60"), None);
        assert_eq!(decode_schedule(&["1-2"; MAX_WINDOWS + 1].join(",")), None);
    }

    #[test]
    fn settings_changes_check_agents_and_proxies() {
        let strict = lodestar_sources::Policy::STRICT;
        let wide = lodestar_sources::Policy {
            max_agents: 4,
            ..strict
        };
        let agents = |n| SourceChange {
            agents: Some(n),
            ..SourceChange::default()
        };
        assert!(settings_changes(&agents(1), &strict).is_ok());
        for (n, p) in [(2, &strict), (0, &wide), (5, &wide)] {
            assert!(
                matches!(
                    settings_changes(&agents(n), p),
                    Err(SourceError::Invalid {
                        field: "agents",
                        ..
                    })
                ),
                "{n}"
            );
        }
        assert_eq!(
            settings_changes(&agents(4), &wide).unwrap(),
            [(SourceSetting::Agents, "4".to_string())]
        );
        let bad = SourceChange {
            egress: Some(EgressSetting::Socks5(ProxySpec {
                host: "a b".into(),
                port: 1,
                dns: None,
            })),
            ..SourceChange::default()
        };
        assert!(matches!(
            settings_changes(&bad, &strict),
            Err(SourceError::Invalid {
                field: "egress.proxy",
                ..
            })
        ));
    }

    #[test]
    fn egress_warnings_follow_what_is_saved() {
        use SocksDns::{Local, Proxy};
        let socks = |host: &str, dns| {
            Ok(EgressSetting::Socks5(ProxySpec {
                host: host.into(),
                port: 1080,
                dns,
            }))
        };
        let direct: SavedEgress = Ok(EgressSetting::Direct);
        assert_eq!(
            egress_warnings(&direct, None, true, Proxy),
            [WARN_PROXY_ENV]
        );
        assert!(egress_warnings(&direct, None, false, Proxy).is_empty());
        // Unreadable: refused, never direct, so no proxy-variable warning.
        assert_eq!(
            egress_warnings(&Err(Unreadable), None, true, Proxy),
            [WARN_SETTING_UNREADABLE]
        );
        // A LAN literal: the trade is named whenever the proxy resolves
        // names, chosen or by `auto`; not when Lodestar does.
        for env in [true, false] {
            for (dns, auto, warned) in [
                (Some(Proxy), Local, true),
                (None, Proxy, true),
                (None, Local, false),
                (Some(Local), Proxy, false),
            ] {
                let got = egress_warnings(&socks("192.168.1.5", dns), None, env, auto);
                assert_eq!(got == [WARN_LAN_PROXY_DNS], warned, "{dns:?} {auto:?}");
                assert_eq!(got.is_empty(), !warned);
            }
        }
        // A public proxy, or a name not yet resolved: nothing to say.
        assert!(egress_warnings(&socks("198.51.100.7", None), None, false, Proxy).is_empty());
        let named = socks("gluetun", None);
        assert!(egress_warnings(&named, None, false, Proxy).is_empty());
        // Running on the saved setting: judged by where the name resolved.
        let live = |setting: SavedEgress| crate::EgressLive {
            status: lodestar_sources::EgressStatus {
                plugin: "p".into(),
                route: lodestar_sources::RouteKind::Socks5,
                state: lodestar_sources::RouteState::Up,
                counters: Default::default(),
                notice: None,
            },
            route: Some(lodestar_sources::Egress::Socks5 {
                proxy: "172.18.0.5:1080".parse().unwrap(),
                dns: Proxy,
            }),
            setting,
        };
        assert_eq!(
            egress_warnings(&named, Some(&live(named.clone())), false, Proxy),
            [WARN_LAN_PROXY_DNS]
        );
        // The user changed it since the start: the saved setting wins, so
        // the warning goes away at once, not at the next restart.
        let fixed = socks("gluetun", Some(Local));
        assert!(egress_warnings(&fixed, Some(&live(named.clone())), false, Proxy).is_empty());
        assert!(egress_warnings(&direct, Some(&live(named)), false, Proxy).is_empty());
    }
}
