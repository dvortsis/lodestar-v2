//! Per-plugin egress and the kill switch (§18.4, §18.7, §5): the one
//! factory every source plugin's HTTP client comes from.
//!
//! - **Routes.** A plugin's traffic goes direct, from a chosen local
//!   address, through a named network interface, or through a SOCKS5
//!   proxy ([`Egress`]). The client is built for that route alone, so
//!   there is nothing to fall back to.
//! - **Kill switch.** For any route but direct, every request first checks
//!   the route is up (the address is assigned, the interface exists and is
//!   up, the proxy accepts connections) and is refused when it isn't. A
//!   request in flight when the route goes down is abandoned. The state is
//!   observable per plugin ([`PluginEgress::status`]) with counters
//!   ([`EgressCounters`]). It's always on for a non-direct route: choosing
//!   a route is the opt-in (§18.4's "optional kill-switch").
//! - **Endpoint policy (§12).** Every request is checked against the SSRF
//!   guard's [`NetPolicy`] after DNS resolution, and against the plugin's
//!   host list when it has one ([`Endpoints`]). [`Fetch`] never follows
//!   redirects (SRC-0), so each hop is a new request, checked again;
//!   [`send_following`] bounds the hops.
//! - **Streamed bodies** ([`Fetch::fetch_stream`], SRC-12b) take the same
//!   checks, and the kill switch's in-flight abort goes on covering the
//!   body after the headers. Their timeout and byte cap are the plugin's,
//!   held to `net`'s own ceiling.
//! - **Credentials** for a SOCKS5 proxy come from [`EgressCredentials`]
//!   (the per-source credential store, SRC-4) and never appear in a log,
//!   an error or a `Debug` string.
//!
//! The host spawns [`PluginEgress::monitor`] per plugin so a route that
//! goes down mid-request is noticed while the request is in flight;
//! without it the check before each request still refuses new ones.

use std::fmt;
use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub use lodestar_integrations::net::SocksDns;
use lodestar_integrations::net::{
    self, AddrClass, Exchange, GuardedClient, NetError, NetPolicy, Resolve, Route, Socks5Route,
    SocksAuth, SystemResolver,
};
use tokio::sync::watch;
use tokio::time::Instant;

use crate::http::{
    BodyStream, Client, Fetch, HttpError, MAX_BODY, Method, Request, Response, StreamLimits,
    StreamResponse, host_key,
};
use crate::manifest::Endpoint;
use crate::plugin::BoxFuture;

/// How a plugin's traffic leaves the machine.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Egress {
    #[default]
    Direct,
    /// From this local address (a VPN interface's address). On Windows that
    /// pins the interface; on Linux prefer [`Egress::Interface`].
    BindAddress(IpAddr),
    /// Through this network interface (Linux only; elsewhere the plugin's
    /// egress stays down with [`DownReason::Unsupported`]).
    Interface(String),
    /// Through a SOCKS5 proxy at a literal address. Its credentials, if
    /// any, come from [`EgressCredentials`].
    Socks5 { proxy: SocketAddr, dns: SocksDns },
}

impl Egress {
    pub fn kind(&self) -> RouteKind {
        match self {
            Egress::Direct => RouteKind::Direct,
            Egress::BindAddress(_) => RouteKind::BindAddress,
            Egress::Interface(_) => RouteKind::Interface,
            Egress::Socks5 { .. } => RouteKind::Socks5,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RouteKind {
    Direct,
    BindAddress,
    Interface,
    Socks5,
}

/// A SOCKS5 proxy as the user named it: a host (a DNS name, or an IP
/// literal without brackets) and a port, and who resolves names (`None`:
/// [`default_dns`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProxySpec {
    pub host: String,
    pub port: u16,
    pub dns: Option<SocksDns>,
}

/// Whether `ip` is on the user's own network: a private or loopback
/// address (link-local too). A proxy there is the user's own
/// infrastructure.
pub fn on_lan(ip: IpAddr) -> bool {
    matches!(
        net::classify(ip),
        AddrClass::Private | AddrClass::Loopback | AddrClass::LinkLocal
    )
}

/// Whether a proxy can be at `ip`: not the unspecified address (which
/// connects to this machine), multicast or broadcast. An IPv4-mapped IPv6
/// address (`::ffff:0.0.0.0`) is judged as the IPv4 address it carries,
/// which is where a connection to it goes.
pub fn usable_proxy(ip: IpAddr) -> bool {
    let ip = match ip {
        IpAddr::V6(v6) => v6.to_ipv4_mapped().map_or(ip, IpAddr::V4),
        IpAddr::V4(_) => ip,
    };
    !ip.is_unspecified() && !ip.is_multicast() && ip != IpAddr::V4(std::net::Ipv4Addr::BROADCAST)
}

/// Who resolves names when the user didn't say (runner, #394 review,
/// replacing the #380 ruling): the proxy (`socks5h`, so no lookup leaves
/// outside the tunnel) for a plugin pinned to its manifest's hosts, where
/// a name can't steer a request anywhere else, whatever the proxy's
/// address. Lodestar (`socks5`, through its address checks) for a plugin
/// whose endpoints the user enters, where a public name that the proxy
/// resolves to its own network would otherwise reach that network.
pub fn default_dns(endpoints: &Endpoints) -> SocksDns {
    if endpoints.pinned {
        SocksDns::Proxy
    } else {
        SocksDns::Local
    }
}

/// The per-source credential store's interface for egress (SRC-4 builds
/// the store). It's asked once, when the plugin's client is built.
pub trait EgressCredentials: Send + Sync {
    /// The SOCKS5 user name and password stored for `source`, or `None`
    /// when the proxy needs none.
    fn socks5<'a>(
        &'a self,
        source: &'a str,
    ) -> BoxFuture<'a, Result<Option<SocksAuth>, CredentialError>>;
}

/// The store couldn't produce a credential. Carries nothing secret.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CredentialError;

impl fmt::Display for CredentialError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("the stored credentials couldn't be read")
    }
}

impl std::error::Error for CredentialError {}

/// No stored credentials: every SOCKS5 proxy is used without
/// authentication.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoCredentials;

impl EgressCredentials for NoCredentials {
    fn socks5<'a>(
        &'a self,
        _: &'a str,
    ) -> BoxFuture<'a, Result<Option<SocksAuth>, CredentialError>> {
        Box::pin(async { Ok(None) })
    }
}

/// Where a plugin may send requests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Endpoints {
    policy: NetPolicy,
    /// The hosts ([`host_key`]) the plugin may reach, each with the one
    /// scheme it may use (`None`: http or https); `None` for any host.
    hosts: Option<Vec<(Option<&'static str>, String)>>,
    /// The hosts are the manifest's own ([`Endpoints::manifest`]), not ones
    /// the user entered.
    pinned: bool,
}

impl Endpoints {
    /// A plugin's built-in public endpoints: public addresses only, and
    /// only the `http`/`https` hosts its manifest declares (exactly, no
    /// subdomains), each over the scheme it's declared with.
    pub fn manifest(endpoints: &[Endpoint]) -> Endpoints {
        let hosts = endpoints
            .iter()
            .filter_map(|e| {
                let scheme = ["http", "https"]
                    .into_iter()
                    .find(|s| e.protocol.eq_ignore_ascii_case(s))?;
                Some((Some(scheme), host_key(&format!("http://{}/", e.host))?))
            })
            .collect();
        Endpoints {
            policy: NetPolicy::notification(false),
            hosts: Some(hosts),
            pinned: true,
        }
    }

    /// A URL the user entered (an indexer, a download client): the SSRF
    /// guard's policy for that kind of endpoint, any host.
    pub fn user_entered(policy: NetPolicy) -> Endpoints {
        Endpoints {
            policy,
            hosts: None,
            pinned: false,
        }
    }

    /// Narrows these endpoints to the given hosts.
    pub fn only_hosts<S: AsRef<str>>(mut self, hosts: &[S]) -> Endpoints {
        self.hosts = Some(
            hosts
                .iter()
                .filter_map(|h| Some((None, host_key(&format!("http://{}/", h.as_ref()))?)))
                .collect(),
        );
        self
    }

    pub fn policy(&self) -> NetPolicy {
        self.policy
    }

    /// The hosts are the manifest's own, not ones the user entered.
    pub fn pinned(&self) -> bool {
        self.pinned
    }

    /// Checks a request's URL before anything is resolved or connected:
    /// the guard's DNS-free checks and the host list.
    fn check(&self, url: &str) -> Result<(), NetError> {
        let parsed = url::Url::parse(url).map_err(|_| NetError::InvalidUrl("it doesn't parse"))?;
        net::check_url(&parsed, self.policy)?;
        if let Some(hosts) = &self.hosts {
            let key = host_key(url).ok_or(NetError::InvalidUrl("it has no host"))?;
            let listed = hosts
                .iter()
                .any(|(scheme, host)| *host == key && scheme.is_none_or(|s| s == parsed.scheme()));
            if !listed {
                return Err(NetError::InvalidUrl(
                    "the host isn't one of the plugin's endpoints",
                ));
            }
        }
        Ok(())
    }
}

/// Why a plugin's egress is down. Names no address, URL or credential.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DownReason {
    /// This platform can't use the route (an interface by name off Linux).
    Unsupported,
    /// The interface name isn't a valid name.
    InvalidInterface,
    /// No interface has that name.
    InterfaceMissing,
    /// The interface exists but isn't up.
    InterfaceDown,
    /// The bind address isn't assigned to this machine.
    AddressUnavailable,
    /// The SOCKS5 proxy doesn't accept connections.
    ProxyUnreachable,
    /// The proxy's credentials couldn't be read.
    CredentialsUnavailable,
    /// The HTTP client couldn't be built.
    ClientUnavailable,
    /// A bind address off Windows, where it doesn't pin the interface.
    BindAddressUnsupported,
    /// The SOCKS5 proxy's name didn't resolve (to a usable address:
    /// [`usable_proxy`]) when the plugin started.
    ProxyUnresolved,
    /// The source's saved egress setting couldn't be read (a rollback, a
    /// newer route, a damaged row): it's refused rather than read as
    /// direct, which would leave the VPN.
    SettingUnreadable,
}

impl fmt::Display for DownReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            DownReason::Unsupported => net::INTERFACE_UNSUPPORTED,
            DownReason::InvalidInterface => "the interface name isn't valid",
            DownReason::InterfaceMissing => "the network interface doesn't exist",
            DownReason::InterfaceDown => "the network interface is down",
            DownReason::AddressUnavailable => "the bind address isn't assigned to this machine",
            DownReason::ProxyUnreachable => "the SOCKS5 proxy isn't reachable",
            DownReason::CredentialsUnavailable => "the proxy's credentials couldn't be read",
            DownReason::ClientUnavailable => "the HTTP client couldn't be built",
            DownReason::SettingUnreadable => {
                "the source's saved network setting couldn't be read; set its network route again"
            }
            DownReason::ProxyUnresolved => {
                "the SOCKS5 proxy's name didn't resolve to a usable address when the source started; restart Lodestar once it does"
            }
            DownReason::BindAddressUnsupported => {
                "a bind address keeps traffic on the VPN only on Windows; choose the interface by name"
            }
        })
    }
}

/// The kill switch's view of a plugin's route.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RouteState {
    /// Not checked yet.
    Unchecked,
    Up,
    /// Requests are refused.
    Down(DownReason),
}

/// Counters for one plugin's egress, monotonic since the client was built.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct EgressCounters {
    /// Requests that got an HTTP answer.
    pub answered: u64,
    /// Requests refused before sending because the route was down.
    pub kill_switch_refusals: u64,
    /// Requests abandoned in flight because the route went down.
    pub kill_switch_aborts: u64,
    /// Requests refused by the endpoint policy (SSRF guard or host list).
    pub policy_refusals: u64,
    /// Requests that failed in transport (connect, TLS, timeout, size).
    pub transport_failures: u64,
    /// Times the route went from up (or unchecked) to down.
    pub trips: u64,
}

/// A plugin's egress status, for the Sources API and `/metrics`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EgressStatus {
    pub plugin: String,
    pub route: RouteKind,
    pub state: RouteState,
    pub counters: EgressCounters,
    /// What the user should know about this route's privacy, if anything.
    pub notice: Option<&'static str>,
}

/// The notice for routes whose DNS lookups don't take the route.
pub const DNS_OUTSIDE_ROUTE: &str = "DNS lookups use the system resolver, outside the chosen route";

fn notice(egress: &Egress) -> Option<&'static str> {
    match egress {
        Egress::Direct
        | Egress::Socks5 {
            dns: SocksDns::Proxy,
            ..
        } => None,
        Egress::BindAddress(_)
        | Egress::Interface(_)
        | Egress::Socks5 {
            dns: SocksDns::Local,
            ..
        } => Some(DNS_OUTSIDE_ROUTE),
    }
}

/// The longest request timeout a plugin gets.
pub const MAX_TIMEOUT: Duration = Duration::from_secs(120);
/// The shortest.
pub const MIN_TIMEOUT: Duration = Duration::from_secs(1);
/// The default, connect included (connecting is capped at 10 s within it).
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);
/// How long a SOCKS5 probe waits for the proxy to accept.
const PROBE_TIMEOUT: Duration = Duration::from_secs(5);
/// How long a good SOCKS5 probe is trusted. Address and interface checks
/// are cheap and run before every request.
const SOCKS_PROBE_TTL: Duration = Duration::from_secs(10);
/// How many redirects [`send_following`] follows.
pub const MAX_REDIRECTS: usize = 5;
/// While the route is down, the first refusal before sending comes back
/// after this long, and each refusal in a row doubles it, up to
/// [`DOWN_DELAY_MAX`]. A refusal comes back sooner when the route comes
/// up. This paces a plugin's retry loop while the kill switch is down
/// (SRC-14b), which a host's back-off can't: the host isn't at fault.
pub const DOWN_DELAY_MIN: Duration = Duration::from_secs(1);
/// The longest a refusal waits before it comes back.
pub const DOWN_DELAY_MAX: Duration = Duration::from_secs(30);

/// How long the refusal that follows `streak` refusals in a row waits.
pub fn down_delay(streak: u32) -> Duration {
    DOWN_DELAY_MIN
        .saturating_mul(1u32 << streak.min(16))
        .min(DOWN_DELAY_MAX)
}

/// Builds every plugin's HTTP client.
#[derive(Clone)]
pub struct EgressFactory {
    resolver: Arc<dyn Resolve>,
    credentials: Arc<dyn EgressCredentials>,
    timeout: Duration,
}

impl fmt::Debug for EgressFactory {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EgressFactory")
            .field("timeout", &self.timeout)
            .finish_non_exhaustive()
    }
}

impl EgressFactory {
    pub fn new(credentials: Arc<dyn EgressCredentials>) -> EgressFactory {
        EgressFactory::with_resolver(credentials, Arc::new(SystemResolver))
    }

    /// A factory resolving names through `resolver` (tests script it).
    pub fn with_resolver(
        credentials: Arc<dyn EgressCredentials>,
        resolver: Arc<dyn Resolve>,
    ) -> EgressFactory {
        EgressFactory {
            resolver,
            credentials,
            timeout: DEFAULT_TIMEOUT,
        }
    }

    /// The per-request timeout, clamped to [`MIN_TIMEOUT`]..=[`MAX_TIMEOUT`].
    pub fn with_timeout(mut self, timeout: Duration) -> EgressFactory {
        self.timeout = timeout.clamp(MIN_TIMEOUT, MAX_TIMEOUT);
        self
    }

    /// The client for `plugin`. It never fails: a route that can't be used
    /// gives a client whose kill switch is down for good, so the reason is
    /// visible in its status and every request is refused.
    pub async fn build(&self, plugin: &str, egress: &Egress, endpoints: Endpoints) -> PluginEgress {
        let probe = match egress {
            Egress::Direct => Probe::Direct,
            Egress::BindAddress(ip) => Probe::Bind(*ip),
            Egress::Interface(name) => Probe::Interface(name.clone()),
            Egress::Socks5 { proxy, .. } => Probe::Socks(*proxy),
        };
        let client = self.client(plugin, egress, endpoints.policy).await;
        let mut built =
            PluginEgress::new(plugin, egress.kind(), client, Arc::new(probe), endpoints);
        built.notice = notice(egress);
        built
    }

    /// The client for `plugin` through the SOCKS5 proxy `spec` names. A
    /// name is resolved once, now, through the factory's resolver (the
    /// first answer); when it doesn't resolve, the client is down for good
    /// with [`DownReason::ProxyUnresolved`]. Also returns the route it
    /// built, for the plugin's status.
    pub async fn build_socks5(
        &self,
        plugin: &str,
        spec: &ProxySpec,
        endpoints: Endpoints,
    ) -> (PluginEgress, Option<Egress>) {
        let ip = match spec.host.parse::<IpAddr>() {
            Ok(ip) => Some(ip).filter(|ip| usable_proxy(*ip)),
            Err(_) => tokio::time::timeout(PROBE_TIMEOUT, self.resolver.resolve(&spec.host))
                .await
                .ok()
                .and_then(Result::ok)
                .and_then(|ips| ips.into_iter().find(|ip| usable_proxy(*ip))),
        };
        let Some(ip) = ip else {
            let down = self.unavailable(
                plugin,
                RouteKind::Socks5,
                DownReason::ProxyUnresolved,
                endpoints,
            );
            return (down, None);
        };
        let egress = Egress::Socks5 {
            proxy: SocketAddr::new(ip, spec.port),
            dns: spec.dns.unwrap_or_else(|| default_dns(&endpoints)),
        };
        (self.build(plugin, &egress, endpoints).await, Some(egress))
    }

    /// A client for `plugin` whose route couldn't even be described (a
    /// proxy name that didn't resolve): its kill switch is down for good
    /// with `reason`, so every request is refused.
    pub fn unavailable(
        &self,
        plugin: &str,
        route: RouteKind,
        reason: DownReason,
        endpoints: Endpoints,
    ) -> PluginEgress {
        PluginEgress::new(
            plugin,
            route,
            Err(reason),
            Arc::new(Probe::Direct),
            endpoints,
        )
    }

    async fn client(
        &self,
        plugin: &str,
        egress: &Egress,
        policy: NetPolicy,
    ) -> Result<GuardedClient, DownReason> {
        let route = match egress {
            Egress::Direct => Route::Direct,
            // Off Windows (weak host model: Linux, macOS, BSD) the routing
            // table, not the bound address, picks the interface, so a downed
            // VPN interface that keeps its address would leak.
            Egress::BindAddress(_) if cfg!(not(windows)) => {
                return Err(DownReason::BindAddressUnsupported);
            }
            Egress::BindAddress(ip) => Route::BindAddress(*ip),
            Egress::Interface(name) => Route::Interface(name.clone()),
            Egress::Socks5 { proxy, dns } => {
                let auth = self
                    .credentials
                    .socks5(plugin)
                    .await
                    .map_err(|_| DownReason::CredentialsUnavailable)?;
                Route::Socks5(Socks5Route {
                    proxy: *proxy,
                    dns: *dns,
                    auth,
                })
            }
        };
        GuardedClient::with_route(policy, self.timeout, Arc::clone(&self.resolver), &route).map_err(
            |e| match e {
                net::RouteError::Unsupported(_) => DownReason::Unsupported,
                net::RouteError::InvalidInterface => DownReason::InvalidInterface,
                net::RouteError::InvalidCredentials => DownReason::CredentialsUnavailable,
                net::RouteError::Build => DownReason::ClientUnavailable,
            },
        )
    }
}

/// How the kill switch checks a route.
trait RouteProbe: Send + Sync {
    fn probe(&self) -> BoxFuture<'_, Result<(), DownReason>>;
    /// How long a good answer is trusted before the next request probes
    /// again. `None`: never re-probed (direct).
    fn ttl(&self) -> Option<Duration>;
}

enum Probe {
    Direct,
    Bind(IpAddr),
    Interface(String),
    Socks(SocketAddr),
}

impl RouteProbe for Probe {
    fn probe(&self) -> BoxFuture<'_, Result<(), DownReason>> {
        Box::pin(async move {
            match self {
                Probe::Direct => Ok(()),
                // Binding succeeds only for an address assigned here.
                Probe::Bind(ip) => std::net::UdpSocket::bind(SocketAddr::new(*ip, 0))
                    .map(drop)
                    .map_err(|_| DownReason::AddressUnavailable),
                Probe::Interface(name) => interface_up(name),
                Probe::Socks(proxy) => {
                    match tokio::time::timeout(PROBE_TIMEOUT, tokio::net::TcpStream::connect(proxy))
                        .await
                    {
                        Ok(Ok(_)) => Ok(()),
                        _ => Err(DownReason::ProxyUnreachable),
                    }
                }
            }
        })
    }

    fn ttl(&self) -> Option<Duration> {
        match self {
            Probe::Direct => None,
            Probe::Socks(_) => Some(SOCKS_PROBE_TTL),
            Probe::Bind(_) | Probe::Interface(_) => Some(Duration::ZERO),
        }
    }
}

/// Whether the interface exists and its link is usable: administratively
/// up (`IFF_UP` in `/sys/class/net/<name>/flags`), an operational state
/// that isn't down (`operstate`; tunnels report `unknown`), and a carrier
/// where the kernel reports one. A WireGuard tunnel with no live peer still
/// reads up: the kernel can't tell. The name is checked first, so it's one
/// path segment.
#[cfg(target_os = "linux")]
fn interface_up(name: &str) -> Result<(), DownReason> {
    net::check_interface_name(name).map_err(|_| DownReason::InvalidInterface)?;
    let flags = std::fs::read_to_string(format!("/sys/class/net/{name}/flags"))
        .map_err(|_| DownReason::InterfaceMissing)?;
    let flags = u32::from_str_radix(flags.trim().trim_start_matches("0x"), 16)
        .map_err(|_| DownReason::InterfaceMissing)?;
    const IFF_UP: u32 = 0x1;
    if flags & IFF_UP == 0 {
        return Err(DownReason::InterfaceDown);
    }
    let read = |file: &str| std::fs::read_to_string(format!("/sys/class/net/{name}/{file}"));
    let oper = read("operstate").map_err(|_| DownReason::InterfaceMissing)?;
    if matches!(
        oper.trim(),
        "down" | "lowerlayerdown" | "notpresent" | "dormant"
    ) {
        return Err(DownReason::InterfaceDown);
    }
    // `carrier` can't be read while the interface is down; it's 0 when the
    // link has no carrier.
    if read("carrier").is_ok_and(|c| c.trim() == "0") {
        return Err(DownReason::InterfaceDown);
    }
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn interface_up(_: &str) -> Result<(), DownReason> {
    Err(DownReason::Unsupported)
}

#[derive(Debug, Default)]
struct Counters {
    answered: AtomicU64,
    kill_switch_refusals: AtomicU64,
    kill_switch_aborts: AtomicU64,
    policy_refusals: AtomicU64,
    transport_failures: AtomicU64,
    trips: AtomicU64,
}

impl Counters {
    fn bump(c: &AtomicU64) {
        c.fetch_add(1, Ordering::Relaxed);
    }

    fn snapshot(&self) -> EgressCounters {
        let l = |c: &AtomicU64| c.load(Ordering::Relaxed);
        EgressCounters {
            answered: l(&self.answered),
            kill_switch_refusals: l(&self.kill_switch_refusals),
            kill_switch_aborts: l(&self.kill_switch_aborts),
            policy_refusals: l(&self.policy_refusals),
            transport_failures: l(&self.transport_failures),
            trips: l(&self.trips),
        }
    }
}

/// One plugin's HTTP client: its route, kill switch and endpoint policy.
/// It's the plugin's [`Fetch`]; put it in the plugin's
/// [`Kit`](crate::Kit).
pub struct PluginEgress {
    plugin: String,
    route: RouteKind,
    client: Result<GuardedClient, DownReason>,
    probe: Arc<dyn RouteProbe>,
    endpoints: Endpoints,
    state: watch::Sender<RouteState>,
    /// When the route last probed good.
    probed_up: Mutex<Option<Instant>>,
    probing: tokio::sync::Mutex<()>,
    notice: Option<&'static str>,
    /// Shared with the bodies of streamed answers still being read.
    counters: Arc<Counters>,
    /// Refusals before sending since the route last let a request
    /// through, for [`down_delay`].
    down_streak: AtomicU32,
}

impl fmt::Debug for PluginEgress {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PluginEgress")
            .field("plugin", &self.plugin)
            .field("route", &self.route)
            .field("state", &*self.state.borrow())
            .finish_non_exhaustive()
    }
}

impl PluginEgress {
    fn new(
        plugin: &str,
        route: RouteKind,
        client: Result<GuardedClient, DownReason>,
        probe: Arc<dyn RouteProbe>,
        endpoints: Endpoints,
    ) -> PluginEgress {
        let egress = PluginEgress {
            plugin: plugin.to_owned(),
            route,
            client,
            probe,
            endpoints,
            state: watch::Sender::new(RouteState::Unchecked),
            probed_up: Mutex::new(None),
            probing: tokio::sync::Mutex::new(()),
            notice: None,
            counters: Arc::default(),
            down_streak: Default::default(),
        };
        match egress.client {
            Err(reason) => egress.publish(RouteState::Down(reason)),
            // A direct route has nothing to check.
            Ok(_) if egress.probe.ttl().is_none() => egress.publish(RouteState::Up),
            Ok(_) => {}
        }
        egress
    }

    pub fn status(&self) -> EgressStatus {
        EgressStatus {
            plugin: self.plugin.clone(),
            route: self.route,
            state: *self.state.borrow(),
            counters: self.counters.snapshot(),
            notice: self.notice,
        }
    }

    /// Live kill-switch state.
    pub fn watch(&self) -> watch::Receiver<RouteState> {
        self.state.subscribe()
    }

    fn publish(&self, next: RouteState) {
        self.state.send_if_modified(|cur| {
            if *cur == next {
                return false;
            }
            if matches!(next, RouteState::Down(_)) && !matches!(*cur, RouteState::Down(_)) {
                Counters::bump(&self.counters.trips);
            }
            *cur = next;
            true
        });
    }

    /// Probes the route now and publishes the result.
    pub async fn check_now(&self) -> RouteState {
        // One probe at a time, so a slow good answer can't overwrite a
        // newer bad one.
        let _probing = self.probing.lock().await;
        let next = match self.client {
            Err(reason) => RouteState::Down(reason),
            Ok(_) => match self.probe.probe().await {
                Ok(()) => RouteState::Up,
                Err(reason) => RouteState::Down(reason),
            },
        };
        *self.probed_up.lock().unwrap_or_else(|e| e.into_inner()) =
            (next == RouteState::Up).then(Instant::now);
        self.publish(next);
        next
    }

    /// Probes the route every `every` until dropped, so a route that goes
    /// down abandons the requests in flight. The host spawns one per
    /// plugin with a non-direct route.
    pub async fn monitor(&self, every: Duration) {
        let mut tick = tokio::time::interval(every.max(Duration::from_millis(10)));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tick.tick().await;
            self.check_now().await;
        }
    }

    /// The client, when the route is up: a fresh probe unless the last
    /// good one is recent enough.
    async fn gate(&self) -> Result<&GuardedClient, DownReason> {
        let client = self.client.as_ref().map_err(|r| *r)?;
        let Some(ttl) = self.probe.ttl() else {
            return Ok(client);
        };
        let fresh = self
            .probed_up
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_some_and(|at| at.elapsed() < ttl);
        let state = if fresh && *self.state.borrow() == RouteState::Up {
            RouteState::Up
        } else {
            self.check_now().await
        };
        match state {
            RouteState::Up => Ok(client),
            RouteState::Down(reason) => Err(reason),
            RouteState::Unchecked => Err(DownReason::ClientUnavailable),
        }
    }

    /// The endpoint policy and the kill switch, before anything is sent:
    /// the client to send with, or the refusal (paced while the route is
    /// down).
    async fn admit(&self, req: &Request) -> Result<&GuardedClient, HttpError> {
        if let Err(e) = self.endpoints.check(&req.url) {
            Counters::bump(&self.counters.policy_refusals);
            return Err(refused(&e));
        }
        match self.gate().await {
            Ok(c) => {
                self.down_streak.store(0, Ordering::Relaxed);
                Ok(c)
            }
            Err(reason) => {
                Counters::bump(&self.counters.kill_switch_refusals);
                // Paced, so a plugin that retries at once can't spin
                // while the route is down; cut short when it comes up.
                // Shutdown relies on the plugin dropping this future (its
                // `select!` on the shutdown signal): one that doesn't waits
                // up to `DOWN_DELAY_MAX` (30 s) here and is aborted at the
                // plugin stop timeout (15 s) instead.
                // Saturating, and a lost update between concurrent
                // refusals only shortens one wait.
                let streak = self.down_streak.load(Ordering::Relaxed);
                self.down_streak
                    .store(streak.saturating_add(1), Ordering::Relaxed);
                let mut up = self.state.subscribe();
                tokio::select! {
                    () = tokio::time::sleep(down_delay(streak)) => {}
                    _ = up.wait_for(|s| *s == RouteState::Up) => {}
                }
                Err(HttpError::EgressDown(reason.to_string()))
            }
        }
    }

    async fn send(&self, req: &Request) -> Result<Response, HttpError> {
        let client = self.admit(req).await?;
        let mut down = self.state.subscribe();
        let exchange = client.exchange(
            req.method.as_str(),
            &req.url,
            &req.headers,
            req.body.clone(),
            MAX_BODY,
        );
        let result = tokio::select! {
            r = exchange => r,
            Ok(state) = down.wait_for(|s| matches!(s, RouteState::Down(_))) => {
                let reason = match *state {
                    RouteState::Down(r) => r,
                    _ => DownReason::ClientUnavailable,
                };
                Counters::bump(&self.counters.kill_switch_aborts);
                return Err(HttpError::EgressDown(format!("went down in flight: {reason}")));
            }
        };
        match result {
            Ok(Exchange {
                status,
                headers,
                body,
            }) => {
                Counters::bump(&self.counters.answered);
                Ok(Response {
                    status,
                    headers,
                    body,
                })
            }
            Err(e) => Err(self.failed(e).await),
        }
    }

    /// What a failed exchange means. A refusal by the policy is the
    /// policy's; a failed connection may mean the route just went down,
    /// so it re-probes: the status then says so, and a route outage is
    /// the kill switch's error, not the host's (which would back it off).
    async fn failed(&self, e: NetError) -> HttpError {
        match e {
            NetError::Blocked(_) | NetError::InvalidUrl(_) => {
                Counters::bump(&self.counters.policy_refusals);
                refused(&e)
            }
            e => {
                if matches!(e, NetError::Connect | NetError::Timeout)
                    && self.probe.ttl().is_some()
                    && let RouteState::Down(reason) = self.check_now().await
                {
                    Counters::bump(&self.counters.kill_switch_aborts);
                    return HttpError::EgressDown(format!("went down in flight: {reason}"));
                }
                Counters::bump(&self.counters.transport_failures);
                HttpError::Transport(e.to_string())
            }
        }
    }

    /// As [`send`](Self::send), with the body streamed: the same endpoint
    /// policy, kill switch and in-flight abort, which goes on covering the
    /// body after the headers ([`EgressBody`]). The transport is given the
    /// plugin's timeout and byte cap in place of [`MAX_BODY`] and the
    /// factory's timeout; `net` holds both to its own ceiling as well.
    async fn send_stream(
        &self,
        req: &Request,
        limits: StreamLimits,
    ) -> Result<StreamResponse, HttpError> {
        let limits = limits.clamped();
        let client = self.admit(req).await?;
        let mut down = self.state.subscribe();
        let exchange = client.exchange_stream(
            req.method.as_str(),
            &req.url,
            &req.headers,
            req.body.clone(),
            limits.max_bytes,
            limits.timeout,
        );
        let result = tokio::select! {
            r = exchange => r,
            Ok(state) = down.wait_for(|s| matches!(s, RouteState::Down(_))) => {
                let reason = match *state {
                    RouteState::Down(r) => r,
                    _ => DownReason::ClientUnavailable,
                };
                Counters::bump(&self.counters.kill_switch_aborts);
                return Err(HttpError::EgressDown(format!("went down in flight: {reason}")));
            }
        };
        match result {
            Ok(net::StreamExchange {
                status,
                headers,
                body,
            }) => {
                Counters::bump(&self.counters.answered);
                Ok(StreamResponse {
                    status,
                    headers,
                    body: Box::new(EgressBody {
                        body,
                        down,
                        probe: Arc::clone(&self.probe),
                        counters: Arc::clone(&self.counters),
                    }),
                })
            }
            Err(e) => Err(self.failed(e).await),
        }
    }
}

/// A streamed answer's body, still under its plugin's kill switch: a
/// route that goes down while the body is being read abandons it with
/// [`HttpError::EgressDown`], as it would a buffered body. A connection
/// that fails part way on a checked route is re-probed, so a route outage
/// reads as the kill switch's and not the host's. (The probe here doesn't
/// publish a new state: the monitor, or the plugin's next request, does.)
struct EgressBody {
    body: net::StreamBody,
    down: watch::Receiver<RouteState>,
    probe: Arc<dyn RouteProbe>,
    counters: Arc<Counters>,
}

impl EgressBody {
    async fn next(&mut self) -> Result<Option<Vec<u8>>, HttpError> {
        let read = tokio::select! {
            r = self.body.chunk() => r,
            Ok(state) = self.down.wait_for(|s| matches!(s, RouteState::Down(_))) => {
                let reason = match *state {
                    RouteState::Down(r) => r,
                    _ => DownReason::ClientUnavailable,
                };
                Counters::bump(&self.counters.kill_switch_aborts);
                return Err(HttpError::EgressDown(format!("went down in flight: {reason}")));
            }
        };
        match read {
            Ok(chunk) => Ok(chunk),
            Err(e) => {
                if matches!(e, NetError::Connect | NetError::Timeout | NetError::Http)
                    && self.probe.ttl().is_some()
                    && let Err(reason) = self.probe.probe().await
                {
                    Counters::bump(&self.counters.kill_switch_aborts);
                    return Err(HttpError::EgressDown(format!(
                        "went down in flight: {reason}"
                    )));
                }
                Counters::bump(&self.counters.transport_failures);
                Err(HttpError::Transport(e.to_string()))
            }
        }
    }
}

impl BodyStream for EgressBody {
    fn next_chunk(&mut self) -> BoxFuture<'_, Result<Option<Vec<u8>>, HttpError>> {
        Box::pin(self.next())
    }
}

/// A refusal by the endpoint policy. The text names the address class or
/// the rule, never the URL.
fn refused(e: &NetError) -> HttpError {
    HttpError::Transport(format!("refused by the endpoint policy: {e}"))
}

impl Fetch for PluginEgress {
    fn fetch<'a>(&'a self, req: &'a Request) -> BoxFuture<'a, Result<Response, HttpError>> {
        Box::pin(self.send(req))
    }

    fn fetch_stream<'a>(
        &'a self,
        req: &'a Request,
        limits: StreamLimits,
    ) -> BoxFuture<'a, Result<StreamResponse, HttpError>> {
        Box::pin(self.send_stream(req, limits))
    }
}

/// Sends `req` through the plugin's [`Client`], following at most
/// [`MAX_REDIRECTS`] redirects. Each hop is a new request: paced for its
/// host and checked by the plugin's egress and endpoint policy like the
/// first. 307 and 308 keep the method and body; 301, 302 and 303 become a
/// `GET` without a body (`HEAD` stays `HEAD`). A redirect from `https` to
/// `http` is refused, and `Authorization`, `Cookie`, `Proxy-Authorization`
/// and `X-Api-Key` are dropped when a hop changes origin.
pub async fn send_following<F: Fetch>(
    client: &Client<F>,
    mut req: Request,
) -> Result<Response, HttpError> {
    for _ in 0..=MAX_REDIRECTS {
        let resp = client.send(&req).await?;
        if !matches!(resp.status, 301 | 302 | 303 | 307 | 308) {
            return Ok(resp);
        }
        let Some(location) = resp.header("location") else {
            return Ok(resp);
        };
        let from = url::Url::parse(&req.url).map_err(|_| HttpError::BadUrl)?;
        let to = from
            .join(location)
            .map_err(|_| HttpError::Transport("a redirect had an unusable location".into()))?;
        if !matches!(to.scheme(), "http" | "https") {
            return Err(HttpError::Transport("a redirect left http(s)".into()));
        }
        if from.scheme() == "https" && to.scheme() != "https" {
            return Err(HttpError::Transport(
                "a redirect from https to http was refused".into(),
            ));
        }
        if to.origin() != from.origin() {
            req.headers.retain(|(name, _)| {
                ![
                    "authorization",
                    "cookie",
                    "proxy-authorization",
                    "x-api-key",
                ]
                .iter()
                .any(|h| name.eq_ignore_ascii_case(h))
            });
        }
        if matches!(resp.status, 301..=303) && req.method != Method::Head {
            req.method = Method::Get;
            req.body.clear();
            req.headers.retain(|(name, _)| {
                !name.eq_ignore_ascii_case("content-type")
                    && !name.eq_ignore_ascii_case("content-length")
            });
        }
        req.url = to.into();
    }
    Err(HttpError::Transport("too many redirects".into()))
}

#[cfg(test)]
#[path = "egress_tests.rs"]
mod tests;
