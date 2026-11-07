//! The adapter traits every companion's detection, health and
//! configuration code implements (plan 2026-11-06-v06-companions §4.3,
//! CMP-0): CMP-8a implements [`Detect`] and [`Health`], CMP-4a's
//! supervisor and CMP-5g's watch loop call [`Health`], and CMP-8b
//! implements [`Configure`]. Defining them here lets those slices run at
//! the same time.
//!
//! **An adapter reaches a companion only through [`GuardedHttp`].** It's
//! handed in by the caller (`crates/lodestar` wires the SSRF guard's
//! companion mode behind it, CMP-8a), so an adapter never holds a raw
//! client. Every request carries its [`Limits`], a timeout and a byte cap
//! bounded by this module, and a [`ProbeResponse`] can't be built over
//! the cap.
//!
//! **Everything an adapter reads from a companion is untrusted**: a
//! companion's API is a third-party program, and a URL you enter can
//! point anywhere. So the traits pass back only closed, bounded values: a
//! [`Version`] parsed by its strict grammar, a status from a closed set,
//! and errors that carry the code's own words, never response text.
//!
//! The traits are object-safe (`Box<dyn Detect>`) without a macro crate:
//! async methods return a boxed future, as `lodestar-api`'s `Ops` does.

use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use lodestar_integrations::secret::Secret;

use crate::inputs::{CompanionId, ContainerName, InputError, PortNumber, Version};

/// A boxed, sendable future.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// The longest URL you may enter for a companion, in bytes.
pub const ENTERED_URL_MAX: usize = 2048;

/// Where an adapter looks for a companion (plan Q12: never a range scan).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Target {
    /// `127.0.0.1` on one of the catalogue's ports.
    Loopback(PortNumber),
    /// A catalogue container name on Docker's network, on one of the
    /// catalogue's ports.
    Container(ContainerName, PortNumber),
    /// A URL you entered.
    Entered(EnteredUrl),
}

/// Whether `b` may appear in a URL at all (RFC 3986: unreserved,
/// reserved and `%`). Refuses space, `"`, `<`, `>`, `\`, `^`, `` ` ``,
/// `{`, `|`, `}` and anything outside printable ASCII.
fn uri_char(b: u8) -> bool {
    b.is_ascii_alphanumeric()
        || matches!(
            b,
            b'-' | b'.'
                | b'_'
                | b'~'
                | b':'
                | b'/'
                | b'?'
                | b'#'
                | b'['
                | b']'
                | b'@'
                | b'!'
                | b'$'
                | b'&'
                | b'\''
                | b'('
                | b')'
                | b'*'
                | b'+'
                | b','
                | b';'
                | b'='
                | b'%'
        )
}

/// Every `%` is followed by two hex digits.
fn percent_ok(s: &str) -> bool {
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' {
            if i + 2 >= b.len() {
                return false;
            }
            if !(b[i + 1].is_ascii_hexdigit() && b[i + 2].is_ascii_hexdigit()) {
                return false;
            }
            i += 3;
        } else {
            i += 1;
        }
    }
    true
}

/// A URL you entered for a companion.
///
/// **Checked for shape only:**
/// - `http` or `https`;
/// - RFC 3986 characters only, with well-formed `%` escapes, and no query
///   or fragment: it's a base that request paths are joined to;
/// - a host of letters, digits, `.` and `-`, or a bracketed IPv6 address;
/// - no user information;
/// - a port, if given, of `1..=65535`;
/// - at most [`ENTERED_URL_MAX`] bytes.
///
/// **Where it points isn't checked here:** every request to it goes through
/// [`GuardedHttp`], whose SSRF guard decides at connect time (CMP-8a).
///
/// **Never rendered into a template.** It's a probe target only. CMP-7's
/// compose and Unraid output is built from [`crate::inputs`] alone.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct EnteredUrl {
    text: String,
    https: bool,
}

impl EnteredUrl {
    pub fn parse(s: &str) -> Result<EnteredUrl, InputError> {
        if s.is_empty() || s.len() > ENTERED_URL_MAX {
            return Err(InputError::Length);
        }
        // A base URL with a query or fragment would swallow the request
        // path joined to it (`http://h/#` + `/api` stays `http://h/`).
        if !s.bytes().all(uri_char) || !percent_ok(s) || s.contains(['?', '#']) {
            return Err(InputError::Character);
        }
        let lower = s.to_ascii_lowercase();
        let (https, rest) = if let Some(r) = lower.strip_prefix("https://") {
            (true, r)
        } else if let Some(r) = lower.strip_prefix("http://") {
            (false, r)
        } else {
            return Err(InputError::Character);
        };
        let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
        // `user:pass@host` would put a credential where it gets logged and
        // shown; it's refused rather than stripped.
        if authority.contains('@') {
            return Err(InputError::Character);
        }
        let (host, port) = if let Some(v6) = authority.strip_prefix('[') {
            let (addr, after) = v6.split_once(']').ok_or(InputError::Character)?;
            if addr.is_empty()
                || !addr
                    .bytes()
                    .all(|b| b.is_ascii_hexdigit() || b == b':' || b == b'.')
            {
                return Err(InputError::Character);
            }
            let port = match after {
                "" => None,
                p => Some(p.strip_prefix(':').ok_or(InputError::Character)?),
            };
            (addr, port)
        } else {
            match authority.split_once(':') {
                Some((h, p)) => (h, Some(p)),
                None => (authority, None),
            }
        };
        if host.is_empty() {
            return Err(InputError::Length);
        }
        // The base path follows the same rules as a request path, so
        // joining the two can't reach elsewhere.
        let base_path = &rest[authority.len()..];
        if !base_path.is_empty() {
            path_ok(base_path)?;
        }
        if !authority.starts_with('[')
            && !host
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-')
        {
            return Err(InputError::Character);
        }
        if let Some(p) = port {
            if p.is_empty() || p.len() > 5 || !p.bytes().all(|b| b.is_ascii_digit()) {
                return Err(InputError::Range);
            }
            match p.parse::<u32>() {
                Ok(1..=65535) => {}
                _ => return Err(InputError::Range),
            }
        }
        Ok(EnteredUrl {
            text: s.to_owned(),
            https,
        })
    }

    pub fn as_str(&self) -> &str {
        &self.text
    }

    pub fn is_https(&self) -> bool {
        self.https
    }
}

/// Shown through `lodestar_log::Url` and the API, never raw, so `Debug`
/// keeps only the scheme.
impl fmt::Debug for EnteredUrl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let scheme = if self.https { "https" } else { "http" };
        write!(f, "EnteredUrl({scheme}://…)")
    }
}

/// The shortest timeout a request may have.
pub const TIMEOUT_MIN: Duration = Duration::from_millis(100);
/// The longest timeout a request may have.
pub const TIMEOUT_MAX: Duration = Duration::from_secs(30);
/// The largest answer a request may read, in bytes (1 MiB).
pub const BODY_CAP_MAX: usize = 1 << 20;

/// A request's bounds: the time it may take in all, and the most body it
/// may read. Built only within [`TIMEOUT_MIN`]..=[`TIMEOUT_MAX`] and
/// `1..=`[`BODY_CAP_MAX`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    timeout: Duration,
    body_cap: usize,
}

impl Limits {
    /// For detection and health probes: 5 s, 64 KiB.
    pub const PROBE: Limits = Limits {
        timeout: Duration::from_secs(5),
        body_cap: 64 * 1024,
    };

    pub fn new(timeout: Duration, body_cap: usize) -> Result<Limits, InputError> {
        if !(TIMEOUT_MIN..=TIMEOUT_MAX).contains(&timeout)
            || !(1..=BODY_CAP_MAX).contains(&body_cap)
        {
            return Err(InputError::Range);
        }
        Ok(Limits { timeout, body_cap })
    }

    pub fn timeout(&self) -> Duration {
        self.timeout
    }

    pub fn body_cap(&self) -> usize {
        self.body_cap
    }
}

/// An HTTP method an adapter may use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    Get,
    Post,
}

/// A request path and query (`/api/v2/app/version`,
/// `/api/v1/system/status?x=1`): `/` first, RFC 3986 characters, well-formed
/// `%` escapes, no fragment, no `//`, no `.` or `..` segment (encoded or
/// not), no `;` and no escaped `;`, `?`, `#`, `/`, `\`, `%` or byte from
/// `0x80` in the path, no escaped control byte anywhere, at most 2048
/// bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestPath(String);

/// The bytes a path's `%` escapes stand for. Call after [`percent_ok`],
/// so every `%` starts a full escape.
fn escaped_bytes(s: &str) -> impl Iterator<Item = u8> + '_ {
    s.as_bytes()
        .windows(3)
        .filter(|w| w[0] == b'%')
        .filter_map(|w| u8::from_str_radix(std::str::from_utf8(&w[1..]).ok()?, 16).ok())
}

/// A control byte, which no path or query may carry even escaped: `%00`
/// ends a string in some servers, and `%0d%0a` splits a line once decoded.
fn control(b: u8) -> bool {
    b < 0x20 || b == 0x7f
}

/// The checks a path (without its query) passes, shared by
/// [`RequestPath`] and [`EnteredUrl`]'s base path.
///
/// - No `;` (path parameters like `/a/..;/b` that some servers strip).
/// - No escape, in either case, of a byte that ends or splits a path once
///   decoded: `;` (`%3b`), `?` (`%3f`), `#` (`%23`), `/` (`%2f`), `\`
///   (`%5c`), `%` itself (`%25`: a server that decodes twice would turn
///   `%252e` into `.`), or a control byte (`%00`–`%1f`, `%7f`).
/// - No encoded byte from `0x80`: that covers overlong and fullwidth
///   spellings of `.` and `/` (`%c0%ae`, `%ef%bc%8e`), which some servers
///   normalise.
/// - No `//`, and no `.` or `..` segment, plain or with `%2e`.
fn path_ok(path: &str) -> Result<(), InputError> {
    if path.contains(';')
        || escaped_bytes(path).any(|b| {
            matches!(b, b';' | b'?' | b'#' | b'/' | b'\\' | b'%') || control(b) || b >= 0x80
        })
    {
        return Err(InputError::Character);
    }
    let lower = path.to_ascii_lowercase();
    let decoded = lower.replace("%2e", ".");
    if decoded.contains("//") || decoded.split('/').any(|g| g == "." || g == "..") {
        return Err(InputError::Component);
    }
    Ok(())
}

impl RequestPath {
    pub fn parse(s: &str) -> Result<RequestPath, InputError> {
        if s.is_empty() || s.len() > 2048 {
            return Err(InputError::Length);
        }
        if !s.starts_with('/') {
            return Err(InputError::NotAbsolute);
        }
        if !s.bytes().all(uri_char) || s.contains(['#', '[', ']', '@']) || !percent_ok(s) {
            return Err(InputError::Character);
        }
        // No control byte anywhere, escaped or not (`%00`, `%0d%0a`).
        if escaped_bytes(s).any(control) {
            return Err(InputError::Character);
        }
        path_ok(s.split('?').next().unwrap_or(""))?;
        Ok(RequestPath(s.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Header names an adapter may never set: message framing, the target,
/// and hop-by-hop headers. The client sets those itself.
const RESERVED_HEADERS: &[&str] = &[
    "host",
    "content-length",
    "transfer-encoding",
    "connection",
    "keep-alive",
    "te",
    "trailer",
    "upgrade",
    "expect",
    "http2-settings",
    "proxy",
];

const fn eq_ignore_case(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut i = 0;
    while i < a.len() {
        if !a[i].eq_ignore_ascii_case(&b[i]) {
            return false;
        }
        i += 1;
    }
    true
}

/// A request header's name, from the adapter's own constants: an HTTP
/// token of letters, digits and `-`, at most 64 bytes, and not one of the
/// framing or hop-by-hop names (`Host`, `Content-Length`,
/// `Transfer-Encoding`, `Connection`, …) in any case.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HeaderName(&'static str);

impl HeaderName {
    pub const fn new(name: &'static str) -> Option<HeaderName> {
        let b = name.as_bytes();
        if b.is_empty() || b.len() > 64 {
            return None;
        }
        let mut i = 0;
        while i < b.len() {
            if !(b[i].is_ascii_alphanumeric() || b[i] == b'-') {
                return None;
            }
            i += 1;
        }
        // Every `Proxy-*` header is the client's.
        if b.len() >= 6 {
            let mut p = 0;
            let mut is_proxy = true;
            while p < 6 {
                if !b[p].eq_ignore_ascii_case(&b"proxy-"[p]) {
                    is_proxy = false;
                }
                p += 1;
            }
            if is_proxy {
                return None;
            }
        }
        let mut r = 0;
        while r < RESERVED_HEADERS.len() {
            if eq_ignore_case(b, RESERVED_HEADERS[r].as_bytes()) {
                return None;
            }
            r += 1;
        }
        Some(HeaderName(name))
    }

    pub fn as_str(&self) -> &'static str {
        self.0
    }
}

/// The longest header value accepted, in bytes.
pub const HEADER_VALUE_MAX: usize = 4096;

/// A request header's value, checked when it's built: a plain value and a
/// credential alike hold only tab, space and printable characters (no CR,
/// LF, NUL or other control), so no value can end its line and start
/// another header. A credential stays a [`Secret`] until the client writes
/// it.
pub struct HeaderValue(HeaderInner);

enum HeaderInner {
    Plain(String),
    Secret(Secret),
}

/// A field value's bytes (RFC 9110 §5.5): tab, space, visible ASCII and
/// bytes from `0x80`; never CR, LF, NUL, another control or DEL.
fn field_value(s: &str) -> Result<(), InputError> {
    if s.len() > HEADER_VALUE_MAX {
        return Err(InputError::Length);
    }
    if !s.bytes().all(|b| b == b'\t' || (b >= 0x20 && b != 0x7f)) {
        return Err(InputError::Character);
    }
    Ok(())
}

impl HeaderValue {
    pub fn plain(s: &str) -> Result<HeaderValue, InputError> {
        field_value(s)?;
        Ok(HeaderValue(HeaderInner::Plain(s.to_owned())))
    }

    /// A credential: refused, like a plain value, if it holds CR, LF, NUL
    /// or another control character.
    pub fn secret(secret: Secret) -> Result<HeaderValue, InputError> {
        field_value(secret.expose())?;
        Ok(HeaderValue(HeaderInner::Secret(secret)))
    }

    pub fn is_secret(&self) -> bool {
        matches!(self.0, HeaderInner::Secret(_))
    }

    /// The value, for the [`GuardedHttp`] implementation that writes it.
    pub fn expose(&self) -> &str {
        match &self.0 {
            HeaderInner::Plain(s) => s,
            HeaderInner::Secret(s) => s.expose(),
        }
    }
}

/// Values may carry a session or a token, so `Debug` shows only their kind.
impl fmt::Debug for HeaderValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(if self.is_secret() {
            "Secret(…)"
        } else {
            "Plain(…)"
        })
    }
}

/// One request to a companion, built with [`ProbeRequest::new`].
#[derive(Debug)]
#[non_exhaustive]
pub struct ProbeRequest<'a> {
    pub target: &'a Target,
    pub method: Method,
    pub path: RequestPath,
    pub headers: Vec<(HeaderName, HeaderValue)>,
    pub body: Option<Vec<u8>>,
    pub limits: Limits,
}

impl<'a> ProbeRequest<'a> {
    /// A `GET` with no headers.
    pub fn new(target: &'a Target, path: RequestPath, limits: Limits) -> ProbeRequest<'a> {
        ProbeRequest {
            target,
            method: Method::Get,
            path,
            headers: Vec::new(),
            body: None,
            limits,
        }
    }

    /// Makes it a `POST` with this body.
    pub fn post(mut self, body: Vec<u8>) -> ProbeRequest<'a> {
        self.method = Method::Post;
        self.body = Some(body);
        self
    }

    pub fn header(mut self, name: HeaderName, value: HeaderValue) -> ProbeRequest<'a> {
        self.headers.push((name, value));
        self
    }
}

/// A companion's answer: its status and a body no larger than the
/// request's cap. Fields are private, so the cap can't be skipped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeResponse {
    status: u16,
    body: Vec<u8>,
}

impl ProbeResponse {
    /// Refuses a body over `limits.body_cap()` ([`AdapterError::TooLarge`])
    /// and a status outside `100..=599`.
    pub fn new(status: u16, body: Vec<u8>, limits: &Limits) -> Result<ProbeResponse, AdapterError> {
        if body.len() > limits.body_cap() {
            return Err(AdapterError::TooLarge);
        }
        if !(100..=599).contains(&status) {
            return Err(AdapterError::Protocol("HTTP status out of range"));
        }
        Ok(ProbeResponse { status, body })
    }

    pub fn status(&self) -> u16 {
        self.status
    }

    pub fn body(&self) -> &[u8] {
        &self.body
    }
}

/// The only way an adapter talks to a companion.
///
/// Implementations (CMP-8a, wired in by `crates/lodestar`) must:
/// - connect only to the request's target, through the SSRF guard's
///   companion mode. Loopback and container targets are fixed; an entered
///   URL is checked at connect time;
/// - not follow a redirect to anywhere but the same target;
/// - stop reading at `limits.body_cap() + 1` bytes and fail with
///   [`AdapterError::TooLarge`];
/// - fail with [`AdapterError::Timeout`] once `limits.timeout()` has passed
///   for the whole request;
/// - send nothing identifying beyond the request's own headers, and keep no
///   cookies or state between requests.
pub trait GuardedHttp: Send + Sync {
    fn send<'a>(
        &'a self,
        request: ProbeRequest<'a>,
    ) -> BoxFuture<'a, Result<ProbeResponse, AdapterError>>;
}

/// Why an adapter call failed. The variants carry the code's own words
/// (`&'static str`), never text a companion sent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum AdapterError {
    /// Nothing answered in time.
    Timeout,
    /// The connection was refused or the host couldn't be reached.
    Unreachable,
    /// The SSRF guard refused the address.
    Blocked,
    /// The answer passed the byte cap.
    TooLarge,
    /// It answered, but isn't the companion asked for.
    NotThisCompanion,
    /// It refused the credentials.
    Unauthorized,
    /// A credential Lodestar holds couldn't be read (see
    /// [`CredentialError`]).
    Credentials,
    /// It answered with something the adapter couldn't read.
    Protocol(&'static str),
    /// A setting couldn't be applied.
    Refused(&'static str),
}

impl fmt::Display for AdapterError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AdapterError::Timeout => f.write_str("timed out"),
            AdapterError::Unreachable => f.write_str("unreachable"),
            AdapterError::Blocked => f.write_str("address not allowed"),
            AdapterError::TooLarge => f.write_str("answer too large"),
            AdapterError::NotThisCompanion => f.write_str("not the expected companion"),
            AdapterError::Unauthorized => f.write_str("credentials refused"),
            AdapterError::Credentials => f.write_str("a stored credential couldn't be read"),
            AdapterError::Protocol(what) => write!(f, "unexpected answer: {what}"),
            AdapterError::Refused(what) => write!(f, "setting refused: {what}"),
        }
    }
}

impl std::error::Error for AdapterError {}

/// A companion found at a target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Detected {
    pub target: Target,
    /// Its version, when it reports one in the strict [`Version`] form.
    pub version: Option<Version>,
}

/// One health check's result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HealthStatus {
    /// It answered its health endpoint as expected.
    Healthy,
    /// It answered, but not as a healthy companion does.
    Unhealthy,
    /// Nothing answered (or the answer was refused before it was read).
    Unreachable,
}

/// Finds a companion at a target, through `http` only.
pub trait Detect: Send + Sync {
    /// The catalogue entry it detects.
    fn companion(&self) -> &CompanionId;

    /// `Ok(None)` when the target answered but isn't this companion, or
    /// nothing is there.
    fn detect<'a>(
        &'a self,
        http: &'a dyn GuardedHttp,
        target: &'a Target,
    ) -> BoxFuture<'a, Result<Option<Detected>, AdapterError>>;
}

/// Checks a running companion's health, through `http` only. The caller
/// sets the cadence (plan Q7).
pub trait Health: Send + Sync {
    fn companion(&self) -> &CompanionId;

    fn check<'a>(
        &'a self,
        http: &'a dyn GuardedHttp,
        target: &'a Target,
    ) -> BoxFuture<'a, HealthStatus>;
}

/// A setting's name, as a configuration plan lists it and the log records
/// it (never its value): `[a-z][a-z0-9_.]{0,63}`, like
/// `webui.bind_address`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SettingKey(&'static str);

impl SettingKey {
    /// For the adapters' own constants. Returns `None` for a name outside
    /// the grammar; adapters test their keys once.
    pub const fn new(key: &'static str) -> Option<SettingKey> {
        let b = key.as_bytes();
        if b.is_empty() || b.len() > 64 || !b[0].is_ascii_lowercase() {
            return None;
        }
        let mut i = 0;
        while i < b.len() {
            let c = b[i];
            if !(c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'_' || c == b'.') {
                return None;
            }
            i += 1;
        }
        Some(SettingKey(key))
    }

    pub fn as_str(&self) -> &'static str {
        self.0
    }
}

/// What a configuration run will change, shown to you before it runs
/// (plan §4.3, CMP-5g) and logged by name as it's applied.
///
/// Only [`Configure::plan`] makes one: the constructor is crate-private, so
/// no caller can forge a plan for a companion or add a change to one.
/// Tests elsewhere build fakes with `ConfigPlan::for_tests`, which exists
/// only under this crate's `testing` feature.
///
/// ```compile_fail,E0624
/// use lodestar_companions::{adapter::ConfigPlan, CompanionId};
/// let _ = ConfigPlan::new(CompanionId::parse("prowlarr").unwrap(), Vec::new());
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigPlan {
    companion: CompanionId,
    changes: Vec<SettingKey>,
}

impl ConfigPlan {
    /// For this crate's adapters (CMP-8b). Only the tests and
    /// [`ConfigPlan::for_tests`] call it until CMP-8b's adapters land.
    #[cfg_attr(
        not(any(test, feature = "testing")),
        expect(dead_code, reason = "CMP-8b's adapters are its first callers")
    )]
    pub(crate) fn new(companion: CompanionId, changes: Vec<SettingKey>) -> ConfigPlan {
        ConfigPlan { companion, changes }
    }

    /// For other crates' `tests/` fakes (CMP-5g). Only under the `testing`
    /// feature, which only dev-dependencies enable, so a shipped build
    /// has no way to make a plan outside [`Configure::plan`].
    #[cfg(feature = "testing")]
    pub fn for_tests(companion: CompanionId, changes: Vec<SettingKey>) -> ConfigPlan {
        ConfigPlan::new(companion, changes)
    }

    pub fn companion(&self) -> &CompanionId {
        &self.companion
    }

    pub fn changes(&self) -> &[SettingKey] {
        &self.changes
    }
}

/// A stored credential couldn't be read. Distinct from "there is none",
/// so a broken store is never mistaken for a missing credential.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum CredentialError {
    /// The secret store couldn't be read, or the value didn't unseal.
    Unavailable,
}

impl fmt::Display for CredentialError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("the stored credential couldn't be read")
    }
}

impl std::error::Error for CredentialError {}

impl From<CredentialError> for AdapterError {
    fn from(_: CredentialError) -> AdapterError {
        AdapterError::Credentials
    }
}

/// The credentials a configuration run may use, by field (`password`,
/// `api_key`). CMP-3 implements it over sealed settings rows.
pub trait Credentials: Send + Sync {
    /// `Ok(None)` when none is stored; an error when the store failed.
    fn get(&self, field: &str) -> Result<Option<Secret>, CredentialError>;
}

/// What [`Configure`] is given beyond the target. Built with
/// [`ConfigureInput::new`]; later slices add fields (the egress route,
/// CMP-8b) without breaking implementations.
#[non_exhaustive]
pub struct ConfigureInput<'a> {
    pub credentials: &'a dyn Credentials,
}

impl<'a> ConfigureInput<'a> {
    pub fn new(credentials: &'a dyn Credentials) -> ConfigureInput<'a> {
        ConfigureInput { credentials }
    }
}

impl fmt::Debug for ConfigureInput<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ConfigureInput").finish_non_exhaustive()
    }
}

/// A plan that [`dyn Configure::apply`](Configure) has checked belongs to
/// the companion it's applied to. Only this module can make one, so an
/// implementation's `apply_plan` can't be reached with an unchecked plan.
#[derive(Debug, Clone, Copy)]
pub struct CheckedPlan<'a> {
    plan: &'a ConfigPlan,
}

impl<'a> CheckedPlan<'a> {
    pub fn plan(&self) -> &'a ConfigPlan {
        self.plan
    }
}

/// Configures a companion through its own API, through `http` only. Each
/// change is idempotent: applying the same plan twice leaves the same
/// state.
///
/// Callers apply a plan with `apply`, an inherent method on
/// `dyn Configure` that implementations can't override. It refuses another
/// companion's plan, then calls `apply_plan` with a [`CheckedPlan`].
pub trait Configure: Send + Sync {
    fn companion(&self) -> &CompanionId;

    /// The settings `apply` would change, without changing anything.
    fn plan<'a>(
        &'a self,
        http: &'a dyn GuardedHttp,
        target: &'a Target,
        input: &'a ConfigureInput<'a>,
    ) -> BoxFuture<'a, Result<ConfigPlan, AdapterError>>;

    /// What implementations write: applies the checked plan, changing
    /// nothing it doesn't list.
    fn apply_plan<'a>(
        &'a self,
        http: &'a dyn GuardedHttp,
        target: &'a Target,
        input: &'a ConfigureInput<'a>,
        plan: CheckedPlan<'a>,
    ) -> BoxFuture<'a, Result<(), AdapterError>>;
}

impl dyn Configure + '_ {
    /// Applies a plan from [`Configure::plan`]. A plan made for another
    /// companion is refused before anything is sent.
    pub fn apply<'a>(
        &'a self,
        http: &'a dyn GuardedHttp,
        target: &'a Target,
        input: &'a ConfigureInput<'a>,
        plan: &'a ConfigPlan,
    ) -> BoxFuture<'a, Result<(), AdapterError>> {
        if plan.companion != *self.companion() {
            return Box::pin(async {
                Err(AdapterError::Refused("the plan is for another companion"))
            });
        }
        self.apply_plan(http, target, input, CheckedPlan { plan })
    }
}

/// The same `apply` for `Arc<dyn Configure + Send + Sync>` and the like,
/// which is a distinct type to the compiler.
impl dyn Configure + Send + Sync + '_ {
    pub fn apply<'a>(
        &'a self,
        http: &'a dyn GuardedHttp,
        target: &'a Target,
        input: &'a ConfigureInput<'a>,
        plan: &'a ConfigPlan,
    ) -> BoxFuture<'a, Result<(), AdapterError>> {
        let this: &'a (dyn Configure + 'a) = self;
        this.apply(http, target, input, plan)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[test]
    fn entered_urls() {
        for ok in [
            "http://192.168.1.10:8080",
            "https://qbit.example.lan/",
            "HTTP://Box:9696/prowlarr",
            "http://[::1]:8080/x",
            "http://[fe80::1]",
            "http://host:65535/a%20b",
            "http://host/a@b",
        ] {
            assert!(EnteredUrl::parse(ok).is_ok(), "{ok}");
        }
        let cases: &[(&str, InputError)] = &[
            ("", InputError::Length),
            ("ftp://x", InputError::Character),
            ("file:///etc/passwd", InputError::Character),
            ("javascript:alert(1)", InputError::Character),
            ("http://user:pass@host/", InputError::Character),
            ("http://@host/", InputError::Character),
            ("http:///path", InputError::Length),
            ("http://", InputError::Length),
            ("http://:8080", InputError::Length),
            ("http://[]:8080", InputError::Character),
            ("http://host/x?y=1", InputError::Character),
            ("http://host/#", InputError::Character),
            ("http://host?", InputError::Character),
            ("http://host/a/../b", InputError::Component),
            ("http://host/a/%2e%2e/b", InputError::Component),
            ("http://host/a%00", InputError::Character),
            ("http://host//x", InputError::Component),
            ("http://host/a;b", InputError::Character),
            ("http://host/a%3bb", InputError::Character),
            ("http://host/a%3B", InputError::Character),
            ("http://host/a%3f", InputError::Character),
            ("http://host/a%23", InputError::Character),
            ("http://host/a%0d%0a", InputError::Character),
            ("http://[::1", InputError::Character),
            ("http://[::1]x", InputError::Character),
            ("http://[::1]:0", InputError::Range),
            ("http://host:0", InputError::Range),
            ("http://host:99999", InputError::Range),
            ("http://host:", InputError::Range),
            ("http://host:80a", InputError::Range),
            ("http://host:1:2", InputError::Range),
            ("http://a b", InputError::Character),
            ("http://a\nb", InputError::Character),
            ("http://é", InputError::Character),
            ("http://host\\@evil/", InputError::Character),
            ("http://host/\"x", InputError::Character),
            ("http://host/<x>", InputError::Character),
            ("http://host/{x}", InputError::Character),
            ("http://host/a|b", InputError::Character),
            ("http://host/a^b", InputError::Character),
            ("http://host/a`b", InputError::Character),
            ("http://host/%zz", InputError::Character),
            ("http://host/%4", InputError::Character),
            ("http://host/%", InputError::Character),
            ("http://ho_st/", InputError::Character),
            ("http://ho%73t/", InputError::Character),
        ];
        for (s, e) in cases {
            assert_eq!(EnteredUrl::parse(s), Err(*e), "{s:?}");
        }
        assert_eq!(
            EnteredUrl::parse(&format!("http://a/{}", "b".repeat(ENTERED_URL_MAX))),
            Err(InputError::Length)
        );
        let u = EnteredUrl::parse("https://secret-host.lan/").unwrap();
        assert!(u.is_https());
        assert_eq!(format!("{u:?}"), "EnteredUrl(https://…)");
        let u = EnteredUrl::parse("HTTP://secret-host.lan/").unwrap();
        assert!(!u.is_https());
        assert!(!format!("{u:?}").contains("secret"));
    }

    #[test]
    fn percent_escapes() {
        assert!(percent_ok("a%20b%2F"));
        assert!(!percent_ok("%"));
        assert!(!percent_ok("%2"));
        assert!(!percent_ok("a%g0"));
        assert!(percent_ok(""));
    }

    #[test]
    fn limits_are_bounded() {
        assert_eq!(Limits::PROBE.timeout(), Duration::from_secs(5));
        assert_eq!(Limits::PROBE.body_cap(), 64 * 1024);
        assert!(Limits::new(TIMEOUT_MIN, 1).is_ok());
        assert!(Limits::new(TIMEOUT_MAX, BODY_CAP_MAX).is_ok());
        assert_eq!(Limits::new(Duration::ZERO, 10), Err(InputError::Range));
        assert_eq!(
            Limits::new(Duration::from_secs(31), 10),
            Err(InputError::Range)
        );
        assert_eq!(Limits::new(TIMEOUT_MAX, 0), Err(InputError::Range));
        assert_eq!(
            Limits::new(TIMEOUT_MAX, BODY_CAP_MAX + 1),
            Err(InputError::Range)
        );
        // A response can't be built over the cap.
        let small = Limits::new(TIMEOUT_MIN, 4).unwrap();
        assert!(ProbeResponse::new(200, vec![0; 4], &small).is_ok());
        assert_eq!(
            ProbeResponse::new(200, vec![0; 5], &small),
            Err(AdapterError::TooLarge)
        );
        assert!(ProbeResponse::new(99, vec![], &small).is_err());
        assert!(ProbeResponse::new(600, vec![], &small).is_err());
    }

    #[test]
    fn request_paths_and_headers() {
        for ok in [
            "/",
            "/ping",
            "/api/v2/app/version",
            "/api/v1/x?a=1&b=%20",
            // In the query a `;` or its escape is data, not a path
            // parameter.
            "/api/v1/x?a=%3b&b=%3F",
            "/a%20b/%41",
            "/a..b/c",
        ] {
            assert!(RequestPath::parse(ok).is_ok(), "{ok}");
        }
        let cases: &[(&str, InputError)] = &[
            ("", InputError::Length),
            ("ping", InputError::NotAbsolute),
            ("//evil.example/x", InputError::Component),
            ("/a/../b", InputError::Component),
            ("/a/./b", InputError::Component),
            ("/a/%2e%2e/b", InputError::Component),
            ("/a/%2E./b", InputError::Component),
            ("/x#frag", InputError::Character),
            ("/x\r\nHost: evil", InputError::Character),
            ("/x y", InputError::Character),
            ("/@evil", InputError::Character),
            ("/a%2f..%2fb", InputError::Character),
            ("/a%2F..", InputError::Character),
            ("/a/..;/b", InputError::Character),
            ("/a;x/b", InputError::Character),
            ("/a/%2e%2e%5c", InputError::Character),
            ("/a/%00", InputError::Character),
            ("/a?x=%00", InputError::Character),
            ("/a/%252e%252e/b", InputError::Character),
            // Overlong and fullwidth spellings of `.` and `/`.
            ("/a/%c0%ae%c0%ae/b", InputError::Character),
            ("/a/%ef%bc%8e%ef%bc%8e/b", InputError::Character),
            ("/a/%C0%AF", InputError::Character),
            ("/a/%80", InputError::Character),
            // An escape is refused wherever its literal byte would be, in
            // either case: `;`, and the other bytes that end or split a
            // path once decoded.
            ("/a/..%3b/b", InputError::Character),
            ("/a%3Bx/b", InputError::Character),
            ("/a%3f/../b", InputError::Character),
            ("/a%3F", InputError::Character),
            ("/a%23", InputError::Character),
            ("/a%5C", InputError::Character),
            ("/a%2f", InputError::Character),
            // Controls, escaped, anywhere: the path or the query.
            ("/a%0d%0aHost:%20evil", InputError::Character),
            ("/a%0A", InputError::Character),
            ("/a%1f", InputError::Character),
            ("/a%7F", InputError::Character),
            ("/a?x=%0d%0a", InputError::Character),
            ("/a?x=%7f", InputError::Character),
        ];
        for (s, e) in cases {
            assert_eq!(RequestPath::parse(s), Err(*e), "{s:?}");
        }
        assert!(HeaderName::new("X-Api-Key").is_some());
        assert!(HeaderName::new("Bad Header").is_none());
        assert!(HeaderName::new("X:y").is_none());
        assert!(HeaderName::new("").is_none());
        // Framing, the target and hop-by-hop names are the client's, in any
        // case.
        for reserved in [
            "Host",
            "HOST",
            "Content-Length",
            "content-length",
            "Transfer-Encoding",
            "Connection",
            "Keep-Alive",
            "Proxy-Authorization",
            "TE",
            "Trailer",
            "Upgrade",
            "Expect",
        ] {
            let leaked: &'static str = Box::leak(reserved.to_owned().into_boxed_str());
            assert!(HeaderName::new(leaked).is_none(), "{reserved}");
        }
        assert!(HeaderName::new("Hosts").is_some());
        assert!(HeaderName::new("Proxy").is_none());
        assert!(HeaderName::new("HTTP2-Settings").is_none());
        assert!(HeaderName::new("Proxyish").is_some());
        assert!(HeaderName::new("X-Proxy-Id").is_some());
        assert!(HeaderValue::plain("application/json").is_ok());
        assert!(HeaderValue::plain("a\tb é").is_ok());
        for bad in ["a\r\nb", "a\nb", "a\rb", "a\0b", "a\u{7f}b", "a\u{1b}b"] {
            assert_eq!(
                HeaderValue::plain(bad).unwrap_err(),
                InputError::Character,
                "{bad:?}"
            );
            assert_eq!(
                HeaderValue::secret(Secret::new(bad)).unwrap_err(),
                InputError::Character,
                "{bad:?}"
            );
        }
        assert_eq!(
            HeaderValue::secret(Secret::new("k\r\nX-Evil: 1")).unwrap_err(),
            InputError::Character
        );
        assert_eq!(
            HeaderValue::plain(&"a".repeat(HEADER_VALUE_MAX + 1)).unwrap_err(),
            InputError::Length
        );
        let v = HeaderValue::secret(Secret::new("hunter2")).unwrap();
        assert!(v.is_secret());
        assert_eq!(v.expose(), "hunter2");
        assert_eq!(format!("{v:?}"), "Secret(…)");
        let v = HeaderValue::plain("SID=abc").unwrap();
        assert!(!v.is_secret());
        assert!(!format!("{v:?}").contains("abc"));
    }

    #[test]
    fn setting_keys() {
        assert_eq!(
            SettingKey::new("webui.bind_address").unwrap().as_str(),
            "webui.bind_address"
        );
        for bad in ["", "Web", "1a", "a-b", "a b", &"a".repeat(65)] {
            let leaked: &'static str = Box::leak(bad.to_owned().into_boxed_str());
            assert!(SettingKey::new(leaked).is_none(), "{bad}");
        }
    }

    /// A fake guarded client: answers loopback 8080 with a version, and
    /// records what it was sent.
    struct FakeHttp {
        sent: Mutex<Vec<(Method, String, Vec<&'static str>)>>,
    }

    impl GuardedHttp for FakeHttp {
        fn send<'a>(
            &'a self,
            request: ProbeRequest<'a>,
        ) -> BoxFuture<'a, Result<ProbeResponse, AdapterError>> {
            Box::pin(async move {
                self.sent.lock().unwrap().push((
                    request.method,
                    request.path.as_str().to_owned(),
                    request.headers.iter().map(|(n, _)| n.as_str()).collect(),
                ));
                match request.target {
                    Target::Loopback(p) if p.get() == 8080 => {
                        ProbeResponse::new(200, b"v5.0.3".to_vec(), &request.limits)
                    }
                    Target::Entered(_) => Err(AdapterError::Blocked),
                    _ => Err(AdapterError::Unreachable),
                }
            })
        }
    }

    // A fake companion, to check the traits are usable as objects and
    // that plan-then-apply composes.
    struct Fake {
        id: CompanionId,
        applied: Mutex<Vec<&'static str>>,
    }

    struct NoCredentials;
    impl Credentials for NoCredentials {
        fn get(&self, _: &str) -> Result<Option<Secret>, CredentialError> {
            Ok(None)
        }
    }

    struct BrokenStore;
    impl Credentials for BrokenStore {
        fn get(&self, _: &str) -> Result<Option<Secret>, CredentialError> {
            Err(CredentialError::Unavailable)
        }
    }

    struct OneCredential;
    impl Credentials for OneCredential {
        fn get(&self, field: &str) -> Result<Option<Secret>, CredentialError> {
            Ok((field == "password").then(|| Secret::new("hunter2")))
        }
    }

    const VERSION_PATH: &str = "/api/v2/app/version";
    const PASSWORD: HeaderName = match HeaderName::new("X-Password") {
        Some(h) => h,
        None => panic!("bad header name"),
    };

    impl Detect for Fake {
        fn companion(&self) -> &CompanionId {
            &self.id
        }
        fn detect<'a>(
            &'a self,
            http: &'a dyn GuardedHttp,
            target: &'a Target,
        ) -> BoxFuture<'a, Result<Option<Detected>, AdapterError>> {
            Box::pin(async move {
                let path =
                    RequestPath::parse(VERSION_PATH).map_err(|_| AdapterError::Protocol("path"))?;
                match http
                    .send(ProbeRequest::new(target, path, Limits::PROBE))
                    .await
                {
                    Ok(r) => {
                        let text = std::str::from_utf8(r.body())
                            .map_err(|_| AdapterError::Protocol("not text"))?;
                        Ok(Some(Detected {
                            target: target.clone(),
                            version: text.strip_prefix('v').and_then(|v| Version::parse(v).ok()),
                        }))
                    }
                    Err(AdapterError::Unreachable) => Ok(None),
                    Err(e) => Err(e),
                }
            })
        }
    }

    impl Health for Fake {
        fn companion(&self) -> &CompanionId {
            &self.id
        }
        fn check<'a>(
            &'a self,
            http: &'a dyn GuardedHttp,
            target: &'a Target,
        ) -> BoxFuture<'a, HealthStatus> {
            Box::pin(async move {
                let Ok(path) = RequestPath::parse(VERSION_PATH) else {
                    return HealthStatus::Unhealthy;
                };
                match http
                    .send(ProbeRequest::new(target, path, Limits::PROBE))
                    .await
                {
                    Ok(r) if r.status() == 200 => HealthStatus::Healthy,
                    Ok(_) => HealthStatus::Unhealthy,
                    Err(_) => HealthStatus::Unreachable,
                }
            })
        }
    }

    impl Configure for Fake {
        fn companion(&self) -> &CompanionId {
            &self.id
        }
        fn plan<'a>(
            &'a self,
            _http: &'a dyn GuardedHttp,
            _target: &'a Target,
            input: &'a ConfigureInput<'a>,
        ) -> BoxFuture<'a, Result<ConfigPlan, AdapterError>> {
            Box::pin(async move {
                if input.credentials.get("password")?.is_none() {
                    return Err(AdapterError::Unauthorized);
                }
                Ok(ConfigPlan::new(
                    self.id.clone(),
                    vec![SettingKey::new("webui.bind_address").unwrap()],
                ))
            })
        }
        fn apply_plan<'a>(
            &'a self,
            http: &'a dyn GuardedHttp,
            target: &'a Target,
            input: &'a ConfigureInput<'a>,
            plan: CheckedPlan<'a>,
        ) -> BoxFuture<'a, Result<(), AdapterError>> {
            Box::pin(async move {
                let plan = plan.plan();
                let password = input
                    .credentials
                    .get("password")?
                    .ok_or(AdapterError::Unauthorized)?;
                let path = RequestPath::parse("/api/v2/app/setPreferences")
                    .map_err(|_| AdapterError::Protocol("path"))?;
                let req = ProbeRequest::new(target, path, Limits::PROBE)
                    .post(b"json={}".to_vec())
                    .header(
                        PASSWORD,
                        HeaderValue::secret(password)
                            .map_err(|_| AdapterError::Protocol("credential"))?,
                    );
                http.send(req).await?;
                let mut applied = self.applied.lock().unwrap();
                for c in plan.changes() {
                    if !applied.contains(&c.as_str()) {
                        applied.push(c.as_str());
                    }
                }
                Ok(())
            })
        }
    }

    #[tokio::test]
    async fn traits_work_as_objects_through_the_guarded_client() {
        let fake = std::sync::Arc::new(Fake {
            id: CompanionId::parse("qbittorrent").unwrap(),
            applied: Mutex::new(Vec::new()),
        });
        let http = FakeHttp {
            sent: Mutex::new(Vec::new()),
        };
        let detect: std::sync::Arc<dyn Detect> = fake.clone();
        let health: std::sync::Arc<dyn Health> = fake.clone();
        let configure: std::sync::Arc<dyn Configure> = fake.clone();
        // The same `apply` works on a `Send + Sync` trait object.
        let shared: std::sync::Arc<dyn Configure + Send + Sync> = fake.clone();
        assert_eq!(detect.companion().as_str(), "qbittorrent");

        let here = Target::Loopback(PortNumber::new(8080).unwrap());
        let found = detect.detect(&http, &here).await.unwrap().unwrap();
        assert_eq!(found.version.unwrap().as_str(), "5.0.3");
        let elsewhere = Target::Container(
            ContainerName::parse("qbittorrent").unwrap(),
            PortNumber::new(8080).unwrap(),
        );
        assert_eq!(detect.detect(&http, &elsewhere).await.unwrap(), None);
        let entered = Target::Entered(EnteredUrl::parse("http://10.0.0.1:8080").unwrap());
        assert_eq!(
            detect.detect(&http, &entered).await,
            Err(AdapterError::Blocked)
        );
        assert_eq!(health.check(&http, &here).await, HealthStatus::Healthy);
        assert_eq!(
            health.check(&http, &elsewhere).await,
            HealthStatus::Unreachable
        );

        // No credential, and a broken store, are different answers.
        let none = NoCredentials;
        let input = ConfigureInput::new(&none);
        assert_eq!(
            configure.plan(&http, &here, &input).await,
            Err(AdapterError::Unauthorized)
        );
        let broken = BrokenStore;
        let input = ConfigureInput::new(&broken);
        assert_eq!(
            configure.plan(&http, &here, &input).await,
            Err(AdapterError::Credentials)
        );

        let creds = OneCredential;
        let input = ConfigureInput::new(&creds);
        assert!(!format!("{input:?}").contains("hunter2"));
        let plan = configure.plan(&http, &here, &input).await.unwrap();
        configure.apply(&http, &here, &input, &plan).await.unwrap();
        configure.apply(&http, &here, &input, &plan).await.unwrap();
        shared.apply(&http, &here, &input, &plan).await.unwrap();
        assert_eq!(*fake.applied.lock().unwrap(), ["webui.bind_address"]);

        // Another companion's plan is refused before anything is sent.
        let sent_before = http.sent.lock().unwrap().len();
        let foreign = ConfigPlan::new(
            CompanionId::parse("prowlarr").unwrap(),
            plan.changes().to_vec(),
        );
        assert_eq!(
            configure.apply(&http, &here, &input, &foreign).await,
            Err(AdapterError::Refused("the plan is for another companion"))
        );
        assert_eq!(http.sent.lock().unwrap().len(), sent_before);

        // Everything went through the guarded client; the credential went
        // as a secret header on the configuration request only.
        let sent = http.sent.lock().unwrap();
        assert!(sent.iter().all(|(_, p, _)| p.starts_with("/api/v2/")));
        let posts: Vec<_> = sent.iter().filter(|(m, _, _)| *m == Method::Post).collect();
        assert_eq!(posts.len(), 3);
        assert!(posts.iter().all(|(_, _, h)| h == &vec!["X-Password"]));
    }

    #[test]
    fn errors_carry_no_response_text() {
        assert_eq!(
            AdapterError::Protocol("no version field").to_string(),
            "unexpected answer: no version field"
        );
        assert_eq!(
            AdapterError::from(CredentialError::Unavailable),
            AdapterError::Credentials
        );
    }
}
