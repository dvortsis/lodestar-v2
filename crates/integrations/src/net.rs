//! The SSRF guard for user-configured URLs (§12).
//!
//! Webhooks, notification endpoints and download clients are URLs a user
//! types in. Without a guard, Lodestar could be pointed at the cloud metadata
//! service, at a router's admin page, or at itself. The rules:
//!
//! - **Classification** ([`classify`]) puts every address in one class.
//!   IPv4 addresses embedded in IPv6 (mapped, compatible, NAT64, 6to4) are
//!   classified by the IPv4 address they carry.
//! - **Policy** ([`NetPolicy`]): link-local, metadata and reserved addresses
//!   are never allowed. Notifications allow public addresses, plus private
//!   ranges when the user opts in per channel; never loopback. Download
//!   clients are LAN services, so they also allow private ranges and
//!   loopback (a companion bound to localhost, §18.10).
//! - **One resolution per connection.** [`GuardedClient`] resolves a host
//!   once, refuses the connection if *any* answer is disallowed, and hands
//!   only the checked addresses to the connector. A second resolution (a new
//!   connection after a rebinding TTL) is checked again from scratch.
//! - **No proxy, no automatic redirects.** System proxies would resolve the
//!   name themselves, so they're off. Redirects are followed only by
//!   [`GuardedClient::send_following`], which checks every hop.
//! - **No URL in any error or log.** URLs can carry tokens (Discord webhooks
//!   do), so errors name only the address class; [`redact`] gives a safe
//!   form for logs.

use std::error::Error as StdError;
use std::fmt;
use std::io;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use reqwest::header::{AUTHORIZATION, COOKIE, LOCATION};
use reqwest::{Method, Request, Response, Url};

use crate::BoxFuture;

/// The class of an IP address, for [`NetPolicy`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AddrClass {
    /// Globally routable.
    Public,
    /// RFC 1918, carrier-grade NAT (`100.64.0.0/10`, used by Tailscale),
    /// IPv6 unique-local and the deprecated site-local range.
    Private,
    /// `127.0.0.0/8` and `::1`.
    Loopback,
    /// `169.254.0.0/16` and `fe80::/10`.
    LinkLocal,
    /// Cloud instance-metadata services: `169.254.169.254`,
    /// `fd00:ec2::254` (AWS IPv6) and `100.100.100.200` (Alibaba).
    Metadata,
    /// Unspecified, "this network", multicast, broadcast, documentation,
    /// benchmarking, Teredo and other special-purpose addresses.
    Reserved,
}

impl AddrClass {
    fn describe(self) -> &'static str {
        match self {
            AddrClass::Public => "a public address",
            AddrClass::Private => "a private-network address",
            AddrClass::Loopback => "a loopback address",
            AddrClass::LinkLocal => "a link-local address",
            AddrClass::Metadata => "a cloud metadata address",
            AddrClass::Reserved => "a reserved address",
        }
    }
}

const METADATA_V4: [Ipv4Addr; 2] = [
    Ipv4Addr::new(169, 254, 169, 254),
    Ipv4Addr::new(100, 100, 100, 200),
];
const METADATA_V6: Ipv6Addr = Ipv6Addr::new(0xfd00, 0x0ec2, 0, 0, 0, 0, 0, 0x0254);

/// Classifies an address. Embedded IPv4 addresses are classified by the
/// IPv4 address, so `::ffff:127.0.0.1` is loopback and
/// `64:ff9b::a9fe:a9fe` is the metadata service.
pub fn classify(ip: IpAddr) -> AddrClass {
    match ip {
        IpAddr::V4(v4) => classify_v4(v4),
        IpAddr::V6(v6) => classify_v6(v6),
    }
}

fn classify_v4(ip: Ipv4Addr) -> AddrClass {
    if METADATA_V4.contains(&ip) {
        return AddrClass::Metadata;
    }
    match ip.octets() {
        [0, ..] => AddrClass::Reserved,
        [127, ..] => AddrClass::Loopback,
        [169, 254, ..] => AddrClass::LinkLocal,
        [10, ..] | [192, 168, ..] => AddrClass::Private,
        [172, b, ..] if (16..=31).contains(&b) => AddrClass::Private,
        [100, b, ..] if (64..=127).contains(&b) => AddrClass::Private,
        [192, 0, 0 | 2, _] | [198, 51, 100, _] | [203, 0, 113, _] | [192, 88, 99, _] => {
            AddrClass::Reserved
        }
        [198, 18 | 19, ..] => AddrClass::Reserved,
        [a, ..] if a >= 224 => AddrClass::Reserved,
        _ => AddrClass::Public,
    }
}

fn classify_v6(ip: Ipv6Addr) -> AddrClass {
    if let Some(v4) = embedded_v4(ip) {
        return classify_v4(v4);
    }
    let s = ip.segments();
    // Local-use NAT64 with any other layout: the embedded address can't be
    // known, so it can't be checked.
    if ip.is_unspecified() || (s[0] == 0x64 && s[1] == 0xff9b) {
        return AddrClass::Reserved;
    }
    if ip.is_loopback() {
        return AddrClass::Loopback;
    }
    if ip == METADATA_V6 {
        return AddrClass::Metadata;
    }
    match s[0] {
        x if x & 0xffc0 == 0xfe80 => AddrClass::LinkLocal,
        x if x & 0xffc0 == 0xfec0 => AddrClass::Private,
        x if x & 0xfe00 == 0xfc00 => AddrClass::Private,
        x if x & 0xff00 == 0xff00 => AddrClass::Reserved,
        // Documentation (`2001:db8::/32`, `3fff::/20`), Teredo
        // (`2001::/32`), benchmarking (`2001:2::/48`) and ORCHID
        // (`2001:10::/28`).
        0x2001 if s[1] == 0x0db8 || s[1] == 0 || (s[1] == 2 && s[2] == 0) => AddrClass::Reserved,
        0x2001 if s[1] & 0xfff0 == 0x0010 => AddrClass::Reserved,
        0x3fff if s[1] & 0xf000 == 0 => AddrClass::Reserved,
        // Outside global unicast `2000::/3`, which also covers the SRv6
        // SID block `5f00::/16`.
        x if x & 0xe000 != 0x2000 => AddrClass::Reserved,
        _ => AddrClass::Public,
    }
}

/// The IPv4 address an IPv6 address carries: mapped (`::ffff:0:0/96`),
/// compatible (`::/96`, except `::` and `::1`), NAT64 (`64:ff9b::/96`, and
/// the local-use `64:ff9b:1::/48` when it follows the /96 layout) and 6to4
/// (`2002::/16`).
fn embedded_v4(ip: Ipv6Addr) -> Option<Ipv4Addr> {
    let s = ip.segments();
    let low = || Ipv4Addr::new((s[6] >> 8) as u8, s[6] as u8, (s[7] >> 8) as u8, s[7] as u8);
    if let Some(v4) = ip.to_ipv4_mapped() {
        return Some(v4);
    }
    if s[..6] == [0; 6] && !(s[6] == 0 && s[7] <= 1) {
        return Some(low());
    }
    if s[0] == 0x64 && s[1] == 0xff9b && s[3..6] == [0; 3] && s[2] <= 1 {
        return Some(low());
    }
    if s[0] == 0x2002 {
        return Some(Ipv4Addr::new(
            (s[1] >> 8) as u8,
            s[1] as u8,
            (s[2] >> 8) as u8,
            s[2] as u8,
        ));
    }
    None
}

/// Which address classes a kind of endpoint may reach.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NetPolicy {
    private: bool,
    loopback: bool,
}

impl NetPolicy {
    /// Webhooks and notification endpoints: public addresses, plus private
    /// ranges when the user opted in for this channel. Never loopback.
    pub const fn notification(allow_private: bool) -> NetPolicy {
        NetPolicy {
            private: allow_private,
            loopback: false,
        }
    }

    /// Download clients: LAN services by nature, so private ranges and
    /// loopback are allowed. Link-local and metadata addresses still aren't.
    pub const fn download_client() -> NetPolicy {
        NetPolicy {
            private: true,
            loopback: true,
        }
    }

    /// Loopback and public only, so unit tests can reach a local mock server
    /// while private ranges stay refused.
    #[cfg(test)]
    pub(crate) const fn loopback_for_tests() -> NetPolicy {
        NetPolicy {
            private: false,
            loopback: true,
        }
    }

    /// Whether this policy lets a connection reach `ip`.
    pub fn check(self, ip: IpAddr) -> Result<(), NetError> {
        let class = classify(ip);
        let ok = match class {
            AddrClass::Public => true,
            AddrClass::Private => self.private,
            AddrClass::Loopback => self.loopback,
            AddrClass::LinkLocal | AddrClass::Metadata | AddrClass::Reserved => false,
        };
        if ok {
            Ok(())
        } else {
            Err(NetError::Blocked(class))
        }
    }
}

/// Everything that can go wrong reaching a user-configured URL. No variant
/// carries the URL, its host or a response body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NetError {
    /// The URL doesn't parse, has no host, or isn't `http`/`https`.
    InvalidUrl(&'static str),
    /// The URL (or a host it resolved to, or a redirect) reaches an address
    /// class the policy refuses.
    Blocked(AddrClass),
    /// The name didn't resolve.
    Resolve,
    /// The connection failed.
    Connect,
    Timeout,
    /// The response was larger than the caller's limit.
    TooLarge,
    /// Too many redirects, a redirect without a usable `Location`, or a
    /// downgrade from `https` to `http`.
    Redirect,
    /// Any other HTTP-level failure.
    Http,
}

impl fmt::Display for NetError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            NetError::InvalidUrl(why) => write!(f, "invalid URL: {why}"),
            NetError::Blocked(class) => {
                write!(f, "refused: the address is {}", class.describe())
            }
            NetError::Resolve => f.write_str("the host name didn't resolve"),
            NetError::Connect => f.write_str("couldn't connect"),
            NetError::Timeout => f.write_str("timed out"),
            NetError::TooLarge => f.write_str("the response was too large"),
            NetError::Redirect => f.write_str("too many redirects, or an unsafe redirect"),
            NetError::Http => f.write_str("the HTTP exchange failed"),
        }
    }
}

impl StdError for NetError {}

/// Parses a user-entered URL and checks what can be checked without DNS:
/// the scheme, the absence of embedded credentials, and an IP-literal host.
pub fn parse_url(raw: &str, policy: NetPolicy) -> Result<Url, NetError> {
    let url = Url::parse(raw.trim()).map_err(|_| NetError::InvalidUrl("it doesn't parse"))?;
    check_url(&url, policy)?;
    Ok(url)
}

/// The DNS-free checks of [`parse_url`], for a URL already parsed.
pub fn check_url(url: &Url, policy: NetPolicy) -> Result<(), NetError> {
    if !matches!(url.scheme(), "http" | "https") {
        return Err(NetError::InvalidUrl("only http and https are supported"));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(NetError::InvalidUrl(
            "credentials in the URL aren't allowed; use the credential fields",
        ));
    }
    if url.host_str().is_none_or(str::is_empty) {
        return Err(NetError::InvalidUrl("it has no host"));
    }
    match literal_ip(url) {
        Some(ip) => policy.check(ip),
        None => Ok(()),
    }
}

/// The host as an IP address, when it's a literal. `Url` has already
/// normalised every IPv4 spelling (`0x7f.1`, `2130706433`) to dotted decimal
/// and puts IPv6 literals in brackets.
fn literal_ip(url: &Url) -> Option<IpAddr> {
    let host = url.host_str()?;
    match host.strip_prefix('[').and_then(|h| h.strip_suffix(']')) {
        Some(v6) => v6.parse::<Ipv6Addr>().ok().map(IpAddr::V6),
        None => host.parse::<Ipv4Addr>().ok().map(IpAddr::V4),
    }
}

/// A safe form of a URL for logs and messages: the scheme, host and port,
/// with the path, query and fragment (which may hold tokens) elided.
pub fn redact(url: &Url) -> String {
    let host = url.host_str().unwrap_or("?");
    match url.port() {
        Some(port) => format!("{}://{host}:{port}/…", url.scheme()),
        None => format!("{}://{host}/…", url.scheme()),
    }
}

/// DNS resolution, injectable so tests can simulate rebinding without a
/// network.
pub trait Resolve: Send + Sync + 'static {
    fn resolve<'a>(&'a self, host: &'a str) -> BoxFuture<'a, io::Result<Vec<IpAddr>>>;
}

/// The operating system's resolver.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemResolver;

impl Resolve for SystemResolver {
    fn resolve<'a>(&'a self, host: &'a str) -> BoxFuture<'a, io::Result<Vec<IpAddr>>> {
        Box::pin(async move {
            let addrs = tokio::net::lookup_host((host, 0)).await?;
            Ok(addrs.map(|a| a.ip()).collect())
        })
    }
}

/// Resolves `host` once and checks every answer. Refuses the host if any
/// answer is disallowed, so a mixed answer can't steer the connector to the
/// bad address on a retry.
pub async fn resolve_checked(
    resolver: &dyn Resolve,
    host: &str,
    policy: NetPolicy,
) -> Result<Vec<IpAddr>, NetError> {
    let addrs = resolver
        .resolve(host)
        .await
        .map_err(|_| NetError::Resolve)?;
    if addrs.is_empty() {
        return Err(NetError::Resolve);
    }
    for ip in &addrs {
        policy.check(*ip)?;
    }
    Ok(addrs)
}

/// Adapts a [`Resolve`] plus a policy to reqwest's resolver hook, so every
/// new connection resolves once and connects only to checked addresses.
struct GuardResolver {
    inner: Arc<dyn Resolve>,
    policy: NetPolicy,
}

impl reqwest::dns::Resolve for GuardResolver {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        let inner = Arc::clone(&self.inner);
        let policy = self.policy;
        Box::pin(async move {
            let addrs = resolve_checked(&*inner, name.as_str(), policy)
                .await
                .map_err(|e| Box::new(e) as Box<dyn StdError + Send + Sync>)?;
            let addrs: reqwest::dns::Addrs =
                Box::new(addrs.into_iter().map(|ip| SocketAddr::new(ip, 0)));
            Ok(addrs)
        })
    }
}

/// How many redirects [`GuardedClient::send_following`] follows.
pub const MAX_REDIRECTS: usize = 3;

/// An HTTP client that can only reach what its [`NetPolicy`] allows.
#[derive(Clone)]
pub struct GuardedClient {
    http: reqwest::Client,
    policy: NetPolicy,
}

impl fmt::Debug for GuardedClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GuardedClient")
            .field("policy", &self.policy)
            .finish_non_exhaustive()
    }
}

impl GuardedClient {
    /// A client with the system resolver and the given overall timeout per
    /// request (connecting is capped at 10 s within it).
    pub fn new(policy: NetPolicy, timeout: Duration) -> Result<GuardedClient, NetError> {
        GuardedClient::with_resolver(policy, timeout, Arc::new(SystemResolver))
    }

    pub fn with_resolver(
        policy: NetPolicy,
        timeout: Duration,
        resolver: Arc<dyn Resolve>,
    ) -> Result<GuardedClient, NetError> {
        // The workspace's TLS provider (§16). Only the first install wins,
        // and one installed by the host process is fine too.
        let _ = rustls::crypto::ring::default_provider().install_default();
        let http = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .referer(false)
            .dns_resolver(Arc::new(GuardResolver {
                inner: resolver,
                policy,
            }))
            .connect_timeout(timeout.min(Duration::from_secs(10)))
            .timeout(timeout)
            .user_agent(crate::USER_AGENT)
            .build()
            .map_err(|_| NetError::Http)?;
        Ok(GuardedClient { http, policy })
    }

    pub fn policy(&self) -> NetPolicy {
        self.policy
    }

    /// A request builder, for adapters in this crate. Finish it with
    /// `.build()` and send it with [`send`](Self::send) or
    /// [`send_following`](Self::send_following). `RequestBuilder::send` and
    /// `Client::execute` would skip the literal-IP check, so `clippy.toml`
    /// forbids them outside this module. Other crates build a
    /// `reqwest::Request` themselves; the raw client never leaves the crate.
    pub(crate) fn request(&self, method: Method, url: Url) -> reqwest::RequestBuilder {
        self.http.request(method, url)
    }

    /// Sends one request without following redirects.
    pub async fn send(&self, req: Request) -> Result<Response, NetError> {
        // Literal IP hosts never reach the resolver, so check them here.
        check_url(req.url(), self.policy)?;
        #[allow(clippy::disallowed_methods)] // the one checked call site
        let sent = self.http.execute(req).await;
        sent.map_err(map_reqwest)
    }

    /// Sends a request, following up to [`MAX_REDIRECTS`] method-preserving
    /// redirects (307/308), each checked like the first URL. Other 3xx
    /// responses are returned as they are: turning a POST into a GET would
    /// report a notification as delivered when its payload was dropped.
    /// Credentials headers are dropped on a cross-origin hop, and a
    /// downgrade from `https` to `http` is refused.
    pub async fn send_following(&self, mut req: Request) -> Result<Response, NetError> {
        for _ in 0..=MAX_REDIRECTS {
            let again = req.try_clone();
            let resp = self.send(req).await?;
            if !matches!(resp.status().as_u16(), 307 | 308) {
                return Ok(resp);
            }
            let Some(mut next) = again else {
                return Ok(resp);
            };
            let target = resp
                .headers()
                .get(LOCATION)
                .and_then(|v| v.to_str().ok())
                .and_then(|loc| resp.url().join(loc).ok())
                .ok_or(NetError::Redirect)?;
            if resp.url().scheme() == "https" && target.scheme() != "https" {
                return Err(NetError::Redirect);
            }
            if target.origin() != resp.url().origin() {
                next.headers_mut().remove(AUTHORIZATION);
                next.headers_mut().remove(COOKIE);
            }
            *next.url_mut() = target;
            req = next;
        }
        Err(NetError::Redirect)
    }
}

/// Maps a reqwest error without keeping its text, which includes the URL.
fn map_reqwest(e: reqwest::Error) -> NetError {
    let mut source: Option<&(dyn StdError + 'static)> = e.source();
    while let Some(err) = source {
        if let Some(net) = err.downcast_ref::<NetError>() {
            return net.clone();
        }
        source = err.source();
    }
    if e.is_timeout() {
        NetError::Timeout
    } else if e.is_connect() {
        NetError::Connect
    } else {
        NetError::Http
    }
}

/// Reads a response body, refusing more than `limit` bytes, so a hostile or
/// broken endpoint can't make Lodestar buffer without bound.
pub async fn read_limited(mut resp: Response, limit: usize) -> Result<Vec<u8>, NetError> {
    if resp.content_length().is_some_and(|n| n > limit as u64) {
        return Err(NetError::TooLarge);
    }
    let mut body = Vec::new();
    while let Some(chunk) = resp.chunk().await.map_err(map_reqwest)? {
        if body.len() + chunk.len() > limit {
            return Err(NetError::TooLarge);
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn class(s: &str) -> AddrClass {
        classify(s.parse().unwrap())
    }

    #[test]
    fn classifies_ipv4() {
        for (ip, want) in [
            ("8.8.8.8", AddrClass::Public),
            ("1.1.1.1", AddrClass::Public),
            ("172.32.0.1", AddrClass::Public),
            ("100.128.0.1", AddrClass::Public),
            ("10.1.2.3", AddrClass::Private),
            ("172.16.0.1", AddrClass::Private),
            ("172.31.255.255", AddrClass::Private),
            ("192.168.1.10", AddrClass::Private),
            ("100.64.0.1", AddrClass::Private),
            ("127.0.0.1", AddrClass::Loopback),
            ("127.255.255.254", AddrClass::Loopback),
            ("169.254.1.1", AddrClass::LinkLocal),
            ("169.254.169.254", AddrClass::Metadata),
            ("100.100.100.200", AddrClass::Metadata),
            ("0.0.0.0", AddrClass::Reserved),
            ("0.1.2.3", AddrClass::Reserved),
            ("224.0.0.1", AddrClass::Reserved),
            ("239.255.255.250", AddrClass::Reserved),
            ("240.0.0.1", AddrClass::Reserved),
            ("255.255.255.255", AddrClass::Reserved),
            ("192.0.2.1", AddrClass::Reserved),
            ("198.51.100.1", AddrClass::Reserved),
            ("203.0.113.1", AddrClass::Reserved),
            ("198.18.0.1", AddrClass::Reserved),
            ("192.0.0.8", AddrClass::Reserved),
        ] {
            assert_eq!(class(ip), want, "{ip}");
        }
    }

    #[test]
    fn classifies_ipv6() {
        for (ip, want) in [
            ("2606:4700:4700::1111", AddrClass::Public),
            ("2a00:1450:4001::200e", AddrClass::Public),
            ("::1", AddrClass::Loopback),
            ("::", AddrClass::Reserved),
            ("fe80::1", AddrClass::LinkLocal),
            ("febf::1", AddrClass::LinkLocal),
            ("fc00::1", AddrClass::Private),
            ("fd12:3456::1", AddrClass::Private),
            ("fec0::1", AddrClass::Private),
            ("fd00:ec2::254", AddrClass::Metadata),
            ("ff02::1", AddrClass::Reserved),
            ("2001:db8::1", AddrClass::Reserved),
            ("2001:0::1", AddrClass::Reserved),
            ("100::1", AddrClass::Reserved),
            ("4000::1", AddrClass::Reserved),
            ("2001:2::1", AddrClass::Reserved),
            ("2001:2:1::1", AddrClass::Public),
            ("2001:10::1", AddrClass::Reserved),
            ("2001:1f::1", AddrClass::Reserved),
            ("2001:20::1", AddrClass::Public),
            ("3fff::1", AddrClass::Reserved),
            ("3fff:fff::1", AddrClass::Reserved),
            ("3fff:1000::1", AddrClass::Public),
            ("5f00::1", AddrClass::Reserved),
        ] {
            assert_eq!(class(ip), want, "{ip}");
        }
    }

    #[test]
    fn embedded_ipv4_is_classified_by_the_ipv4_address() {
        for (ip, want) in [
            ("::ffff:127.0.0.1", AddrClass::Loopback),
            ("::ffff:7f00:1", AddrClass::Loopback),
            ("::ffff:169.254.169.254", AddrClass::Metadata),
            ("::ffff:10.0.0.1", AddrClass::Private),
            ("::ffff:8.8.8.8", AddrClass::Public),
            ("::127.0.0.1", AddrClass::Loopback),
            ("::169.254.169.254", AddrClass::Metadata),
            ("64:ff9b::a9fe:a9fe", AddrClass::Metadata),
            ("64:ff9b::7f00:1", AddrClass::Loopback),
            ("64:ff9b::808:808", AddrClass::Public),
            ("64:ff9b:1::c0a8:1", AddrClass::Private),
            ("64:ff9b:1::a9fe:a9fe", AddrClass::Metadata),
            // Local-use NAT64 laid out other than /96: unknowable, refused.
            ("64:ff9b:1:a9fe:a9fe::", AddrClass::Reserved),
            ("64:ff9b:1:0:0:1:808:808", AddrClass::Reserved),
            ("64:ff9b:2::808:808", AddrClass::Reserved),
            ("2002:7f00:1::1", AddrClass::Loopback),
            ("2002:a9fe:a9fe::", AddrClass::Metadata),
            ("2002:0808:0808::1", AddrClass::Public),
        ] {
            assert_eq!(class(ip), want, "{ip}");
        }
    }

    #[test]
    fn policies() {
        let n = NetPolicy::notification(false);
        let np = NetPolicy::notification(true);
        let dc = NetPolicy::download_client();
        let ip = |s: &str| s.parse::<IpAddr>().unwrap();
        assert!(n.check(ip("8.8.8.8")).is_ok());
        assert_eq!(
            n.check(ip("192.168.1.2")),
            Err(NetError::Blocked(AddrClass::Private))
        );
        assert!(np.check(ip("192.168.1.2")).is_ok());
        assert!(np.check(ip("fd00::2")).is_ok());
        for policy in [n, np] {
            assert_eq!(
                policy.check(ip("127.0.0.1")),
                Err(NetError::Blocked(AddrClass::Loopback))
            );
            assert_eq!(
                policy.check(ip("::ffff:127.0.0.1")),
                Err(NetError::Blocked(AddrClass::Loopback))
            );
        }
        assert!(dc.check(ip("127.0.0.1")).is_ok());
        assert!(dc.check(ip("::1")).is_ok());
        assert!(dc.check(ip("10.0.0.5")).is_ok());
        for policy in [n, np, dc] {
            for bad in [
                "169.254.169.254",
                "::ffff:169.254.169.254",
                "fd00:ec2::254",
                "169.254.10.10",
                "fe80::1",
                "0.0.0.0",
                "::",
                "224.0.0.251",
            ] {
                assert!(policy.check(ip(bad)).is_err(), "{policy:?} {bad}");
            }
        }
    }

    #[test]
    fn url_checks() {
        let n = NetPolicy::notification(false);
        assert!(parse_url("https://hooks.example.com/x?token=1", n).is_ok());
        assert_eq!(
            parse_url("ftp://example.com/", n),
            Err(NetError::InvalidUrl("only http and https are supported"))
        );
        assert!(matches!(
            parse_url("https://user:pw@example.com/", n),
            Err(NetError::InvalidUrl(_))
        ));
        assert!(matches!(
            parse_url("not a url", n),
            Err(NetError::InvalidUrl(_))
        ));
        for (literal, loopback) in [
            ("http://127.0.0.1/", true),
            ("http://127.1/", true),
            ("http://2130706433/", true),
            ("http://0x7f.0.0.1/", true),
            ("http://[::1]/", true),
            ("http://[::ffff:127.0.0.1]/", true),
            ("http://169.254.169.254/latest/meta-data/", false),
            ("http://[fd00:ec2::254]/", false),
            ("http://0/", false),
        ] {
            assert!(
                matches!(parse_url(literal, n), Err(NetError::Blocked(_))),
                "{literal}"
            );
            assert_eq!(
                parse_url(literal, NetPolicy::download_client()).is_ok(),
                loopback,
                "{literal}"
            );
        }
    }

    #[test]
    fn redaction_drops_path_and_query() {
        let url = Url::parse("https://discord.com/api/webhooks/123/SECRETTOKEN?x=1").unwrap();
        assert_eq!(redact(&url), "https://discord.com/…");
        let url = Url::parse("http://10.0.0.2:8080/api").unwrap();
        assert_eq!(redact(&url), "http://10.0.0.2:8080/…");
    }

    /// Answers from a script: each lookup of a host takes the next answer.
    pub(crate) struct ScriptedResolver {
        answers: Mutex<HashMap<String, Vec<Vec<IpAddr>>>>,
        pub(crate) lookups: AtomicUsize,
    }

    impl ScriptedResolver {
        pub(crate) fn new(script: &[(&str, &[&[&str]])]) -> Arc<ScriptedResolver> {
            let answers = script
                .iter()
                .map(|(host, answers)| {
                    let answers = answers
                        .iter()
                        .map(|a| a.iter().map(|ip| ip.parse().unwrap()).collect())
                        .collect();
                    (host.to_string(), answers)
                })
                .collect();
            Arc::new(ScriptedResolver {
                answers: Mutex::new(answers),
                lookups: AtomicUsize::new(0),
            })
        }
    }

    impl Resolve for ScriptedResolver {
        fn resolve<'a>(&'a self, host: &'a str) -> BoxFuture<'a, io::Result<Vec<IpAddr>>> {
            self.lookups.fetch_add(1, Ordering::SeqCst);
            let mut answers = self.answers.lock().unwrap();
            let next = match answers.get_mut(host) {
                Some(list) if list.len() > 1 => Ok(list.remove(0)),
                Some(list) => Ok(list[0].clone()),
                None => Err(io::Error::new(io::ErrorKind::NotFound, "no such host")),
            };
            Box::pin(async move { next })
        }
    }

    #[tokio::test]
    async fn rebinding_is_caught_on_the_second_resolution() {
        let r = ScriptedResolver::new(&[(
            "rebind.example",
            &[&["93.184.215.14"], &["169.254.169.254"], &["127.0.0.1"]],
        )]);
        let policy = NetPolicy::notification(true);
        assert_eq!(
            resolve_checked(&*r, "rebind.example", policy).await,
            Ok(vec!["93.184.215.14".parse().unwrap()])
        );
        assert_eq!(
            resolve_checked(&*r, "rebind.example", policy).await,
            Err(NetError::Blocked(AddrClass::Metadata))
        );
        assert_eq!(
            resolve_checked(&*r, "rebind.example", policy).await,
            Err(NetError::Blocked(AddrClass::Loopback))
        );
    }

    #[tokio::test]
    async fn a_mixed_answer_is_refused_whole() {
        let r = ScriptedResolver::new(&[("mixed.example", &[&["93.184.215.14", "10.0.0.1"]])]);
        assert_eq!(
            resolve_checked(&*r, "mixed.example", NetPolicy::notification(false)).await,
            Err(NetError::Blocked(AddrClass::Private))
        );
        assert_eq!(
            resolve_checked(&*r, "missing.example", NetPolicy::notification(false)).await,
            Err(NetError::Resolve)
        );
    }

    /// A local server that counts requests and answers each with `respond`.
    async fn serve(
        respond: impl Fn(&str) -> axum::response::Response + Send + Sync + 'static,
    ) -> (SocketAddr, Arc<AtomicUsize>) {
        use axum::extract::Request as AxReq;
        let hits = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&hits);
        let respond = Arc::new(respond);
        let app = axum::Router::new().fallback(move |req: AxReq| {
            let respond = Arc::clone(&respond);
            counter.fetch_add(1, Ordering::SeqCst);
            async move { respond(req.uri().path()) }
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (addr, hits)
    }

    fn redirect_to(location: &str) -> axum::response::Response {
        axum::response::Response::builder()
            .status(307)
            .header("location", location)
            .body(axum::body::Body::empty())
            .unwrap()
    }

    #[tokio::test]
    async fn a_host_resolving_to_loopback_never_gets_a_connection() {
        let (addr, hits) = serve(|_| axum::response::Response::new("ok".into())).await;
        let r = ScriptedResolver::new(&[("hook.example", &[&["127.0.0.1"]])]);
        let client = GuardedClient::with_resolver(
            NetPolicy::notification(true),
            Duration::from_secs(5),
            r.clone(),
        )
        .unwrap();
        let url = format!("http://hook.example:{}/x", addr.port());
        let req = client
            .request(Method::POST, Url::parse(&url).unwrap())
            .build()
            .unwrap();
        assert_eq!(
            client.send(req).await.unwrap_err(),
            NetError::Blocked(AddrClass::Loopback)
        );
        // An IP literal is refused before any resolution or connection.
        let req = client
            .request(
                Method::POST,
                Url::parse(&format!("http://127.0.0.1:{}/x", addr.port())).unwrap(),
            )
            .build()
            .unwrap();
        assert_eq!(
            client.send(req).await.unwrap_err(),
            NetError::Blocked(AddrClass::Loopback)
        );
        assert_eq!(hits.load(Ordering::SeqCst), 0);
        assert_eq!(r.lookups.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn the_checked_address_is_the_one_connected_to() {
        let (addr, hits) = serve(|_| axum::response::Response::new("ok".into())).await;
        let r = ScriptedResolver::new(&[("local.example", &[&["127.0.0.1"]])]);
        let client = GuardedClient::with_resolver(
            NetPolicy::loopback_for_tests(),
            Duration::from_secs(5),
            r,
        )
        .unwrap();
        let req = client
            .request(
                Method::GET,
                Url::parse(&format!("http://local.example:{}/", addr.port())).unwrap(),
            )
            .build()
            .unwrap();
        let resp = client.send(req).await.unwrap();
        assert_eq!(read_limited(resp, 16).await.unwrap(), b"ok");
        assert_eq!(hits.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn redirects_are_checked_hop_by_hop() {
        let r = ScriptedResolver::new(&[
            ("local.example", &[&["127.0.0.1"]]),
            ("metadata.example", &[&["169.254.169.254"]]),
        ]);
        let (addr, hits) = serve(move |path| match path {
            "/to-private" => redirect_to("http://10.0.0.1/"),
            "/to-metadata-name" => redirect_to("http://metadata.example/latest"),
            "/to-metadata" => redirect_to("http://169.254.169.254/latest"),
            "/loop" => redirect_to("/loop"),
            "/to-ok" => redirect_to("/ok"),
            "/see-other" => axum::response::Response::builder()
                .status(303)
                .header("location", "/ok")
                .body(axum::body::Body::empty())
                .unwrap(),
            _ => axum::response::Response::new("fine".into()),
        })
        .await;
        let client = GuardedClient::with_resolver(
            NetPolicy::loopback_for_tests(),
            Duration::from_secs(5),
            r,
        )
        .unwrap();
        let go = |path: &str| {
            client
                .request(
                    Method::POST,
                    Url::parse(&format!("http://local.example:{}{path}", addr.port())).unwrap(),
                )
                .body("{}")
                .build()
                .unwrap()
        };
        assert_eq!(
            client.send_following(go("/to-private")).await.unwrap_err(),
            NetError::Blocked(AddrClass::Private)
        );
        assert_eq!(
            client
                .send_following(go("/to-metadata-name"))
                .await
                .unwrap_err(),
            NetError::Blocked(AddrClass::Metadata)
        );
        assert_eq!(
            client.send_following(go("/to-metadata")).await.unwrap_err(),
            NetError::Blocked(AddrClass::Metadata)
        );
        assert_eq!(
            client.send_following(go("/loop")).await.unwrap_err(),
            NetError::Redirect
        );
        let ok = client.send_following(go("/to-ok")).await.unwrap();
        assert_eq!(ok.status(), 200);
        // A 303 isn't followed: it would turn the POST into a GET.
        let other = client.send_following(go("/see-other")).await.unwrap();
        assert_eq!(other.status(), 303);
        // to-private, to-metadata-name, to-metadata: 1 each; loop: 4;
        // to-ok: 2; see-other: 1.
        assert_eq!(hits.load(Ordering::SeqCst), 10);
    }

    #[tokio::test]
    async fn bodies_over_the_limit_are_refused() {
        let (addr, _) = serve(|_| axum::response::Response::new("x".repeat(100).into())).await;
        let r = ScriptedResolver::new(&[("local.example", &[&["127.0.0.1"]])]);
        let client = GuardedClient::with_resolver(
            NetPolicy::loopback_for_tests(),
            Duration::from_secs(5),
            r,
        )
        .unwrap();
        let req = client
            .request(
                Method::GET,
                Url::parse(&format!("http://local.example:{}/", addr.port())).unwrap(),
            )
            .build()
            .unwrap();
        let resp = client.send(req).await.unwrap();
        assert_eq!(read_limited(resp, 99).await, Err(NetError::TooLarge));
    }

    #[test]
    fn errors_never_carry_the_url() {
        for e in [
            NetError::InvalidUrl("x"),
            NetError::Blocked(AddrClass::Metadata),
            NetError::Resolve,
            NetError::Connect,
            NetError::Timeout,
            NetError::TooLarge,
            NetError::Redirect,
            NetError::Http,
        ] {
            let text = format!("{e} {e:?}");
            assert!(!text.contains("http://") && !text.contains("https://"));
        }
    }
}
