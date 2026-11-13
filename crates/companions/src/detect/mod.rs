//! Finding companions and checking their health (plan
//! 2026-11-06-v06-companions §4 CMP-8a, Q12; architecture §18.9, §18.10).
//!
//! **Where detection looks (Q12), and nowhere else:**
//! - loopback (`127.0.0.1`, then `::1`) on each catalogue port;
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

use std::io;
use std::net::IpAddr;
use std::sync::Arc;

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
pub struct CompanionHttp {
    fixed: GuardedClient,
    entered: GuardedClient,
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
            entered: build(resolver)?,
        })
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
            let name = reqwest::header::HeaderName::from_bytes(name.as_str().as_bytes())
                .map_err(|_| AdapterError::Protocol("header name"))?;
            let mut v = ReqHeaderValue::from_bytes(value.expose().as_bytes())
                .map_err(|_| AdapterError::Protocol("header value"))?;
            v.set_sensitive(value.is_secret());
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
                let v4 = parse(&format!("http://127.0.0.1:{p}{path}"))?;
                match self.once(&self.fixed, v4, request).await {
                    // Nothing on IPv4 loopback: a companion may listen on
                    // `::1` only.
                    Err(AdapterError::Unreachable) => {
                        let v6 = parse(&format!("http://[::1]:{p}{path}"))?;
                        self.once(&self.fixed, v6, request).await
                    }
                    other => other,
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
