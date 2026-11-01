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
}

impl std::fmt::Display for HttpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HttpError::BadUrl => f.write_str("URL has no host"),
            HttpError::Blocked { .. } => f.write_str("blocked by source"),
            HttpError::Transport(e) => write!(f, "transport: {e}"),
            HttpError::Unrecorded(e) => write!(f, "no recorded exchange: {e}"),
            HttpError::NoEgress => f.write_str("no HTTP egress configured for this plugin"),
        }
    }
}

impl std::error::Error for HttpError {}

/// Sends one request. SRC-5's client and the fixture replayer implement
/// it; plugins use it only through the [`Client`] in their
/// [`Kit`](crate::Kit).
///
/// **An implementation never follows redirects.** It returns the 3xx as
/// it is, so a plugin that follows one sends a new request through the
/// [`Client`], paced for the new host and checked against its egress.
pub trait Fetch: Send + Sync {
    fn fetch<'a>(&'a self, req: &'a Request) -> BoxFuture<'a, Result<Response, HttpError>>;
}

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

/// The host of `scheme://[user@]host[:port]/…`, lower-cased and without a
/// trailing dot. IPv6 literals keep their brackets.
///
/// `None` (so [`HttpError::BadUrl`]) for anything a URL parser might
/// read differently: a backslash, `%`, whitespace, control or non-ASCII
/// bytes before the path. A plugin sends plain ASCII URLs (IDNs in
/// punycode), so refusing the rest keeps the limiter's host the one the
/// client connects to.
pub fn host_key(url: &str) -> Option<String> {
    let (scheme, rest) = url.split_once("://")?;
    if scheme.is_empty()
        || !scheme
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"+-.".contains(&b))
    {
        return None;
    }
    let authority = rest.split(['/', '?', '#']).next()?;
    if authority
        .bytes()
        .any(|b| b == b'\\' || b == b'%' || !b.is_ascii_graphic())
    {
        return None;
    }
    let hostport = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    let host = if hostport.starts_with('[') {
        &hostport[..=hostport.find(']')?]
    } else {
        hostport.split(':').next()?
    };
    let host = host.trim_end_matches('.');
    if host.is_empty() {
        return None;
    }
    Some(host.to_ascii_lowercase())
}

/// The delay before retry after the `failures`th consecutive failure:
/// in `[full / 2, full]` where `full = min(max, base × 2^(failures−1))`.
/// `entropy` picks the point; any value keeps the bounds.
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
    let half = full_ns / 2;
    // `full_ns` is below 2^94, so this can't overflow.
    let extra = ((full_ns - half) * u128::from(entropy >> 32)) >> 32;
    let ns = half + extra;
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
}

impl Host {
    fn new(now: Instant) -> Host {
        Host {
            tat: now,
            failures: 0,
            retry_at: None,
            blocked_until: None,
            scrapes: VecDeque::new(),
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
    pub fn new(limits: Limits) -> HostLimiter {
        HostLimiter::with_seed(limits, RandomState::new().hash_one(0u8))
    }

    /// Deterministic jitter, for tests.
    pub fn with_seed(limits: Limits, seed: u64) -> HostLimiter {
        HostLimiter {
            limits,
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
        let Ok(h) = hosts.entry(host, now, &self.limits) else {
            return Duration::ZERO;
        };
        h.failures = h.failures.saturating_add(1);
        let d = backoff_delay(self.limits.backoff, h.failures, entropy);
        h.retry_at = Some(later(now, d));
        d
    }

    /// The source refused us: nothing goes to `host` before `until`. A
    /// later block only ever extends an earlier one.
    pub fn block(&self, host: &str, until: Instant, now: Instant) {
        let mut hosts = self.lock();
        if let Ok(h) = hosts.entry(host, now, &self.limits) {
            h.blocked_until = Some(h.blocked_until.map_or(until, |u| u.max(until)));
        }
    }

    pub fn state(&self, host: &str, now: Instant) -> HostState {
        let hosts = self.lock();
        let Some(h) = hosts.map.get(host) else {
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
            Err(_) => {}
        }
    }
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
        assert_eq!(host_key("http:///x"), None);
        // Anything a URL parser might read as another host is refused.
        assert_eq!(host_key("http://evil.example\\@nyaa.si/"), None);
        assert_eq!(host_key("http://ny%61a.si/"), None);
        assert_eq!(host_key("http://ny aa.si/"), None);
        assert_eq!(host_key("http://bücher.example/"), None);
        assert_eq!(host_key("ht tp://h/"), None);
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
        assert!(d1 >= Duration::from_millis(500) && d1 <= Duration::from_secs(1));
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
}
