//! Finding companions and checking their health (plan
//! 2026-11-06-v06-companions §4 CMP-8a, Q12; architecture §18.9, §18.10).
//!
//! **Where detection looks (Q12), and nowhere else:**
//! - loopback (`127.0.0.1`, then `::1`) on each catalogue port. Only a bare
//!   probe ever looks for a companion on `::1`, and once `127.0.0.1` has
//!   answered on a port, that port's requests go nowhere else
//!   ([`CompanionHttp`]);
//! - each catalogue container name on Docker's network, on the same ports;
//! - one URL you enter.
//!
//! Never a range, a subnet or a port sweep: [`probe_targets`] builds the
//! fixed list from the catalogue, and a test pins it.
//!
//! **Every probe goes through [`CompanionHttp`]**, this crate's
//! [`GuardedHttp`] over the SSRF guard (`lodestar_integrations::net`,
//! architecture §12.2). It never follows a redirect, resolves each name once
//! per connection, and holds every request to its [`Limits`]: one timeout
//! for the whole exchange, body included, and a byte cap.
//!
//! **Answers are untrusted.** The adapters ([`qbittorrent`], [`prowlarr`],
//! [`gluetun`]) read only the status and a capped body, match fixed markers,
//! and hand back [`Detected`] and [`HealthStatus`]: a target Lodestar chose,
//! a [`crate::Version`] in its strict grammar, or a status from a closed
//! set. No text a companion sent reaches anything Lodestar stores, shows or
//! logs. Detection never sends a credential.

use std::collections::HashMap;
use std::io;
use std::net::IpAddr;
use std::sync::{Arc, Mutex, PoisonError};

use lodestar_integrations::BoxFuture as NetFuture;
use lodestar_integrations::net::{
    AddrClass, GuardedClient, NetError, NetPolicy, Resolve, SystemResolver, classify, read_limited,
};
use lodestar_integrations::secret::Secret;
use lodestar_sources_feeds::json::{self, Walk};
use reqwest::Url;
use reqwest::header::{HeaderValue as ReqHeaderValue, SET_COOKIE, USER_AGENT};

use crate::adapter::{
    AdapterError, BoxFuture, CookieName, Detect, Detected, EnteredUrl, GuardedHttp, Health,
    HealthStatus, Limits, Method, ProbeRequest, ProbeResponse, RequestPath, TIMEOUT_MAX, Target,
    cookie_value_ok,
};
use crate::catalogue::{Catalogue, Entry, Exposure, Proto};
use crate::flaresolverr;
use crate::inputs::CompanionId;
use crate::supervise::bind::Bindings;

pub mod gluetun;
pub mod prowlarr;
pub mod qbittorrent;

/// The `User-Agent` every probe sends, as CMP-1's downloads do: it names
/// Lodestar's companion manager and nothing about this install.
const PROBE_USER_AGENT: &str = "Lodestar-Companions";

/// The companions [`auto_detect`] probes for. FlareSolverr has adapters
/// too, but is opt-in (owner §2.47, Q21): only
/// [`crate::flaresolverr::find`] probes it, once you switch it on.
const ADAPTERS: [&str; 3] = [qbittorrent::ID, prowlarr::ID, gluetun::ID];

/// The detection adapter for a catalogue entry, if there is one.
pub fn detector(id: &CompanionId) -> Option<Box<dyn Detect>> {
    match id.as_str() {
        qbittorrent::ID => Some(Box::new(qbittorrent::QBittorrent::new(id.clone()))),
        prowlarr::ID => Some(Box::new(prowlarr::Prowlarr::new(id.clone()))),
        gluetun::ID => Some(Box::new(gluetun::Gluetun::new(id.clone()))),
        flaresolverr::ID => Some(Box::new(flaresolverr::FlareSolverr::new(id.clone()))),
        _ => None,
    }
}

/// The health adapter for a catalogue entry, if there is one.
pub fn health(id: &CompanionId) -> Option<Box<dyn Health>> {
    match id.as_str() {
        qbittorrent::ID => Some(Box::new(qbittorrent::QBittorrent::new(id.clone()))),
        prowlarr::ID => Some(Box::new(prowlarr::Prowlarr::new(id.clone()))),
        gluetun::ID => Some(Box::new(gluetun::Gluetun::new(id.clone()))),
        flaresolverr::ID => Some(Box::new(flaresolverr::FlareSolverr::new(id.clone()))),
        _ => None,
    }
}

/// Where Lodestar runs, which decides whether container names are probed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Runtime {
    /// A native install. Docker's names resolve only inside a Docker
    /// network, so a single-label lookup here would go to the LAN's
    /// broadcast name services (LLMNR, NetBIOS, mDNS), where any host can
    /// answer. Container names aren't probed.
    Native,
    /// Inside a container, where Docker's embedded DNS answers the names.
    Container,
}

/// The fixed targets auto-detection probes for one entry (Q12): loopback
/// on each internal TCP port, then, in a container, the entry's container
/// name on the same ports. A peer-to-peer port isn't an HTTP service, so
/// it's never probed.
pub fn probe_targets(entry: &Entry, runtime: Runtime) -> Vec<Target> {
    let ports: Vec<_> = entry
        .ports
        .iter()
        .filter(|p| p.proto == Proto::Tcp && p.exposure == Exposure::Internal)
        .map(|p| p.number)
        .collect();
    let mut targets: Vec<Target> = ports.iter().map(|p| Target::Loopback(*p)).collect();
    if let (Runtime::Container, Some(c)) = (runtime, &entry.container) {
        targets.extend(
            ports
                .iter()
                .map(|p| Target::Container(c.name().clone(), *p)),
        );
    }
    targets
}

/// Every probe auto-detection makes: [`probe_targets`] for each entry this
/// module has an adapter for, in catalogue order.
pub fn auto_probes(catalogue: &Catalogue, runtime: Runtime) -> Vec<(&CompanionId, Target)> {
    catalogue
        .entries()
        .iter()
        .filter(|e| ADAPTERS.contains(&e.id.as_str()))
        .flat_map(move |e| {
            probe_targets(e, runtime)
                .into_iter()
                .map(move |t| (&e.id, t))
        })
        .collect()
}

/// Runs [`auto_probes`] one at a time and returns what was found. A probe
/// that fails is skipped: each companion is reported at most once, at the
/// first target that answered as it. At most six probes of up to two
/// requests each, each within [`Limits::PROBE`]: a minute at worst, so a
/// caller runs it off the request path.
pub async fn auto_detect(
    http: &dyn GuardedHttp,
    catalogue: &Catalogue,
    runtime: Runtime,
) -> Vec<(CompanionId, Detected)> {
    let mut found: Vec<(CompanionId, Detected)> = Vec::new();
    for (id, target) in auto_probes(catalogue, runtime) {
        if found.iter().any(|(f, _)| f == id) {
            continue;
        }
        let Some(adapter) = detector(id) else {
            continue;
        };
        if let Ok(Some(d)) = adapter.detect(http, &target).await {
            found.push((id.clone(), d));
        }
    }
    found
}

/// One GET to `path` on `target`, within [`Limits::PROBE`].
pub(crate) async fn get(
    http: &dyn GuardedHttp,
    target: &Target,
    path: &'static str,
) -> Result<ProbeResponse, AdapterError> {
    let path = RequestPath::parse(path).map_err(|_| AdapterError::Protocol("request path"))?;
    http.send(ProbeRequest::new(target, path, Limits::PROBE))
        .await
}

/// What a failed probe means for detection: nothing listening is "not
/// here", and so is nothing answering in time at a fixed target (on
/// Windows a refused loopback connection takes about 2 s per address
/// family). Anything else, and any failure at a URL you entered, is the
/// caller's to see.
pub(crate) fn not_found_or(
    e: AdapterError,
    target: &Target,
) -> Result<Option<Detected>, AdapterError> {
    match (e, target) {
        (AdapterError::Unreachable | AdapterError::NotThisCompanion, _) => Ok(None),
        (AdapterError::Timeout, Target::Loopback(_) | Target::Container(..)) => Ok(None),
        (e, _) => Err(e),
    }
}

/// What a failed probe means for health: refused before anything was read
/// is unreachable; an answer that broke a rule is unhealthy.
pub(crate) fn health_of(e: AdapterError) -> HealthStatus {
    match e {
        AdapterError::Timeout | AdapterError::Unreachable | AdapterError::Blocked => {
            HealthStatus::Unreachable
        }
        _ => HealthStatus::Unhealthy,
    }
}

/// The caps for the small JSON answers the adapters read (`/ping`,
/// `/v1/vpn/status`): a flat object, a handful of keys, a short value.
const JSON_LIMITS: json::Limits = json::Limits {
    max_depth: 4,
    max_values: 64,
    max_array: 16,
    max_text: 32,
};

/// The string `status` of a JSON object, read with the bounded reader
/// (`lodestar_sources_feeds::json`). `None` for a body that isn't UTF-8 or
/// one JSON object, has no `status` string or has it twice, has a key
/// with an escape (so `st\u0061tus` can't hide a second one), or whose
/// value passes [`JSON_LIMITS`]. Other keys are passed over.
///
/// Crate-private: the value is a companion's text, and only the adapters'
/// closed enums leave this module.
pub(crate) fn status_field(body: &[u8]) -> Option<String> {
    let s = std::str::from_utf8(body).ok()?;
    let mut j = json::Reader::new(s, JSON_LIMITS);
    if j.peek() != Some(b'{') {
        return None;
    }
    let mut status: Option<Option<String>> = None;
    j.object(|j, key| {
        if key.contains('\\') {
            return Err(json::Stop::Malformed);
        }
        if key == "status" && status.is_none() && j.peek() == Some(b'"') {
            status = Some(j.text()?.ok());
        } else {
            if key == "status" {
                status = Some(None);
            }
            j.skip()?;
        }
        Ok(())
    })
    .ok()?;
    j.end().ok()?;
    status.flatten()
}

/// A resolver for the fixed container targets: a container name must
/// resolve to Docker's network (a private address) or loopback, never to a
/// public address a DNS search domain might hand back. The guard still
/// checks every answer against its policy after this.
struct LocalOnly(Arc<dyn Resolve>);

impl Resolve for LocalOnly {
    fn resolve<'a>(&'a self, host: &'a str) -> NetFuture<'a, io::Result<Vec<IpAddr>>> {
        Box::pin(async move {
            let addrs = self.0.resolve(host).await?;
            if addrs
                .iter()
                .all(|ip| matches!(classify(*ip), AddrClass::Private | AddrClass::Loopback))
            {
                Ok(addrs)
            } else {
                Err(io::Error::other("not a local address"))
            }
        })
    }
}

/// The resolver for entered URLs: `localhost` is `127.0.0.1` alone, never
/// `::1`, which the system resolver may hand back first (Windows does), so
/// a companion you named as `localhost` is asked where Lodestar's own
/// companions listen. Every other name goes to the system.
struct LocalhostIsV4(Arc<dyn Resolve>);

impl Resolve for LocalhostIsV4 {
    fn resolve<'a>(&'a self, host: &'a str) -> NetFuture<'a, io::Result<Vec<IpAddr>>> {
        Box::pin(async move {
            if is_localhost(host) {
                return Ok(vec![IpAddr::from([127, 0, 0, 1])]);
            }
            self.0.resolve(host).await
        })
    }
}

/// `localhost`, in any case, with or without the root's trailing dot.
fn is_localhost(host: &str) -> bool {
    host.strip_suffix('.')
        .unwrap_or(host)
        .eq_ignore_ascii_case("localhost")
}

/// The [`GuardedHttp`] that `crates/lodestar` hands the adapters (CMP-5g).
///
/// - **Policy:** the SSRF guard's download-client mode: public and private
///   addresses and loopback; never link-local, metadata or reserved. A
///   URL you enter may name a companion on this machine
///   (`http://localhost:8080`, CMP-11's Windows qBittorrent). Loopback and
///   container targets are fixed by this crate; a container name must also
///   resolve to a private or loopback address. A URL you entered is checked
///   by the guard at connect time, its literal before any connection.
/// - **No redirects:** a 3xx comes back as its status, and its `Location`
///   is never read.
/// - **Bounds:** the request's [`Limits`] cover the whole exchange, from
///   connecting to the last body byte; one byte past the cap fails with
///   [`AdapterError::TooLarge`].
/// - **Nothing kept:** no cookie jar, no proxy, no `Referer`; only the
///   request's own headers and [`PROBE_USER_AGENT`]. The one session cookie
///   a request names is handed back in the answer, for the adapter to send
///   itself (CMP-8b's qBittorrent sign-in).
/// - **One loopback address a port:** a loopback target is tried on
///   `127.0.0.1`, and on `::1` only when nothing listens there and the
///   request is a bare probe (a `GET` with no header, no body and no
///   query).
///   - Once `127.0.0.1` has answered on a port, every later request to
///     that port goes to it alone, for as long as this value lives.
///   - A port a bare probe found on `::1` is remembered as that until
///     `127.0.0.1` answers. Every later request still asks `127.0.0.1`
///     first, and an answer there takes the port for good. While nothing
///     listens there, a request that carries a cookie, a key or a body
///     goes on to `::1`, the address the probe found the companion on.
///   - Before any probe, a request that carries anything stops at
///     `127.0.0.1`.
///   - The memory is this value's own: another `CompanionHttp` starts
///     with none.
/// - **A companion Lodestar runs** (X-cmp-supervisor-bind): a port the
///   supervisor holds a child on ([`Bindings`]) is asked on `127.0.0.1`
///   alone, never on `::1`, and a request that carries anything goes out
///   only while that child is running and was found ready in its current
///   run. Otherwise it's refused as [`AdapterError::Unreachable`] before
///   any connection. Bare probes still go (they find it ready), to
///   `127.0.0.1` alone.
/// - **`localhost` in a URL you entered** is `127.0.0.1` alone.
pub struct CompanionHttp {
    fixed: GuardedClient,
    entered: GuardedClient,
    /// The ports of companions Lodestar runs, when this client serves the
    /// supervisor too.
    bindings: Option<Arc<Bindings>>,
    /// The loopback address each port's requests go to: `false` once
    /// `127.0.0.1` has answered (for good), `true` while only `::1` has.
    /// At most one entry a port.
    loopback: Mutex<HashMap<u16, bool>>,
}

impl std::fmt::Debug for CompanionHttp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CompanionHttp").finish_non_exhaustive()
    }
}

impl CompanionHttp {
    /// With the system resolver.
    pub fn new() -> Result<CompanionHttp, AdapterError> {
        CompanionHttp::with_resolver(Arc::new(SystemResolver))
    }

    /// With another resolver (tests; a resolver must still answer for every
    /// name, and the guard checks each answer).
    pub fn with_resolver(resolver: Arc<dyn Resolve>) -> Result<CompanionHttp, AdapterError> {
        let policy = NetPolicy::download_client();
        let build = |r: Arc<dyn Resolve>| {
            GuardedClient::with_resolver(policy, TIMEOUT_MAX, r)
                .map_err(|_| AdapterError::Protocol("HTTP client"))
        };
        Ok(CompanionHttp {
            fixed: build(Arc::new(LocalOnly(Arc::clone(&resolver))))?,
            entered: build(Arc::new(LocalhostIsV4(resolver)))?,
            bindings: None,
            loopback: Mutex::new(HashMap::new()),
        })
    }

    /// The same, serving the supervisor too: the ports in `bindings` are
    /// companions Lodestar runs (see the type's notes). One such client is
    /// shared by every path, so a probe by any of them serves the rest.
    pub fn with_bindings(mut self, bindings: Arc<Bindings>) -> CompanionHttp {
        self.bindings = Some(bindings);
        self
    }

    async fn once(
        &self,
        client: &GuardedClient,
        url: Url,
        request: &ProbeRequest<'_>,
    ) -> Result<ProbeResponse, AdapterError> {
        let method = match request.method {
            Method::Get => reqwest::Method::GET,
            Method::Post => reqwest::Method::POST,
            Method::Put => reqwest::Method::PUT,
            Method::Delete => reqwest::Method::DELETE,
        };
        let mut req = reqwest::Request::new(method, url);
        *req.timeout_mut() = Some(request.limits.timeout());
        let headers = req.headers_mut();
        headers.insert(USER_AGENT, ReqHeaderValue::from_static(PROBE_USER_AGENT));
        for (name, value) in &request.headers {
            let (name, v) = wire_header(name, value)?;
            headers.append(name, v);
        }
        if let Some(body) = &request.body {
            *req.body_mut() = Some(body.clone().into());
        }
        let cap = request.limits.body_cap();
        let (status, session, body) = async {
            let resp = client.send(req).await?;
            let status = resp.status().as_u16();
            // Only the one cookie the request names is read, and only its
            // value; no other header of the answer is.
            let session = request.session_cookie.and_then(|names| {
                session_cookie(
                    resp.headers()
                        .get_all(SET_COOKIE)
                        .iter()
                        .filter_map(|v| v.to_str().ok()),
                    names,
                )
            });
            Ok::<_, NetError>((status, session, read_limited(resp, cap).await?))
        }
        .await
        .map_err(net_error)?;
        let response = ProbeResponse::new(status, body, &request.limits)?;
        match session {
            Some(value) => response.with_session_cookie(Secret::new(value)),
            None => Ok(response),
        }
    }

    async fn dispatch(&self, request: &ProbeRequest<'_>) -> Result<ProbeResponse, AdapterError> {
        let path = request.path.as_str();
        match request.target {
            Target::Loopback(port) => {
                let p = port.get();
                let url = |v6: bool| {
                    let host = if v6 { "[::1]" } else { "127.0.0.1" };
                    parse(&format!("http://{host}:{p}{path}"))
                };
                // A request that can't be built never tells where the
                // companion listens.
                sendable(request)?;
                // A companion Lodestar runs: `127.0.0.1` alone, and what
                // carries something only to the child that's ready there.
                if let Some(b) = self.bindings.as_ref().filter(|b| b.managed(p)) {
                    if !bare(request) && !b.ready(p) {
                        return Err(AdapterError::Unreachable);
                    }
                    return self.once(&self.fixed, url(false)?, request).await;
                }
                let remembered = self.loopback_of(p);
                // `127.0.0.1` keeps a port for good.
                if remembered == Some(false) {
                    return self.once(&self.fixed, url(false)?, request).await;
                }
                // Everything asks `127.0.0.1` first. With nothing there, a
                // bare probe goes looking on `::1`, and what carries
                // something follows only where a probe has found the port.
                let look = bare(request) || remembered == Some(true);
                match self.once(&self.fixed, url(false)?, request).await {
                    Err(AdapterError::Unreachable) if look => {
                        let got = self.once(&self.fixed, url(true)?, request).await;
                        if got.is_ok() && bare(request) {
                            self.remember_loopback(p, true);
                        }
                        got
                    }
                    got => {
                        if listening(&got) {
                            self.remember_loopback(p, false);
                        }
                        got
                    }
                }
            }
            Target::Container(name, port) => {
                let url = parse(&format!("http://{}:{}{path}", name.as_str(), port.get()))?;
                self.once(&self.fixed, url, request).await
            }
            Target::Entered(base) => {
                self.once(&self.entered, join(base, &request.path)?, request)
                    .await
            }
        }
    }
}

impl CompanionHttp {
    fn loopback_of(&self, port: u16) -> Option<bool> {
        let map = self.loopback.lock().unwrap_or_else(PoisonError::into_inner);
        map.get(&port).copied()
    }

    /// `127.0.0.1` takes a port for good, over `::1` too; `::1` is
    /// remembered only while nothing is.
    fn remember_loopback(&self, port: u16, v6: bool) {
        let mut map = self.loopback.lock().unwrap_or_else(PoisonError::into_inner);
        if v6 {
            map.entry(port).or_insert(true);
        } else {
            map.insert(port, false);
        }
    }
}

/// A request header as it goes on the wire, marked sensitive if secret.
fn wire_header(
    name: &crate::adapter::HeaderName,
    value: &crate::adapter::HeaderValue,
) -> Result<(reqwest::header::HeaderName, ReqHeaderValue), AdapterError> {
    let name = reqwest::header::HeaderName::from_bytes(name.as_str().as_bytes())
        .map_err(|_| AdapterError::Protocol("header name"))?;
    let mut v = ReqHeaderValue::from_bytes(value.expose().as_bytes())
        .map_err(|_| AdapterError::Protocol("header value"))?;
    v.set_sensitive(value.is_secret());
    Ok((name, v))
}

/// Whether every header of `request` can go on the wire: what
/// [`CompanionHttp::once`] would refuse before it connects.
fn sendable(request: &ProbeRequest<'_>) -> Result<(), AdapterError> {
    request
        .headers
        .iter()
        .try_for_each(|(name, value)| wire_header(name, value).map(drop))
}

/// A probe that carries nothing: no header (so no cookie and no key), no
/// body and no query. [`ProbeRequest::new`] with a plain path builds
/// exactly this.
fn bare(request: &ProbeRequest<'_>) -> bool {
    matches!(request.method, Method::Get)
        && request.headers.is_empty()
        && request.body.is_none()
        && !request.path.as_str().contains('?')
}

/// Whether the outcome shows something accepted the connection: an answer,
/// or a failure that can only follow one.
fn listening(got: &Result<ProbeResponse, AdapterError>) -> bool {
    !matches!(
        got,
        Err(AdapterError::Unreachable | AdapterError::Timeout | AdapterError::Blocked)
    )
}

impl GuardedHttp for CompanionHttp {
    fn send<'a>(
        &'a self,
        request: ProbeRequest<'a>,
    ) -> BoxFuture<'a, Result<ProbeResponse, AdapterError>> {
        Box::pin(async move {
            // One deadline for everything, both loopback families and the
            // whole body included, so a slow drip can't outlast it.
            tokio::time::timeout(request.limits.timeout(), self.dispatch(&request))
                .await
                .unwrap_or(Err(AdapterError::Timeout))
        })
    }
}

/// The `name=value` of the first `Set-Cookie` whose name one of `names`
/// matches and whose value passes [`cookie_value_ok`]; attributes after
/// `;` are ignored. Cookies that fail are passed over, never returned.
fn session_cookie<'a>(
    headers: impl Iterator<Item = &'a str>,
    names: &[CookieName],
) -> Option<String> {
    headers
        .filter_map(|h| h.split(';').next())
        .filter_map(|pair| pair.trim().split_once('='))
        .find(|(n, v)| names.iter().any(|c| c.matches(n)) && cookie_value_ok(v))
        .map(|(n, v)| format!("{n}={v}"))
}

fn parse(s: &str) -> Result<Url, AdapterError> {
    Url::parse(s).map_err(|_| AdapterError::Protocol("URL"))
}

/// An entered base URL with a request path joined to it. Both follow
/// grammars that keep the result on the entered host: no query or
/// fragment in the base, no `//`, `.` or `..` in either.
fn join(base: &EnteredUrl, path: &RequestPath) -> Result<Url, AdapterError> {
    parse(&format!(
        "{}{}",
        base.as_str().trim_end_matches('/'),
        path.as_str()
    ))
}

/// A guard error as an adapter error, keeping nothing but the class.
fn net_error(e: NetError) -> AdapterError {
    match e {
        NetError::Blocked(_) => AdapterError::Blocked,
        NetError::Resolve | NetError::Connect => AdapterError::Unreachable,
        NetError::Timeout => AdapterError::Timeout,
        NetError::TooLarge => AdapterError::TooLarge,
        NetError::InvalidUrl(_) => AdapterError::Protocol("URL"),
        NetError::Redirect | NetError::Http => AdapterError::Protocol("HTTP exchange"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Only [`ProbeRequest::new`]'s request is bare: anything that could
    /// carry a key, a cookie or a password isn't, whatever its values.
    #[test]
    fn only_a_request_that_carries_nothing_is_bare() {
        let t = Target::Loopback(crate::inputs::PortNumber::new(8080).unwrap());
        let new = || ProbeRequest::new(&t, RequestPath::parse("/x").unwrap(), Limits::PROBE);
        assert!(bare(&new()));
        assert!(bare(&new().session_cookie(&[])));
        let name = crate::adapter::HeaderName::new("X-Api-Key").unwrap();
        let plain = crate::adapter::HeaderValue::plain("v").unwrap();
        let secret = crate::adapter::HeaderValue::secret(Secret::new("k")).unwrap();
        assert!(!bare(&new().header(name, plain)));
        assert!(!bare(&new().header(name, secret)));
        assert!(!bare(&new().post(Vec::new())));
        assert!(!bare(&new().put(b"{}".to_vec())));
        let query = RequestPath::parse("/x?apikey=k").unwrap();
        assert!(!bare(&ProbeRequest::new(&t, query, Limits::PROBE)));
    }

    /// `127.0.0.1` keeps a port for good and takes one remembered as `::1`;
    /// ports don't mix.
    #[test]
    fn ipv4_loopback_keeps_a_port_and_takes_one_from_ipv6() {
        let http = CompanionHttp::new().unwrap();
        assert_eq!(http.loopback_of(8080), None);
        http.remember_loopback(8080, false);
        http.remember_loopback(8080, true);
        assert_eq!(http.loopback_of(8080), Some(false), "kept for good");
        http.remember_loopback(9696, true);
        assert_eq!(http.loopback_of(9696), Some(true));
        http.remember_loopback(9696, false);
        assert_eq!(http.loopback_of(9696), Some(false), "127.0.0.1 takes it");
        http.remember_loopback(9696, true);
        assert_eq!(http.loopback_of(9696), Some(false));
        assert_eq!(http.loopback_of(8081), None);
    }

    /// An answer, or a failure that only an answer can cause, shows a
    /// listener; a refusal, a timeout or the guard's refusal doesn't.
    #[test]
    fn only_an_exchange_shows_a_listener() {
        for e in [
            AdapterError::Unreachable,
            AdapterError::Timeout,
            AdapterError::Blocked,
        ] {
            assert!(!listening(&Err(e)));
        }
        for e in [AdapterError::TooLarge, AdapterError::Protocol("x")] {
            assert!(listening(&Err(e)));
        }
        let ok = ProbeResponse::new(200, Vec::new(), &Limits::PROBE);
        assert!(listening(&ok));
    }

    #[test]
    fn entered_urls_join_on_the_same_host() {
        let p = RequestPath::parse("/ping").unwrap();
        for (base, want) in [
            ("http://10.0.0.2:9696", "http://10.0.0.2:9696/ping"),
            ("http://10.0.0.2:9696/", "http://10.0.0.2:9696/ping"),
            ("HTTPS://Box/prowlarr/", "https://box/prowlarr/ping"),
            ("http://[fd00::1]:8080/a", "http://[fd00::1]:8080/a/ping"),
        ] {
            let u = join(&EnteredUrl::parse(base).unwrap(), &p).unwrap();
            assert_eq!(u.as_str(), want);
        }
    }

    #[test]
    fn only_the_named_session_cookie_is_read() {
        const NAMES: &[CookieName] = &[
            match CookieName::new("SID") {
                Some(c) => c,
                None => panic!(),
            },
            match CookieName::numbered("QBT_SID_") {
                Some(c) => c,
                None => panic!(),
            },
        ];
        let pick = |hs: &[&str]| session_cookie(hs.iter().copied(), NAMES);
        assert_eq!(
            pick(&["SID=abc123; HttpOnly; path=/"]),
            Some("SID=abc123".into())
        );
        assert_eq!(pick(&["X=1", "SID=b"]), Some("SID=b".into()));
        assert_eq!(pick(&["QBT_SID_8080=a"]), Some("QBT_SID_8080=a".into()));
        for none in [
            &["QBT_SID_=a"][..],
            &["QBT_SID_123456=a"],
            &["QBT_SID_80x=a"],
            &["sid=a"],
            &["SID="],
            &["SID=a,b=c"],
            &[],
        ] {
            assert_eq!(pick(none), None, "{none:?}");
        }
        assert_eq!(pick(&["SID=a b", "SID=ok"]), Some("SID=ok".into()));
        assert_eq!(pick(&[&format!("SID={}", "a".repeat(513))]), None);
    }

    mod cookie_props {
        use super::*;
        use proptest::prelude::*;

        const NAMES: &[CookieName] = &[
            match CookieName::new("SID") {
                Some(c) => c,
                None => panic!(),
            },
            match CookieName::numbered("QBT_SID_") {
                Some(c) => c,
                None => panic!(),
            },
        ];

        fn header() -> impl Strategy<Value = String> {
            prop_oneof![
                any::<String>(),
                "[ -~]{0,80}",
                "(SID|QBT_SID_[0-9]{0,7}|sid|X)=[ -~]{0,40}(; [a-zA-Z=/]{0,20}){0,3}",
            ]
        }

        proptest! {
            /// Any list of headers: no panic, and anything handed back is
            /// a matching name with a value that may be sent back.
            #[test]
            fn never_panics_and_returns_only_safe_pairs(
                headers in proptest::collection::vec(header(), 0..6)
            ) {
                if let Some(pair) = session_cookie(headers.iter().map(String::as_str), NAMES) {
                    let (n, v) = pair.split_once('=').unwrap();
                    prop_assert!(NAMES.iter().any(|c| c.matches(n)));
                    prop_assert!(cookie_value_ok(v));
                    prop_assert!(ProbeResponse::new(200, Vec::new(), &Limits::PROBE)
                        .unwrap()
                        .with_session_cookie(Secret::new(pair.clone()))
                        .is_ok());
                }
            }

            /// A well-formed cookie with attributes round-trips.
            #[test]
            fn well_formed_cookies_round_trip(
                port in 1u32..=65535,
                numbered: bool,
                value in "[A-Za-z0-9_.+/=%:-]{1,64}",
                attrs in "(; (HttpOnly|path=/|SameSite=Strict)){0,3}",
            ) {
                let name = if numbered { format!("QBT_SID_{port}") } else { "SID".to_owned() };
                let header = format!("{name}={value}{attrs}");
                prop_assert_eq!(
                    session_cookie(std::iter::once(header.as_str()), NAMES),
                    Some(format!("{name}={value}"))
                );
            }
        }
    }

    #[test]
    fn guard_errors_keep_only_the_class() {
        assert_eq!(
            net_error(NetError::Blocked(AddrClass::Metadata)),
            AdapterError::Blocked
        );
        assert_eq!(net_error(NetError::Connect), AdapterError::Unreachable);
        assert_eq!(net_error(NetError::Resolve), AdapterError::Unreachable);
        assert_eq!(net_error(NetError::Timeout), AdapterError::Timeout);
        assert_eq!(net_error(NetError::TooLarge), AdapterError::TooLarge);
        assert!(matches!(
            net_error(NetError::Redirect),
            AdapterError::Protocol(_)
        ));
    }

    #[test]
    fn health_and_detection_read_errors_the_same_way() {
        assert_eq!(health_of(AdapterError::Blocked), HealthStatus::Unreachable);
        assert_eq!(health_of(AdapterError::Timeout), HealthStatus::Unreachable);
        assert_eq!(health_of(AdapterError::TooLarge), HealthStatus::Unhealthy);
        let lo = Target::Loopback(crate::inputs::PortNumber::new(8080).unwrap());
        let entered = Target::Entered(EnteredUrl::parse("http://10.0.0.2").unwrap());
        assert_eq!(not_found_or(AdapterError::Unreachable, &entered), Ok(None));
        assert_eq!(not_found_or(AdapterError::Timeout, &lo), Ok(None));
        assert_eq!(
            not_found_or(AdapterError::Timeout, &entered),
            Err(AdapterError::Timeout)
        );
        assert_eq!(
            not_found_or(AdapterError::Blocked, &lo),
            Err(AdapterError::Blocked)
        );
    }

    #[test]
    fn every_registered_adapter_is_in_the_catalogue() {
        let c = crate::builtin().unwrap();
        for id in ADAPTERS {
            let id = &c.get(id).unwrap().id;
            assert_eq!(detector(id).unwrap().companion(), id);
            assert_eq!(health(id).unwrap().companion(), id);
        }
        // Health asks the path the catalogue names, where it names one.
        let path = |id: &str| c.get(id).unwrap().health.as_ref().map(|h| h.path.as_str());
        assert_eq!(path(qbittorrent::ID), Some(qbittorrent::VERSION_PATH));
        assert_eq!(path(prowlarr::ID), Some(prowlarr::PING_PATH));
        assert_eq!(path(gluetun::ID), None);
        assert_eq!(path(flaresolverr::ID), Some(flaresolverr::HEALTH_PATH));
        // FlareSolverr has adapters, but no default probe (Q21).
        let fs = &c.get(flaresolverr::ID).unwrap().id;
        assert_eq!(detector(fs).unwrap().companion(), fs);
        assert_eq!(health(fs).unwrap().companion(), fs);
        assert!(!ADAPTERS.contains(&flaresolverr::ID));
        assert!(
            auto_probes(c, Runtime::Container)
                .iter()
                .all(|(id, _)| id.as_str() != flaresolverr::ID)
        );
        let other = CompanionId::parse("jackett").unwrap();
        assert!(detector(&other).is_none() && health(&other).is_none());
    }
}
