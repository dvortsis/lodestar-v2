//! BEP 48 full scrape, source plugin (architecture §18.2 rank 16).
//!
//! A scrape request with no `info_hash` asks an HTTP tracker for every
//! torrent it knows. Trackers that allow it answer with one large `files`
//! dictionary; each 20-byte key in it is an info hash, emitted here as a
//! discovery. The trackers are the ones the user lists.
//!
//! **Politeness (§18.1, §18.4).** A full scrape is heavy for the tracker,
//! so it runs:
//! - **at most once per tracker per day.** Three guards, any one of which
//!   holds the cap alone: the last attempt per tracker kept in the
//!   plugin's state (so a restart doesn't reset it), the kit's daily
//!   full-scrape quota ([`HostLimiter::try_full_scrape`], from
//!   `max_full_scrapes_per_day: 1`), and the poll gate (a 24-hour poll
//!   interval). The attempt is recorded before the request goes out, so a
//!   failure or a crash mid-reply still counts;
//! - **only inside the schedule window**, 01:00–07:00 by default, in the
//!   user's time zone ([`Clock`]). A request starts only with a minute of
//!   the window left, and is abandoned when the window closes.
//!
//! **Never on a private tracker (§18.7).** A scrape tells the tracker
//! nothing about us that an announce wouldn't, but scraping a private
//! tracker breaks its rules and its address usually carries a passkey.
//! [`Config`] refuses addresses that look private (a query, or a path
//! segment shaped like a passkey), and before every scrape the plugin
//! asks [`PrivateTrackers`] whether any torrent from a private source, or
//! marked `private`, lists the host. An error there counts as "yes".
//!
//! **ToS (Q2, `docs/owner-decisions.md` §2.28).** A full scrape is part of
//! the tracker protocol (BEP 48), made for automated clients, and a
//! tracker that doesn't allow it refuses; private trackers are never
//! scraped. Lodestar ships no tracker list: the user names each public
//! tracker. A tracker whose published rules forbid scraping is the
//! owner's call (`docs/decisions/2026-11-04-SRC-12.md`).
//!
//! **Egress** goes only through `ctx.kit.http`, which SRC-5's
//! `EgressFactory` builds for the plugin; [`FullScrape::endpoints`]
//! narrows it to the configured hosts on public addresses.
//!
//! **The reply is untrusted.** It is parsed by
//! [`lodestar_tracker_scrape::full::Parser`], streaming, with caps on
//! bytes, entries, entry size and depth. The body is streamed
//! ([`Client::poll_stream`](lodestar_sources::Client::poll_stream)) under
//! this plugin's own [`STREAM`] limits (256 MiB, ten minutes, a stall of
//! a minute), never held whole: each chunk is fed to the parser and its
//! keys emitted before the next chunk is read.

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use lodestar_integrations::net::{self, NetPolicy};
use lodestar_sources::claims::{Claims, Confidence, Provenance};
use lodestar_sources::http::{StreamLimits, StreamResponse, host_key};
use lodestar_sources::{
    Backoff, BoxFuture, Caps, Consent, Distribution, Emitter, Endpoints, Found, HostRate, Kind,
    Kit, Manifest, Network, PluginError, Policy, Priority, Request, Risk, Schedule, SourceCtx,
    SourcePlugin, Window,
};
use lodestar_tracker_scrape::full::{self, Limits, Parser};
use lodestar_tracker_scrape::{Scheme, TrackerUrl};
use serde::Deserialize;

/// The plugin's id in the store. Never rename.
pub const ID: &str = "full_scrape";
/// The strategy code on every key this plugin emits.
pub const STRATEGY_FULL_SCRAPE: u8 = 1;
/// Trackers one plugin instance scrapes.
pub const MAX_TRACKERS: usize = 32;
/// A path segment this long, all letters and digits, is taken for a
/// passkey (private trackers put theirs in the path or the query).
pub const PASSKEY_MIN: usize = 16;
/// The daily cap's period.
pub const DAY: Duration = Duration::from_secs(24 * 3600);
/// How often the plugin looks again while the window is open.
pub const CHECK_EVERY: Duration = Duration::from_secs(15 * 60);
/// The default schedule window, 01:00–07:00 (§18.4).
pub const WINDOW: Window = Window::new(60, 7 * 60);
const SCHEDULE: Schedule = Schedule(&[WINDOW]);

/// How large and how slow one tracker's reply may be: 256 MiB within ten
/// minutes (the largest public trackers' full scrapes, with room), and no
/// wait over a minute for the headers or any chunk. The schedule window's
/// close cuts it shorter when that comes first.
pub const STREAM: StreamLimits = StreamLimits {
    max_bytes: 256 * 1024 * 1024,
    timeout: Duration::from_secs(10 * 60),
    idle: Duration::from_secs(60),
};

/// The parser's caps for one reply: the defaults, with the byte cap at
/// the stream's.
pub const REPLY_LIMITS: Limits = Limits {
    max_bytes: STREAM.max_bytes,
    ..full::LIMITS
};

/// The key under which a tracker's last attempt (Unix seconds) is kept.
fn state_key(host: &str) -> String {
    format!("last:{host}")
}

const MANIFEST: Manifest = Manifest {
    id: ID,
    name: "Tracker full scrape",
    version: "1",
    kinds: &[Kind::Discovery],
    networks: &[Network::Bt],
    // None built in: the trackers are the ones the user lists
    // (`FullScrape::endpoints`).
    endpoints: &[],
    risk: Risk::Low,
    // §18.7: BEP 48 is in the "None" class, a plain toggle.
    consent: Consent::None,
    default_priority: Priority::Low,
    enabled_by_default: false,
    always_adult: false,
    finds_private: false,
};

/// The settings form (§10.10).
pub const CONFIG_SCHEMA: &str = r#"{"$schema":"https://json-schema.org/draft/2020-12/schema","type":"object","properties":{"trackers":{"type":"array","title":"Public HTTP trackers","description":"Announce addresses of public trackers that allow a full scrape, such as https://tracker.example/announce. Never a private tracker.","items":{"type":"string","format":"uri"},"minItems":1,"maxItems":32,"uniqueItems":true}},"required":["trackers"],"additionalProperties":false}"#;

/// §18.1: at most one full scrape per tracker per day. The poll interval
/// is the day too, so the poll gate holds the cap as well.
const POLICY: Policy = Policy {
    caps: Caps {
        min_poll_interval: DAY,
        per_host: HostRate {
            interval: Duration::from_secs(60),
            burst: 1,
        },
        max_full_scrapes_per_day: 1,
        backoff: Backoff {
            base: Duration::from_secs(3600),
            max: DAY,
        },
    },
    config_schema: CONFIG_SCHEMA,
    // §19.5: sharded per tracker. Each tracker lives on one worker, so its
    // daily cap is held there.
    distribution: Distribution::ShardedUnit,
    schedule: SCHEDULE,
    max_agents: 1,
};

/// One tracker, checked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tracker {
    /// The announce URL in its canonical form: the tracker claim.
    announce: String,
    /// The scrape URL, with no query.
    scrape: String,
    /// The limiter's, the state's and the egress host list's key.
    host: String,
}

impl Tracker {
    pub fn announce(&self) -> &str {
        &self.announce
    }

    pub fn scrape_url(&self) -> &str {
        &self.scrape
    }

    pub fn host(&self) -> &str {
        &self.host
    }
}

/// The plugin's settings, checked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    trackers: Vec<Tracker>,
}

/// Why settings were refused. Names the rule, never the value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConfigError(pub &'static str);

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0)
    }
}

impl std::error::Error for ConfigError {}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawConfig {
    trackers: Vec<String>,
}

impl Config {
    /// Checks each tracker address:
    /// - http or https (UDP trackers have no full scrape), with a host,
    ///   no user name or password, no query and no fragment;
    /// - no path segment shaped like a passkey ([`PASSKEY_MIN`]);
    /// - a last path segment starting with `announce`, so it has a scrape
    ///   address (BEP 48);
    /// - not a literal LAN, loopback or link-local address: these are
    ///   public trackers;
    /// - at most one tracker per host, since the daily cap is per host.
    pub fn new<S: AsRef<str>>(trackers: &[S]) -> Result<Config, ConfigError> {
        if trackers.is_empty() {
            return Err(ConfigError("list at least one tracker"));
        }
        if trackers.len() > MAX_TRACKERS {
            return Err(ConfigError("at most 32 trackers"));
        }
        let mut out: Vec<Tracker> = Vec::with_capacity(trackers.len());
        for t in trackers {
            let t = tracker(t.as_ref().trim())?;
            if out.iter().any(|o| o.host == t.host) {
                return Err(ConfigError("two trackers share a host"));
            }
            out.push(t);
        }
        Ok(Config { trackers: out })
    }

    /// The settings as the form saves them (see [`CONFIG_SCHEMA`]).
    pub fn from_json(json: &str) -> Result<Config, ConfigError> {
        let raw: RawConfig = serde_json::from_str(json)
            .map_err(|_| ConfigError("the settings don't match the form"))?;
        Config::new(&raw.trackers)
    }

    pub fn trackers(&self) -> &[Tracker] {
        &self.trackers
    }
}

fn tracker(raw: &str) -> Result<Tracker, ConfigError> {
    let url = TrackerUrl::parse(raw).map_err(|_| ConfigError("a tracker isn't a valid address"))?;
    if url.scheme() == Scheme::Udp {
        return Err(ConfigError(
            "only http and https trackers have a full scrape",
        ));
    }
    let parsed =
        url::Url::parse(raw).map_err(|_| ConfigError("a tracker isn't a valid address"))?;
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(ConfigError(
            "a tracker address has a user name or password in it",
        ));
    }
    if parsed.query().is_some() || parsed.fragment().is_some() {
        return Err(ConfigError(
            "a tracker address has a query or fragment; private trackers put passkeys there",
        ));
    }
    let passkey = parsed
        .path_segments()
        .into_iter()
        .flatten()
        .any(|s| s.len() >= PASSKEY_MIN && s.bytes().all(|b| b.is_ascii_alphanumeric()));
    if passkey {
        return Err(ConfigError(
            "a tracker address has a passkey-like part; private trackers aren't scraped",
        ));
    }
    let scrape = url.scrape_url().ok_or(ConfigError(
        "a tracker address doesn't end in /announce, so it has no scrape",
    ))?;
    if scrape.query().is_some() {
        return Err(ConfigError(
            "a tracker address has a query or fragment; private trackers put passkeys there",
        ));
    }
    net::check_url(&scrape, NetPolicy::notification(false))
        .map_err(|_| ConfigError("a tracker address isn't a public one"))?;
    let host = host_key(scrape.as_str()).ok_or(ConfigError("a tracker address has no host"))?;
    Ok(Tracker {
        announce: url.as_str().to_owned(),
        scrape: scrape.as_str().to_owned(),
        host,
    })
}

/// A lookup failed; the plugin treats the tracker as private.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LookupError;

/// Which trackers are private: the host answers from the store whether
/// any torrent that is `private`, or was found only through a private
/// source (`private_source`, §18.7), lists a tracker on `host`. Asked
/// before every scrape, so a tracker that turns out to be private stops
/// being scraped at once.
pub trait PrivateTrackers: Send + Sync {
    fn is_private<'a>(&'a self, host: &'a str) -> BoxFuture<'a, Result<bool, LookupError>>;
}

/// A fixed list of private hosts. For tests only: never wire it into the
/// running app, where the lookup must come from the store. It has no
/// `Default`, so "nothing is private" is always spelled out
/// ([`StaticPrivateTrackers::none`]).
#[cfg(any(test, feature = "testing"))]
#[derive(Debug, Clone)]
pub struct StaticPrivateTrackers {
    hosts: Vec<String>,
    /// Every lookup fails.
    pub fail: bool,
}

#[cfg(any(test, feature = "testing"))]
impl StaticPrivateTrackers {
    pub fn new<S: AsRef<str>>(hosts: &[S]) -> StaticPrivateTrackers {
        StaticPrivateTrackers {
            hosts: hosts.iter().map(|h| h.as_ref().to_owned()).collect(),
            fail: false,
        }
    }

    /// No host is private. Tests only.
    pub fn none() -> StaticPrivateTrackers {
        StaticPrivateTrackers::new::<&str>(&[])
    }

    pub fn failing() -> StaticPrivateTrackers {
        StaticPrivateTrackers {
            hosts: Vec::new(),
            fail: true,
        }
    }
}

#[cfg(any(test, feature = "testing"))]
impl PrivateTrackers for StaticPrivateTrackers {
    fn is_private<'a>(&'a self, host: &'a str) -> BoxFuture<'a, Result<bool, LookupError>> {
        let r = if self.fail {
            Err(LookupError)
        } else {
            Ok(self.hosts.iter().any(|h| h == host))
        };
        Box::pin(async move { r })
    }
}

/// Wall-clock time for the daily cap and the schedule window.
pub trait Clock: Send + Sync + fmt::Debug {
    /// Unix seconds.
    fn now(&self) -> i64;
    /// The user's time zone as minutes east of UTC, for the window.
    fn utc_offset_minutes(&self) -> i32 {
        0
    }
    /// Minutes past local midnight.
    fn minute_of_day(&self) -> u16 {
        let local = self
            .now()
            .saturating_add(i64::from(self.utc_offset_minutes()) * 60);
        (local.rem_euclid(86_400) / 60) as u16
    }
}

/// The system clock, with the user's UTC offset for the window.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock {
    pub utc_offset_minutes: i32,
}

impl Clock for SystemClock {
    fn now(&self) -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs() as i64)
    }

    fn utc_offset_minutes(&self) -> i32 {
        self.utc_offset_minutes
    }
}

/// A clock that starts at `base` and moves with tokio's clock, so a test
/// on a paused clock moves the wall clock too. For tests only.
#[cfg(any(test, feature = "testing"))]
#[derive(Debug, Clone, Copy)]
pub struct TokioClock {
    base: i64,
    start: tokio::time::Instant,
}

#[cfg(any(test, feature = "testing"))]
impl TokioClock {
    pub fn starting_at(base: i64) -> TokioClock {
        TokioClock {
            base,
            start: tokio::time::Instant::now(),
        }
    }
}

#[cfg(any(test, feature = "testing"))]
impl Clock for TokioClock {
    fn now(&self) -> i64 {
        self.base + self.start.elapsed().as_secs() as i64
    }
}

/// The full-scrape plugin.
pub struct FullScrape {
    config: Option<Config>,
    private: Arc<dyn PrivateTrackers>,
    clock: Arc<dyn Clock>,
}

impl fmt::Debug for FullScrape {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FullScrape")
            .field("config", &self.config)
            .field("clock", &self.clock)
            .finish_non_exhaustive()
    }
}

impl FullScrape {
    pub fn new(
        config: Config,
        private: Arc<dyn PrivateTrackers>,
        clock: Arc<dyn Clock>,
    ) -> FullScrape {
        FullScrape {
            config: Some(config),
            private,
            clock,
        }
    }

    /// Listed in Settings → Sources before it's set up; fails at once if
    /// run.
    pub fn unconfigured(private: Arc<dyn PrivateTrackers>, clock: Arc<dyn Clock>) -> FullScrape {
        FullScrape {
            config: None,
            private,
            clock,
        }
    }

    /// Where the plugin's egress may send requests (SRC-5): the
    /// configured trackers' hosts only, on public addresses. `None` until
    /// it's set up, so nothing may be reached.
    pub fn endpoints(&self) -> Option<Endpoints> {
        let c = self.config.as_ref()?;
        let hosts: Vec<&str> = c.trackers.iter().map(|t| t.host.as_str()).collect();
        Some(Endpoints::user_entered(NetPolicy::notification(false)).only_hosts(&hosts))
    }
}

impl SourcePlugin for FullScrape {
    fn manifest(&self) -> &Manifest {
        &MANIFEST
    }

    fn policy(&self) -> &Policy {
        &POLICY
    }

    fn run(&self, ctx: SourceCtx) -> BoxFuture<'_, Result<(), PluginError>> {
        Box::pin(self.run_inner(ctx))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Outcome {
    /// Scraped, or refused: the loop goes on.
    Done,
    /// Scraped within the last day: due again at this Unix time.
    DueAt(i64),
    /// The pipeline is gone.
    Closed,
}

/// Minutes from `minute` (minutes past local midnight) until the window
/// closes; 0 when it's closed.
pub fn minutes_until_close(minute: u16) -> u16 {
    const DAY_MINUTES: u16 = 24 * 60;
    (0..DAY_MINUTES)
        .find(|d| !SCHEDULE.is_open((minute + d) % DAY_MINUTES))
        .unwrap_or(DAY_MINUTES)
}

impl FullScrape {
    async fn run_inner(&self, ctx: SourceCtx) -> Result<(), PluginError> {
        let SourceCtx {
            emitter,
            mut shutdown,
            kit,
            ..
        } = ctx;
        let config = self
            .config
            .clone()
            .ok_or_else(|| PluginError("needs setup: no trackers are set".to_owned()))?;
        loop {
            let minute = self.clock.minute_of_day();
            let wait = if SCHEDULE.is_open(minute) {
                let mut wait = CHECK_EVERY;
                for t in &config.trackers {
                    let out = tokio::select! {
                        _ = shutdown.requested() => return Ok(()),
                        o = self.attempt(t, &kit, &emitter) => o,
                    };
                    match out {
                        Outcome::Closed => return Ok(()),
                        // Wake a second after it's due, so a tracker keeps
                        // its slot in the window rather than sliding later
                        // by up to a tick each day.
                        Outcome::DueAt(at) => {
                            let secs = at.saturating_sub(self.clock.now()).max(0) as u64 + 1;
                            wait = wait.min(Duration::from_secs(secs));
                        }
                        Outcome::Done => {}
                    }
                }
                wait
            } else {
                let m = SCHEDULE.minutes_until_open(minute).max(1);
                Duration::from_secs(u64::from(m) * 60)
            };
            tokio::select! {
                _ = shutdown.requested() => return Ok(()),
                _ = tokio::time::sleep(wait) => {}
            }
        }
    }

    /// One tracker's turn: scrapes it if every guard allows.
    async fn attempt(&self, t: &Tracker, kit: &Kit, emitter: &Emitter) -> Outcome {
        // Started inside the window or not at all, and abandoned by its
        // close. Checked before anything is used up, so a tracker reached
        // in the window's last minute keeps its day. The minute may be
        // part gone, so the deadline counts only the whole minutes after.
        let left = minutes_until_close(self.clock.minute_of_day());
        if left <= 1 {
            return Outcome::Done;
        }
        let deadline = Duration::from_secs(u64::from(left - 1) * 60);
        let now = self.clock.now();
        let key = state_key(&t.host);
        // The persisted cap. An unreadable state stops the scrape: the
        // cap can't be shown to hold.
        let last = match kit.state.get(&key).await {
            Ok(v) => v,
            Err(_) => return Outcome::Done,
        };
        if let Some(v) = last {
            let day = DAY.as_secs() as i64;
            match std::str::from_utf8(&v)
                .ok()
                .and_then(|s| s.parse::<i64>().ok())
                // More than a day ahead was written under a wrong clock:
                // reset below rather than obeyed until then.
                .filter(|last| *last <= now.saturating_add(day))
            {
                // Also not due when the clock has gone back past it.
                Some(last) if now < last.saturating_add(day) => {
                    return Outcome::DueAt(last.saturating_add(day));
                }
                Some(_) => {}
                // Unreadable, or from a wrong clock: the day starts now.
                None => {
                    let _ = kit.state.set(&key, now.to_string().into_bytes()).await;
                    return Outcome::Done;
                }
            }
        }
        if !matches!(self.private.is_private(&t.host).await, Ok(false)) {
            return Outcome::Done;
        }
        if kit
            .limiter()
            .try_full_scrape(&t.host, tokio::time::Instant::now())
            .is_err()
        {
            return Outcome::Done;
        }
        // Recorded before sending: a failed or interrupted scrape counts.
        if kit
            .state
            .set(&key, now.to_string().into_bytes())
            .await
            .is_err()
        {
            return Outcome::Done;
        }
        let req = Request::get(t.scrape.clone());
        let limits = StreamLimits {
            timeout: STREAM.timeout.min(deadline),
            ..STREAM
        };
        let scrape = async {
            match kit.http.poll_stream(&t.announce, &req, limits).await {
                Ok(res) if res.status == 200 => emit_reply(res, t, emitter).await,
                _ => Outcome::Done,
            }
        };
        // The stream's own timeout is the same or sooner; this also bounds
        // the time spent waiting for the pipeline to take the keys.
        tokio::time::timeout(deadline, scrape)
            .await
            .unwrap_or(Outcome::Done)
    }
}

/// Feeds the body to the parser a chunk at a time, as it arrives, and
/// emits each chunk's keys before reading the next. A parse error or a
/// body that fails (too slow, too large, cut off) ends it; the keys
/// already emitted stand, as each came from a complete entry.
async fn emit_reply(mut res: StreamResponse, t: &Tracker, emitter: &Emitter) -> Outcome {
    let mut parser = Parser::new(REPLY_LIMITS);
    let mut keys = Vec::new();
    loop {
        let chunk = match res.next_chunk().await {
            Ok(Some(chunk)) => chunk,
            Ok(None) => break,
            Err(_) => return Outcome::Done,
        };
        let fed = parser.feed(&chunk, &mut |b| keys.extend_from_slice(b));
        if send(&mut keys, t, emitter).await == Outcome::Closed {
            return Outcome::Closed;
        }
        if fed.is_err() {
            return Outcome::Done;
        }
    }
    let _ = parser.finish(&mut |b| keys.extend_from_slice(b));
    send(&mut keys, t, emitter).await
}

async fn send(
    keys: &mut Vec<lodestar_metainfo::InfoHash>,
    t: &Tracker,
    emitter: &Emitter,
) -> Outcome {
    for key in keys.drain(..) {
        if emitter.emit(to_found(key, t)).await.is_err() {
            return Outcome::Closed;
        }
    }
    Outcome::Done
}

/// One key from `t`'s full scrape. The tracker listing the hash is a
/// claim at Medium confidence (the tracker's own record); there is no
/// name, size or reference. Never `private_source`: private trackers
/// aren't scraped.
pub fn to_found(key: lodestar_metainfo::InfoHash, t: &Tracker) -> Found {
    Found::key(key, STRATEGY_FULL_SCRAPE)
        .with_claims(Claims {
            trackers: vec![t.announce.clone()],
            ..Claims::default()
        })
        .with_provenance(Provenance {
            external_ref: None,
            published_at: None,
            confidence: Confidence::Medium,
        })
}
