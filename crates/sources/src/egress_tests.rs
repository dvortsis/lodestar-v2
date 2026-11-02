//! Egress tests. Local fixtures only: an HTTP server and a SOCKS5 stub on
//! loopback, and a scripted resolver, so nothing reaches the internet and
//! no test depends on real DNS.

use super::*;
use std::collections::HashMap;
use std::io;
use std::sync::atomic::{AtomicBool, AtomicUsize};

use lodestar_integrations::Secret;
use lodestar_integrations::net::AddrClass;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::http::HostLimiter;
use crate::manifest::{Backoff, HostRate, Limits};

/// Every wait in these tests is a condition with this deadline.
const DEADLINE: Duration = Duration::from_secs(10);

async fn within<F: std::future::Future>(f: F) -> F::Output {
    tokio::time::timeout(DEADLINE, f)
        .await
        .expect("condition not met within the deadline")
}

// ---------------------------------------------------------------- fixtures

enum Reply {
    Bytes(Vec<u8>),
    /// Hold the connection open without answering.
    Stall,
}

fn ok(body: &str) -> Reply {
    Reply::Bytes(
        format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
        .into_bytes(),
    )
}

fn redirect(status: u16, location: &str) -> Reply {
    Reply::Bytes(
        format!(
            "HTTP/1.1 {status} Moved\r\nLocation: {location}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        )
        .into_bytes(),
    )
}

/// A one-request-per-connection HTTP/1.1 server on loopback.
struct Server {
    addr: SocketAddr,
    /// Connections accepted.
    conns: Arc<AtomicUsize>,
    /// The client address of each connection.
    peers: Arc<Mutex<Vec<SocketAddr>>>,
    /// The request line of each request, as it arrives.
    requests: mpsc::UnboundedReceiver<String>,
    _task: JoinHandle<()>,
}

impl Server {
    fn conns(&self) -> usize {
        self.conns.load(Ordering::SeqCst)
    }

    fn url(&self, path: &str) -> String {
        format!("http://{}{path}", self.addr)
    }
}

async fn serve(handler: impl Fn(&str) -> Reply + Send + Sync + 'static) -> Server {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let conns = Arc::new(AtomicUsize::new(0));
    let peers = Arc::new(Mutex::new(Vec::new()));
    let (tx, requests) = mpsc::unbounded_channel();
    let handler = Arc::new(handler);
    let counted = Arc::clone(&conns);
    let seen = Arc::clone(&peers);
    let task = tokio::spawn(async move {
        loop {
            let Ok((mut sock, peer)) = listener.accept().await else {
                return;
            };
            seen.lock().unwrap().push(peer);
            counted.fetch_add(1, Ordering::SeqCst);
            let tx = tx.clone();
            let handler = Arc::clone(&handler);
            tokio::spawn(async move {
                let head = read_head(&mut sock).await;
                let line = head.lines().next().unwrap_or_default().to_owned();
                let _ = tx.send(line.clone());
                match handler(&line) {
                    Reply::Bytes(b) => {
                        let _ = sock.write_all(&b).await;
                        let _ = sock.shutdown().await;
                    }
                    Reply::Stall => {
                        let mut sink = [0u8; 64];
                        while matches!(sock.read(&mut sink).await, Ok(n) if n > 0) {}
                    }
                }
            });
        }
    });
    Server {
        addr,
        conns,
        peers,
        requests,
        _task: task,
    }
}

async fn read_head(sock: &mut TcpStream) -> String {
    let mut buf = Vec::new();
    let mut byte = [0u8; 1];
    while !buf.ends_with(b"\r\n\r\n") && buf.len() < 16 * 1024 {
        match sock.read(&mut byte).await {
            Ok(1) => buf.push(byte[0]),
            _ => break,
        }
    }
    String::from_utf8_lossy(&buf).into_owned()
}

/// What a client asked the SOCKS5 stub to connect to.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Target {
    Domain(String, u16),
    Addr(SocketAddr),
}

/// A minimal SOCKS5 server (RFC 1928, CONNECT only, with RFC 1929 auth
/// when `require` is set). Domain targets resolve through `names`, the
/// stand-in for the proxy's own DNS.
struct Socks {
    addr: SocketAddr,
    targets: Arc<Mutex<Vec<Target>>>,
    creds: Arc<Mutex<Vec<(String, String)>>>,
    tunnels: Arc<Mutex<Vec<JoinHandle<()>>>>,
    accept: JoinHandle<()>,
}

impl Socks {
    fn targets(&self) -> Vec<Target> {
        self.targets.lock().unwrap().clone()
    }

    /// Stops the proxy: no new connections, and open tunnels are cut.
    async fn stop(&self) {
        self.accept.abort();
        for t in self.tunnels.lock().unwrap().drain(..) {
            t.abort();
        }
    }
}

async fn socks_stub(
    require: Option<(&'static str, &'static str)>,
    names: HashMap<String, SocketAddr>,
) -> Socks {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let targets = Arc::new(Mutex::new(Vec::new()));
    let creds = Arc::new(Mutex::new(Vec::new()));
    let tunnels: Arc<Mutex<Vec<JoinHandle<()>>>> = Arc::new(Mutex::new(Vec::new()));
    let names = Arc::new(names);
    let (t, c, tun) = (
        Arc::clone(&targets),
        Arc::clone(&creds),
        Arc::clone(&tunnels),
    );
    let accept = tokio::spawn(async move {
        loop {
            let Ok((sock, _)) = listener.accept().await else {
                return;
            };
            let (t, c, names) = (Arc::clone(&t), Arc::clone(&c), Arc::clone(&names));
            let h = tokio::spawn(async move {
                let _ = socks_session(sock, require, &names, &t, &c).await;
            });
            tun.lock().unwrap().push(h);
        }
    });
    Socks {
        addr,
        targets,
        creds,
        tunnels,
        accept,
    }
}

async fn socks_session(
    mut sock: TcpStream,
    require: Option<(&str, &str)>,
    names: &HashMap<String, SocketAddr>,
    targets: &Mutex<Vec<Target>>,
    creds: &Mutex<Vec<(String, String)>>,
) -> io::Result<()> {
    let mut hdr = [0u8; 2];
    sock.read_exact(&mut hdr).await?; // a probe closes here
    let mut methods = vec![0u8; usize::from(hdr[1])];
    sock.read_exact(&mut methods).await?;
    match require {
        Some((user, pass)) => {
            if !methods.contains(&2) {
                sock.write_all(&[5, 0xff]).await?;
                return Ok(());
            }
            sock.write_all(&[5, 2]).await?;
            let mut ver = [0u8; 2];
            sock.read_exact(&mut ver).await?;
            let mut u = vec![0u8; usize::from(ver[1])];
            sock.read_exact(&mut u).await?;
            let mut plen = [0u8; 1];
            sock.read_exact(&mut plen).await?;
            let mut p = vec![0u8; usize::from(plen[0])];
            sock.read_exact(&mut p).await?;
            let (u, p) = (
                String::from_utf8_lossy(&u).into_owned(),
                String::from_utf8_lossy(&p).into_owned(),
            );
            let good = u == user && p == pass;
            creds.lock().unwrap().push((u, p));
            sock.write_all(&[1, if good { 0 } else { 1 }]).await?;
            if !good {
                return Ok(());
            }
        }
        None => sock.write_all(&[5, 0]).await?,
    }
    let mut req = [0u8; 4];
    sock.read_exact(&mut req).await?;
    let target = match req[3] {
        1 => {
            let mut a = [0u8; 6];
            sock.read_exact(&mut a).await?;
            let ip = IpAddr::from([a[0], a[1], a[2], a[3]]);
            Target::Addr(SocketAddr::new(ip, u16::from_be_bytes([a[4], a[5]])))
        }
        3 => {
            let mut len = [0u8; 1];
            sock.read_exact(&mut len).await?;
            let mut d = vec![0u8; usize::from(len[0]) + 2];
            sock.read_exact(&mut d).await?;
            let port = u16::from_be_bytes([d[d.len() - 2], d[d.len() - 1]]);
            d.truncate(d.len() - 2);
            Target::Domain(String::from_utf8_lossy(&d).into_owned(), port)
        }
        _ => {
            let mut a = [0u8; 18];
            sock.read_exact(&mut a).await?;
            let mut o = [0u8; 16];
            o.copy_from_slice(&a[..16]);
            let ip = IpAddr::from(o);
            Target::Addr(SocketAddr::new(ip, u16::from_be_bytes([a[16], a[17]])))
        }
    };
    targets.lock().unwrap().push(target.clone());
    let dest = match &target {
        Target::Addr(a) => Some(*a),
        Target::Domain(d, port) => names.get(d).map(|a| SocketAddr::new(a.ip(), *port)),
    };
    let Some(dest) = dest else {
        sock.write_all(&[5, 4, 0, 1, 0, 0, 0, 0, 0, 0]).await?;
        return Ok(());
    };
    let Ok(mut upstream) = TcpStream::connect(dest).await else {
        sock.write_all(&[5, 5, 0, 1, 0, 0, 0, 0, 0, 0]).await?;
        return Ok(());
    };
    sock.write_all(&[5, 0, 0, 1, 0, 0, 0, 0, 0, 0]).await?;
    tokio::io::copy_bidirectional(&mut sock, &mut upstream).await?;
    Ok(())
}

/// Answers each name from a script, one entry per lookup (the last entry
/// repeats), and counts lookups.
struct Scripted {
    script: Mutex<HashMap<String, Vec<Vec<IpAddr>>>>,
    lookups: AtomicUsize,
}

impl Scripted {
    fn new(script: &[(&str, &[&[&str]])]) -> Arc<Scripted> {
        let script = script
            .iter()
            .map(|(name, answers)| {
                let answers = answers
                    .iter()
                    .map(|a| a.iter().map(|ip| ip.parse().unwrap()).collect())
                    .collect();
                ((*name).to_owned(), answers)
            })
            .collect();
        Arc::new(Scripted {
            script: Mutex::new(script),
            lookups: AtomicUsize::new(0),
        })
    }

    fn lookups(&self) -> usize {
        self.lookups.load(Ordering::SeqCst)
    }
}

impl Resolve for Scripted {
    fn resolve<'a>(&'a self, host: &'a str) -> BoxFuture<'a, io::Result<Vec<IpAddr>>> {
        self.lookups.fetch_add(1, Ordering::SeqCst);
        let mut script = self.script.lock().unwrap();
        let answer = match script.get_mut(host) {
            Some(answers) if answers.len() > 1 => Some(answers.remove(0)),
            Some(answers) => answers.first().cloned(),
            None => None,
        };
        Box::pin(async move { answer.ok_or_else(|| io::Error::other("no such name")) })
    }
}

struct Creds(Option<(&'static str, &'static str)>);

impl EgressCredentials for Creds {
    fn socks5<'a>(
        &'a self,
        _: &'a str,
    ) -> BoxFuture<'a, Result<Option<SocksAuth>, CredentialError>> {
        let auth = self.0.map(|(u, p)| SocksAuth {
            username: Secret::new(u),
            password: Secret::new(p),
        });
        Box::pin(async move { Ok(auth) })
    }
}

struct BrokenStore;

impl EgressCredentials for BrokenStore {
    fn socks5<'a>(
        &'a self,
        _: &'a str,
    ) -> BoxFuture<'a, Result<Option<SocksAuth>, CredentialError>> {
        Box::pin(async { Err(CredentialError) })
    }
}

fn factory(resolver: Arc<Scripted>) -> EgressFactory {
    EgressFactory::with_resolver(Arc::new(NoCredentials), resolver).with_timeout(DEADLINE)
}

/// LAN-style endpoints: loopback and private allowed, so the loopback
/// fixtures are reachable; link-local, metadata and reserved never are.
fn lan() -> Endpoints {
    Endpoints::user_entered(NetPolicy::download_client())
}

fn public_only() -> Endpoints {
    Endpoints::user_entered(NetPolicy::notification(false))
}

fn transport(r: Result<Response, HttpError>) -> String {
    match r {
        Err(HttpError::Transport(e)) => e,
        other => panic!("expected a transport error, got {other:?}"),
    }
}

fn unused_port_addr() -> SocketAddr {
    // Bind and drop: nothing listens there afterwards.
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    l.local_addr().unwrap()
}

// ---------------------------------------------------------------- direct

#[tokio::test]
async fn direct_route_reaches_what_the_policy_allows() {
    let server = serve(|_| ok("hello")).await;
    let egress = factory(Scripted::new(&[]))
        .build("p", &Egress::Direct, lan())
        .await;
    let resp = egress.fetch(&Request::get(server.url("/"))).await.unwrap();
    assert_eq!((resp.status, resp.body.as_slice()), (200, &b"hello"[..]));
    let status = egress.status();
    assert_eq!(status.route, RouteKind::Direct);
    assert_eq!(status.state, RouteState::Up);
    assert_eq!(status.notice, None);
    assert_eq!(status.counters.answered, 1);

    // The same fixture under a public-only policy is never connected to.
    let egress = factory(Scripted::new(&[]))
        .build("p", &Egress::Direct, public_only())
        .await;
    let err = transport(egress.fetch(&Request::get(server.url("/"))).await);
    assert!(err.contains("loopback"), "{err}");
    assert_eq!(server.conns(), 1);
    assert_eq!(egress.status().counters.policy_refusals, 1);
}

// ---------------------------------------------------------------- bind address

#[cfg(windows)]
#[tokio::test]
async fn a_bind_address_route_binds_and_fails_closed_when_the_address_is_gone() {
    let server = serve(|_| ok("bound")).await;
    let lo: IpAddr = "127.0.0.1".parse().unwrap();
    let egress = factory(Scripted::new(&[]))
        .build("p", &Egress::BindAddress(lo), lan())
        .await;
    let resp = egress.fetch(&Request::get(server.url("/"))).await.unwrap();
    assert_eq!(resp.body, b"bound");
    assert_eq!(egress.status().state, RouteState::Up);

    // 192.0.2.1 (TEST-NET-1) is never assigned here: the stand-in for a
    // VPN address that went away.
    let gone: IpAddr = "192.0.2.1".parse().unwrap();
    let egress = factory(Scripted::new(&[]))
        .build("p", &Egress::BindAddress(gone), lan())
        .await;
    let err = transport(egress.fetch(&Request::get(server.url("/"))).await);
    assert!(err.contains("egress down"), "{err}");
    let status = egress.status();
    assert_eq!(
        status.state,
        RouteState::Down(DownReason::AddressUnavailable)
    );
    assert_eq!(status.counters.kill_switch_refusals, 1);
    assert_eq!(status.counters.trips, 1);
    assert_eq!(server.conns(), 1, "nothing went out another way");
}

#[cfg(not(windows))]
#[tokio::test]
async fn a_bind_address_egress_is_refused_off_windows() {
    let server = serve(|_| ok("no")).await;
    let lo: IpAddr = "127.0.0.1".parse().unwrap();
    let egress = factory(Scripted::new(&[]))
        .build("p", &Egress::BindAddress(lo), lan())
        .await;
    let err = transport(egress.fetch(&Request::get(server.url("/"))).await);
    assert!(err.contains("only on Windows"), "{err}");
    assert_eq!(
        egress.status().state,
        RouteState::Down(DownReason::BindAddressUnsupported)
    );
    assert_eq!(server.conns(), 0);
}

/// A client on `route` whose kill switch always says up, so a test sees
/// what the socket itself does when the route is unusable.
fn probe_says_up(route: &Route, resolver: Arc<Scripted>) -> PluginEgress {
    let client =
        GuardedClient::with_route(NetPolicy::download_client(), DEADLINE, resolver, route).unwrap();
    PluginEgress::new(
        "p",
        RouteKind::Interface,
        Ok(client),
        Arc::new(Switch(AtomicBool::new(true))),
        lan(),
    )
}

#[tokio::test]
async fn the_bound_socket_fails_closed_even_when_the_probe_says_up() {
    let server = serve(|_| ok("leak")).await;
    // 192.0.2.1 (TEST-NET-1) is never assigned here.
    let route = Route::BindAddress("192.0.2.1".parse().unwrap());
    let egress = probe_says_up(&route, Scripted::new(&[]));
    transport(egress.fetch(&Request::get(server.url("/"))).await);
    assert_eq!(server.conns(), 0, "the connect never left unbound");
}

#[tokio::test]
async fn a_dead_proxy_fails_closed_even_when_the_probe_says_up() {
    let server = serve(|_| ok("leak")).await;
    let route = Route::Socks5(Socks5Route {
        proxy: unused_port_addr(),
        dns: SocksDns::Proxy,
        auth: None,
    });
    let egress = probe_says_up(&route, Scripted::new(&[]));
    transport(egress.fetch(&Request::get(server.url("/"))).await);
    assert_eq!(server.conns(), 0, "no direct fallback");
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn the_socket_is_bound_to_the_chosen_address() {
    let server = serve(|_| ok("bound")).await;
    let route = Route::BindAddress("127.0.0.2".parse().unwrap());
    let egress = probe_says_up(&route, Scripted::new(&[]));
    let resp = egress.fetch(&Request::get(server.url("/"))).await.unwrap();
    assert_eq!(resp.body, b"bound");
    let peers = server.peers.lock().unwrap().clone();
    assert_eq!(peers.len(), 1);
    assert_eq!(peers[0].ip(), "127.0.0.2".parse::<IpAddr>().unwrap());
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn the_device_bound_socket_fails_closed_even_when_the_probe_says_up() {
    let server = serve(|_| ok("leak")).await;
    let route = Route::Interface("lodestar-nope0".into());
    let egress = probe_says_up(&route, Scripted::new(&[]));
    transport(egress.fetch(&Request::get(server.url("/"))).await);
    assert_eq!(
        server.conns(),
        0,
        "the connect never left on another device"
    );
}

#[tokio::test]
async fn a_bind_address_route_never_connects_to_the_other_family() {
    // An IPv4 bind would leave an IPv6 socket unbound (on the default
    // route), so IPv6 answers and literals are refused.
    let resolver = Scripted::new(&[("v6only.test", &[&["::1"]])]);
    let route = Route::BindAddress("127.0.0.1".parse().unwrap());
    let egress = probe_says_up(&route, Arc::clone(&resolver));
    transport(egress.fetch(&Request::get("http://v6only.test:9/")).await);
    assert_eq!(resolver.lookups(), 1);
    transport(egress.fetch(&Request::get("http://[::1]:9/")).await);
    assert_eq!(egress.status().counters.answered, 0);
}

// ---------------------------------------------------------------- interface

#[cfg(target_os = "linux")]
#[tokio::test]
async fn an_interface_route_binds_by_name_and_fails_closed_without_it() {
    let server = serve(|_| ok("via lo")).await;
    let egress = factory(Scripted::new(&[]))
        .build("p", &Egress::Interface("lo".into()), lan())
        .await;
    let resp = egress.fetch(&Request::get(server.url("/"))).await.unwrap();
    assert_eq!(resp.body, b"via lo");
    assert_eq!(egress.status().state, RouteState::Up);
    assert_eq!(egress.status().notice, Some(DNS_OUTSIDE_ROUTE));

    // A missing interface: a VPN tunnel that isn't there.
    let egress = factory(Scripted::new(&[]))
        .build("p", &Egress::Interface("lodestar-nope0".into()), lan())
        .await;
    let err = transport(egress.fetch(&Request::get(server.url("/"))).await);
    assert!(err.contains("doesn't exist"), "{err}");
    assert_eq!(
        egress.status().state,
        RouteState::Down(DownReason::InterfaceMissing)
    );

    // A name that could escape `/sys/class/net` is refused outright.
    let egress = factory(Scripted::new(&[]))
        .build("p", &Egress::Interface("../lo".into()), lan())
        .await;
    transport(egress.fetch(&Request::get(server.url("/"))).await);
    assert_eq!(
        egress.status().state,
        RouteState::Down(DownReason::InvalidInterface)
    );
    assert_eq!(server.conns(), 1);
}

#[cfg(not(target_os = "linux"))]
#[tokio::test]
async fn an_interface_route_is_unsupported_and_fails_closed() {
    let server = serve(|_| ok("no")).await;
    let egress = factory(Scripted::new(&[]))
        .build("p", &Egress::Interface("Ethernet".into()), lan())
        .await;
    let err = transport(egress.fetch(&Request::get(server.url("/"))).await);
    assert!(err.contains("only on Linux"), "{err}");
    assert_eq!(
        egress.status().state,
        RouteState::Down(DownReason::Unsupported)
    );
    assert_eq!(server.conns(), 0);
}

// ---------------------------------------------------------------- SOCKS5

#[tokio::test]
async fn socks_with_proxy_dns_sends_the_name_and_resolves_nothing_locally() {
    let server = serve(|_| ok("tunnelled")).await;
    let names = HashMap::from([("fixture.test".to_owned(), server.addr)]);
    let socks = socks_stub(None, names).await;
    let resolver = Scripted::new(&[]);
    let egress = factory(Arc::clone(&resolver))
        .build(
            "p",
            &Egress::Socks5 {
                proxy: socks.addr,
                dns: SocksDns::Proxy,
            },
            public_only(),
        )
        .await;
    let url = format!("http://fixture.test:{}/", server.addr.port());
    let resp = egress.fetch(&Request::get(&url)).await.unwrap();
    assert_eq!(resp.body, b"tunnelled");
    assert_eq!(
        socks.targets(),
        vec![Target::Domain("fixture.test".into(), server.addr.port())]
    );
    assert_eq!(resolver.lookups(), 0, "no lookup outside the tunnel");
    assert_eq!(egress.status().state, RouteState::Up);

    // What the guard can still see is checked: plainly local names and
    // literals never reach the proxy under a public-only policy.
    for url in [
        format!("http://localhost:{}/", server.addr.port()),
        "http://admin.localhost/".to_owned(),
        "http://nas.lan/".to_owned(),
        "http://router/".to_owned(),
        "http://printer.local./".to_owned(),
        format!("http://127.0.0.1:{}/", server.addr.port()),
        "http://169.254.169.254/".to_owned(),
    ] {
        let err = transport(egress.fetch(&Request::get(&url)).await);
        assert!(err.contains("refused"), "{url}: {err}");
    }
    assert_eq!(socks.targets().len(), 1);
    assert_eq!(egress.status().counters.policy_refusals, 7);
}

#[tokio::test]
async fn socks_with_local_dns_checks_every_answer_and_sends_an_address() {
    let server = serve(|_| ok("checked")).await;
    let socks = socks_stub(None, HashMap::new()).await;
    let resolver = Scripted::new(&[("fixture.test", &[&["127.0.0.1"]])]);
    let socks5 = Egress::Socks5 {
        proxy: socks.addr,
        dns: SocksDns::Local,
    };
    let url = format!("http://fixture.test:{}/", server.addr.port());

    let egress = factory(Arc::clone(&resolver))
        .build("p", &socks5, lan())
        .await;
    let resp = egress.fetch(&Request::get(&url)).await.unwrap();
    assert_eq!(resp.body, b"checked");
    assert_eq!(socks.targets(), vec![Target::Addr(server.addr)]);
    assert_eq!(resolver.lookups(), 1);

    // Under a public-only policy the loopback answer is refused before the
    // proxy is asked for anything.
    let egress = factory(Arc::clone(&resolver))
        .build("p", &socks5, public_only())
        .await;
    let err = transport(egress.fetch(&Request::get(&url)).await);
    assert!(err.contains("loopback"), "{err}");
    assert_eq!(socks.targets().len(), 1);
}

#[tokio::test]
async fn socks_credentials_come_from_the_store_and_never_show() {
    const PASS: &str = "s3cr3t p@ss:w/rd%";
    let server = serve(|_| ok("authed")).await;
    let socks = socks_stub(Some(("lodestar-user", PASS)), HashMap::new()).await;
    let route = Egress::Socks5 {
        proxy: socks.addr,
        dns: SocksDns::Proxy,
    };
    let url = server.url("/");
    let lan_any = || Endpoints::user_entered(NetPolicy::download_client());

    let good = EgressFactory::with_resolver(
        Arc::new(Creds(Some(("lodestar-user", PASS)))),
        Scripted::new(&[]),
    )
    .build("p", &route, lan_any())
    .await;
    let resp = good.fetch(&Request::get(&url)).await.unwrap();
    assert_eq!(resp.body, b"authed");
    assert_eq!(
        socks.creds.lock().unwrap().last().cloned(),
        Some(("lodestar-user".to_owned(), PASS.to_owned()))
    );

    let wrong = EgressFactory::with_resolver(
        Arc::new(Creds(Some(("lodestar-user", "not-it")))),
        Scripted::new(&[]),
    )
    .build("p", &route, lan_any())
    .await;
    let err = transport(wrong.fetch(&Request::get(&url)).await);
    let none = EgressFactory::with_resolver(Arc::new(NoCredentials), Scripted::new(&[]))
        .build("p", &route, lan_any())
        .await;
    let err2 = transport(none.fetch(&Request::get(&url)).await);

    for shown in [
        err,
        err2,
        format!("{good:?}"),
        format!("{:?}", good.status()),
        format!("{route:?}"),
        format!(
            "{:?}",
            Route::Socks5(Socks5Route {
                proxy: socks.addr,
                dns: SocksDns::Proxy,
                auth: Some(SocksAuth {
                    username: Secret::new("lodestar-user"),
                    password: Secret::new(PASS),
                }),
            })
        ),
    ] {
        assert!(
            !shown.contains(PASS) && !shown.contains("s3cr3t"),
            "{shown}"
        );
        assert!(!shown.contains("lodestar-user"), "{shown}");
    }

    // RFC 1929 caps each at 255 bytes; a longer one keeps the route down.
    let long: &'static str = Box::leak("x".repeat(256).into_boxed_str());
    let too_long = EgressFactory::with_resolver(
        Arc::new(Creds(Some(("lodestar-user", long)))),
        Scripted::new(&[]),
    )
    .build("p", &route, lan_any())
    .await;
    transport(too_long.fetch(&Request::get(&url)).await);
    assert_eq!(
        too_long.status().state,
        RouteState::Down(DownReason::CredentialsUnavailable)
    );

    // A store that can't answer keeps the route down.
    let broken = EgressFactory::with_resolver(Arc::new(BrokenStore), Scripted::new(&[]))
        .build("p", &route, lan_any())
        .await;
    transport(broken.fetch(&Request::get(&url)).await);
    assert_eq!(
        broken.status().state,
        RouteState::Down(DownReason::CredentialsUnavailable)
    );
}

#[tokio::test]
async fn an_unreachable_proxy_refuses_and_never_goes_direct() {
    let server = serve(|_| ok("leak")).await;
    let egress = factory(Scripted::new(&[]))
        .build(
            "p",
            &Egress::Socks5 {
                proxy: unused_port_addr(),
                dns: SocksDns::Proxy,
            },
            lan(),
        )
        .await;
    let err = transport(egress.fetch(&Request::get(server.url("/"))).await);
    assert!(err.contains("SOCKS5 proxy isn't reachable"), "{err}");
    let status = egress.status();
    assert_eq!(status.state, RouteState::Down(DownReason::ProxyUnreachable));
    assert_eq!(status.counters.kill_switch_refusals, 1);
    assert_eq!(server.conns(), 0, "no direct fallback");
}

#[tokio::test]
async fn a_proxy_that_dies_mid_request_fails_the_request_and_trips() {
    let server = serve(|line| {
        if line.contains("/stall") {
            Reply::Stall
        } else {
            ok("fine")
        }
    })
    .await;
    let mut requests = server.requests;
    let socks = socks_stub(None, HashMap::new()).await;
    let egress = Arc::new(
        factory(Scripted::new(&[]))
            .build(
                "p",
                &Egress::Socks5 {
                    proxy: socks.addr,
                    dns: SocksDns::Local,
                },
                lan(),
            )
            .await,
    );
    let url = format!("http://{}/stall", server.addr);
    let inflight = {
        let egress = Arc::clone(&egress);
        tokio::spawn(async move { egress.fetch(&Request::get(url)).await })
    };
    within(requests.recv()).await.unwrap();
    socks.stop().await;
    let result = within(inflight).await.unwrap();
    assert!(result.is_err(), "{result:?}");

    // The next request finds the proxy gone and the switch trips; the
    // server only ever saw the tunnelled connection.
    transport(
        egress
            .fetch(&Request::get(format!("http://{}/", server.addr)))
            .await,
    );
    let mut watch = egress.watch();
    within(watch.wait_for(|s| *s == RouteState::Down(DownReason::ProxyUnreachable)))
        .await
        .unwrap();
    assert_eq!(egress.status().counters.trips, 1);
    assert_eq!(server.conns.load(Ordering::SeqCst), 1);
}

// ---------------------------------------------------------------- kill switch

/// A route whose availability the test controls.
struct Switch(AtomicBool);

impl RouteProbe for Switch {
    fn probe(&self) -> BoxFuture<'_, Result<(), DownReason>> {
        let up = self.0.load(Ordering::SeqCst);
        Box::pin(async move {
            if up {
                Ok(())
            } else {
                Err(DownReason::InterfaceDown)
            }
        })
    }

    fn ttl(&self) -> Option<Duration> {
        Some(Duration::ZERO)
    }
}

#[tokio::test]
async fn the_kill_switch_abandons_a_request_in_flight_and_refuses_until_up() {
    let server = serve(|line| {
        if line.contains("/stall") {
            Reply::Stall
        } else {
            ok("back")
        }
    })
    .await;
    let mut requests = server.requests;
    let switch = Arc::new(Switch(AtomicBool::new(true)));
    let client = GuardedClient::with_route(
        NetPolicy::download_client(),
        DEADLINE * 3,
        Scripted::new(&[]),
        &Route::Direct,
    )
    .unwrap();
    let egress = Arc::new(PluginEgress::new(
        "p",
        RouteKind::Interface,
        Ok(client),
        Arc::clone(&switch) as Arc<dyn RouteProbe>,
        lan(),
    ));
    let monitor = {
        let egress = Arc::clone(&egress);
        tokio::spawn(async move { egress.monitor(Duration::from_millis(20)).await })
    };

    let url = format!("http://{}/stall", server.addr);
    let inflight = {
        let egress = Arc::clone(&egress);
        tokio::spawn(async move { egress.fetch(&Request::get(url)).await })
    };
    within(requests.recv()).await.unwrap();
    switch.0.store(false, Ordering::SeqCst);
    // The request would stall for 30 s; the switch ends it well before
    // the deadline.
    let err = transport(within(inflight).await.unwrap());
    assert!(err.contains("egress went down"), "{err}");
    let status = egress.status();
    assert_eq!(status.state, RouteState::Down(DownReason::InterfaceDown));
    assert_eq!(status.counters.kill_switch_aborts, 1);
    assert_eq!(status.counters.trips, 1);

    let conns = server.addr;
    let err = transport(
        egress
            .fetch(&Request::get(format!("http://{conns}/")))
            .await,
    );
    assert!(err.contains("egress down"), "{err}");
    assert_eq!(egress.status().counters.kill_switch_refusals, 1);
    assert_eq!(server.conns.load(Ordering::SeqCst), 1);

    switch.0.store(true, Ordering::SeqCst);
    let resp = egress
        .fetch(&Request::get(format!("http://{conns}/")))
        .await
        .unwrap();
    assert_eq!(resp.body, b"back");
    assert_eq!(egress.status().state, RouteState::Up);
    monitor.abort();
}

// ---------------------------------------------------------------- policy

#[tokio::test]
async fn redirects_to_forbidden_addresses_are_refused_before_connecting() {
    let server = serve(|line| {
        if line.contains("/meta") {
            redirect(302, "http://169.254.169.254/latest/meta-data/")
        } else if line.contains("/linklocal") {
            redirect(307, "http://[fe80::1]/")
        } else if line.contains("/named") {
            redirect(301, "http://evil.test/")
        } else {
            ok("x")
        }
    })
    .await;
    let resolver = Scripted::new(&[("evil.test", &[&["169.254.169.254"]])]);
    let egress = Arc::new(
        factory(Arc::clone(&resolver))
            .build("p", &Egress::Direct, lan())
            .await,
    );
    let client = Client::new(Arc::clone(&egress), HostLimiter::new(fast_limits()));
    for (path, class) in [
        ("/meta", AddrClass::Metadata),
        ("/linklocal", AddrClass::LinkLocal),
        ("/named", AddrClass::Metadata),
    ] {
        let err = transport(send_following(&client, Request::get(server.url(path))).await);
        assert!(
            err.contains(&NetError::Blocked(class).to_string()),
            "{path}: {err}"
        );
    }
    assert_eq!(resolver.lookups(), 1, "the name was checked after DNS");
    assert_eq!(egress.status().counters.policy_refusals, 3);
    assert_eq!(egress.status().counters.answered, 3);
}

#[tokio::test]
async fn dns_rebinding_is_caught_on_the_next_connection() {
    let server = serve(|_| ok("first")).await;
    let resolver = Scripted::new(&[("rebind.test", &[&["127.0.0.1"], &["169.254.169.254"]])]);
    let egress = factory(Arc::clone(&resolver))
        .build("p", &Egress::Direct, lan())
        .await;
    let url = format!("http://rebind.test:{}/", server.addr.port());
    let resp = egress.fetch(&Request::get(&url)).await.unwrap();
    assert_eq!(resp.body, b"first");
    // The server closed the connection, so the next request resolves
    // again and gets the metadata address.
    let err = transport(egress.fetch(&Request::get(&url)).await);
    assert!(err.contains("metadata"), "{err}");
    assert_eq!(resolver.lookups(), 2);
    assert_eq!(server.conns(), 1);
}

#[tokio::test]
async fn manifest_endpoints_are_public_only_and_limited_to_declared_hosts() {
    let resolver = Scripted::new(&[("nyaa.si", &[&["127.0.0.1"]])]);
    let endpoints = Endpoints::manifest(&[
        Endpoint {
            protocol: "https",
            host: "nyaa.si",
        },
        Endpoint {
            protocol: "udp",
            host: "tracker.example",
        },
    ]);
    let egress = factory(Arc::clone(&resolver))
        .build("nyaa", &Egress::Direct, endpoints)
        .await;
    for url in [
        "https://other.example/",
        "https://sub.nyaa.si/",
        "https://tracker.example/",
        "https://nyaa.si.evil.example/",
        "http://plain.example/",
    ] {
        let err = transport(egress.fetch(&Request::get(url)).await);
        assert!(
            err.contains("isn't one of the plugin's endpoints"),
            "{url}: {err}"
        );
    }
    assert_eq!(resolver.lookups(), 0, "refused before any lookup");
    // A declared host that resolves somewhere private is refused after DNS.
    let err = transport(egress.fetch(&Request::get("https://NYAA.si/")).await);
    assert!(err.contains("loopback"), "{err}");
    assert_eq!(resolver.lookups(), 1);
    // Declared as https, so not over http.
    let err = transport(egress.fetch(&Request::get("http://nyaa.si/")).await);
    assert!(err.contains("isn't one of the plugin's endpoints"), "{err}");
    assert_eq!(resolver.lookups(), 1);
}

#[tokio::test]
async fn urls_with_credentials_or_other_schemes_are_refused() {
    let egress = factory(Scripted::new(&[]))
        .build("p", &Egress::Direct, lan())
        .await;
    for url in [
        "http://user:pass@127.0.0.1/",
        "ftp://127.0.0.1/",
        "file:///etc/passwd",
        "not a url",
    ] {
        let err = transport(egress.fetch(&Request::get(url)).await);
        assert!(!err.contains("pass"), "{err}");
    }
}

#[tokio::test]
async fn oversized_bodies_are_refused() {
    let big = "x".repeat(MAX_BODY + 1);
    let server = serve(move |_| ok(&big)).await;
    let egress = factory(Scripted::new(&[]))
        .build("p", &Egress::Direct, lan())
        .await;
    let err = transport(egress.fetch(&Request::get(server.url("/"))).await);
    assert!(err.contains("too large"), "{err}");
}

#[test]
fn timeouts_are_clamped() {
    let f = EgressFactory::new(Arc::new(NoCredentials));
    assert_eq!(f.timeout, DEFAULT_TIMEOUT);
    assert_eq!(f.clone().with_timeout(Duration::ZERO).timeout, MIN_TIMEOUT);
    assert_eq!(
        f.with_timeout(Duration::from_secs(3600)).timeout,
        MAX_TIMEOUT
    );
}

// ---------------------------------------------------------------- redirects

fn fast_limits() -> Limits {
    Limits {
        poll_interval: Duration::from_secs(60),
        per_host: HostRate {
            interval: Duration::from_secs(1),
            burst: 4,
        },
        max_full_scrapes_per_day: 0,
        backoff: Backoff {
            base: Duration::from_secs(1),
            max: Duration::from_secs(60),
        },
    }
}

/// Answers from a table and records every request, for the redirect
/// rules alone.
struct Table {
    answers: HashMap<String, Response>,
    seen: Mutex<Vec<Request>>,
}

impl Fetch for Table {
    fn fetch<'a>(&'a self, req: &'a Request) -> BoxFuture<'a, Result<Response, HttpError>> {
        self.seen.lock().unwrap().push(req.clone());
        let r = self.answers.get(&req.url).cloned().unwrap_or(Response {
            status: 200,
            headers: Vec::new(),
            body: b"end".to_vec(),
        });
        Box::pin(async move { Ok(r) })
    }
}

fn moved(status: u16, to: &str) -> Response {
    Response {
        status,
        headers: vec![("Location".into(), to.into())],
        body: Vec::new(),
    }
}

fn table(answers: &[(&str, Response)]) -> (Client<Arc<Table>>, Arc<Table>) {
    let table = Arc::new(Table {
        answers: answers
            .iter()
            .map(|(u, r)| ((*u).to_owned(), r.clone()))
            .collect(),
        seen: Mutex::new(Vec::new()),
    });
    let client = Client::new(Arc::clone(&table), HostLimiter::with_seed(fast_limits(), 1));
    (client, table)
}

fn seen(table: &Table) -> Vec<Request> {
    table.seen.lock().unwrap().clone()
}

#[tokio::test(start_paused = true)]
async fn redirect_hops_are_bounded() {
    let (client, t) = table(&[("http://a.example/loop", moved(302, "/loop"))]);
    let err = transport(send_following(&client, Request::get("http://a.example/loop")).await);
    assert!(err.contains("too many redirects"), "{err}");
    assert_eq!(seen(&t).len(), MAX_REDIRECTS + 1);
}

#[tokio::test(start_paused = true)]
async fn redirects_drop_credentials_across_origins_and_refuse_downgrades() {
    let (client, t) = table(&[
        ("https://a.example/1", moved(307, "https://a.example/2")),
        ("https://a.example/2", moved(308, "https://b.example/3")),
        ("https://b.example/3", moved(303, "https://b.example/4")),
    ]);
    let mut req = Request::get("https://a.example/1")
        .header("Authorization", "Bearer t")
        .header("X-Api-Key", "k")
        .header("Accept", "*/*");
    req.method = Method::Post;
    req.body = b"payload".to_vec();
    let resp = send_following(&client, req).await.unwrap();
    assert_eq!(resp.body, b"end");
    let hops = seen(&t);
    assert_eq!(hops.len(), 4);
    // Same origin: the method, body and credentials stay.
    assert_eq!(hops[1].method, Method::Post);
    assert_eq!(hops[1].body, b"payload");
    assert_eq!(hops[1].headers.len(), 3);
    // Another origin: the credentials go; 308 keeps the method.
    assert_eq!(hops[2].method, Method::Post);
    assert_eq!(hops[2].headers, vec![("Accept".into(), "*/*".into())]);
    // 303 turns it into a GET without a body.
    assert_eq!(hops[3].method, Method::Get);
    assert!(hops[3].body.is_empty());

    let (client, _) = table(&[("https://a.example/", moved(302, "http://a.example/"))]);
    let err = transport(send_following(&client, Request::get("https://a.example/")).await);
    assert!(err.contains("https to http"), "{err}");
    let (client, _) = table(&[("https://a.example/", moved(302, "file:///etc/passwd"))]);
    let err = transport(send_following(&client, Request::get("https://a.example/")).await);
    assert!(err.contains("left http"), "{err}");
}
