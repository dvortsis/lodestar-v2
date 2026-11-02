//! Politeness for a plugin's HTTP traffic (§18.1, §18.4): a per-host
//! token bucket, exponential back-off with jitter, a "blocked by source"
//! state, and the daily full-scrape quota.
//!
//! Nothing here opens a connection. [`Fetch`] is the seam: SRC-5's client
//! factory implements it with reqwest and the plugin's egress, and the
//! fixture [`Replayer`](crate::testing::Replayer) implements it from
//! recorded exchanges. [`Client`] wraps any [`Fetch`] with a
//! [`HostLimiter`], so every request a plugin makes is paced and every
//! answer feeds the back-off.
//!
//! All times are `tokio::time::Instant`, so tests run on paused time.

use std::collections::hash_map::RandomState;
use std::collections::{HashMap, VecDeque};
use std::hash::BuildHasher;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::time::{Instant, sleep_until};

use crate::manifest::{Backoff, Limits};
use crate::plugin::BoxFuture;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    Get,
    Head,
    Post,
}

impl Method {
    pub fn as_str(self) -> &'static str {
        match self {
            Method::Get => "GET",
            Method::Head => "HEAD",
            Method::Post => "POST",
        }
    }

    pub fn parse(s: &str) -> Option<Method> {
        match s {
            "GET" => Some(Method::Get),
            "HEAD" => Some(Method::Head),
            "POST" => Some(Method::Post),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    pub method: Method,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Request {
    pub fn get(url: impl Into<String>) -> Request {
        Request {
            method: Method::Get,
            url: url.into(),
            headers: Vec::new(),
            body: Vec::new(),
        }
    }

    pub fn header(mut self, name: impl Into<String>, value: impl Into<String>) -> Request {
        self.headers.push((name.into(), value.into()));
        self
    }

    /// The limiter's key for this request's host.
    pub fn host(&self) -> Option<String> {
        host_key(&self.url)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Response {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Response {
    /// The first header called `name`, ignoring ASCII case.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HttpError {
    /// No `scheme://host` in the URL.
    BadUrl,
    /// The source has told us to stop until `until` (see [`Verdict`]).
    Blocked { until: Instant },
    /// The request never got an answer (connect, TLS, timeout, egress).
    Transport(String),
    /// A fixture replayer had no recorded exchange for this request.
    Unrecorded(String),
    /// No HTTP client is configured for this plugin (SRC-5 supplies one).
    NoEgress,
    /// The plugin's egress route is down, so its kill switch refused the
    /// request or abandoned it in flight. The host isn't at fault, so this
    /// doesn't back it off: the route coming back is what matters.
    ///
    /// **Plugin contract:** while the route is down the host isn't backed
    /// off, so a plugin must not retry in a loop: it waits at least its
    /// poll interval before trying again. The plugin's egress paces it
    /// too, so the contract doesn't rest on the plugin alone: a refusal
    /// before sending comes back only after a delay that grows with each
    /// refusal in a row, up to `egress::DOWN_DELAY_MAX`, or sooner when
    /// the route comes back (SRC-14b).
    EgressDown(String),
}

impl std::fmt::Display for HttpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HttpError::BadUrl => f.write_str("URL has no host"),
            HttpError::Blocked { .. } => f.write_str("blocked by source"),
            HttpError::Transport(e) => write!(f, "transport: {e}"),
            HttpError::Unrecorded(e) => write!(f, "no recorded exchange: {e}"),
            HttpError::NoEgress => f.write_str("no HTTP egress configured for this plugin"),
            HttpError::EgressDown(e) => write!(f, "egress down: {e}"),
        }
    }
}

impl std::error::Error for HttpError {}

/// Sends one request. SRC-5's client and the fixture replayer implement
/// it; plugins use it only through the [`Client`] in their
/// [`Kit`](crate::Kit).
///
/// The contract an implementation keeps:
/// - **It never follows redirects.** It returns the 3xx as it is, so a
///   plugin that follows one sends a new request through the [`Client`],
///   paced for the new host and checked against its egress.
/// - **It reads at most [`MAX_BODY`] bytes of body** and fails with
///   [`HttpError::Transport`] past that, so a hostile server can't fill
///   memory. A plugin that needs more (a multi-megabyte full scrape)
///   waits for streaming: a later, additive `fetch_stream` method with a
///   default implementation, so existing implementations keep compiling.
pub trait Fetch: Send + Sync {
    fn fetch<'a>(&'a self, req: &'a Request) -> BoxFuture<'a, Result<Response, HttpError>>;
}

/// The largest response body a [`Fetch`] returns.
pub const MAX_BODY: usize = 16 * 1024 * 1024;

impl<T: Fetch + ?Sized> Fetch for Arc<T> {
    fn fetch<'a>(&'a self, req: &'a Request) -> BoxFuture<'a, Result<Response, HttpError>> {
        (**self).fetch(req)
    }
}

/// The [`Fetch`] a plugin has until egress is configured: every request
/// fails with [`HttpError::NoEgress`] and nothing leaves the machine.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoEgress;

impl Fetch for NoEgress {
    fn fetch<'a>(&'a self, _: &'a Request) -> BoxFuture<'a, Result<Response, HttpError>> {
        Box::pin(async { Err(HttpError::NoEgress) })
    }
}

/// The longest `Retry-After` honoured. A source can't park a plugin for
/// longer than this with one header; it can block again after.
pub const MAX_RETRY_AFTER: Duration = Duration::from_secs(24 * 3600);

/// What an answer means for pacing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// The source answered normally. Resets the back-off. Includes 404 and
    /// other client errors: retrying faster or slower won't change them.
    Ok,
    /// Overloaded or failing (5xx, or 429 without `Retry-After`): back off.
    Retry,
    /// The source refused us (401, 403, or 429/503 with `Retry-After`):
    /// no requests to the host until the later of the given time and the
    /// next back-off delay, or until the back-off ceiling when the source
    /// gave none. It counts as a failure, so repeated refusals escalate.
    Blocked(Option<Duration>),
}

pub fn classify(res: &Response) -> Verdict {
    let retry_after = res.header("retry-after").and_then(parse_retry_after);
    match res.status {
        401 | 403 => Verdict::Blocked(retry_after),
        429 | 503 => match retry_after {
            Some(d) => Verdict::Blocked(Some(d)),
            None => Verdict::Retry,
        },
        500..=599 => Verdict::Retry,
        _ => Verdict::Ok,
    }
}

/// `Retry-After` in delta-seconds, capped at [`MAX_RETRY_AFTER`]. The
/// HTTP-date form reads as absent: the back-off covers it.
pub fn parse_retry_after(v: &str) -> Option<Duration> {
    let v = v.trim();
    if v.is_empty() || v.len() > 12 || !v.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let secs: u64 = v.parse().ok()?;
    Some(Duration::from_secs(secs).min(MAX_RETRY_AFTER))
}

/// The host an HTTP(S) URL connects to, as the WHATWG URL parser (the
/// `url` crate reqwest uses) reads it, so the limiter paces the host the
/// client actually reaches. IPv4 in any form (`127.1`, `0x7f000001`,
/// `2130706433`) becomes dotted quads, IPv6 is compressed in brackets
/// (`[::1]`), an IPv4-mapped IPv6 address is its IPv4 address, and
/// domains are lower-case punycode without a trailing dot. `None` (so
/// [`HttpError::BadUrl`]) for anything else, other schemes included.
pub fn host_key(url: &str) -> Option<String> {
    let url = url::Url::parse(url).ok()?;
    if !matches!(url.scheme(), "http" | "https") {
        return None;
    }
    let key = match url.host()? {
        url::Host::Domain(d) => d.trim_end_matches('.').to_ascii_lowercase(),
        url::Host::Ipv4(a) => a.to_string(),
        url::Host::Ipv6(a) => match a.to_ipv4_mapped() {
            Some(v4) => v4.to_string(),
            None => format!("[{a}]"),
        },
    };
    (!key.is_empty()).then_some(key)
}

/// The delay before retry after the `failures`th consecutive failure:
/// in `[full, 1.5 × full]` capped at `max`, where
/// `full = min(max, base × 2^(failures−1))`. So it's never below `base`
/// nor above `max`. `entropy` picks the point; any value keeps the bounds.
pub fn backoff_delay(b: Backoff, failures: u32, entropy: u64) -> Duration {
    if failures == 0 {
        return Duration::ZERO;
    }
    let max = b.max.max(b.base);
    let shift = failures - 1;
    let full = if shift >= 32 {
        max
    } else {
        b.base.checked_mul(1 << shift).map_or(max, |d| d.min(max))
    };
    let full_ns = full.as_nanos();
    // `full_ns` is below 2^94, so this can't overflow.
    let extra = ((full_ns / 2) * u128::from(entropy >> 32)) >> 32;
    let ns = (full_ns + extra).min(max.as_nanos());
    Duration::new((ns / 1_000_000_000) as u64, (ns % 1_000_000_000) as u32)
}

/// `now + d`, saturating ten years out instead of overflowing, for
/// durations a plugin or a user declared.
fn later(now: Instant, d: Duration) -> Instant {
    now.checked_add(d)
        .unwrap_or_else(|| now + Duration::from_secs(10 * 365 * 24 * 3600))
}

/// Why a request has to wait.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaitReason {
    /// The host's token bucket is empty.
    Rate,
    /// Backing off after failures.
    Backoff,
    /// The source refused us; don't retry before `until`.
    Blocked,
    /// The full-scrape quota for the last 24 h is spent.
    DailyCap,
    /// The unit was polled less than the poll interval ago.
    Poll,
    /// The limiter tracks [`MAX_HOSTS`] busy hosts (or polled units)
    /// already.
    Hosts,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Wait {
    pub until: Instant,
    pub reason: WaitReason,
}

/// A host's state as the Sources screen shows it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostState {
    Ready,
    BackingOff { until: Instant, failures: u32 },
    Blocked { until: Instant },
}

/// Hosts tracked at once, and separately polled units. Idle entries are
/// forgotten to make room, which loses nothing: an idle host's bucket is
/// full, and a host whose last failure is a whole back-off ceiling old
/// starts over as a first failure would.
pub const MAX_HOSTS: usize = 1024;

const DAY: Duration = Duration::from_secs(24 * 3600);

#[derive(Debug)]
struct Host {
    /// GCRA's theoretical arrival time: the bucket is full once `now`
    /// reaches it.
    tat: Instant,
    failures: u32,
    retry_at: Option<Instant>,
    blocked_until: Option<Instant>,
    /// Full-scrape grants in the last 24 h, oldest first.
    scrapes: VecDeque<Instant>,
    /// The poll unit whose failure was this host's last answer
    /// ([`HostLimiter::record_poll`]): a different unit failing next
    /// means the host itself is failing.
    unit_failed: Option<String>,
}

impl Host {
    fn new(now: Instant) -> Host {
        Host {
            tat: now,
            failures: 0,
            retry_at: None,
            blocked_until: None,
            scrapes: VecDeque::new(),
            unit_failed: None,
        }
    }

    fn idle(&mut self, now: Instant, forget: Duration) -> bool {
        while self.scrapes.front().is_some_and(|t| *t + DAY <= now) {
            self.scrapes.pop_front();
        }
        self.tat <= now
            && (self.failures == 0 || self.retry_at.is_none_or(|r| later(r, forget) <= now))
            && self.blocked_until.is_none_or(|u| u <= now)
            && self.scrapes.is_empty()
    }

    fn hold(&self, now: Instant) -> Option<Wait> {
        if let Some(until) = self.blocked_until.filter(|u| *u > now) {
            return Some(Wait {
                until,
                reason: WaitReason::Blocked,
            });
        }
        if let Some(until) = self.retry_at.filter(|u| *u > now) {
            return Some(Wait {
                until,
                reason: WaitReason::Backoff,
            });
        }
        None
    }
}

#[derive(Debug)]
struct Hosts {
    map: HashMap<String, Host>,
    /// When each polled unit may next be polled.
    polls: HashMap<String, Instant>,
    /// splitmix64 state for the back-off jitter.
    rng: u64,
}

impl Hosts {
    fn entropy(&mut self) -> u64 {
        self.rng = self.rng.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.rng;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// The host's entry for recording an answer. It always exists: a
    /// host that answered held a token a moment ago, so its outcome is
    /// never dropped for want of room. The map can pass [`MAX_HOSTS`]
    /// only by hosts with requests in flight, and shrinks back as they go
    /// idle. A poll unit's entry is made through the capped
    /// [`entry`](Self::entry) first ([`HostLimiter::record_poll`]), so
    /// units never pass it.
    fn entry_for_answer(&mut self, host: &str, now: Instant) -> &mut Host {
        self.map
            .entry(host.to_owned())
            .or_insert_with(|| Host::new(now))
    }

    /// The host's entry, making room if needed.
    fn entry(&mut self, host: &str, now: Instant, l: &Limits) -> Result<&mut Host, Wait> {
        let interval = l.per_host.interval;
        if !self.map.contains_key(host) {
            if self.map.len() >= MAX_HOSTS {
                let forget = l.backoff.max;
                self.map.retain(|_, h| !h.idle(now, forget));
            }
            if self.map.len() >= MAX_HOSTS {
                return Err(Wait {
                    until: later(now, interval.max(Duration::from_secs(1))),
                    reason: WaitReason::Hosts,
                });
            }
            self.map.insert(host.to_owned(), Host::new(now));
        }
        Ok(self.map.get_mut(host).expect("present"))
    }
}

/// Paces one plugin's requests per host. Cheap to clone; clones share
/// state.
#[derive(Debug, Clone)]
pub struct HostLimiter {
    limits: Limits,
    hosts: Arc<Mutex<Hosts>>,
}

impl HostLimiter {
    /// `limits` are raised to the global floors
    /// ([`Limits::floored`]) whatever their source.
    pub fn new(limits: Limits) -> HostLimiter {
        HostLimiter::with_seed(limits, RandomState::new().hash_one(0u8))
    }

    /// Deterministic jitter, for tests. Floored as [`new`](Self::new).
    pub fn with_seed(limits: Limits, seed: u64) -> HostLimiter {
        HostLimiter {
            limits: limits.floored(),
            hosts: Arc::new(Mutex::new(Hosts {
                map: HashMap::new(),
                polls: HashMap::new(),
                rng: seed,
            })),
        }
    }

    pub fn limits(&self) -> &Limits {
        &self.limits
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Hosts> {
        // Every update below is complete before it can panic.
        self.hosts.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Takes a token for `host` if one is free now, or says until when to
    /// wait. A refusal takes nothing.
    pub fn try_acquire(&self, host: &str, now: Instant) -> Result<(), Wait> {
        let rate = self.limits.per_host;
        let mut hosts = self.lock();
        let h = hosts.entry(host, now, &self.limits)?;
        if let Some(w) = h.hold(now) {
            return Err(w);
        }
        // GCRA: a full bucket is `tat <= now`; each grant moves `tat` on
        // by one interval, and up to `burst` grants may be outstanding.
        let tat = h.tat.max(now);
        let tolerance = rate.interval.saturating_mul(rate.burst.max(1) - 1);
        if tat > later(now, tolerance) {
            return Err(Wait {
                until: tat.checked_sub(tolerance).unwrap_or(now),
                reason: WaitReason::Rate,
            });
        }
        h.tat = later(tat, rate.interval);
        Ok(())
    }

    /// Waits for a token for `host`. Fails at once if the source has
    /// blocked us; the caller decides whether to wait that out. Cancel
    /// safe: dropping the future takes nothing.
    pub async fn acquire(&self, host: &str) -> Result<(), HttpError> {
        loop {
            match self.try_acquire(host, Instant::now()) {
                Ok(()) => return Ok(()),
                Err(Wait {
                    until,
                    reason: WaitReason::Blocked,
                }) => return Err(HttpError::Blocked { until }),
                Err(w) => sleep_until(w.until).await,
            }
        }
    }

    /// Takes one of the host's full scrapes for the last 24 h. With a
    /// quota of 0 it never grants.
    pub fn try_full_scrape(&self, host: &str, now: Instant) -> Result<(), Wait> {
        let max = self.limits.max_full_scrapes_per_day as usize;
        let mut hosts = self.lock();
        let h = hosts.entry(host, now, &self.limits)?;
        while h.scrapes.front().is_some_and(|t| *t + DAY <= now) {
            h.scrapes.pop_front();
        }
        if h.scrapes.len() >= max {
            let until = h.scrapes.front().map_or(now + DAY, |t| *t + DAY);
            return Err(Wait {
                until,
                reason: WaitReason::DailyCap,
            });
        }
        h.scrapes.push_back(now);
        Ok(())
    }

    /// Takes a poll of `unit` (a feed URL, an indexer id) if the poll
    /// interval has passed since the last one. Plugins call it before
    /// each poll, so no setting or bug polls faster than the cap.
    pub fn try_poll(&self, unit: &str, now: Instant) -> Result<(), Wait> {
        let interval = self.limits.poll_interval;
        let mut hosts = self.lock();
        if let Some(next) = hosts.polls.get(unit).copied().filter(|n| *n > now) {
            return Err(Wait {
                until: next,
                reason: WaitReason::Poll,
            });
        }
        if !hosts.polls.contains_key(unit) && hosts.polls.len() >= MAX_HOSTS {
            hosts.polls.retain(|_, n| *n > now);
            if hosts.polls.len() >= MAX_HOSTS {
                let until = hosts.polls.values().min().copied().unwrap_or(now);
                return Err(Wait {
                    until,
                    reason: WaitReason::Hosts,
                });
            }
        }
        hosts.polls.insert(unit.to_owned(), later(now, interval));
        Ok(())
    }

    /// The host answered normally: back-off resets.
    pub fn success(&self, host: &str) {
        if let Some(h) = self.lock().map.get_mut(host) {
            h.failures = 0;
            h.retry_at = None;
        }
    }

    /// The host failed: back off. Returns the delay chosen.
    pub fn failure(&self, host: &str, now: Instant) -> Duration {
        let mut hosts = self.lock();
        let entropy = hosts.entropy();
        let h = hosts.entry_for_answer(host, now);
        h.failures = h.failures.saturating_add(1);
        let d = backoff_delay(self.limits.backoff, h.failures, entropy);
        h.retry_at = Some(later(now, d));
        d
    }

    /// The source refused us: nothing goes to `host` before `until`. A
    /// later block only ever extends an earlier one.
    pub fn block(&self, host: &str, until: Instant, now: Instant) {
        let mut hosts = self.lock();
        let h = hosts.entry_for_answer(host, now);
        h.blocked_until = Some(h.blocked_until.map_or(until, |u| u.max(until)));
    }

    pub fn state(&self, host: &str, now: Instant) -> HostState {
        self.state_of(host, now)
    }

    /// A polled unit's own state: what its answers earned it
    /// ([`record_poll`](Self::record_poll)), apart from its host's.
    pub fn unit_state(&self, unit: &str, now: Instant) -> HostState {
        self.state_of(&unit_key(unit), now)
    }

    fn state_of(&self, key: &str, now: Instant) -> HostState {
        let hosts = self.lock();
        let Some(h) = hosts.map.get(key) else {
            return HostState::Ready;
        };
        match h.hold(now) {
            Some(Wait {
                until,
                reason: WaitReason::Blocked,
            }) => HostState::Blocked { until },
            Some(Wait { until, .. }) => HostState::BackingOff {
                until,
                failures: h.failures,
            },
            None => HostState::Ready,
        }
    }

    /// The hold on polled `unit`'s own pacing: a block or back-off its
    /// answers earned ([`record_poll`](Self::record_poll)).
    fn unit_hold(&self, unit: &str, now: Instant) -> Option<Wait> {
        self.lock().map.get(&unit_key(unit))?.hold(now)
    }

    /// Feeds the answer to a poll of `unit` on `host` into the pacing.
    /// What only that unit earned stays on it (SRC-14b): a 429 or a 503
    /// with `Retry-After` blocks the unit, and a 5xx or a 429 without one
    /// backs it off, so one indexer behind a manager that rate-limits us
    /// doesn't stop the manager's others. What says the host itself is
    /// failing stays on the host: a transport failure (connect, reset,
    /// timeout) and a 401 or 403 (a key the whole host refuses), as in
    /// [`record`](Self::record). A normal answer resets both.
    ///
    /// **Two different units failing in a row** (a 5xx or a rate limit,
    /// with no normal answer between) is the host failing, not one unit:
    /// the host backs off too, so a manager that's down behind a reverse
    /// proxy isn't asked once per unit per back-off step. And when the
    /// table has no room for the unit ([`MAX_HOSTS`]), the answer goes
    /// on the host, the safer side.
    pub fn record_poll(
        &self,
        host: &str,
        unit: &str,
        result: &Result<Response, HttpError>,
        now: Instant,
    ) {
        let res = match result {
            Ok(res) if !matches!(res.status, 401 | 403) => res,
            _ => return self.record(host, result, now),
        };
        let key = unit_key(unit);
        let verdict = classify(res);
        // The unit's entry, made within the cap (or the host takes it).
        if self.lock().entry(&key, now, &self.limits).is_err() {
            return self.record(host, result, now);
        }
        let host_failing = {
            let mut hosts = self.lock();
            let h = hosts.entry_for_answer(host, now);
            if verdict == Verdict::Ok {
                h.unit_failed = None;
                false
            } else {
                let other = h.unit_failed.as_deref().is_some_and(|u| u != unit);
                h.unit_failed = Some(unit.to_owned());
                other
            }
        };
        match verdict {
            Verdict::Ok => {
                self.success(host);
                self.success(&key);
            }
            Verdict::Retry => {
                self.failure(&key, now);
            }
            Verdict::Blocked(d) => {
                let backoff = self.failure(&key, now);
                let wait = d.map_or(self.limits.backoff.max, |d| d.max(backoff));
                self.block(&key, later(now, wait), now);
            }
        }
        if host_failing {
            self.failure(host, now);
        }
    }

    /// Feeds an answer (or its absence) into the host's pacing.
    pub fn record(&self, host: &str, result: &Result<Response, HttpError>, now: Instant) {
        match result {
            Ok(res) => match classify(res) {
                Verdict::Ok => self.success(host),
                Verdict::Retry => {
                    self.failure(host, now);
                }
                Verdict::Blocked(d) => {
                    let backoff = self.failure(host, now);
                    let wait = d.map_or(self.limits.backoff.max, |d| d.max(backoff));
                    self.block(host, later(now, wait), now);
                }
            },
            Err(HttpError::Transport(_)) => {
                self.failure(host, now);
            }
            // Not the host's fault (a kill switch, a missing client, a
            // block already recorded).
            Err(_) => {}
        }
    }
}

/// Where a polled unit's own back-off and blocks are kept in the
/// limiter's host table: a host key never holds a space, so the two
/// never meet. Units count toward [`MAX_HOSTS`] and are forgotten when
/// idle, as hosts are.
fn unit_key(unit: &str) -> String {
    format!("unit {unit}")
}

/// A [`Fetch`] behind a [`HostLimiter`]: the only way a plugin sends HTTP.
#[derive(Clone)]
pub struct Client<F = Arc<dyn Fetch>> {
    fetch: F,
    limiter: HostLimiter,
}

impl<F> std::fmt::Debug for Client<F> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Client")
            .field("limiter", &self.limiter)
            .finish_non_exhaustive()
    }
}

impl<F: Fetch> Client<F> {
    pub fn new(fetch: F, limiter: HostLimiter) -> Client<F> {
        Client { fetch, limiter }
    }

    pub fn limiter(&self) -> &HostLimiter {
        &self.limiter
    }

    /// One poll of `unit` (a feed URL, an indexer id): waits out the
    /// unit's own back-off and the poll gate ([`HostLimiter::try_poll`]),
    /// then sends as [`send`](Self::send) does, but records the answer
    /// with [`HostLimiter::record_poll`]: a rate limit or a 5xx backs off
    /// the unit, not its whole host. A blocked unit's poll is taken, then
    /// fails with [`HttpError::Blocked`], as a blocked host's does, so a
    /// plugin that retries at once is still paced by its poll interval.
    ///
    /// This is how a plugin polls. Each plugin's contract test checks the
    /// spacing of its polls from the replayer's timestamps (SRC-7's
    /// pattern), so a plugin that polls through `send` instead is caught.
    /// Cancel safe until the gate opens; after that the poll counts.
    pub async fn poll(&self, unit: &str, req: &Request) -> Result<Response, HttpError> {
        let host = req.host().ok_or(HttpError::BadUrl)?;
        loop {
            match self.limiter.try_poll(unit, Instant::now()) {
                Ok(()) => break,
                Err(w) => sleep_until(w.until).await,
            }
        }
        // After the poll is taken, so the poll interval paces a plugin
        // that retries a blocked unit at once.
        loop {
            match self.limiter.unit_hold(unit, Instant::now()) {
                None => break,
                Some(Wait {
                    until,
                    reason: WaitReason::Blocked,
                }) => return Err(HttpError::Blocked { until }),
                Some(w) => sleep_until(w.until).await,
            }
        }
        self.limiter.acquire(&host).await?;
        let result = self.fetch.fetch(req).await;
        self.limiter
            .record_poll(&host, unit, &result, Instant::now());
        result
    }

    /// Waits for the host's token, sends, and records the answer. A
    /// blocked host fails at once with [`HttpError::Blocked`].
    pub async fn send(&self, req: &Request) -> Result<Response, HttpError> {
        let host = req.host().ok_or(HttpError::BadUrl)?;
        self.limiter.acquire(&host).await?;
        let result = self.fetch.fetch(req).await;
        self.limiter.record(&host, &result, Instant::now());
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::HostRate;

    fn limits(interval: u64, burst: u32) -> Limits {
        Limits {
            poll_interval: Duration::from_secs(60),
            per_host: HostRate {
                interval: Duration::from_secs(interval),
                burst,
            },
            max_full_scrapes_per_day: 1,
            backoff: Backoff {
                base: Duration::from_secs(1),
                max: Duration::from_secs(60),
            },
        }
    }

    #[test]
    fn host_keys() {
        assert_eq!(
            host_key("https://Nyaa.SI./?page=rss").as_deref(),
            Some("nyaa.si")
        );
        assert_eq!(host_key("http://u:p@h:8080/x").as_deref(), Some("h"));
        assert_eq!(host_key("http://[::1]:9696/api").as_deref(), Some("[::1]"));
        assert_eq!(host_key("nyaa.si/rss"), None);
        assert_eq!(host_key("http://"), None);
        // WHATWG skips extra slashes, as reqwest does.
        assert_eq!(host_key("http:///x").as_deref(), Some("x"));
        assert_eq!(host_key("ftp://h/x"), None);
        // The key is the host the client connects to, however it's spelt.
        let k = |u| host_key(u).unwrap();
        assert_eq!(k("http://evil.example\\@nyaa.si/"), "evil.example");
        assert_eq!(k("http://ny%61a.si/"), "nyaa.si");
        assert_eq!(k("http://bücher.example/"), "xn--bcher-kva.example");
        for v4 in [
            "http://127.0.0.1/",
            "http://127.1/",
            "http://0x7f000001/",
            "http://2130706433/",
            "http://[::ffff:127.0.0.1]/",
        ] {
            assert_eq!(k(v4), "127.0.0.1", "{v4}");
        }
        assert_eq!(k("http://[0:0::1]/"), "[::1]");
    }

    #[test]
    fn retry_after() {
        assert_eq!(parse_retry_after(" 120 "), Some(Duration::from_secs(120)));
        assert_eq!(parse_retry_after("999999999999"), Some(MAX_RETRY_AFTER));
        assert_eq!(parse_retry_after("Wed, 21 Oct 2015 07:28:00 GMT"), None);
        assert_eq!(parse_retry_after("-1"), None);
    }

    #[test]
    fn verdicts() {
        let res = |status, ra: Option<&str>| Response {
            status,
            headers: ra
                .map(|v| vec![("Retry-After".to_owned(), v.to_owned())])
                .unwrap_or_default(),
            body: Vec::new(),
        };
        assert_eq!(classify(&res(200, None)), Verdict::Ok);
        assert_eq!(classify(&res(404, None)), Verdict::Ok);
        assert_eq!(classify(&res(429, None)), Verdict::Retry);
        assert_eq!(
            classify(&res(429, Some("30"))),
            Verdict::Blocked(Some(Duration::from_secs(30)))
        );
        assert_eq!(classify(&res(502, None)), Verdict::Retry);
        assert_eq!(classify(&res(403, None)), Verdict::Blocked(None));
    }

    #[tokio::test(start_paused = true)]
    async fn declared_limits_are_floored() {
        let wild = Limits {
            poll_interval: Duration::ZERO,
            per_host: HostRate {
                interval: Duration::ZERO,
                burst: u32::MAX,
            },
            max_full_scrapes_per_day: u32::MAX,
            backoff: Backoff {
                base: Duration::ZERO,
                max: Duration::ZERO,
            },
        };
        let l = HostLimiter::with_seed(wild, 1);
        assert_eq!(*l.limits(), wild.floored());
        let t0 = Instant::now();
        let granted = (0..100).filter(|_| l.try_acquire("h", t0).is_ok()).count();
        assert_eq!(granted, crate::manifest::floors::MAX_BURST as usize);
        assert!(l.failure("h", t0) >= crate::manifest::floors::BACKOFF_BASE);
    }

    #[tokio::test(start_paused = true)]
    async fn bucket_spaces_requests() {
        let l = HostLimiter::with_seed(limits(10, 2), 1);
        let t0 = Instant::now();
        assert!(l.try_acquire("h", t0).is_ok());
        assert!(l.try_acquire("h", t0).is_ok());
        let w = l.try_acquire("h", t0).unwrap_err();
        assert_eq!(w.reason, WaitReason::Rate);
        assert_eq!(w.until, t0 + Duration::from_secs(10));
        // Other hosts have their own bucket.
        assert!(l.try_acquire("other", t0).is_ok());
        assert!(l.try_acquire("h", w.until).is_ok());
    }

    #[tokio::test(start_paused = true)]
    async fn failures_back_off_and_success_resets() {
        let l = HostLimiter::with_seed(limits(0, 1), 7);
        let t0 = Instant::now();
        let d1 = l.failure("h", t0);
        assert!(d1 >= Duration::from_secs(1) && d1 <= Duration::from_millis(1500));
        let w = l.try_acquire("h", t0).unwrap_err();
        assert_eq!(w.reason, WaitReason::Backoff);
        assert!(matches!(
            l.state("h", t0),
            HostState::BackingOff { failures: 1, .. }
        ));
        l.success("h");
        assert!(l.try_acquire("h", t0).is_ok());
    }

    #[tokio::test(start_paused = true)]
    async fn block_stops_acquire() {
        let l = HostLimiter::with_seed(limits(0, 1), 7);
        let until = Instant::now() + Duration::from_secs(300);
        let now = Instant::now();
        l.block("h", until, now);
        l.block("h", until - Duration::from_secs(100), now);
        assert_eq!(l.acquire("h").await, Err(HttpError::Blocked { until }));
        assert_eq!(l.state("h", Instant::now()), HostState::Blocked { until });
        assert!(l.try_acquire("h", until).is_ok());
    }

    #[tokio::test(start_paused = true)]
    async fn full_scrape_quota() {
        let l = HostLimiter::with_seed(limits(0, 1), 7);
        let t0 = Instant::now();
        assert!(l.try_full_scrape("t", t0).is_ok());
        let w = l
            .try_full_scrape("t", t0 + Duration::from_secs(3600))
            .unwrap_err();
        assert_eq!(w.reason, WaitReason::DailyCap);
        assert_eq!(w.until, t0 + DAY);
        assert!(l.try_full_scrape("t", t0 + DAY).is_ok());
        let none = HostLimiter::with_seed(
            Limits {
                max_full_scrapes_per_day: 0,
                ..limits(0, 1)
            },
            7,
        );
        assert!(none.try_full_scrape("t", t0).is_err());
    }

    #[tokio::test(start_paused = true)]
    async fn idle_hosts_are_forgotten_busy_ones_kept() {
        let l = HostLimiter::with_seed(limits(10, 1), 7);
        let t0 = Instant::now();
        for i in 0..MAX_HOSTS {
            l.try_acquire(&i.to_string(), t0).unwrap();
        }
        // Every bucket is in use, so a new host waits.
        let w = l.try_acquire("new", t0).unwrap_err();
        assert_eq!(w.reason, WaitReason::Hosts);
        // Once they refill they're idle and make room.
        let later = t0 + Duration::from_secs(10);
        assert!(l.try_acquire("new", later).is_ok());
        assert!(l.lock().map.len() <= MAX_HOSTS);
    }

    #[tokio::test(start_paused = true)]
    async fn failed_hosts_are_forgotten_once_stale() {
        let l = HostLimiter::with_seed(limits(0, 1), 7);
        let t0 = Instant::now();
        for i in 0..MAX_HOSTS {
            l.failure(&i.to_string(), t0);
        }
        assert_eq!(
            l.try_acquire("new", t0).unwrap_err().reason,
            WaitReason::Hosts
        );
        // A back-off ceiling after the last retry time, they're forgotten.
        let later = t0 + Duration::from_secs(121);
        assert!(l.try_acquire("new", later).is_ok());
    }

    #[tokio::test(start_paused = true)]
    async fn polls_are_spaced_per_unit() {
        let l = HostLimiter::with_seed(limits(0, 1), 7);
        let t0 = Instant::now();
        assert!(l.try_poll("feed-a", t0).is_ok());
        let w = l
            .try_poll("feed-a", t0 + Duration::from_secs(59))
            .unwrap_err();
        assert_eq!(w.reason, WaitReason::Poll);
        assert_eq!(w.until, t0 + Duration::from_secs(60));
        assert!(l.try_poll("feed-b", t0).is_ok());
        assert!(l.try_poll("feed-a", w.until).is_ok());
    }

    #[tokio::test(start_paused = true)]
    async fn refusals_escalate() {
        let l = HostLimiter::with_seed(limits(0, 1), 7);
        let refuse = Ok(Response {
            status: 429,
            headers: vec![("Retry-After".into(), "0".into())],
            body: vec![],
        });
        let mut now = Instant::now();
        let mut last = Duration::ZERO;
        for _ in 0..8 {
            l.record("h", &refuse, now);
            let HostState::Blocked { until } = l.state("h", now) else {
                panic!("not blocked");
            };
            let wait = until - now;
            assert!(wait >= last / 2, "{wait:?} after {last:?}");
            last = wait;
            now = until;
        }
        // The ceiling (60 s) is reached and kept.
        assert!(last >= Duration::from_secs(30));
    }

    /// Answers by URL path: `/<status>`, `/<status>/<retry-after>`, or
    /// `/down` for a connection that failed.
    struct ByPath(std::sync::atomic::AtomicUsize);

    impl Fetch for ByPath {
        fn fetch<'a>(&'a self, req: &'a Request) -> BoxFuture<'a, Result<Response, HttpError>> {
            self.0.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let path = req.url.rsplit_once(".test/").map_or("", |(_, p)| p);
            let mut parts = path.split('/');
            let result = match parts.next() {
                Some("down") => Err(HttpError::Transport("connect".into())),
                Some(code) => Ok(Response {
                    status: code.parse().unwrap_or(200),
                    headers: parts
                        .next()
                        .map(|v| vec![("Retry-After".to_owned(), v.to_owned())])
                        .unwrap_or_default(),
                    body: vec![],
                }),
                None => unreachable!(),
            };
            Box::pin(async move { result })
        }
    }

    fn client() -> Client<ByPath> {
        Client::new(
            ByPath(Default::default()),
            HostLimiter::with_seed(limits(0, 1), 7),
        )
    }

    fn sent(c: &Client<ByPath>) -> usize {
        c.fetch.0.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// One indexer behind a manager that rate-limits us (or fails) stops
    /// only that indexer, not the manager's others (SRC-14b).
    #[tokio::test(start_paused = true)]
    async fn a_poll_units_rate_limit_and_5xx_stay_on_the_unit() {
        let c = client();
        let t0 = Instant::now();
        let res = c
            .poll("indexer-1", &Request::get("http://m.test/429/3600"))
            .await;
        assert_eq!(res.unwrap().status, 429);
        // The unit is blocked for the hour; its next poll isn't sent.
        let err = c
            .poll("indexer-1", &Request::get("http://m.test/200"))
            .await
            .unwrap_err();
        assert!(
            matches!(err, HttpError::Blocked { until } if until >= t0 + Duration::from_secs(3600))
        );
        assert_eq!(sent(&c), 1);
        // The host and the manager's other indexers go on.
        assert_eq!(
            c.limiter().state("m.test", Instant::now()),
            HostState::Ready
        );
        let res = c
            .poll("indexer-2", &Request::get("http://m.test/200"))
            .await;
        assert_eq!(res.unwrap().status, 200);
        // A 5xx backs off its own unit only.
        c.poll("indexer-3", &Request::get("http://m.test/502"))
            .await
            .unwrap();
        assert_eq!(
            c.limiter()
                .unit_hold("indexer-3", Instant::now())
                .map(|w| w.reason),
            Some(WaitReason::Backoff)
        );
        assert_eq!(
            c.limiter().state("m.test", Instant::now()),
            HostState::Ready
        );
        let before = sent(&c);
        c.poll("indexer-2", &Request::get("http://m.test/200"))
            .await
            .unwrap();
        assert_eq!(sent(&c), before + 1);
    }

    /// Two different units failing in a row is the host failing: it backs
    /// off too. A normal answer between them keeps it on the units.
    #[tokio::test(start_paused = true)]
    async fn different_units_failing_in_a_row_back_the_host_off() {
        let l = HostLimiter::with_seed(limits(0, 1), 7);
        let now = Instant::now();
        let answer = |status: u16| {
            Ok(Response {
                status,
                headers: vec![],
                body: vec![],
            })
        };
        l.record_poll("m", "a", &answer(502), now);
        // The same unit again is still that unit.
        l.record_poll("m", "a", &answer(502), now);
        l.record_poll("m", "b", &answer(200), now);
        l.record_poll("m", "c", &answer(503), now);
        assert_eq!(l.state("m", now), HostState::Ready);
        l.record_poll("m", "d", &answer(500), now);
        assert!(matches!(l.state("m", now), HostState::BackingOff { .. }));
    }

    /// Units never grow the table past [`MAX_HOSTS`]: without room, the
    /// answer goes on the host.
    #[tokio::test(start_paused = true)]
    async fn units_stay_within_the_table() {
        let l = HostLimiter::with_seed(limits(0, 1), 7);
        let now = Instant::now();
        let limited = Ok(Response {
            status: 429,
            headers: vec![("Retry-After".into(), "3600".into())],
            body: vec![],
        });
        for i in 0..MAX_HOSTS + 10 {
            l.record_poll("m", &format!("ix-{i}"), &limited, now);
        }
        assert!(
            l.lock().map.len() <= MAX_HOSTS + 1,
            "{}",
            l.lock().map.len()
        );
        assert!(matches!(l.state("m", now), HostState::Blocked { .. }));
    }

    /// What says the host is failing (a connection that fails, a key it
    /// refuses) still backs off or blocks the whole host.
    #[tokio::test(start_paused = true)]
    async fn host_wide_failures_in_a_poll_stay_on_the_host() {
        let c = client();
        let res = c
            .poll("indexer-1", &Request::get("http://m.test/down"))
            .await;
        assert!(matches!(res, Err(HttpError::Transport(_))));
        let now = Instant::now();
        assert!(matches!(
            c.limiter().state("m.test", now),
            HostState::BackingOff { failures: 1, .. }
        ));
        assert_eq!(c.limiter().unit_hold("indexer-1", now), None);
        // Another unit waits out the host's back-off before it's sent.
        let HostState::BackingOff { until, .. } = c.limiter().state("m.test", now) else {
            unreachable!()
        };
        c.poll("indexer-2", &Request::get("http://m.test/200"))
            .await
            .unwrap();
        assert!(Instant::now() >= until);
        // A 401 blocks the host for every unit.
        c.poll("indexer-3", &Request::get("http://m.test/401"))
            .await
            .unwrap();
        let err = c
            .poll("indexer-4", &Request::get("http://m.test/200"))
            .await
            .unwrap_err();
        assert!(matches!(err, HttpError::Blocked { .. }));
        assert!(matches!(
            c.limiter().state("m.test", Instant::now()),
            HostState::Blocked { .. }
        ));
    }

    #[tokio::test(start_paused = true)]
    async fn a_kill_switch_refusal_doesnt_back_the_host_off() {
        let l = HostLimiter::with_seed(limits(0, 1), 7);
        let now = Instant::now();
        for _ in 0..5 {
            l.record("h", &Err(HttpError::EgressDown("vpn".into())), now);
        }
        assert_eq!(l.state("h", now), HostState::Ready);
        // A transport failure does.
        l.record("h", &Err(HttpError::Transport("reset".into())), now);
        assert!(matches!(l.state("h", now), HostState::BackingOff { .. }));
    }
}
