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
use lodestar_sources::{Consent, Priority, SourcePlugin, Stage};
use lodestar_store::{Event, SourceId, SourceState, Store, WriteBatch};
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
pub fn saved_priority(store: &Store, plugin: &str) -> Result<Option<Priority>, Error> {
    let Some(text) = store.setting_text(&format!("{PRIORITY_KEY}{plugin}"))? else {
        return Ok(None);
    };
    let parsed = serde_json_str(&text).and_then(parse_priority);
    if parsed.is_none() {
        tracing::warn!(plugin, "ignoring a saved source priority that isn't one");
    }
    Ok(parsed)
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

/// Whether enabling a source of this class needs the consent screen
/// (§18.7): Restricted and Overlay do; None and External are a toggle,
/// and Account asks for credentials instead.
pub fn needs_consent(c: Consent) -> bool {
    matches!(c, Consent::Restricted | Consent::Overlay)
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
}

/// A change to one source. `None` leaves a field as it is.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SourceChange {
    pub enabled: Option<bool>,
    pub priority: Option<Priority>,
    /// The consent class the user agreed to, for enabling a source whose
    /// class needs it.
    pub consent: Option<Consent>,
}

#[derive(Debug)]
pub enum SourceError {
    Unknown,
    /// Enabling needs consent to this class first.
    ConsentRequired(Consent),
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
        self.inner.monitor.pool().set_paused(paused).await;
        if let Some(dht) = &self.inner.dht {
            dht.set_paused(paused);
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
        tracing::info!("crawler {}", if paused { "paused" } else { "resumed" });
        let kind = if paused { event::PAUSE } else { event::RESUME };
        // The pause is applied either way; a lost event costs Insights a
        // gap annotation, not the user their pause.
        if let Err(e) = self.record(kind, String::new()).await {
            tracing::error!("crawler: recording the pause event failed: {e}");
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
            tracing::error!("crawler: recording the intensity event failed: {e}");
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
        let (rows, cohorts) = {
            let store = self.inner.store.lock().expect("store lock");
            (store.sources()?, store.cohorts(hour - 23, hour + 1)?)
        };
        let priorities = self.state().priorities.clone();
        let started_at = self.inner.monitor.started_at();
        Ok(statuses
            .into_iter()
            .map(|status| {
                let row = rows.iter().find(|r| r.id == status.id);
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
                    status,
                }
            })
            .collect())
    }

    /// Enables or disables a source and sets its priority. The priority
    /// applies at once; whether the source runs changes at the next
    /// start. Enabling a source whose consent class needs it requires
    /// `change.consent` to name that class.
    pub async fn update_source(
        &self,
        id: SourceId,
        change: SourceChange,
    ) -> Result<SourceView, SourceError> {
        let status = self
            .inner
            .monitor
            .sources()
            .into_iter()
            .find(|s| s.id == id)
            .ok_or(SourceError::Unknown)?;
        let manifest = status.manifest.clone();
        let this = self.clone();
        tokio::task::spawn_blocking(move || {
            let mut store = this.inner.store.lock().expect("store lock");
            let current = store
                .sources()?
                .into_iter()
                .find(|r| r.id == id)
                .map_or(SourceState::Enabled, |r| r.state);
            let state = match change.enabled {
                Some(true) if current != SourceState::Enabled => {
                    if needs_consent(manifest.consent) && change.consent != Some(manifest.consent) {
                        return Err(SourceError::ConsentRequired(manifest.consent));
                    }
                    Some(SourceState::Enabled)
                }
                Some(false) if current == SourceState::Enabled => Some(SourceState::Disabled),
                _ => None,
            };
            if let Some(p) = change.priority {
                let key = format!("{PRIORITY_KEY}{}", manifest.id);
                let value = format!("\"{}\"", priority_name(p));
                store.set_settings_text(&[(key.as_str(), value.as_str())])?;
            }
            if let Some(state) = state {
                store.set_source_state(id, state)?;
                tracing::info!(plugin = manifest.id, ?state, "source state changed");
            }
            Ok::<_, SourceError>(())
        })
        .await
        .map_err(|e| SourceError::Store(Error::Join(e)))??;
        if let Some(p) = change.priority {
            self.state().priorities.insert(id, p);
            self.inner.monitor.pool().set_weight(id, p.weight()).await;
        }
        let this = self.clone();
        tokio::task::spawn_blocking(move || this.sources())
            .await
            .map_err(|e| SourceError::Store(Error::Join(e)))??
            .into_iter()
            .find(|v| v.status.id == id)
            .ok_or(SourceError::Unknown)
    }
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
        assert!(!needs_consent(Consent::None));
        assert!(!needs_consent(Consent::External));
        assert!(!needs_consent(Consent::Account));
        assert!(needs_consent(Consent::Restricted));
        assert!(needs_consent(Consent::Overlay));
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
}
