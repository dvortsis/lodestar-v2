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
//! - **Routes** ([`Route`]): a client can be pinned to a local address, a
//!   network interface or a SOCKS5 proxy, for per-plugin egress (§18.4).
//!   The route is the only way out: there's no fallback to a direct
//!   connection when it's down.
//! - **Raw TCP** ([`connect_tcp_routed`]) takes the same routes, with an
//!   in-house SOCKS5 handshake ([`socks`]) and an optional kill-switch
//!   ([`KillSwitch`]). [`connect_tcp`] keeps the direct route.
//! - **No URL in any error or log.** URLs can carry tokens (Discord webhooks
//!   do), so errors name only the address class; [`redact`] gives a safe
//!   form for logs.

use std::error::Error as StdError;
use std::fmt;
use std::io;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use reqwest::header::{HeaderMap, HeaderName, LOCATION};
use reqwest::{Method, Request, Response, Url};

use crate::BoxFuture;
use crate::secret::Secret;

pub mod socks;
mod tracker;
pub use tracker::public_tracker_url;

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
        self.check_class(classify(ip))
    }

    /// Whether this policy lets a connection reach an address of `class`.
    pub fn check_class(self, class: AddrClass) -> Result<(), NetError> {
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
    /// A bind-address route binds one address family only; the other
    /// family's sockets would connect unbound, so its answers are dropped.
    family: Option<Family>,
}

/// An IP address family.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Family {
    V4,
    V6,
}

impl Family {
    fn of(ip: IpAddr) -> Family {
        if ip.is_ipv4() { Family::V4 } else { Family::V6 }
    }
}

impl reqwest::dns::Resolve for GuardResolver {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        let inner = Arc::clone(&self.inner);
        let policy = self.policy;
        let family = self.family;
        Box::pin(async move {
            // A SOCKS5 route with local DNS hands IP literals here too.
            // reqwest passes an IPv6 literal in brackets.
            let host = name.as_str();
            let literal = host
                .strip_prefix('[')
                .and_then(|h| h.strip_suffix(']'))
                .unwrap_or(host)
                .parse::<IpAddr>()
                .ok();
            let mut addrs = match literal {
                Some(ip) => policy.check(ip).map(|()| vec![ip]),
                None => resolve_checked(&*inner, name.as_str(), policy).await,
            }
            .map_err(|e| Box::new(e) as Box<dyn StdError + Send + Sync>)?;
            if let Some(family) = family {
                addrs.retain(|ip| Family::of(*ip) == family);
                if addrs.is_empty() {
                    return Err(Box::new(NetError::Resolve) as Box<dyn StdError + Send + Sync>);
                }
            }
            let addrs: reqwest::dns::Addrs =
                Box::new(addrs.into_iter().map(|ip| SocketAddr::new(ip, 0)));
            Ok(addrs)
        })
    }
}

/// How many redirects [`GuardedClient::send_following`] follows.
pub const MAX_REDIRECTS: usize = 3;

/// How a [`GuardedClient`]'s connections leave the machine (§5, §18.4).
///
/// A route is fixed when the client is built and is the only way out: when
/// it's unavailable (the address is gone, the interface is down, the proxy
/// doesn't answer) connections fail. Nothing falls back to a direct
/// connection.
#[derive(Clone, PartialEq, Eq)]
pub enum Route {
    /// The operating system's routing table.
    Direct,
    /// Every socket is bound to this local address before connecting, and
    /// only destinations of its address family are connected to. Binding
    /// fails once the address is no longer assigned. On Windows (strong
    /// host model) that pins the interface owning the address; on Linux
    /// the routing table still picks the interface, so [`Route::Interface`]
    /// is the stronger choice there.
    BindAddress(IpAddr),
    /// Every socket is bound to this network interface by name
    /// (`SO_BINDTODEVICE`), so its packets can leave only through it. Linux
    /// only; elsewhere building the client fails with
    /// [`RouteError::Unsupported`].
    Interface(String),
    /// Every connection is tunnelled through a SOCKS5 proxy.
    Socks5(Socks5Route),
}

impl fmt::Debug for Route {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Route::Direct => f.write_str("Direct"),
            Route::BindAddress(ip) => f.debug_tuple("BindAddress").field(ip).finish(),
            Route::Interface(name) => f.debug_tuple("Interface").field(name).finish(),
            Route::Socks5(s) => f.debug_tuple("Socks5").field(s).finish(),
        }
    }
}

/// A SOCKS5 proxy route.
#[derive(Clone, PartialEq, Eq)]
pub struct Socks5Route {
    /// The proxy's address. A literal address, not a name: the proxy is
    /// infrastructure the user chose, so it isn't checked against the
    /// destination policy, and a name would be resolved through the guard.
    pub proxy: SocketAddr,
    pub dns: SocksDns,
    /// RFC 1929 user name and password, if the proxy wants them.
    pub auth: Option<SocksAuth>,
}

impl fmt::Debug for Socks5Route {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Socks5Route")
            .field("proxy", &self.proxy)
            .field("dns", &self.dns)
            .field("auth", &self.auth.as_ref().map(|_| "…"))
            .finish()
    }
}

/// Where destination names are resolved on a SOCKS5 route.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SocksDns {
    /// The proxy resolves names (`socks5h`), so lookups don't leak outside
    /// the tunnel. The guard can't see the address the proxy connects to,
    /// so it checks what it can: literal IP hosts, and names that are
    /// plainly local (`localhost`, single labels, `.local`, `.lan`,
    /// `.home.arpa`, `.internal`) are refused unless the policy allows
    /// loopback or private addresses. Rebinding on the proxy's side can't
    /// be caught.
    #[default]
    Proxy,
    /// Lodestar resolves names through the guard, refuses the name if any
    /// answer is disallowed, and sends the proxy a checked address. The
    /// lookup happens outside the tunnel.
    Local,
}

/// SOCKS5 user name and password (RFC 1929). Neither is ever printed.
#[derive(Clone)]
pub struct SocksAuth {
    pub username: Secret,
    pub password: Secret,
}

impl PartialEq for SocksAuth {
    fn eq(&self, other: &SocksAuth) -> bool {
        self.username.expose() == other.username.expose()
            && self.password.expose() == other.password.expose()
    }
}

impl Eq for SocksAuth {}

impl fmt::Debug for SocksAuth {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SocksAuth(…)")
    }
}

/// Why a [`Route`] can't be built. No variant carries a credential.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RouteError {
    /// This platform can't use the route.
    Unsupported(&'static str),
    /// The interface name can't be a network interface's name.
    InvalidInterface,
    /// A SOCKS5 user name or password is empty or longer than RFC 1929's
    /// 255 bytes.
    InvalidCredentials,
    /// The HTTP client couldn't be built.
    Build,
}

impl fmt::Display for RouteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RouteError::Unsupported(why) => f.write_str(why),
            RouteError::InvalidInterface => f.write_str("that isn't a valid interface name"),
            RouteError::InvalidCredentials => {
                f.write_str("the proxy's user name and password must each be 1 to 255 bytes")
            }
            RouteError::Build => f.write_str("the HTTP client couldn't be built"),
        }
    }
}

impl StdError for RouteError {}

/// Checks a network interface name as Linux does (`dev_valid_name`): 1 to
/// 15 bytes, not `.` or `..`, and no `/`, `:`, whitespace or control
/// characters. So the name is also safe as a single path segment.
pub fn check_interface_name(name: &str) -> Result<(), RouteError> {
    let ok = !name.is_empty()
        && name.len() < 16
        && name != "."
        && name != ".."
        && name
            .bytes()
            .all(|b| b.is_ascii_graphic() && b != b'/' && b != b':');
    if ok {
        Ok(())
    } else {
        Err(RouteError::InvalidInterface)
    }
}

/// Why [`Route::Interface`] fails off Linux.
pub const INTERFACE_UNSUPPORTED: &str = "choosing a network interface by name works only on Linux; \
     choose the interface's address instead";

/// Whether a name is plainly local, so a proxy resolving it would reach
/// its own loopback, LAN or cloud metadata service: the well-known
/// metadata names (`metadata.google.internal`, `metadata`,
/// `instance-data` and `instance-data.*`) are metadata; `localhost` and
/// `*.localhost` are loopback; single labels and the `.local`, `.lan`,
/// `.home.arpa` and `.internal` suffixes are private.
fn local_name_class(host: &str) -> Option<AddrClass> {
    let d = host.trim_end_matches('.').to_ascii_lowercase();
    if d == "metadata.google.internal"
        || d == "metadata"
        || d == "instance-data"
        || d.starts_with("instance-data.")
    {
        return Some(AddrClass::Metadata);
    }
    if d == "localhost" || d.ends_with(".localhost") {
        return Some(AddrClass::Loopback);
    }
    let local = !d.contains('.')
        || [".local", ".lan", ".home.arpa", ".internal"]
            .iter()
            .any(|s| d.ends_with(s));
    local.then_some(AddrClass::Private)
}

/// One HTTP answer, read whole within a size limit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Exchange {
    pub status: u16,
    /// Header values that aren't visible ASCII are dropped.
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

/// An HTTP client that can only reach what its [`NetPolicy`] allows.
#[derive(Clone)]
pub struct GuardedClient {
    http: reqwest::Client,
    policy: NetPolicy,
    /// Names are resolved by a SOCKS5 proxy, out of the guard's sight.
    remote_dns: bool,
    /// The only address family a bind-address route can connect to.
    family: Option<Family>,
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
        GuardedClient::with_route(policy, timeout, resolver, &Route::Direct)
            .map_err(|_| NetError::Http)
    }

    /// A client whose every connection takes `route`, and nothing else.
    pub fn with_route(
        policy: NetPolicy,
        timeout: Duration,
        resolver: Arc<dyn Resolve>,
        route: &Route,
    ) -> Result<GuardedClient, RouteError> {
        // The workspace's TLS provider (§16). Only the first install wins,
        // and one installed by the host process is fine too.
        let _ = rustls::crypto::ring::default_provider().install_default();
        let family = match route {
            Route::BindAddress(ip) => Some(Family::of(*ip)),
            _ => None,
        };
        // `no_proxy` first: it clears the proxy list, and turns system
        // proxies (and `NO_PROXY`) off.
        let mut builder = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .referer(false)
            .dns_resolver(Arc::new(GuardResolver {
                inner: resolver,
                policy,
                family,
            }))
            .connect_timeout(timeout.min(Duration::from_secs(10)))
            .timeout(timeout)
            .user_agent(crate::USER_AGENT);
        let mut remote_dns = false;
        match route {
            Route::Direct => {}
            Route::BindAddress(ip) => builder = builder.local_address(*ip),
            Route::Interface(name) => {
                check_interface_name(name)?;
                #[cfg(target_os = "linux")]
                {
                    builder = builder.interface(name);
                }
                #[cfg(not(target_os = "linux"))]
                return Err(RouteError::Unsupported(INTERFACE_UNSUPPORTED));
            }
            Route::Socks5(socks) => {
                let scheme = match socks.dns {
                    SocksDns::Proxy => "socks5h",
                    SocksDns::Local => "socks5",
                };
                remote_dns = socks.dns == SocksDns::Proxy;
                // A proxy for every scheme with no exceptions: `Proxy::all`
                // has no `NO_PROXY` list unless one is given.
                let mut proxy = reqwest::Proxy::all(format!("{scheme}://{}", socks.proxy))
                    .map_err(|_| RouteError::Build)?;
                if let Some(auth) = &socks.auth {
                    // hyper-util writes each length as one byte unchecked;
                    // a longer value would desynchronise the handshake.
                    let fits = |s: &Secret| (1..=255).contains(&s.expose().len());
                    if !fits(&auth.username) || !fits(&auth.password) {
                        return Err(RouteError::InvalidCredentials);
                    }
                    proxy = proxy.basic_auth(auth.username.expose(), auth.password.expose());
                }
                builder = builder.proxy(proxy);
            }
        }
        let http = builder.build().map_err(|_| RouteError::Build)?;
        Ok(GuardedClient {
            http,
            policy,
            remote_dns,
            family,
        })
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
        self.check_route(req.url())?;
        #[allow(clippy::disallowed_methods)] // the one checked call site
        let sent = self.http.execute(req).await;
        sent.map_err(map_reqwest)
    }

    /// What the route adds to [`check_url`]: a literal of the other family
    /// on a bind-address route would connect unbound, and a name a proxy
    /// resolves is checked by its form.
    fn check_route(&self, url: &Url) -> Result<(), NetError> {
        match literal_ip(url) {
            Some(ip) => {
                if self.family.is_some_and(|f| f != Family::of(ip)) {
                    return Err(NetError::InvalidUrl(
                        "the bind address can't reach that address family",
                    ));
                }
            }
            None if self.remote_dns => {
                let host = url.host_str().unwrap_or_default();
                if let Some(class) = local_name_class(host) {
                    self.policy.check_class(class)?;
                }
            }
            None => {}
        }
        Ok(())
    }

    /// One request without following redirects, the whole body read within
    /// `limit` bytes and the client's timeout. For callers outside this
    /// crate that don't build `reqwest` types (source plugins' egress).
    pub async fn exchange(
        &self,
        method: &str,
        url: &str,
        headers: &[(String, String)],
        body: Vec<u8>,
        limit: usize,
    ) -> Result<Exchange, NetError> {
        let req = build_request(method, url, headers, body)?;
        let resp = self.send(req).await?;
        let status = resp.status().as_u16();
        let headers = visible_headers(&resp);
        let body = read_limited(resp, limit).await?;
        Ok(Exchange {
            status,
            headers,
            body,
        })
    }

    /// One request without following redirects, as
    /// [`exchange`](Self::exchange), but with the body handed back as a
    /// [`StreamBody`] read a chunk at a time, never whole. `timeout`
    /// replaces the client's own for this request and covers everything
    /// from connecting to the last body byte (connecting is still capped
    /// at 10 s); the body carries at most `limit` bytes. Both are held to
    /// [`STREAM_CEILING_BYTES`] and [`STREAM_CEILING_TIMEOUT`] here,
    /// whatever the caller asks. The same checks as every request: the
    /// policy's address classes before and after DNS, and the route.
    pub async fn exchange_stream(
        &self,
        method: &str,
        url: &str,
        headers: &[(String, String)],
        body: Vec<u8>,
        limit: u64,
        timeout: Duration,
    ) -> Result<StreamExchange, NetError> {
        let mut req = build_request(method, url, headers, body)?;
        *req.timeout_mut() = Some(stream_timeout(timeout));
        let limit = limit.min(STREAM_CEILING_BYTES);
        let resp = self.send(req).await?;
        if resp.content_length().is_some_and(|n| n > limit) {
            return Err(NetError::TooLarge);
        }
        Ok(StreamExchange {
            status: resp.status().as_u16(),
            headers: visible_headers(&resp),
            body: StreamBody {
                resp: Some(resp),
                remaining: limit,
            },
        })
    }

    /// Sends a request, following up to [`MAX_REDIRECTS`] method-preserving
    /// redirects (307/308), each checked like the first URL. Other 3xx
    /// responses are returned as they are: turning a POST into a GET would
    /// report a notification as delivered when its payload was dropped.
    /// A downgrade from `https` to `http` is refused. A hop to another
    /// origin (scheme, host or port) keeps only `accept`,
    /// `accept-language`, `content-type`, `content-length` and `user-agent`
    /// (`CROSS_ORIGIN_HEADERS`) and drops every other header the request set, so no credential
    /// (`Authorization`, `Cookie`, `Proxy-Authorization`, `X-Api-Key`, or
    /// one a later channel names) follows it to another party. A hop on
    /// the same origin keeps every header.
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
                keep_cross_origin_headers(next.headers_mut());
            }
            *next.url_mut() = target;
            req = next;
        }
        Err(NetError::Redirect)
    }
}

/// The headers a redirect to another origin keeps: content negotiation and
/// the body's description, none of which can carry a credential. Every
/// other header goes (an allow list, so a credential header under a name
/// nobody listed can't leak). The same list as the sources egress
/// (`CROSS_ORIGIN_HEADERS` in `crates/sources/src/egress.rs`); keep the two
/// in step. Lowercase, as `HeaderName::as_str` gives them.
const CROSS_ORIGIN_HEADERS: [&str; 5] = [
    "accept",
    "accept-language",
    "content-type",
    "content-length",
    "user-agent",
];

/// Drops every header not in [`CROSS_ORIGIN_HEADERS`].
fn keep_cross_origin_headers(headers: &mut HeaderMap) {
    let gone: Vec<HeaderName> = headers
        .keys()
        .filter(|name| !CROSS_ORIGIN_HEADERS.contains(&name.as_str()))
        .cloned()
        .collect();
    for name in gone {
        headers.remove(name);
    }
}

/// The most body bytes [`GuardedClient::exchange_stream`] reads, whatever
/// its caller asks.
pub const STREAM_CEILING_BYTES: u64 = 1024 * 1024 * 1024;
/// The longest a streamed exchange may take, whatever its caller asks.
pub const STREAM_CEILING_TIMEOUT: Duration = Duration::from_secs(3600);

/// A streamed exchange's timeout: the caller's, held to the ceiling.
fn stream_timeout(asked: Duration) -> Duration {
    asked.min(STREAM_CEILING_TIMEOUT)
}

/// One HTTP answer whose body is read a chunk at a time
/// ([`GuardedClient::exchange_stream`]). Its `Debug` shows header names
/// only: a value may be a session cookie.
pub struct StreamExchange {
    pub status: u16,
    /// Header values that aren't visible ASCII are dropped.
    pub headers: Vec<(String, String)>,
    pub body: StreamBody,
}

/// A response body read a chunk at a time, within a byte limit and the
/// request's timeout. Dropping it closes the connection's stream.
pub struct StreamBody {
    /// `None` once the body has ended or failed: it isn't read again.
    resp: Option<Response>,
    remaining: u64,
}

impl fmt::Debug for StreamExchange {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let names: Vec<&str> = self.headers.iter().map(|(n, _)| n.as_str()).collect();
        f.debug_struct("StreamExchange")
            .field("status", &self.status)
            .field("headers", &names)
            .finish_non_exhaustive()
    }
}

impl fmt::Debug for StreamBody {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StreamBody")
            .field("remaining", &self.remaining)
            .finish_non_exhaustive()
    }
}

impl StreamBody {
    /// The next chunk, `Ok(None)` at the end. A chunk that would pass the
    /// limit fails with [`NetError::TooLarge`] and isn't returned. After
    /// the end or an error, every call returns `Ok(None)` without reading.
    pub async fn chunk(&mut self) -> Result<Option<Vec<u8>>, NetError> {
        let Some(resp) = self.resp.as_mut() else {
            return Ok(None);
        };
        let next = match resp.chunk().await {
            Ok(Some(c)) if c.len() as u64 > self.remaining => Err(NetError::TooLarge),
            Ok(Some(c)) => {
                self.remaining -= c.len() as u64;
                return Ok(Some(c.to_vec()));
            }
            Ok(None) => Ok(None),
            Err(e) => Err(map_reqwest(e)),
        };
        self.resp = None;
        next
    }
}

fn build_request(
    method: &str,
    url: &str,
    headers: &[(String, String)],
    body: Vec<u8>,
) -> Result<Request, NetError> {
    let method = Method::from_bytes(method.as_bytes()).map_err(|_| NetError::Http)?;
    let url = Url::parse(url).map_err(|_| NetError::InvalidUrl("it doesn't parse"))?;
    let mut req = Request::new(method, url);
    for (name, value) in headers {
        let name =
            reqwest::header::HeaderName::from_bytes(name.as_bytes()).map_err(|_| NetError::Http)?;
        let value = reqwest::header::HeaderValue::from_str(value).map_err(|_| NetError::Http)?;
        req.headers_mut().append(name, value);
    }
    if !body.is_empty() {
        *req.body_mut() = Some(body.into());
    }
    Ok(req)
}

fn visible_headers(resp: &Response) -> Vec<(String, String)> {
    resp.headers()
        .iter()
        .filter_map(|(n, v)| Some((n.as_str().to_owned(), v.to_str().ok()?.to_owned())))
        .collect()
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

/// Whether `url`'s host is plainly on the local network: a private IP
/// literal, a single-label name, or a `.lan`, `.local` or `.home.arpa`
/// name. Credentials may cross a plaintext connection only to such a host,
/// so `http://ntfy.example.com` with a token is refused even when the
/// channel may reach private addresses.
pub(crate) fn is_local_host(url: &Url) -> bool {
    let Some(host) = url.host_str() else {
        return false;
    };
    let literal = host.trim_start_matches('[').trim_end_matches(']');
    if let Ok(ip) = literal.parse::<IpAddr>() {
        return classify(ip) == AddrClass::Private;
    }
    let d = host.trim_end_matches('.').to_ascii_lowercase();
    !d.contains('.') || d.ends_with(".lan") || d.ends_with(".local") || d.ends_with(".home.arpa")
}

/// A user-entered host name or address for a raw TCP endpoint (SMTP),
/// normalised the way `Url` normalises an `http` host: every IPv4
/// spelling (`0x7f.1`, `2130706433`) becomes dotted decimal, names are
/// lowercased and IDNA-encoded, and a bare IPv6 address is bracketed. The
/// result is an `http` URL with only that host, so the URL checks
/// ([`check_url`], [`is_local_host`]) apply to it unchanged.
pub fn host_url(host: &str) -> Result<Url, NetError> {
    let host = host.trim();
    if host.is_empty() {
        return Err(NetError::InvalidUrl("it has no host"));
    }
    let bracketed;
    let host = if host.parse::<Ipv6Addr>().is_ok() {
        bracketed = format!("[{host}]");
        bracketed.as_str()
    } else {
        host
    };
    // `set_host` would quietly drop a `:port` suffix; the port is its own
    // setting, so a colon outside an IPv6 literal is refused.
    if !host.starts_with('[') && host.contains(':') {
        return Err(NetError::InvalidUrl("the host name isn't valid"));
    }
    let mut url = Url::parse("http://host.invalid/").map_err(|_| NetError::InvalidUrl("x"))?;
    url.set_host(Some(host))
        .map_err(|_| NetError::InvalidUrl("the host name isn't valid"))?;
    if url.host_str().is_none_or(str::is_empty) {
        return Err(NetError::InvalidUrl("it has no host"));
    }
    Ok(url)
}

/// The DNS-free checks for a TCP endpoint: the host as [`host_url`]
/// normalises it, a usable port, and a literal IP against `policy`.
pub fn check_host(host: &str, port: u16, policy: NetPolicy) -> Result<Url, NetError> {
    if port == 0 {
        return Err(NetError::InvalidUrl("the port isn't valid"));
    }
    let url = host_url(host)?;
    check_url(&url, policy)?;
    Ok(url)
}

/// Resolves a TCP endpoint once and checks every answer, as the HTTP guard
/// does: a literal IP is checked without DNS, and a name is refused whole if
/// any answer is disallowed. The result is the only addresses a caller may
/// connect to.
pub async fn resolve_tcp(
    resolver: &dyn Resolve,
    host: &str,
    port: u16,
    policy: NetPolicy,
) -> Result<Vec<SocketAddr>, NetError> {
    let url = check_host(host, port, policy)?;
    if let Some(ip) = literal_ip(&url) {
        return Ok(vec![SocketAddr::new(ip, port)]);
    }
    let name = url
        .host_str()
        .ok_or(NetError::InvalidUrl("it has no host"))?;
    let addrs = resolve_checked(resolver, name, policy).await?;
    Ok(addrs
        .into_iter()
        .map(|ip| SocketAddr::new(ip, port))
        .collect())
}

/// A guarded TCP connection, for protocols that can't use
/// [`GuardedClient`]'s HTTP stack (SMTP): the host is resolved once through
/// the system resolver and checked like an HTTP host, and only a checked
/// address is connected to. `timeout` covers resolving and connecting.
pub async fn connect_tcp(
    host: &str,
    port: u16,
    policy: NetPolicy,
    timeout: Duration,
) -> Result<tokio::net::TcpStream, NetError> {
    connect_tcp_with(&SystemResolver, host, port, policy, timeout).await
}

/// [`connect_tcp`] through a given resolver (tests simulate rebinding this
/// way). Each call resolves and checks from scratch.
pub async fn connect_tcp_with(
    resolver: &dyn Resolve,
    host: &str,
    port: u16,
    policy: NetPolicy,
    timeout: Duration,
) -> Result<tokio::net::TcpStream, NetError> {
    let connect = async {
        let addrs = resolve_tcp(resolver, host, port, policy).await?;
        for addr in addrs {
            if let Ok(stream) = tokio::net::TcpStream::connect(addr).await {
                return Ok(stream);
            }
        }
        Err(NetError::Connect)
    };
    tokio::time::timeout(timeout, connect)
        .await
        .map_err(|_| NetError::Timeout)?
}

/// A route's kill-switch, for [`connect_tcp_routed`] (§18.4, §5): while
/// it's down, no connection is opened, and a connection still being made
/// when it goes down is abandoned. Once the stream is handed over, keeping
/// it under the switch is the caller's job (it owns the stream).
pub trait KillSwitch: Send + Sync {
    /// Whether connections may be opened now.
    fn is_up(&self) -> bool;
    /// Resolves once the route is down. It must also resolve when the
    /// state can no longer change, so a lost switch fails closed.
    fn went_down(&self) -> BoxFuture<'_, ()>;
}

/// A `watch` channel of "the route is up". A closed channel counts as down.
impl KillSwitch for tokio::sync::watch::Receiver<bool> {
    fn is_up(&self) -> bool {
        self.has_changed().is_ok() && *self.borrow()
    }

    fn went_down(&self) -> BoxFuture<'_, ()> {
        let mut rx = self.clone();
        Box::pin(async move {
            // `Err` means the sender is gone: down for good.
            let _ = rx.wait_for(|up| !*up).await;
        })
    }
}

/// Why [`connect_tcp_routed`] failed. No variant carries a host, a
/// credential or proxy bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TcpRouteError {
    /// The guard refused the destination, or resolving or connecting
    /// failed. [`NetError::Connect`] also covers a bind address or
    /// interface that's gone, and a destination with no address in the
    /// bind address's family.
    Net(NetError),
    /// The route can't be used here (an interface off Linux, a bad
    /// interface name or proxy login).
    Route(RouteError),
    /// The SOCKS5 proxy's handshake failed.
    Socks(socks::SocksError),
    /// The kill-switch is down.
    KillSwitch,
}

impl fmt::Display for TcpRouteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TcpRouteError::Net(e) => e.fmt(f),
            TcpRouteError::Route(e) => e.fmt(f),
            TcpRouteError::Socks(e) => e.fmt(f),
            TcpRouteError::KillSwitch => {
                f.write_str("the route is down, so the kill-switch refused the connection")
            }
        }
    }
}

impl StdError for TcpRouteError {}

impl From<NetError> for TcpRouteError {
    fn from(e: NetError) -> Self {
        TcpRouteError::Net(e)
    }
}

impl From<RouteError> for TcpRouteError {
    fn from(e: RouteError) -> Self {
        TcpRouteError::Route(e)
    }
}

impl From<socks::SocksError> for TcpRouteError {
    fn from(e: socks::SocksError) -> Self {
        TcpRouteError::Socks(e)
    }
}

/// A guarded TCP connection over a [`Route`], for protocols that can't use
/// [`GuardedClient`]'s HTTP stack (IRC, WebSocket).
///
/// - **The guard** is [`connect_tcp_with`]'s: the destination is resolved
///   once and every answer checked, or, on a SOCKS5 route that resolves on
///   the proxy ([`SocksDns::Proxy`]), a literal address is checked and a
///   plainly local name refused as the HTTP client does.
/// - **The route** is the only way out: a bind address or interface that's
///   gone, or a proxy that doesn't answer, fails the connection. Nothing
///   falls back to a direct connection.
/// - **The kill-switch**, when given, is checked before anything is sent,
///   races the whole connection (an abandoned attempt's socket is closed),
///   and is checked again before the stream is returned.
/// - `timeout` covers resolving, connecting and the proxy handshake.
pub async fn connect_tcp_routed(
    resolver: &dyn Resolve,
    host: &str,
    port: u16,
    policy: NetPolicy,
    route: &Route,
    kill: Option<&dyn KillSwitch>,
    timeout: Duration,
) -> Result<tokio::net::TcpStream, TcpRouteError> {
    let refused = || kill.is_some_and(|k| !k.is_up());
    if refused() {
        return Err(TcpRouteError::KillSwitch);
    }
    check_route(route)?;
    let work = async {
        tokio::time::timeout(timeout, connect_on(resolver, host, port, policy, route))
            .await
            .map_err(|_| TcpRouteError::Net(NetError::Timeout))?
    };
    let stream = match kill {
        None => work.await?,
        // A biased race without tokio's `macros` feature: the switch is
        // polled first, so a switch that went down wins over a connection
        // finishing in the same wake-up.
        Some(k) => {
            let mut work = std::pin::pin!(work);
            let mut down = k.went_down();
            std::future::poll_fn(|cx| {
                if down.as_mut().poll(cx).is_ready() {
                    return std::task::Poll::Ready(Err(TcpRouteError::KillSwitch));
                }
                work.as_mut().poll(cx)
            })
            .await?
        }
    };
    if refused() {
        return Err(TcpRouteError::KillSwitch);
    }
    Ok(stream)
}

/// The DNS-free checks of a route, so a route that can't work fails before
/// any lookup or connection.
fn check_route(route: &Route) -> Result<(), RouteError> {
    match route {
        Route::Direct | Route::BindAddress(_) => Ok(()),
        Route::Interface(name) => {
            check_interface_name(name)?;
            if cfg!(target_os = "linux") {
                Ok(())
            } else {
                Err(RouteError::Unsupported(INTERFACE_UNSUPPORTED))
            }
        }
        Route::Socks5(s) => match &s.auth {
            Some(a)
                if !(1..=255).contains(&a.username.expose().len())
                    || !(1..=255).contains(&a.password.expose().len()) =>
            {
                Err(RouteError::InvalidCredentials)
            }
            _ => Ok(()),
        },
    }
}

async fn connect_on(
    resolver: &dyn Resolve,
    host: &str,
    port: u16,
    policy: NetPolicy,
    route: &Route,
) -> Result<tokio::net::TcpStream, TcpRouteError> {
    let socket_for = |addr: SocketAddr| {
        if addr.is_ipv4() {
            tokio::net::TcpSocket::new_v4()
        } else {
            tokio::net::TcpSocket::new_v6()
        }
    };
    match route {
        Route::Direct => {
            for addr in resolve_tcp(resolver, host, port, policy).await? {
                if let Ok(s) = tokio::net::TcpStream::connect(addr).await {
                    return Ok(s);
                }
            }
        }
        Route::BindAddress(local) => {
            let family = Family::of(*local);
            let addrs = resolve_tcp(resolver, host, port, policy).await?;
            for addr in addrs.into_iter().filter(|a| Family::of(a.ip()) == family) {
                let Ok(sock) = socket_for(addr) else { continue };
                if sock.bind(SocketAddr::new(*local, 0)).is_err() {
                    // The address is gone: every attempt would fail alike.
                    break;
                }
                if let Ok(s) = sock.connect(addr).await {
                    return Ok(s);
                }
            }
        }
        #[cfg(target_os = "linux")]
        Route::Interface(name) => {
            for addr in resolve_tcp(resolver, host, port, policy).await? {
                let Ok(sock) = socket_for(addr) else { continue };
                if sock.bind_device(Some(name.as_bytes())).is_err() {
                    break;
                }
                if let Ok(s) = sock.connect(addr).await {
                    return Ok(s);
                }
            }
        }
        #[cfg(not(target_os = "linux"))]
        Route::Interface(_) => {
            return Err(RouteError::Unsupported(INTERFACE_UNSUPPORTED).into());
        }
        Route::Socks5(s) => return via_socks(resolver, host, port, policy, s).await,
    }
    Err(TcpRouteError::Net(NetError::Connect))
}

/// The destinations to ask the proxy for, checked by the guard, then one
/// handshake per destination until one connects. A proxy that refuses a
/// destination is asked for the next; any other failure (a malformed
/// answer, a refused login) ends the attempt.
async fn via_socks(
    resolver: &dyn Resolve,
    host: &str,
    port: u16,
    policy: NetPolicy,
    route: &Socks5Route,
) -> Result<tokio::net::TcpStream, TcpRouteError> {
    use socks::Target;
    let targets: Vec<Target> = match route.dns {
        SocksDns::Local => resolve_tcp(resolver, host, port, policy)
            .await?
            .into_iter()
            .map(Target::Addr)
            .collect(),
        SocksDns::Proxy => {
            let url = check_host(host, port, policy)?;
            match literal_ip(&url) {
                Some(ip) => vec![Target::Addr(SocketAddr::new(ip, port))],
                None => {
                    let name = url
                        .host_str()
                        .ok_or(NetError::InvalidUrl("it has no host"))?;
                    if let Some(class) = local_name_class(name) {
                        policy.check_class(class)?;
                    }
                    vec![Target::Name(name.to_owned(), port)]
                }
            }
        }
    };
    let mut last = TcpRouteError::Net(NetError::Connect);
    for target in &targets {
        match socks_handshake(route, target).await {
            Ok(s) => return Ok(s),
            Err(e @ TcpRouteError::Socks(socks::SocksError::Refused(_))) => last = e,
            Err(e) => return Err(e),
        }
    }
    Err(last)
}

/// One SOCKS5 `CONNECT` through a fresh connection to the proxy. The proxy
/// is infrastructure the user chose, so its own address isn't checked
/// against the policy (as on the HTTP route).
async fn socks_handshake(
    route: &Socks5Route,
    target: &socks::Target,
) -> Result<tokio::net::TcpStream, TcpRouteError> {
    let stream = tokio::net::TcpStream::connect(route.proxy)
        .await
        .map_err(|_| NetError::Connect)?;
    let with_auth = route.auth.is_some();
    write_all(&stream, &socks::greeting(with_auth)).await?;
    socks::parse_method(read_array(&stream).await?, with_auth)?;
    if let Some(auth) = &route.auth {
        let req = zeroize::Zeroizing::new(socks::auth_request(
            auth.username.expose().as_bytes(),
            auth.password.expose().as_bytes(),
        )?);
        write_all(&stream, &req).await?;
        socks::parse_auth(read_array(&stream).await?)?;
    }
    write_all(&stream, &socks::connect_request(target)?).await?;
    let prefix: [u8; socks::REPLY_PREFIX] = read_array(&stream).await?;
    let len = socks::reply_len(prefix)?;
    let mut reply = [0u8; socks::MAX_REPLY];
    reply[..socks::REPLY_PREFIX].copy_from_slice(&prefix);
    read_full(&stream, &mut reply[socks::REPLY_PREFIX..len]).await?;
    // The bound address is the proxy's own outbound socket, not the
    // destination, so it's checked for form only (decisions NOS-2a).
    socks::parse_reply(&reply[..len])?;
    Ok(stream)
}

/// Writes all of `buf`. Uses the socket's readiness API, so the crate needs
/// no `io-util`.
async fn write_all(stream: &tokio::net::TcpStream, mut buf: &[u8]) -> Result<(), TcpRouteError> {
    while !buf.is_empty() {
        stream.writable().await.map_err(|_| NetError::Connect)?;
        match stream.try_write(buf) {
            Ok(0) => return Err(NetError::Connect.into()),
            Ok(n) => buf = &buf[n..],
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {}
            Err(_) => return Err(NetError::Connect.into()),
        }
    }
    Ok(())
}

/// Reads exactly `buf.len()` bytes and not one more, so nothing of the
/// tunnelled stream is consumed. An early close is a malformed answer.
async fn read_full(
    stream: &tokio::net::TcpStream,
    mut buf: &mut [u8],
) -> Result<(), TcpRouteError> {
    while !buf.is_empty() {
        stream.readable().await.map_err(|_| NetError::Connect)?;
        match stream.try_read(buf) {
            Ok(0) => {
                return Err(socks::SocksError::Malformed("the proxy closed mid-handshake").into());
            }
            Ok(n) => buf = &mut buf[n..],
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {}
            Err(_) => return Err(NetError::Connect.into()),
        }
    }
    Ok(())
}

async fn read_array<const N: usize>(
    stream: &tokio::net::TcpStream,
) -> Result<[u8; N], TcpRouteError> {
    let mut buf = [0u8; N];
    read_full(stream, &mut buf).await?;
    Ok(buf)
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
        redirect_with(307, location)
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

    /// A streamed exchange takes the same address checks as any other:
    /// a name resolving to loopback and a loopback literal are refused
    /// before a connection.
    #[tokio::test]
    async fn exchange_stream_refuses_private_addresses() {
        let (addr, hits) = serve(|_| axum::response::Response::new("ok".into())).await;
        let r = ScriptedResolver::new(&[
            ("hook.example", &[&["127.0.0.1"]]),
            ("lan.example", &[&["192.168.1.10"]]),
        ]);
        let client =
            GuardedClient::with_resolver(NetPolicy::notification(false), Duration::from_secs(5), r)
                .unwrap();
        for (url, class) in [
            (
                format!("http://hook.example:{}/x", addr.port()),
                AddrClass::Loopback,
            ),
            (
                format!("http://127.0.0.1:{}/x", addr.port()),
                AddrClass::Loopback,
            ),
            (
                format!("http://lan.example:{}/x", addr.port()),
                AddrClass::Private,
            ),
            ("http://10.0.0.1/x".to_owned(), AddrClass::Private),
        ] {
            let e = client
                .exchange_stream(
                    "GET",
                    &url,
                    &[],
                    Vec::new(),
                    1 << 20,
                    Duration::from_secs(5),
                )
                .await
                .unwrap_err();
            assert_eq!(e, NetError::Blocked(class), "{url}");
        }
        assert_eq!(hits.load(Ordering::SeqCst), 0);
    }

    /// The caller's timeout replaces the client's for a streamed exchange,
    /// held to [`STREAM_CEILING_TIMEOUT`]: no request can ask for more.
    #[tokio::test]
    async fn exchange_stream_holds_its_time_ceiling() {
        assert_eq!(stream_timeout(Duration::MAX), STREAM_CEILING_TIMEOUT);
        assert_eq!(
            stream_timeout(STREAM_CEILING_TIMEOUT + Duration::from_secs(1)),
            STREAM_CEILING_TIMEOUT
        );
        assert_eq!(
            stream_timeout(Duration::from_secs(5)),
            Duration::from_secs(5)
        );
        let (addr, _) = serve(|_| axum::response::Response::new("ok".into())).await;
        let r = ScriptedResolver::new(&[("local.example", &[&["127.0.0.1"]])]);
        // A client whose own timeout is a second: a streamed exchange
        // asking for any length, even the largest, gets the ceiling and
        // still works.
        let client = GuardedClient::with_resolver(
            NetPolicy::loopback_for_tests(),
            Duration::from_secs(1),
            r,
        )
        .unwrap();
        let url = format!("http://local.example:{}/", addr.port());
        let mut ex = client
            .exchange_stream("GET", &url, &[], Vec::new(), u64::MAX, Duration::MAX)
            .await
            .unwrap();
        let mut body = Vec::new();
        while let Some(c) = ex.body.chunk().await.unwrap() {
            body.extend_from_slice(&c);
        }
        assert_eq!(body, b"ok");
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

    /// One request a [`serve_recording`] server saw.
    struct Seen {
        path: String,
        headers: axum::http::HeaderMap,
        body: Vec<u8>,
    }

    /// A local server that records every request (path, headers, body)
    /// before answering it with `respond`, so a request is on record once
    /// its response has arrived.
    async fn serve_recording(
        respond: impl Fn(&str) -> axum::response::Response + Send + Sync + 'static,
    ) -> (SocketAddr, Arc<Mutex<Vec<Seen>>>) {
        use axum::extract::Request as AxReq;
        let seen = Arc::new(Mutex::new(Vec::new()));
        let log = Arc::clone(&seen);
        let respond = Arc::new(respond);
        let app = axum::Router::new().fallback(move |req: AxReq| {
            let respond = Arc::clone(&respond);
            let log = Arc::clone(&log);
            async move {
                let (parts, body) = req.into_parts();
                let body = axum::body::to_bytes(body, 1 << 20).await.unwrap();
                log.lock().unwrap().push(Seen {
                    path: parts.uri.path().to_string(),
                    headers: parts.headers,
                    body: body.to_vec(),
                });
                respond(parts.uri.path())
            }
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (addr, seen)
    }

    fn redirect_with(status: u16, location: &str) -> axum::response::Response {
        axum::response::Response::builder()
            .status(status)
            .header("location", location)
            .body(axum::body::Body::empty())
            .unwrap()
    }

    /// The headers a test request sets that may carry a credential, or that
    /// no allow list names.
    const NOT_ALLOWED: [(&str, &str); 6] = [
        ("authorization", "Bearer secret-token"),
        ("cookie", "session=secret"),
        ("proxy-authorization", "Basic c2VjcmV0"),
        ("x-api-key", "secret-key"),
        ("x-custom-secret", "anything"),
        ("x-lodestar-delivery", "n-1"),
    ];
    /// Allow-listed headers a test request sets, with their values.
    const ALLOWED: [(&str, &str); 4] = [
        ("accept", "application/json"),
        ("accept-language", "en"),
        ("content-type", "application/json"),
        ("user-agent", "lodestar-test"),
    ];
    const BODY: &str = r#"{"id":"n-1"}"#;

    fn hop_client() -> GuardedClient {
        let r = ScriptedResolver::new(&[
            ("local.example", &[&["127.0.0.1"]]),
            ("other.example", &[&["127.0.0.1"]]),
        ]);
        GuardedClient::with_resolver(NetPolicy::loopback_for_tests(), Duration::from_secs(5), r)
            .unwrap()
    }

    fn hop_request(client: &GuardedClient, url: &str) -> Request {
        let mut req = client
            .request(Method::POST, Url::parse(url).unwrap())
            .body(BODY);
        for (name, value) in NOT_ALLOWED.iter().chain(ALLOWED.iter()) {
            req = req.header(*name, *value);
        }
        req.build().unwrap()
    }

    /// The landing request kept the allow-listed headers and the body.
    fn assert_kept_allowed(seen: &Seen) {
        for (name, value) in ALLOWED {
            assert_eq!(
                seen.headers.get(name).unwrap(),
                value,
                "{name} on {}",
                seen.path
            );
        }
        assert_eq!(seen.body, BODY.as_bytes(), "body on {}", seen.path);
    }

    /// The landing request carried none of the headers outside the list.
    fn assert_dropped(seen: &Seen) {
        for (name, _) in NOT_ALLOWED {
            assert!(
                seen.headers.get(name).is_none(),
                "{name} followed a cross-origin redirect to {}",
                seen.path
            );
        }
        for name in seen.headers.keys() {
            assert!(
                CROSS_ORIGIN_HEADERS.contains(&name.as_str()) || name == "host",
                "{name} followed a cross-origin redirect to {}",
                seen.path
            );
        }
    }

    #[tokio::test]
    async fn a_cross_host_redirect_keeps_only_allow_listed_headers() {
        // Same scheme and port, another host: another origin.
        let port = Arc::new(std::sync::OnceLock::<u16>::new());
        let at = Arc::clone(&port);
        let (addr, seen) = serve_recording(move |path| {
            let port = at.get().unwrap();
            match path {
                "/307" => redirect_with(307, &format!("http://other.example:{port}/land-307")),
                "/308" => redirect_with(308, &format!("http://other.example:{port}/land-308")),
                _ => axum::response::Response::new("fine".into()),
            }
        })
        .await;
        port.set(addr.port()).unwrap();
        let client = hop_client();
        for status in [307, 308] {
            let url = format!("http://local.example:{}/{status}", addr.port());
            let resp = client
                .send_following(hop_request(&client, &url))
                .await
                .unwrap();
            assert_eq!(resp.status(), 200);
        }
        let seen = seen.lock().unwrap();
        let paths: Vec<&str> = seen.iter().map(|s| s.path.as_str()).collect();
        assert_eq!(paths, ["/307", "/land-307", "/308", "/land-308"]);
        // The first hop got everything; the landing only the allow list.
        for (name, value) in NOT_ALLOWED {
            assert_eq!(seen[0].headers.get(name).unwrap(), value);
        }
        for landing in [&seen[1], &seen[3]] {
            assert_eq!(
                landing.headers.get("host").unwrap(),
                &format!("other.example:{}", addr.port())
            );
            assert_dropped(landing);
            assert_kept_allowed(landing);
        }
    }

    #[tokio::test]
    async fn a_change_of_port_alone_is_another_origin() {
        let (landing_addr, landing) =
            serve_recording(|_| axum::response::Response::new("fine".into())).await;
        let target = format!("http://local.example:{}/land", landing_addr.port());
        let (first_addr, first) = serve_recording(move |path| match path {
            "/307" => redirect_with(307, &target),
            "/308" => redirect_with(308, &target),
            _ => axum::response::Response::new("unexpected".into()),
        })
        .await;
        let client = hop_client();
        for status in [307, 308] {
            let url = format!("http://local.example:{}/{status}", first_addr.port());
            let resp = client
                .send_following(hop_request(&client, &url))
                .await
                .unwrap();
            assert_eq!(resp.status(), 200);
        }
        assert_eq!(first.lock().unwrap().len(), 2);
        let landing = landing.lock().unwrap();
        assert_eq!(landing.len(), 2);
        for seen in landing.iter() {
            assert_eq!(seen.path, "/land");
            assert_dropped(seen);
            assert_kept_allowed(seen);
        }
    }

    #[tokio::test]
    async fn credentials_dropped_on_the_way_out_dont_come_back() {
        // A -> B -> A: each hop starts from the request already filtered,
        // so the return to A carries only the allow list too.
        let a_port = Arc::new(std::sync::OnceLock::<u16>::new());
        let at = Arc::clone(&a_port);
        let (b_addr, b_seen) = serve_recording(move |path| match path {
            "/back" => redirect_with(
                307,
                &format!("http://local.example:{}/home", at.get().unwrap()),
            ),
            _ => axum::response::Response::new("unexpected".into()),
        })
        .await;
        let to_b = format!("http://local.example:{}/back", b_addr.port());
        let (a_addr, a_seen) = serve_recording(move |path| match path {
            "/start" => redirect_with(307, &to_b),
            _ => axum::response::Response::new("fine".into()),
        })
        .await;
        a_port.set(a_addr.port()).unwrap();
        let client = hop_client();
        let url = format!("http://local.example:{}/start", a_addr.port());
        let resp = client
            .send_following(hop_request(&client, &url))
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        let a_seen = a_seen.lock().unwrap();
        let paths: Vec<&str> = a_seen.iter().map(|s| s.path.as_str()).collect();
        assert_eq!(paths, ["/start", "/home"]);
        assert_eq!(b_seen.lock().unwrap().len(), 1);
        assert_dropped(&b_seen.lock().unwrap()[0]);
        let home = &a_seen[1];
        assert!(home.headers.get("authorization").is_none());
        assert!(home.headers.get("x-custom-secret").is_none());
        assert_dropped(home);
        assert_kept_allowed(home);
    }

    #[tokio::test]
    async fn a_same_origin_redirect_keeps_every_header() {
        let port = Arc::new(std::sync::OnceLock::<u16>::new());
        let at = Arc::clone(&port);
        let (addr, seen) = serve_recording(move |path| match path {
            "/307" => redirect_with(307, "/land-307"),
            // An absolute URL naming the same origin is still the same origin.
            "/308" => redirect_with(
                308,
                &format!("http://local.example:{}/land-308", at.get().unwrap()),
            ),
            _ => axum::response::Response::new("fine".into()),
        })
        .await;
        port.set(addr.port()).unwrap();
        let client = hop_client();
        for status in [307, 308] {
            let url = format!("http://local.example:{}/{status}", addr.port());
            let resp = client
                .send_following(hop_request(&client, &url))
                .await
                .unwrap();
            assert_eq!(resp.status(), 200);
        }
        let seen = seen.lock().unwrap();
        let paths: Vec<&str> = seen.iter().map(|s| s.path.as_str()).collect();
        assert_eq!(paths, ["/307", "/land-307", "/308", "/land-308"]);
        for landing in [&seen[1], &seen[3]] {
            for (name, value) in NOT_ALLOWED.iter().chain(ALLOWED.iter()) {
                assert_eq!(landing.headers.get(*name).unwrap(), value, "{name}");
            }
            assert_eq!(landing.body, BODY.as_bytes());
        }
    }

    #[test]
    fn the_cross_origin_list_drops_everything_else() {
        let mut headers = HeaderMap::new();
        for (name, value) in NOT_ALLOWED.iter().chain(ALLOWED.iter()) {
            headers.insert(*name, value.parse().unwrap());
        }
        headers.insert("content-length", "12".parse().unwrap());
        // A repeated header goes whole.
        headers.append("x-api-key", "second".parse().unwrap());
        keep_cross_origin_headers(&mut headers);
        let mut names: Vec<&str> = headers.keys().map(HeaderName::as_str).collect();
        names.sort_unstable();
        let mut want = CROSS_ORIGIN_HEADERS.to_vec();
        want.sort_unstable();
        assert_eq!(names, want);
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
    fn tcp_hosts_are_normalised_like_url_hosts() {
        let host = |h: &str| host_url(h).map(|u| u.host_str().unwrap().to_owned());
        assert_eq!(host("Mail.Example.COM").unwrap(), "mail.example.com");
        assert_eq!(host("0x7f.1").unwrap(), "127.0.0.1");
        assert_eq!(host("2130706433").unwrap(), "127.0.0.1");
        assert_eq!(host("::1").unwrap(), "[::1]");
        assert_eq!(host("[::ffff:7f00:1]").unwrap(), "[::ffff:7f00:1]");
        assert_eq!(host("bücher.example").unwrap(), "xn--bcher-kva.example");
        for bad in ["", " ", "a/b", "user@host", "host:25", "a b", "[::1"] {
            assert!(host(bad).is_err(), "{bad:?}");
        }
        assert!(check_host("mail.example.com", 0, NetPolicy::notification(false)).is_err());
    }

    #[tokio::test]
    async fn tcp_literals_are_refused_before_any_lookup_or_connection() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let r = ScriptedResolver::new(&[]);
        let n = NetPolicy::notification(false);
        let np = NetPolicy::notification(true);
        for (host, policy, want) in [
            ("127.0.0.1", np, AddrClass::Loopback),
            ("127.1", np, AddrClass::Loopback),
            ("0x7f.0.0.1", np, AddrClass::Loopback),
            ("2130706433", np, AddrClass::Loopback),
            ("::1", np, AddrClass::Loopback),
            ("[::ffff:127.0.0.1]", np, AddrClass::Loopback),
            ("169.254.169.254", np, AddrClass::Metadata),
            ("fd00:ec2::254", np, AddrClass::Metadata),
            ("10.0.0.1", n, AddrClass::Private),
            ("0", np, AddrClass::Reserved),
        ] {
            assert_eq!(
                connect_tcp_with(&*r, host, port, policy, Duration::from_secs(5))
                    .await
                    .unwrap_err(),
                NetError::Blocked(want),
                "{host}"
            );
        }
        assert_eq!(r.lookups.load(Ordering::SeqCst), 0);
        // Nothing connected: the listener has no pending connection.
        let accepted = tokio::time::timeout(Duration::from_millis(50), listener.accept()).await;
        assert!(accepted.is_err());
    }

    #[tokio::test]
    async fn tcp_names_are_resolved_once_and_checked_whole() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let r = ScriptedResolver::new(&[
            ("metadata.example", &[&["169.254.169.254"]]),
            ("mixed.example", &[&["93.184.215.14", "127.0.0.1"]]),
            ("loopback.example", &[&["127.0.0.1"]]),
            ("rebind.example", &[&["127.0.0.1"], &["169.254.169.254"]]),
        ]);
        let t = Duration::from_secs(5);
        let np = NetPolicy::notification(true);
        let test = NetPolicy::loopback_for_tests();
        assert_eq!(
            connect_tcp_with(&*r, "metadata.example", port, test, t)
                .await
                .unwrap_err(),
            NetError::Blocked(AddrClass::Metadata)
        );
        // A mixed answer is refused whole, even though one address is fine.
        assert_eq!(
            connect_tcp_with(&*r, "mixed.example", port, np, t)
                .await
                .unwrap_err(),
            NetError::Blocked(AddrClass::Loopback)
        );
        assert_eq!(
            connect_tcp_with(&*r, "loopback.example", port, np, t)
                .await
                .unwrap_err(),
            NetError::Blocked(AddrClass::Loopback)
        );
        assert_eq!(
            connect_tcp_with(&*r, "missing.example", port, test, t)
                .await
                .unwrap_err(),
            NetError::Resolve
        );
        // Rebinding: the first connection is checked and made; the second
        // resolves again and is refused.
        let stream = connect_tcp_with(&*r, "rebind.example", port, test, t)
            .await
            .unwrap();
        assert_eq!(stream.peer_addr().unwrap().port(), port);
        assert!(listener.accept().await.is_ok());
        assert_eq!(
            connect_tcp_with(&*r, "rebind.example", port, test, t)
                .await
                .unwrap_err(),
            NetError::Blocked(AddrClass::Metadata)
        );
        assert_eq!(r.lookups.load(Ordering::SeqCst), 6);
    }

    #[tokio::test]
    async fn tcp_connection_failures_are_connect_errors() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let r = ScriptedResolver::new(&[("local.example", &[&["127.0.0.1"]])]);
        assert_eq!(
            connect_tcp_with(
                &*r,
                "local.example",
                port,
                NetPolicy::loopback_for_tests(),
                Duration::from_secs(5)
            )
            .await
            .unwrap_err(),
            NetError::Connect
        );
    }

    #[test]
    fn local_hosts() {
        let local = |h: &str| is_local_host(&host_url(h).unwrap());
        for yes in [
            "192.168.1.5",
            "10.0.0.2",
            "fd00::5",
            "relay",
            "relay.lan",
            "R.Local",
            "x.home.arpa",
            "relay.",
        ] {
            assert!(local(yes), "{yes}");
        }
        for no in [
            "smtp.example.com",
            "relay.lan.example.com",
            "93.184.216.34",
            "127.0.0.1",
            "x.arpa",
        ] {
            assert!(!local(no), "{no}");
        }
    }

    #[test]
    fn interface_names_are_checked_like_linux_does() {
        for ok in ["lo", "eth0", "wg0", "tun-vpn.1", "a234567890abcde"] {
            assert_eq!(check_interface_name(ok), Ok(()), "{ok}");
        }
        for bad in [
            "",
            ".",
            "..",
            "../lo",
            "eth0:1",
            "has space",
            "tab\t",
            "a234567890abcdef",
            "é",
        ] {
            assert_eq!(
                check_interface_name(bad),
                Err(RouteError::InvalidInterface),
                "{bad}"
            );
        }
    }

    #[test]
    fn plainly_local_names_are_classified() {
        for (host, want) in [
            ("localhost", Some(AddrClass::Loopback)),
            ("LOCALHOST.", Some(AddrClass::Loopback)),
            ("admin.localhost", Some(AddrClass::Loopback)),
            ("router", Some(AddrClass::Private)),
            ("nas.lan", Some(AddrClass::Private)),
            ("printer.local", Some(AddrClass::Private)),
            ("box.home.arpa", Some(AddrClass::Private)),
            ("db.internal", Some(AddrClass::Private)),
            ("metadata.google.internal.", Some(AddrClass::Metadata)),
            ("metadata", Some(AddrClass::Metadata)),
            ("instance-data.ec2.internal", Some(AddrClass::Metadata)),
            ("nyaa.si", None),
            ("localhost.example.com", None),
            ("internal.example", None),
        ] {
            assert_eq!(local_name_class(host), want, "{host}");
        }
    }

    #[test]
    fn route_debug_hides_credentials() {
        let route = Route::Socks5(Socks5Route {
            proxy: "10.64.0.1:1080".parse().unwrap(),
            dns: SocksDns::Proxy,
            auth: Some(SocksAuth {
                username: Secret::new("acct-1234"),
                password: Secret::new("hunter2"),
            }),
        });
        let shown = format!("{route:?}");
        assert!(shown.contains("10.64.0.1:1080"), "{shown}");
        assert!(
            !shown.contains("acct-1234") && !shown.contains("hunter2"),
            "{shown}"
        );
    }

    #[tokio::test]
    async fn guard_resolver_checks_literals_without_the_inner_resolver() {
        use reqwest::dns::Resolve as _;
        let inner = ScriptedResolver::new(&[]);
        let guard = GuardResolver {
            inner: inner.clone(),
            policy: NetPolicy::notification(false),
            family: None,
        };
        let name = |s: &str| s.parse::<reqwest::dns::Name>().unwrap();
        assert!(guard.resolve(name("127.0.0.1")).await.is_err());
        assert!(guard.resolve(name("[::1]")).await.is_err());
        let v6: Vec<_> = guard
            .resolve(name("[2606:4700:4700::1111]"))
            .await
            .unwrap()
            .collect();
        assert_eq!(
            v6,
            vec!["[2606:4700:4700::1111]:0".parse::<SocketAddr>().unwrap()]
        );
        let addrs: Vec<_> = guard.resolve(name("8.8.8.8")).await.unwrap().collect();
        assert_eq!(addrs, vec!["8.8.8.8:0".parse::<SocketAddr>().unwrap()]);
        // A v6 bind drops v4 answers, so nothing is left.
        let guard = GuardResolver {
            inner: inner.clone(),
            policy: NetPolicy::notification(false),
            family: Some(Family::V6),
        };
        assert!(guard.resolve(name("8.8.8.8")).await.is_err());
        assert_eq!(inner.lookups.load(Ordering::SeqCst), 0);
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
