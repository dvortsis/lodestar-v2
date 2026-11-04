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
//! A reply too large or too slow for one buffered body (a tracker's full
//! scrape) is read with [`Client::poll_stream`]: chunks are handed to the
//! plugin as they arrive, under the plugin's own [`StreamLimits`], which
//! the kit holds to [`STREAM_CEILING`].
//!
//! All times are `tokio::time::Instant`, so tests run on paused time.

use std::collections::hash_map::RandomState;
use std::collections::{HashMap, VecDeque};
use std::hash::BuildHasher;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::time::{Instant, sleep_until, timeout_at};

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

/// `Debug` is hand-written ([`Request`]'s impl): it never prints a
/// credential, a cookie, a login body or a token in the URL.
#[derive(Clone, PartialEq, Eq)]
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

/// `Debug` is hand-written, as [`Request`]'s: `Set-Cookie` and other
/// header values outside [`SHOWN_HEADERS`] are redacted, and the body is
/// shown as its length.
#[derive(Clone, PartialEq, Eq)]
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

/// The headers whose values `Debug` shows. Every other value (Cookie,
/// Authorization, an API-key header, a session header) prints as
/// `<redacted>`: a list of what is safe, so a header nobody thought of
/// stays hidden.
pub const SHOWN_HEADERS: &[&str] = &[
    "accept",
    "accept-encoding",
    "accept-language",
    "cache-control",
    "content-encoding",
    "content-length",
    "content-type",
    "date",
    "etag",
    "if-modified-since",
    "if-none-match",
    "last-modified",
    "location",
    "retry-after",
    "user-agent",
];

/// Headers as `Debug` shows them: every name, and a value only where
/// [`SHOWN_HEADERS`] lists the name.
struct RedactedHeaders<'a>(&'a [(String, String)]);

impl std::fmt::Debug for RedactedHeaders<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut list = f.debug_list();
        for (name, value) in self.0 {
            let shown = SHOWN_HEADERS.iter().any(|s| name.eq_ignore_ascii_case(s));
            if shown && !name.eq_ignore_ascii_case("location") {
                list.entry(&(name.as_str(), value.as_str()));
            } else if shown {
                // A redirect target may carry a token, as a URL does.
                list.entry(&(name.as_str(), redact_url(value)));
            } else {
                list.entry(&(name.as_str(), "<redacted>"));
            }
        }
        list.finish()
    }
}

/// A URL as `Debug` shows it: the scheme, host and port, with the path,
/// query and fragment (where trackers keep passkeys and APIs keep keys)
/// elided. Anything that doesn't parse is elided whole.
fn redact_url(url: &str) -> String {
    match url::Url::parse(url) {
        Ok(u) => lodestar_integrations::net::redact(&u),
        Err(_) => "<unparsed URL>".to_owned(),
    }
}

/// A body as `Debug` shows it: its length only. A login form's body is a
/// password.
struct BodyLen(usize);

impl std::fmt::Debug for BodyLen {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "<{} bytes>", self.0)
    }
}

impl std::fmt::Debug for Request {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Request")
            .field("method", &self.method)
            .field("url", &redact_url(&self.url))
            .field("headers", &RedactedHeaders(&self.headers))
            .field("body", &BodyLen(self.body.len()))
            .finish()
    }
}

impl std::fmt::Debug for Response {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Response")
            .field("status", &self.status)
            .field("headers", &RedactedHeaders(&self.headers))
            .field("body", &BodyLen(self.body.len()))
            .finish()
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
///   streams it ([`fetch_stream`](Fetch::fetch_stream)).
pub trait Fetch: Send + Sync {
    fn fetch<'a>(&'a self, req: &'a Request) -> BoxFuture<'a, Result<Response, HttpError>>;

    /// Sends one request and hands back its body as a stream of chunks,
    /// never the whole body at once. `limits` say how long the whole
    /// exchange may take, how many body bytes it may carry, and how long
    /// to wait for the headers once the request is admitted (past the
    /// egress's checks). The [`Client`] clamps them to [`STREAM_CEILING`];
    /// any other caller must clamp them itself, and `PluginEgress` does
    /// again. An implementation that opens connections gives its
    /// transport that timeout in place of its usual one. The [`Client`] enforces both
    /// limits again on the chunks it passes on, so an implementation that
    /// ignores them can't exceed them. The same contract as
    /// [`fetch`](Fetch::fetch) otherwise: no redirects followed, and the
    /// same egress, endpoint and kill-switch rules.
    ///
    /// The default buffers: it calls [`fetch`](Fetch::fetch) (so the body
    /// is at most [`MAX_BODY`], read within the implementation's own
    /// timeout) and hands it out in [`STREAM_CHUNK`]s. Fixtures use it
    /// as is.
    fn fetch_stream<'a>(
        &'a self,
        req: &'a Request,
        limits: StreamLimits,
    ) -> BoxFuture<'a, Result<StreamResponse, HttpError>> {
        let _ = limits;
        Box::pin(async move { self.fetch(req).await.map(StreamResponse::buffered) })
    }
}

/// The largest response body a [`Fetch`] returns.
pub const MAX_BODY: usize = 16 * 1024 * 1024;

impl<T: Fetch + ?Sized> Fetch for Arc<T> {
    fn fetch<'a>(&'a self, req: &'a Request) -> BoxFuture<'a, Result<Response, HttpError>> {
        (**self).fetch(req)
    }

    fn fetch_stream<'a>(
        &'a self,
        req: &'a Request,
        limits: StreamLimits,
    ) -> BoxFuture<'a, Result<StreamResponse, HttpError>> {
        (**self).fetch_stream(req, limits)
    }
}

/// How long a streamed exchange may take and how much it may carry. A
/// plugin declares its own (the full scrape's are
/// `full_scrape::STREAM`); the [`Client`] holds them to
/// [`STREAM_CEILING`] and [`STREAM_FLOOR`] whatever they say.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StreamLimits {
    /// Body bytes, all chunks together. Past it the stream fails.
    pub max_bytes: u64,
    /// The whole exchange, from sending to the last chunk, connecting
    /// included.
    pub timeout: Duration,
    /// The longest wait for the headers or for any one chunk: a reply that
    /// stalls fails then, not at the end of `timeout`.
    pub idle: Duration,
}

/// The most any plugin's stream may carry or take, whatever it declares.
pub const STREAM_CEILING: StreamLimits = StreamLimits {
    max_bytes: 1024 * 1024 * 1024,
    timeout: Duration::from_secs(3600),
    idle: Duration::from_secs(120),
};

// The kit's ceiling is the transport's: `net` enforces the same one on
// its own, so the two can't drift apart.
const _: () = assert!(
    STREAM_CEILING.max_bytes == lodestar_integrations::net::STREAM_CEILING_BYTES
        && STREAM_CEILING.timeout.as_secs()
            == lodestar_integrations::net::STREAM_CEILING_TIMEOUT.as_secs()
);

/// The least: a timeout or idle wait under a second would fail every
/// real reply.
pub const STREAM_FLOOR: StreamLimits = StreamLimits {
    max_bytes: 0,
    timeout: Duration::from_secs(1),
    idle: Duration::from_secs(1),
};

impl StreamLimits {
    /// Held to [`STREAM_FLOOR`]..=[`STREAM_CEILING`], with `idle` no
    /// longer than `timeout`.
    pub fn clamped(self) -> StreamLimits {
        let timeout = self
            .timeout
            .clamp(STREAM_FLOOR.timeout, STREAM_CEILING.timeout);
        StreamLimits {
            max_bytes: self.max_bytes.min(STREAM_CEILING.max_bytes),
            timeout,
            idle: self
                .idle
                .clamp(STREAM_FLOOR.idle, STREAM_CEILING.idle)
                .min(timeout),
        }
    }
}

/// Bytes per chunk when a buffered body is handed out as a stream.
pub const STREAM_CHUNK: usize = 64 * 1024;

/// A response body read a chunk at a time. Dropping it abandons the
/// rest of the reply.
pub trait BodyStream: Send {
    /// The next chunk, `Ok(None)` at the end. After an error or the end
    /// the stream isn't polled again.
    fn next_chunk(&mut self) -> BoxFuture<'_, Result<Option<Vec<u8>>, HttpError>>;
}

/// A whole body handed out in [`STREAM_CHUNK`]s, front first.
struct Buffered {
    body: Vec<u8>,
    at: usize,
}

impl BodyStream for Buffered {
    fn next_chunk(&mut self) -> BoxFuture<'_, Result<Option<Vec<u8>>, HttpError>> {
        let end = (self.at + STREAM_CHUNK).min(self.body.len());
        let chunk = (end > self.at).then(|| self.body[self.at..end].to_vec());
        self.at = end;
        if chunk.is_none() {
            // Done: give the memory back now, not when the stream drops.
            self.body = Vec::new();
            self.at = 0;
        }
        Box::pin(async move { Ok(chunk) })
    }
}

/// An answer whose body comes as a stream.
pub struct StreamResponse {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Box<dyn BodyStream>,
}

impl std::fmt::Debug for StreamResponse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StreamResponse")
            .field("status", &self.status)
            .field("headers", &RedactedHeaders(&self.headers))
            .finish_non_exhaustive()
    }
}

impl StreamResponse {
    /// A whole body as a stream (the default
    /// [`fetch_stream`](Fetch::fetch_stream)).
    pub fn buffered(res: Response) -> StreamResponse {
        StreamResponse {
            status: res.status,
            headers: res.headers,
            body: Box::new(Buffered {
                body: res.body,
                at: 0,
            }),
        }
    }

    /// The first header called `name`, ignoring ASCII case.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    /// The next chunk of the body (see [`BodyStream::next_chunk`]).
    pub async fn next_chunk(&mut self) -> Result<Option<Vec<u8>>, HttpError> {
        self.body.next_chunk().await
    }

    /// Status and headers, without the body, for the limiter.
    fn head(&self) -> Response {
        Response {
            status: self.status,
            headers: self.headers.clone(),
            body: Vec::new(),
        }
    }
}

/// The [`Client`]'s guard on a streamed body: the plugin's byte cap and
/// deadline, the idle wait per chunk, and the host's back-off when the
/// body fails part way. Fused: once it has failed it keeps failing, and
/// once it has ended it keeps ending, without polling the inner stream.
struct Guarded {
    /// Dropped as soon as the stream ends or fails (the cap, the
    /// deadline, a kill-switch abort), so its connection closes then.
    inner: Option<Box<dyn BodyStream>>,
    deadline: Instant,
    idle: Duration,
    remaining: u64,
    limiter: HostLimiter,
    host: String,
    state: Result<bool, HttpError>,
}

impl Guarded {
    async fn next(&mut self) -> Result<Option<Vec<u8>>, HttpError> {
        match &self.state {
            Err(e) => return Err(e.clone()),
            Ok(true) => return Ok(None),
            Ok(false) => {}
        }
        let r = self.read().await;
        if !matches!(r, Ok(Some(_))) {
            self.inner = None;
        }
        match &r {
            Ok(None) => self.state = Ok(true),
            Ok(Some(_)) => {}
            Err(e) => {
                // The body failing part way is the host failing, as it is
                // for a buffered body (a reset, a timeout, too large); a
                // kill switch's abort isn't.
                self.limiter
                    .record(&self.host, &Err(e.clone()), Instant::now());
                self.state = Err(e.clone());
            }
        }
        r
    }

    async fn read(&mut self) -> Result<Option<Vec<u8>>, HttpError> {
        let now = Instant::now();
        // Checked before polling: a stream that is always ready would
        // otherwise never meet the deadline.
        if now >= self.deadline {
            return Err(HttpError::Transport(TOO_SLOW.into()));
        }
        let wait = later(now, self.idle).min(self.deadline);
        let Some(inner) = self.inner.as_mut() else {
            return Ok(None);
        };
        let chunk = match timeout_at(wait, inner.next_chunk()).await {
            Ok(r) => r?,
            Err(_) if wait == self.deadline => {
                return Err(HttpError::Transport(TOO_SLOW.into()));
            }
            Err(_) => return Err(HttpError::Transport(STALLED.into())),
        };
        let Some(chunk) = chunk else {
            return Ok(None);
        };
        let len = chunk.len() as u64;
        if len > self.remaining {
            return Err(HttpError::Transport(TOO_LARGE.into()));
        }
        self.remaining -= len;
        Ok(Some(chunk))
    }
}

impl BodyStream for Guarded {
    fn next_chunk(&mut self) -> BoxFuture<'_, Result<Option<Vec<u8>>, HttpError>> {
        Box::pin(self.next())
    }
}

/// A streamed reply that ran past its plugin's timeout.
const TOO_SLOW: &str = "the reply took longer than the plugin's timeout";
/// One that waited past its idle limit for headers or a chunk.
pub(crate) const STALLED: &str = "the reply stalled";
/// One whose body passed the plugin's byte cap.
const TOO_LARGE: &str = "the reply is larger than the plugin's limit";

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
    /// means the host itself is failing. With it, the time its
    /// `Retry-After` asked for, if it gave one.
    unit_failed: Option<(String, Option<Instant>)>,
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
        // What this answer's `Retry-After` asked for, if anything.
        let asked = match verdict {
            Verdict::Blocked(Some(d)) => Some(later(now, d)),
            _ => None,
        };
        // `Some(previous unit's Retry-After)` when a different unit failed
        // just before this one: the host is failing.
        let host_failing = {
            let mut hosts = self.lock();
            let h = hosts.entry_for_answer(host, now);
            if verdict == Verdict::Ok {
                h.unit_failed = None;
                None
            } else {
                let other = h
                    .unit_failed
                    .take()
                    .filter(|(u, _)| u != unit)
                    .map(|(_, until)| until);
                h.unit_failed = Some((unit.to_owned(), asked));
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
        if let Some(previous) = host_failing {
            self.failure(host, now);
            // Both asked to wait: the host is rate-limiting us as a whole.
            // Honour it on the host, to the earlier of the two times, so
            // one unit's long `Retry-After` can't stretch the host's.
            if let (Some(a), Some(b)) = (previous, asked) {
                self.block(host, a.min(b), now);
            }
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
        self.gate_poll(unit, &host).await?;
        let result = self.fetch.fetch(req).await;
        self.limiter
            .record_poll(&host, unit, &result, Instant::now());
        result
    }

    /// One poll of `unit` with a streamed body: the same gates and pacing
    /// as [`poll`](Self::poll), but the body comes a chunk at a time, under
    /// `limits` held to [`STREAM_CEILING`] ([`StreamLimits::clamped`]).
    ///
    /// - The headers must come within `limits.timeout` (and within
    ///   `limits.idle` of the request being admitted, which the `Fetch`
    ///   enforces), each chunk within `limits.idle` of being asked for, and
    ///   the whole body within `limits.timeout` of sending. A reply that misses any of them fails
    ///   with [`HttpError::Transport`].
    /// - A `Content-Length` over `limits.max_bytes` fails before the body
    ///   is read; a body that grows past it fails at the chunk that
    ///   crosses it, which is never handed out.
    /// - The answer's status and headers are recorded as a poll's are. A
    ///   body that fails part way backs the host off, as a buffered body's
    ///   failure would; a kill-switch abort doesn't.
    ///
    /// Nothing is buffered here: each chunk is read only when the caller
    /// asks for it. Cancel safe until the gate opens; after that the poll
    /// counts.
    pub async fn poll_stream(
        &self,
        unit: &str,
        req: &Request,
        limits: StreamLimits,
    ) -> Result<StreamResponse, HttpError> {
        let host = req.host().ok_or(HttpError::BadUrl)?;
        let limits = limits.clamped();
        self.gate_poll(unit, &host).await?;
        let sent = Instant::now();
        let deadline = later(sent, limits.timeout);
        // The headers are bounded here by the whole deadline only. The
        // idle wait for them is the `Fetch`'s to start once it has
        // admitted the request (`PluginEgress` does), so a kill-switch
        // refusal paced while the route is down never reads as a stall
        // that backs the host off.
        let result = match timeout_at(deadline, self.fetch.fetch_stream(req, limits)).await {
            Ok(r) => r,
            // A backstop. `PluginEgress` answers well before it: its own
            // stall wait after admission, or a kill-switch refusal within
            // half the timeout. So reaching it says nothing about the
            // host, which isn't backed off; and a `Fetch` still admitting
            // may be the kill switch pacing it, so it reads as that.
            Err(_) => {
                return Err(HttpError::EgressDown(
                    "no answer within the plugin's timeout".into(),
                ));
            }
        };
        let res = match result {
            Ok(res) => res,
            Err(e) => {
                self.limiter
                    .record_poll(&host, unit, &Err(e.clone()), Instant::now());
                return Err(e);
            }
        };
        let declared = res
            .header("content-length")
            .and_then(|v| v.trim().parse::<u64>().ok());
        if declared.is_some_and(|n| n > limits.max_bytes) {
            let e = HttpError::Transport(TOO_LARGE.into());
            self.limiter
                .record_poll(&host, unit, &Err(e.clone()), Instant::now());
            return Err(e);
        }
        self.limiter
            .record_poll(&host, unit, &Ok(res.head()), Instant::now());
        let StreamResponse {
            status,
            headers,
            body,
        } = res;
        Ok(StreamResponse {
            status,
            headers,
            body: Box::new(Guarded {
                inner: Some(body),
                deadline,
                idle: limits.idle,
                remaining: limits.max_bytes,
                limiter: self.limiter.clone(),
                host,
                state: Ok(false),
            }),
        })
    }

    /// The gates in front of a poll ([`poll`](Self::poll)): the poll
    /// interval, then the unit's own hold, then the host's token.
    async fn gate_poll(&self, unit: &str, host: &str) -> Result<(), HttpError> {
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
        self.limiter.acquire(host).await
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

    /// A host that rate-limits every unit with `Retry-After` is blocked
    /// as a whole, until the earlier of the two times (runner, #399 B1).
    #[tokio::test(start_paused = true)]
    async fn a_host_wide_retry_after_blocks_the_host() {
        let l = HostLimiter::with_seed(limits(0, 1), 7);
        let now = Instant::now();
        let limited = |secs: &str| {
            Ok(Response {
                status: 429,
                headers: vec![("Retry-After".into(), secs.into())],
                body: vec![],
            })
        };
        l.record_poll("m", "a", &limited("3600"), now);
        assert_eq!(l.state("m", now), HostState::Ready);
        l.record_poll("m", "b", &limited("86400"), now);
        assert_eq!(
            l.state("m", now),
            HostState::Blocked {
                until: now + Duration::from_secs(3600)
            }
        );
        // A 5xx after a 429 backs the host off, but doesn't block it.
        let l = HostLimiter::with_seed(limits(0, 1), 7);
        l.record_poll("m", "a", &limited("3600"), now);
        let failing = Ok(Response {
            status: 502,
            headers: vec![],
            body: vec![],
        });
        l.record_poll("m", "b", &failing, now);
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

    // ---- Streamed bodies (SRC-12b) ----

    use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};

    /// A server that streams `chunks` chunks of `size` bytes, each after
    /// `every` on the paused clock (it paces the fixture, not the test),
    /// and counts the bytes it has produced. With `fail_at`, chunk
    /// `fail_at` is that error instead.
    #[derive(Clone)]
    struct Streamer {
        chunks: u64,
        size: usize,
        every: Duration,
        /// A `Content-Length` to declare.
        length: Option<u64>,
        fail_at: Option<(u64, HttpError)>,
        produced: Arc<AtomicU64>,
        asked: Arc<Mutex<Vec<StreamLimits>>>,
    }

    impl Streamer {
        fn new(chunks: u64, size: usize, every: Duration) -> Streamer {
            Streamer {
                chunks,
                size,
                every,
                length: None,
                fail_at: None,
                produced: Arc::default(),
                asked: Arc::default(),
            }
        }
    }

    struct StreamerBody {
        s: Streamer,
        sent: u64,
    }

    impl BodyStream for StreamerBody {
        fn next_chunk(&mut self) -> BoxFuture<'_, Result<Option<Vec<u8>>, HttpError>> {
            Box::pin(async move {
                tokio::time::sleep(self.s.every).await;
                if let Some((at, e)) = &self.s.fail_at
                    && *at == self.sent
                {
                    return Err(e.clone());
                }
                if self.sent == self.s.chunks {
                    return Ok(None);
                }
                self.sent += 1;
                self.s
                    .produced
                    .fetch_add(self.s.size as u64, AtomicOrdering::SeqCst);
                Ok(Some(vec![0x5a; self.s.size]))
            })
        }
    }

    impl Fetch for Streamer {
        fn fetch<'a>(&'a self, _: &'a Request) -> BoxFuture<'a, Result<Response, HttpError>> {
            Box::pin(async { Err(HttpError::Transport("streams only".into())) })
        }

        fn fetch_stream<'a>(
            &'a self,
            _: &'a Request,
            limits: StreamLimits,
        ) -> BoxFuture<'a, Result<StreamResponse, HttpError>> {
            self.asked.lock().unwrap().push(limits);
            let headers = self
                .length
                .map(|n| vec![("Content-Length".to_owned(), n.to_string())])
                .unwrap_or_default();
            let body = Box::new(StreamerBody {
                s: self.clone(),
                sent: 0,
            });
            Box::pin(async move {
                Ok(StreamResponse {
                    status: 200,
                    headers,
                    body,
                })
            })
        }
    }

    const MIB: usize = 1024 * 1024;

    fn stream_limits(max_bytes: u64, timeout: u64, idle: u64) -> StreamLimits {
        StreamLimits {
            max_bytes,
            timeout: Duration::from_secs(timeout),
            idle: Duration::from_secs(idle),
        }
    }

    fn stream_client(s: &Streamer) -> Client<Arc<dyn Fetch>> {
        let fetch: Arc<dyn Fetch> = Arc::new(s.clone());
        Client::new(fetch, HostLimiter::with_seed(limits(0, 1), 7))
    }

    /// Reads to the end; the bytes read, or the error.
    async fn drain(res: &mut StreamResponse) -> Result<u64, (u64, HttpError)> {
        let mut read = 0;
        loop {
            match res.next_chunk().await {
                Ok(Some(c)) => read += c.len() as u64,
                Ok(None) => return Ok(read),
                Err(e) => return Err((read, e)),
            }
        }
    }

    /// A large body comes a chunk at a time: the server is never asked
    /// for a chunk ahead of the one the caller reads, so nothing between
    /// them holds more than one chunk.
    #[tokio::test(start_paused = true)]
    async fn a_large_stream_is_never_read_ahead() {
        let s = Streamer::new(4096, 64 * 1024, Duration::ZERO);
        let c = stream_client(&s);
        let req = Request::get("http://t.test/scrape");
        let mut res = c
            .poll_stream("t", &req, stream_limits(u64::MAX, 600, 60))
            .await
            .unwrap();
        let mut read = 0u64;
        while let Some(chunk) = res.next_chunk().await.unwrap() {
            read += chunk.len() as u64;
            // Produced so far is exactly what was handed out.
            assert_eq!(s.produced.load(AtomicOrdering::SeqCst), read);
        }
        assert_eq!(read, 256 * MIB as u64);
        assert_eq!(
            c.limiter().state("t.test", Instant::now()),
            HostState::Ready
        );
    }

    /// A reply that dribbles in for five minutes, under its ten-minute
    /// timeout and with no gap over its idle limit, is read whole.
    #[tokio::test(start_paused = true)]
    async fn a_slow_stream_within_the_timeout_succeeds() {
        let s = Streamer::new(10, 1024, Duration::from_secs(30));
        let c = stream_client(&s);
        let t0 = Instant::now();
        let mut res = c
            .poll_stream(
                "t",
                &Request::get("http://t.test/x"),
                stream_limits(1 << 20, 600, 60),
            )
            .await
            .unwrap();
        assert_eq!(drain(&mut res).await, Ok(10 * 1024));
        assert!(Instant::now() - t0 >= Duration::from_secs(300));
        // The egress was handed the plugin's limits, so its transport can
        // allow that long.
        assert_eq!(
            s.asked.lock().unwrap().as_slice(),
            &[stream_limits(1 << 20, 600, 60)]
        );
    }

    /// Past the timeout the stream fails, fails again if asked, and the
    /// host backs off.
    #[tokio::test(start_paused = true)]
    async fn a_stream_over_its_timeout_fails_closed() {
        let s = Streamer::new(100, 1024, Duration::from_secs(30));
        let c = stream_client(&s);
        let t0 = Instant::now();
        let mut res = c
            .poll_stream(
                "t",
                &Request::get("http://t.test/x"),
                stream_limits(1 << 20, 120, 60),
            )
            .await
            .unwrap();
        let (read, e) = drain(&mut res).await.unwrap_err();
        assert_eq!(e, HttpError::Transport(TOO_SLOW.into()));
        // The chunks that came within the two minutes, and no more.
        assert!(read <= 4 * 1024, "{read}");
        assert_eq!(Instant::now() - t0, Duration::from_secs(120));
        let produced = s.produced.load(AtomicOrdering::SeqCst);
        assert_eq!(res.next_chunk().await, Err(e));
        assert_eq!(s.produced.load(AtomicOrdering::SeqCst), produced);
        assert!(matches!(
            c.limiter().state("t.test", Instant::now()),
            HostState::BackingOff { failures: 1, .. }
        ));
    }

    /// The inner stream (the connection) is dropped the moment the stream
    /// fails, not when the caller drops the response.
    #[tokio::test(start_paused = true)]
    async fn a_failed_stream_drops_its_connection_at_once() {
        struct Tracked(Arc<AtomicU64>);
        impl Drop for Tracked {
            fn drop(&mut self) {
                self.0.fetch_add(1, AtomicOrdering::SeqCst);
            }
        }
        /// Holds the connection (`Tracked`) for as long as it lives.
        struct Endless {
            _conn: Tracked,
        }
        impl BodyStream for Endless {
            fn next_chunk(&mut self) -> BoxFuture<'_, Result<Option<Vec<u8>>, HttpError>> {
                Box::pin(async { Ok(Some(vec![0; 1000])) })
            }
        }
        struct Source(Arc<AtomicU64>);
        impl Fetch for Source {
            fn fetch<'a>(&'a self, _: &'a Request) -> BoxFuture<'a, Result<Response, HttpError>> {
                unreachable!()
            }
            fn fetch_stream<'a>(
                &'a self,
                _: &'a Request,
                _: StreamLimits,
            ) -> BoxFuture<'a, Result<StreamResponse, HttpError>> {
                let body = Box::new(Endless {
                    _conn: Tracked(self.0.clone()),
                });
                Box::pin(async move {
                    Ok(StreamResponse {
                        status: 200,
                        headers: vec![],
                        body,
                    })
                })
            }
        }
        let dropped = Arc::new(AtomicU64::new(0));
        let c = Client::new(
            Source(dropped.clone()),
            HostLimiter::with_seed(limits(0, 1), 7),
        );
        let mut res = c
            .poll_stream(
                "t",
                &Request::get("http://t.test/x"),
                stream_limits(2500, 60, 60),
            )
            .await
            .unwrap();
        let (read, e) = drain(&mut res).await.unwrap_err();
        assert_eq!((read, e), (2000, HttpError::Transport(TOO_LARGE.into())));
        assert_eq!(dropped.load(AtomicOrdering::SeqCst), 1, "still held");
    }

    /// A gap over the idle limit fails then, not at the timeout's end.
    #[tokio::test(start_paused = true)]
    async fn a_stalled_stream_fails_at_its_idle_limit() {
        let s = Streamer::new(3, 1024, Duration::from_secs(90));
        let c = stream_client(&s);
        let t0 = Instant::now();
        let mut res = c
            .poll_stream(
                "t",
                &Request::get("http://t.test/x"),
                stream_limits(1 << 20, 600, 60),
            )
            .await
            .unwrap();
        let (read, e) = drain(&mut res).await.unwrap_err();
        assert_eq!((read, e), (0, HttpError::Transport(STALLED.into())));
        assert_eq!(Instant::now() - t0, Duration::from_secs(60));
    }

    /// Headers that never come fail the poll at the deadline. (The idle
    /// wait for them is the egress's, after admission: see
    /// `egress_tests`.)
    #[tokio::test(start_paused = true)]
    async fn headers_that_never_come_fail_closed_at_the_deadline() {
        struct Silent;
        impl Fetch for Silent {
            fn fetch<'a>(&'a self, _: &'a Request) -> BoxFuture<'a, Result<Response, HttpError>> {
                Box::pin(std::future::pending())
            }
        }
        let c = Client::new(Silent, HostLimiter::with_seed(limits(0, 1), 7));
        let t0 = Instant::now();
        let e = c
            .poll_stream(
                "t",
                &Request::get("http://t.test/x"),
                stream_limits(1 << 20, 600, 45),
            )
            .await
            .unwrap_err();
        assert!(matches!(e, HttpError::EgressDown(_)), "{e:?}");
        assert_eq!(Instant::now() - t0, Duration::from_secs(600));
        // Not the host's fault for all the kit can tell: no back-off.
        assert_eq!(
            c.limiter().state("t.test", Instant::now()),
            HostState::Ready
        );
    }

    /// A body that grows past the cap fails at the chunk that crosses it,
    /// which is never handed out; a declared length over it fails before
    /// any body is read.
    #[tokio::test(start_paused = true)]
    async fn a_stream_over_its_cap_fails_closed() {
        let s = Streamer::new(10, 1000, Duration::ZERO);
        let c = stream_client(&s);
        let mut res = c
            .poll_stream(
                "t",
                &Request::get("http://t.test/x"),
                stream_limits(3500, 600, 60),
            )
            .await
            .unwrap();
        let (read, e) = drain(&mut res).await.unwrap_err();
        assert_eq!((read, e), (3000, HttpError::Transport(TOO_LARGE.into())));
        // Exactly the cap is fine.
        let s = Streamer::new(4, 1000, Duration::ZERO);
        let mut res = stream_client(&s)
            .poll_stream(
                "t",
                &Request::get("http://t.test/x"),
                stream_limits(4000, 600, 60),
            )
            .await
            .unwrap();
        assert_eq!(drain(&mut res).await, Ok(4000));
        // Declared too large: refused before the body, and the host backs
        // off as it would for a buffered body over `MAX_BODY`.
        let mut s = Streamer::new(10, 1000, Duration::ZERO);
        s.length = Some(10_000);
        let c = stream_client(&s);
        let e = c
            .poll_stream(
                "t",
                &Request::get("http://t.test/x"),
                stream_limits(3500, 600, 60),
            )
            .await
            .unwrap_err();
        assert_eq!(e, HttpError::Transport(TOO_LARGE.into()));
        assert_eq!(s.produced.load(AtomicOrdering::SeqCst), 0);
        assert!(matches!(
            c.limiter().state("t.test", Instant::now()),
            HostState::BackingOff { .. }
        ));
    }

    /// No declaration loosens the ceiling, and none goes under the floor.
    #[tokio::test(start_paused = true)]
    async fn stream_limits_are_held_to_the_ceiling() {
        let wild = stream_limits(u64::MAX, u64::MAX / 4, u64::MAX / 4);
        assert_eq!(wild.clamped(), STREAM_CEILING);
        let tiny = stream_limits(0, 0, 0).clamped();
        assert_eq!(tiny.timeout, STREAM_FLOOR.timeout);
        assert_eq!(tiny.idle, STREAM_FLOOR.idle);
        // Idle is never longer than the whole timeout.
        assert_eq!(
            stream_limits(10, 5, 60).clamped().idle,
            Duration::from_secs(5)
        );
        // What the egress is handed is the clamped value.
        let s = Streamer::new(0, 0, Duration::ZERO);
        stream_client(&s)
            .poll_stream("t", &Request::get("http://t.test/x"), wild)
            .await
            .unwrap();
        assert_eq!(s.asked.lock().unwrap().as_slice(), &[STREAM_CEILING]);
    }

    /// A streamed poll is gated and recorded as a poll is: a second poll
    /// of the unit waits out the poll interval.
    #[tokio::test(start_paused = true)]
    async fn a_streamed_poll_is_paced_as_a_poll() {
        let s = Streamer::new(1, 10, Duration::ZERO);
        let c = stream_client(&s);
        let req = Request::get("http://t.test/x");
        let lim = stream_limits(100, 60, 60);
        let t0 = Instant::now();
        c.poll_stream("t", &req, lim).await.unwrap();
        c.poll_stream("t", &req, lim).await.unwrap();
        assert!(Instant::now() - t0 >= Duration::from_secs(60));
        // A blocked host isn't sent to.
        c.limiter()
            .block("t.test", Instant::now() + DAY, Instant::now());
        let before = s.asked.lock().unwrap().len();
        let e = c
            .poll_stream("u", &Request::get("http://t.test/y"), lim)
            .await
            .unwrap_err();
        assert!(matches!(e, HttpError::Blocked { .. }));
        assert_eq!(s.asked.lock().unwrap().len(), before);
        assert_eq!(
            c.poll_stream("t", &Request::get("nope"), lim)
                .await
                .unwrap_err(),
            HttpError::BadUrl
        );
    }

    /// A kill switch's abort mid-body ends the stream without backing the
    /// host off; a reset mid-body backs it off.
    #[tokio::test(start_paused = true)]
    async fn a_body_failing_part_way_is_recorded_like_a_buffered_one() {
        for (err, backs_off) in [
            (HttpError::EgressDown("vpn".into()), false),
            (HttpError::Transport("reset".into()), true),
        ] {
            let mut s = Streamer::new(10, 100, Duration::ZERO);
            s.fail_at = Some((2, err.clone()));
            let c = stream_client(&s);
            let mut res = c
                .poll_stream(
                    "t",
                    &Request::get("http://t.test/x"),
                    stream_limits(1 << 20, 60, 60),
                )
                .await
                .unwrap();
            assert_eq!(drain(&mut res).await, Err((200, err.clone())));
            let state = c.limiter().state("t.test", Instant::now());
            assert_eq!(
                matches!(state, HostState::BackingOff { .. }),
                backs_off,
                "{err}"
            );
        }
    }

    /// A [`Fetch`] without its own streaming hands its buffered body out
    /// in chunks, so fixtures and [`NoEgress`] work unchanged.
    #[tokio::test(start_paused = true)]
    async fn the_default_stream_chunks_a_buffered_body() {
        struct Whole;
        impl Fetch for Whole {
            fn fetch<'a>(&'a self, _: &'a Request) -> BoxFuture<'a, Result<Response, HttpError>> {
                Box::pin(async {
                    Ok(Response {
                        status: 200,
                        headers: vec![],
                        body: (0..STREAM_CHUNK * 2 + 5).map(|i| i as u8).collect(),
                    })
                })
            }
        }
        let fetch: Arc<dyn Fetch> = Arc::new(Whole);
        let c = Client::new(fetch, HostLimiter::with_seed(limits(0, 1), 7));
        let mut res = c
            .poll_stream(
                "t",
                &Request::get("http://t.test/x"),
                stream_limits(1 << 20, 60, 60),
            )
            .await
            .unwrap();
        let mut sizes = Vec::new();
        let mut all = Vec::new();
        while let Some(chunk) = res.next_chunk().await.unwrap() {
            sizes.push(chunk.len());
            all.extend_from_slice(&chunk);
        }
        assert_eq!(sizes, [STREAM_CHUNK, STREAM_CHUNK, 5]);
        assert!(all.iter().enumerate().all(|(i, b)| *b == i as u8));
        let none = Client::new(NoEgress, HostLimiter::with_seed(limits(0, 1), 7));
        assert_eq!(
            none.poll_stream(
                "t",
                &Request::get("http://t.test/x"),
                stream_limits(1, 1, 1)
            )
            .await
            .unwrap_err(),
            HttpError::NoEgress
        );
    }

    // ---- Debug never prints secrets ----

    #[test]
    fn request_debug_redacts_credentials_bodies_and_url_tokens() {
        let mut req = Request::get(
            "https://u:hunter2@idx.example:8443/api/passkey0123456789?apikey=SECRETKEY#frag",
        )
        .header("Cookie", "session=SECRETCOOKIE")
        .header("authorization", "Bearer SECRETTOKEN")
        .header("Proxy-Authorization", "Basic SECRETPROXY")
        .header("X-Api-Key", "SECRETAPIKEY")
        .header("User-Agent", "lodestar")
        .header("Content-Type", "application/x-www-form-urlencoded");
        req.method = Method::Post;
        req.body = b"username=me&password=SECRETPASSWORD".to_vec();
        let shown = format!("{req:?}");
        for secret in [
            "hunter2",
            "passkey0123456789",
            "SECRETKEY",
            "frag",
            "SECRETCOOKIE",
            "SECRETTOKEN",
            "SECRETPROXY",
            "SECRETAPIKEY",
            "SECRETPASSWORD",
            "password",
        ] {
            assert!(!shown.contains(secret), "{secret} in {shown}");
        }
        for kept in [
            "Post",
            "https://idx.example:8443/…",
            "Cookie",
            "authorization",
            "<redacted>",
            "lodestar",
            "application/x-www-form-urlencoded",
            "<35 bytes>",
        ] {
            assert!(shown.contains(kept), "{kept} not in {shown}");
        }
        // The alternate form redacts too.
        let pretty = format!("{req:#?}");
        assert!(
            !pretty.contains("SECRET") && !pretty.contains("hunter2"),
            "{pretty}"
        );
        // A URL that doesn't parse is elided whole.
        assert!(!format!("{:?}", Request::get("not a url SECRET")).contains("SECRET"));
    }

    #[test]
    fn response_debug_redacts_cookies_and_bodies() {
        let res = Response {
            status: 302,
            headers: vec![
                ("Set-Cookie".into(), "sid=SECRETSESSION".into()),
                (
                    "Location".into(),
                    "https://h.example/login?token=SECRETTOKEN".into(),
                ),
                ("Retry-After".into(), "120".into()),
            ],
            body: b"SECRETBODY".to_vec(),
        };
        let shown = format!("{res:?}");
        assert!(!shown.contains("SECRET"), "{shown}");
        for kept in [
            "302",
            "Set-Cookie",
            "https://h.example/…",
            "120",
            "<10 bytes>",
        ] {
            assert!(shown.contains(kept), "{kept} not in {shown}");
        }
        let streamed = StreamResponse::buffered(res);
        assert!(!format!("{streamed:?}").contains("SECRET"));
    }
}
