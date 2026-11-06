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
    /// Write the head, then each chunk `every` apart (pacing the server,
    /// not the test), then close.
    Dribble {
        head: Vec<u8>,
        chunks: Vec<Vec<u8>>,
        every: Duration,
    },
    /// Write these bytes, then hold the connection open.
    ThenStall(Vec<u8>),
}

/// A 200 head with no length: the body runs until the connection closes.
fn open_head() -> Vec<u8> {
    b"HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n".to_vec()
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
                    Reply::Dribble {
                        head,
                        chunks,
                        every,
                    } => {
                        let _ = sock.write_all(&head).await;
                        for c in chunks {
                            tokio::time::sleep(every).await;
                            if sock.write_all(&c).await.is_err() {
                                return;
                            }
                        }
                        let _ = sock.shutdown().await;
                    }
                    Reply::ThenStall(b) => {
                        let _ = sock.write_all(&b).await;
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

    fn source_enabled<'a>(&'a self, _: &'a str) -> BoxFuture<'a, Result<bool, CredentialError>> {
        Box::pin(async { Ok(true) })
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

    fn source_enabled<'a>(&'a self, _: &'a str) -> BoxFuture<'a, Result<bool, CredentialError>> {
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

/// A request that failed: in transport, by policy, or at the kill switch
/// (whose text reads "egress down: …").
fn transport(r: Result<Response, HttpError>) -> String {
    match r {
        Err(HttpError::Transport(e)) => e,
        Err(e @ HttpError::EgressDown(_)) => e.to_string(),
        other => panic!("expected a failed request, got {other:?}"),
    }
}

/// A kill-switch refusal or abort: its own variant, so the host isn't
/// backed off.
fn egress_down(r: Result<Response, HttpError>) -> String {
    match r {
        Err(e @ HttpError::EgressDown(_)) => e.to_string(),
        other => panic!("expected the kill switch, got {other:?}"),
    }
}

/// A streamed request's error text, as [`transport`] gives a buffered one's.
fn stream_err(r: Result<StreamResponse, HttpError>) -> HttpError {
    match r {
        Err(e) => e,
        Ok(res) => panic!("expected a failed request, got {res:?}"),
    }
}

/// Reads a streamed body to its end: the bytes, or the error with what
/// was read before it.
async fn read_stream(res: &mut StreamResponse) -> Result<Vec<u8>, (usize, HttpError)> {
    let mut all = Vec::new();
    loop {
        match res.next_chunk().await {
            Ok(Some(c)) => all.extend_from_slice(&c),
            Ok(None) => return Ok(all),
            Err(e) => return Err((all.len(), e)),
        }
    }
}

fn stream_limits(max_bytes: u64, timeout: Duration) -> StreamLimits {
    StreamLimits {
        max_bytes,
        timeout,
        idle: timeout,
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
    let err = egress_down(egress.fetch(&Request::get(server.url("/"))).await);
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
    let err = egress_down(within(inflight).await.unwrap());
    assert!(err.contains("went down in flight"), "{err}");
    let status = egress.status();
    assert_eq!(status.state, RouteState::Down(DownReason::InterfaceDown));
    assert_eq!(status.counters.kill_switch_aborts, 1);
    assert_eq!(status.counters.trips, 1);

    let conns = server.addr;
    let err = egress_down(
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

/// While the route is down, refusals come back paced: 1 s, doubling to
/// 30 s, cut short when the route comes up, and starting over once a
/// request goes through (SRC-14b).
#[tokio::test(start_paused = true)]
async fn refusals_while_down_are_paced_and_bounded() {
    assert_eq!(down_delay(0), DOWN_DELAY_MIN);
    assert_eq!(down_delay(u32::MAX), DOWN_DELAY_MAX);
    let switch = Arc::new(Switch(AtomicBool::new(false)));
    let client = GuardedClient::with_route(
        NetPolicy::download_client(),
        DEADLINE,
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
    let url = format!("http://{}/", unused_port_addr());
    let timed = |egress: Arc<PluginEgress>, url: String| async move {
        let t = Instant::now();
        let r = egress.fetch(&Request::get(url)).await;
        (r, t.elapsed())
    };
    let mut waits = Vec::new();
    for _ in 0..7 {
        let (r, took) = timed(Arc::clone(&egress), url.clone()).await;
        egress_down(r);
        waits.push(took.as_secs());
    }
    assert_eq!(waits, [1, 2, 4, 8, 16, 30, 30]);
    assert_eq!(egress.status().counters.kill_switch_refusals, 7);

    // The route coming up ends the wait early.
    let pending = tokio::spawn(timed(Arc::clone(&egress), url.clone()));
    tokio::time::sleep(Duration::from_secs(5)).await;
    switch.0.store(true, Ordering::SeqCst);
    egress.check_now().await;
    let (r, took) = pending.await.unwrap();
    egress_down(r);
    assert_eq!(took.as_secs(), 5);

    // A request that goes through (here to a closed port) starts the
    // pacing over.
    let (r, _) = timed(Arc::clone(&egress), url.clone()).await;
    transport(r);
    switch.0.store(false, Ordering::SeqCst);
    let (r, took) = timed(Arc::clone(&egress), url).await;
    egress_down(r);
    assert_eq!(took, DOWN_DELAY_MIN);
}

// ---------------------------------------------------------------- policy

#[tokio::test]
async fn a_redirect_to_another_port_on_the_listed_host_is_refused() {
    // Another service on the same loopback host: reachable by address, but
    // not one of the plugin's endpoints.
    let other = serve(|_| ok("other")).await;
    let to = other.url("/");
    let server = serve(move |_| redirect(302, &to)).await;
    let egress = Arc::new(
        factory(Scripted::new(&[]))
            .build("p", &Egress::Direct, lan().only_origins(&[server.url("/")]))
            .await,
    );
    let client = Client::new(Arc::clone(&egress), HostLimiter::new(fast_limits()));
    let err = transport(send_following(&client, Request::get(server.url("/start"))).await);
    assert!(
        err.contains("the address isn't one of the plugin's endpoints"),
        "{err}"
    );
    assert_eq!(egress.status().counters.answered, 1, "the first hop went");
    assert_eq!(egress.status().counters.policy_refusals, 1);
    assert_eq!(other.conns(), 0, "the other port was never connected to");
}

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

#[test]
fn a_host_list_is_keyed_on_scheme_host_and_port() {
    let listed = |e: &Endpoints, url: &str| match e.check(url) {
        Ok(()) => true,
        Err(err) => {
            assert!(
                err.to_string()
                    .contains("isn't one of the plugin's endpoints"),
                "{url}: {err}"
            );
            false
        }
    };
    // A user-entered address on loopback: another service on the same
    // host (another port) isn't reachable through it.
    let e = Endpoints::user_entered(NetPolicy::download_client())
        .only_origins(&["http://127.0.0.1:9696/sub"]);
    assert!(listed(&e, "http://127.0.0.1:9696/api/v1/search"));
    assert!(listed(&e, "http://[::ffff:127.0.0.1]:9696/"));
    assert!(e.permits("http://127.0.0.1:9696/"));
    assert!(!e.permits("http://127.0.0.1:9697/"));
    for url in [
        "http://127.0.0.1/",
        "http://127.0.0.1:80/",
        "http://127.0.0.1:9697/",
        "http://127.0.0.1:6379/",
        "https://127.0.0.1:9696/",
    ] {
        assert!(!listed(&e, url), "{url}");
    }
    let e =
        Endpoints::user_entered(NetPolicy::download_client()).only_origins(&["http://[::1]:9696/"]);
    assert!(listed(&e, "http://[::1]:9696/x"));
    assert!(listed(&e, "http://[0:0::1]:9696/x"));
    assert!(!listed(&e, "http://[::1]:9697/"));
    assert!(!listed(&e, "http://127.0.0.1:9696/"));
    // No port given: the scheme's default, written or not.
    let e = Endpoints::user_entered(NetPolicy::download_client())
        .only_origins(&["http://nas.lan", "https://Client.LAN./x"]);
    for url in [
        "http://nas.lan/",
        "http://nas.lan:80/",
        "https://client.lan/",
        "https://client.lan:443/",
    ] {
        assert!(listed(&e, url), "{url}");
    }
    for url in [
        "http://nas.lan:8080/",
        "https://nas.lan/",
        "http://client.lan/",
        "https://client.lan:8443/",
    ] {
        assert!(!listed(&e, url), "{url}");
    }
    // An explicit default port in the list matches a URL without one.
    let e = Endpoints::user_entered(NetPolicy::download_client())
        .only_origins(&["https://client.lan:443/"]);
    assert!(listed(&e, "https://client.lan/"));
    // Something that isn't an http(s) URL adds nothing.
    let e = Endpoints::user_entered(NetPolicy::download_client())
        .only_origins(&["client.lan", "ftp://client.lan/"]);
    assert!(!listed(&e, "http://client.lan/"));
    // A manifest's hosts are on their scheme's default port only.
    let e = Endpoints::manifest(&[Endpoint {
        protocol: "https",
        host: "nyaa.si",
    }]);
    assert!(listed(&e, "https://nyaa.si:443/"));
    assert!(!listed(&e, "https://nyaa.si:8443/"));
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

    // Every header the request set goes across origins, under any name
    // (StashDB's `ApiKey`, a cookie, a custom one), and stays gone if a
    // later hop comes back; only the non-credential ones are kept.
    let (client, t) = table(&[
        ("https://a.example/1", moved(307, "https://b.example/2")),
        ("https://b.example/2", moved(302, "https://a.example/3")),
    ]);
    let mut req = Request::get("https://a.example/1")
        .header("ApiKey", "stash-secret")
        .header("apikey", "lower")
        .header("Cookie", "s=1")
        .header("Proxy-Authorization", "Basic x")
        .header("X-Plugin-Token", "t")
        .header("Accept", "application/json")
        .header("Content-Type", "application/json")
        .header("User-Agent", "Lodestar");
    req.method = Method::Post;
    req.body = b"{}".to_vec();
    send_following(&client, req).await.unwrap();
    let hops = seen(&t);
    assert_eq!(hops.len(), 3);
    assert_eq!(hops[0].headers.len(), 8);
    let kept = vec![
        ("Accept".to_owned(), "application/json".to_owned()),
        ("Content-Type".to_owned(), "application/json".to_owned()),
        ("User-Agent".to_owned(), "Lodestar".to_owned()),
    ];
    assert_eq!(hops[1].headers, kept);
    assert_eq!(hops[1].body, b"{}");
    // 302 makes it a GET and drops the body's headers; back on the first
    // origin, nothing that was dropped returns.
    assert_eq!(hops[2].url, "https://a.example/3");
    assert_eq!(
        hops[2].headers,
        vec![
            ("Accept".to_owned(), "application/json".to_owned()),
            ("User-Agent".to_owned(), "Lodestar".to_owned()),
        ]
    );

    let (client, _) = table(&[("https://a.example/", moved(302, "http://a.example/"))]);
    let err = transport(send_following(&client, Request::get("https://a.example/")).await);
    assert!(err.contains("https to http"), "{err}");
    let (client, _) = table(&[("https://a.example/", moved(302, "file:///etc/passwd"))]);
    let err = transport(send_following(&client, Request::get("https://a.example/")).await);
    assert!(err.contains("left http"), "{err}");
}

/// The manifest's own hosts (pinned) for a plugin with one endpoint.
fn pinned() -> Endpoints {
    Endpoints::manifest(&[Endpoint {
        protocol: "https",
        host: "feed.example",
    }])
}

/// Who resolves names when the user didn't say depends on where the
/// plugin's endpoints come from, never on where the proxy is (runner,
/// #394): the proxy for manifest-pinned hosts, Lodestar for hosts the
/// user entered.
#[tokio::test]
async fn auto_dns_follows_the_endpoints_not_the_proxys_address() {
    let resolver = Scripted::new(&[
        ("gluetun", &[&["172.18.0.5"]]),
        ("tailnet-proxy", &[&["100.64.0.7"]]),
        ("proxy.vpn.example", &[&["198.51.100.7"]]),
    ]);
    let f = factory(resolver);
    let auto = |host: &str| ProxySpec {
        host: host.into(),
        port: 1080,
        dns: None,
    };
    // Private, Mullvad's in-tunnel address, loopback, CGNAT and public;
    // literals and names alike.
    for host in [
        "gluetun",
        "10.64.0.1",
        "127.0.0.1",
        "::1",
        "tailnet-proxy",
        "100.64.0.1",
        "proxy.vpn.example",
        "203.0.113.9",
    ] {
        for (endpoints, want) in [
            (pinned(), SocksDns::Proxy),
            (public_only(), SocksDns::Local),
        ] {
            let (e, route) = f.build_socks5("p", &auto(host), endpoints).await;
            match route {
                Some(Egress::Socks5 { dns, .. }) => assert_eq!(dns, want, "{host}"),
                other => panic!("{host}: {other:?}"),
            }
            let notice = (want == SocksDns::Local).then_some(DNS_OUTSIDE_ROUTE);
            assert_eq!(e.status().notice, notice, "{host}");
        }
    }
}

// Paused: each refusal waits `DOWN_DELAY_MIN` before it comes back.
#[tokio::test(start_paused = true)]
async fn a_proxy_by_name_resolves_once_and_an_unusable_one_stays_down() {
    let resolver = Scripted::new(&[
        ("gluetun", &[&["172.18.0.5"]]),
        (
            "nowhere",
            &[&["0.0.0.0", "224.0.0.1", "::ffff:0.0.0.0", "::ffff:224.0.0.1"]],
        ),
        ("mixed", &[&["0.0.0.0", "172.18.0.9"]]),
    ]);
    let f = factory(resolver.clone());
    let spec = |host: &str, dns| ProxySpec {
        host: host.into(),
        port: 1080,
        dns,
    };
    let (_, route) = f.build_socks5("p", &spec("gluetun", None), pinned()).await;
    assert_eq!(
        route,
        Some(Egress::Socks5 {
            proxy: "172.18.0.5:1080".parse().unwrap(),
            dns: SocksDns::Proxy
        })
    );
    // The first usable answer is taken.
    let (_, route) = f.build_socks5("p", &spec("mixed", None), pinned()).await;
    assert!(
        matches!(route, Some(Egress::Socks5 { proxy, .. }) if proxy.ip().to_string() == "172.18.0.9")
    );
    // Mapped forms of usable addresses are still usable.
    for host in ["::ffff:127.0.0.1", "::ffff:192.168.1.10"] {
        let (_, route) = f.build_socks5("p", &spec(host, None), lan()).await;
        assert!(route.is_some(), "{host}");
    }
    // A literal isn't looked up, and an explicit choice wins.
    let before = resolver.lookups();
    let (_, route) = f
        .build_socks5("p", &spec("10.8.0.1", Some(SocksDns::Local)), pinned())
        .await;
    assert_eq!(resolver.lookups(), before);
    assert!(matches!(
        route,
        Some(Egress::Socks5 {
            dns: SocksDns::Local,
            ..
        })
    ));
    // A name that doesn't resolve, or only to addresses no proxy can be
    // at, and an unusable literal: down for good, and nothing is sent.
    for host in [
        "missing",
        "nowhere",
        "0.0.0.0",
        "::",
        "239.1.1.1",
        "255.255.255.255",
        // IPv4-mapped: judged as the IPv4 address they carry.
        "::ffff:0.0.0.0",
        "::ffff:224.0.0.1",
        "::ffff:255.255.255.255",
    ] {
        let (e, route) = f.build_socks5("p", &spec(host, None), lan()).await;
        assert_eq!(route, None, "{host}");
        assert_eq!(
            e.status().state,
            RouteState::Down(DownReason::ProxyUnresolved),
            "{host}"
        );
        // A URL the endpoint policy allows, so the kill switch refuses it.
        let err = egress_down(e.fetch(&Request::get("http://127.0.0.1:9/")).await);
        assert!(err.contains("didn't resolve"), "{err}");
        assert_eq!(e.status().counters.kill_switch_refusals, 1);
    }
}

#[tokio::test]
async fn an_unreadable_setting_is_a_route_that_stays_down() {
    let f = factory(Scripted::new(&[]));
    let e = f.unavailable("p", RouteKind::Socks5, DownReason::SettingUnreadable, lan());
    assert_eq!(
        e.status().state,
        RouteState::Down(DownReason::SettingUnreadable)
    );
    let err = egress_down(e.fetch(&Request::get("http://127.0.0.1:9/")).await);
    assert!(err.contains("couldn't be read"), "{err}");
    assert_eq!(
        e.check_now().await,
        RouteState::Down(DownReason::SettingUnreadable)
    );
}

#[test]
fn lan_means_private_loopback_or_link_local() {
    for ip in [
        "10.0.0.1",
        "192.168.1.2",
        "127.0.0.1",
        "::1",
        "fd00::1",
        "169.254.1.1",
        "100.64.0.1",
    ] {
        assert!(on_lan(ip.parse().unwrap()), "{ip}");
    }
    for ip in ["198.51.100.7", "2001:db8::1", "8.8.8.8"] {
        let ip: IpAddr = ip.parse().unwrap();
        if net::classify(ip) == AddrClass::Public {
            assert!(!on_lan(ip));
        }
    }
}

/// A probe whose good answer is trusted for a minute: the window in which
/// a route can die without the gate noticing.
struct Trusting(AtomicBool);

impl RouteProbe for Trusting {
    fn probe(&self) -> BoxFuture<'_, Result<(), DownReason>> {
        let up = self.0.load(Ordering::SeqCst);
        Box::pin(async move {
            if up {
                Ok(())
            } else {
                Err(DownReason::ProxyUnreachable)
            }
        })
    }

    fn ttl(&self) -> Option<Duration> {
        Some(Duration::from_secs(60))
    }
}

#[tokio::test]
async fn a_route_that_dies_between_probes_is_the_kill_switchs_error_not_the_hosts() {
    let probe = Arc::new(Trusting(AtomicBool::new(true)));
    let client = GuardedClient::with_route(
        NetPolicy::download_client(),
        DEADLINE,
        Scripted::new(&[]),
        &Route::Direct,
    )
    .unwrap();
    let egress = PluginEgress::new(
        "p",
        RouteKind::Socks5,
        Ok(client),
        Arc::clone(&probe) as Arc<dyn RouteProbe>,
        lan(),
    );
    assert_eq!(egress.check_now().await, RouteState::Up);
    // The route dies; the gate still trusts its last good probe, so the
    // request is sent and fails to connect.
    probe.0.store(false, Ordering::SeqCst);
    let url = format!("http://{}/", unused_port_addr());
    let result = egress.fetch(&Request::get(url.clone())).await;
    let err = egress_down(result.clone());
    assert!(err.contains("went down in flight"), "{err}");
    let c = egress.status().counters;
    assert_eq!((c.kill_switch_aborts, c.transport_failures), (1, 0));
    // So the plugin's limiter doesn't back the host off.
    let limiter = HostLimiter::with_seed(
        crate::Policy::STRICT.limits(&crate::UserLimits::default()),
        1,
    );
    let host = host_key(&url).unwrap();
    limiter.record(&host, &result, Instant::now());
    assert_eq!(
        limiter.state(&host, Instant::now()),
        crate::HostState::Ready
    );
    // With the route up, a refused connection is the host's.
    probe.0.store(true, Ordering::SeqCst);
    assert_eq!(egress.check_now().await, RouteState::Up);
    let result = egress.fetch(&Request::get(url)).await;
    assert!(matches!(result, Err(HttpError::Transport(_))), "{result:?}");
    assert_eq!(egress.status().counters.transport_failures, 1);
}

// ---------------------------------------------------------------- streamed bodies (SRC-12b)

/// A body over `MAX_BODY`, which `fetch` refuses, comes whole through
/// `fetch_stream` within the plugin's cap.
#[tokio::test]
async fn a_body_over_max_body_streams_within_the_plugins_cap() {
    let big = vec![b'z'; MAX_BODY + 4 * 1024 * 1024];
    let mut reply = open_head();
    reply.extend_from_slice(&big);
    let server = serve(move |_| Reply::Bytes(reply.clone())).await;
    let egress = factory(Scripted::new(&[]))
        .build("p", &Egress::Direct, lan())
        .await;
    let err = transport(egress.fetch(&Request::get(server.url("/"))).await);
    assert!(err.contains("too large"), "{err}");
    let mut res = within(egress.fetch_stream(
        &Request::get(server.url("/")),
        stream_limits(64 << 20, DEADLINE),
    ))
    .await
    .unwrap();
    assert_eq!(res.status, 200);
    let body = within(read_stream(&mut res)).await.unwrap();
    assert_eq!(body.len(), big.len());
    assert!(body.iter().all(|b| *b == b'z'));
    assert_eq!(egress.status().counters.answered, 1);
}

/// A body that takes longer than the factory's timeout is read whole
/// when the plugin's own timeout allows it; `fetch` on the same reply
/// times out.
#[tokio::test]
async fn a_slow_body_streams_past_the_factorys_timeout() {
    let server = serve(|_| Reply::Dribble {
        head: open_head(),
        chunks: vec![b"one,".to_vec(), b"two,".to_vec(), b"three".to_vec()],
        every: Duration::from_millis(700),
    })
    .await;
    let egress = EgressFactory::with_resolver(Arc::new(NoCredentials), Scripted::new(&[]))
        .with_timeout(MIN_TIMEOUT)
        .build("p", &Egress::Direct, lan())
        .await;
    let err = transport(within(egress.fetch(&Request::get(server.url("/")))).await);
    assert!(err.contains("timed out"), "{err}");
    let mut res = within(egress.fetch_stream(
        &Request::get(server.url("/")),
        stream_limits(1 << 20, DEADLINE),
    ))
    .await
    .unwrap();
    assert_eq!(
        within(read_stream(&mut res)).await.unwrap(),
        b"one,two,three"
    );
}

/// Past the plugin's timeout the transport ends the body, and the
/// stream fails closed.
#[tokio::test]
async fn a_body_past_the_plugins_timeout_fails_closed() {
    let server = serve(|_| Reply::Dribble {
        head: open_head(),
        chunks: (0..6).map(|_| b"slow".to_vec()).collect(),
        every: Duration::from_millis(1500),
    })
    .await;
    let egress = factory(Scripted::new(&[]))
        .build("p", &Egress::Direct, lan())
        .await;
    let mut res = within(egress.fetch_stream(
        &Request::get(server.url("/")),
        stream_limits(1 << 20, Duration::from_secs(1)),
    ))
    .await
    .unwrap();
    let (read, e) = within(read_stream(&mut res)).await.unwrap_err();
    assert!(read < 24, "{read}");
    assert!(
        matches!(&e, HttpError::Transport(t) if t.contains("timed out")),
        "{e:?}"
    );
    // Fused: the same error again, and the transport isn't read again.
    assert_eq!(res.next_chunk().await, Err(e));
    assert_eq!(egress.status().counters.transport_failures, 1);
}

/// The byte cap holds in the transport: a declared length over it is
/// refused at the head, and a body that grows past it fails there.
#[tokio::test]
async fn a_body_over_the_plugins_cap_fails_closed_in_the_transport() {
    let server = serve(|line| {
        if line.contains("/declared") {
            ok(&"d".repeat(5000))
        } else {
            let mut b = open_head();
            b.extend_from_slice(&[b'u'; 5000]);
            Reply::Bytes(b)
        }
    })
    .await;
    let egress = factory(Scripted::new(&[]))
        .build("p", &Egress::Direct, lan())
        .await;
    let e = stream_err(
        egress
            .fetch_stream(
                &Request::get(server.url("/declared")),
                stream_limits(1000, DEADLINE),
            )
            .await,
    );
    assert!(
        matches!(&e, HttpError::Transport(t) if t.contains("too large")),
        "{e:?}"
    );
    let mut res = within(egress.fetch_stream(
        &Request::get(server.url("/undeclared")),
        stream_limits(1000, DEADLINE),
    ))
    .await
    .unwrap();
    let (read, e) = within(read_stream(&mut res)).await.unwrap_err();
    assert!(read <= 1000, "{read}");
    assert!(
        matches!(&e, HttpError::Transport(t) if t.contains("too large")),
        "{e:?}"
    );
}

/// `net` holds a streamed exchange to its own ceiling whatever its
/// caller asks: a declared length past it is refused.
#[tokio::test]
async fn the_transport_holds_its_own_ceiling() {
    let declared = net::STREAM_CEILING_BYTES + 1;
    let server = serve(move |_| {
        Reply::ThenStall(
            format!("HTTP/1.1 200 OK\r\nContent-Length: {declared}\r\n\r\n").into_bytes(),
        )
    })
    .await;
    let client =
        GuardedClient::with_resolver(NetPolicy::download_client(), DEADLINE, Scripted::new(&[]))
            .unwrap();
    let e = within(client.exchange_stream(
        "GET",
        &server.url("/"),
        &[],
        Vec::new(),
        u64::MAX,
        Duration::MAX,
    ))
    .await
    .unwrap_err();
    assert_eq!(e, NetError::TooLarge);
}

/// The kill switch abandons a body still being read, and refuses a
/// streamed request before sending while the route is down.
#[tokio::test]
async fn the_kill_switch_abandons_a_streamed_body_in_flight() {
    let mut first = open_head();
    first.extend_from_slice(b"first");
    let server = serve(move |_| Reply::ThenStall(first.clone())).await;
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
    let url = format!("http://{}/", server.addr);
    let mut res = within(egress.fetch_stream(
        &Request::get(url.clone()),
        stream_limits(1 << 20, DEADLINE * 3),
    ))
    .await
    .unwrap();
    within(requests.recv()).await.unwrap();
    assert_eq!(within(res.next_chunk()).await.unwrap().unwrap(), b"first");
    // The body would stall for 30 s; the switch ends it well before the
    // deadline.
    let reading = tokio::spawn(async move { res.next_chunk().await });
    switch.0.store(false, Ordering::SeqCst);
    let e = within(reading).await.unwrap().unwrap_err();
    assert!(
        matches!(&e, HttpError::EgressDown(t) if t.contains("went down in flight")),
        "{e:?}"
    );
    assert_eq!(egress.status().counters.kill_switch_aborts, 1);
    // Down: refused before anything is sent.
    let e = stream_err(
        within(egress.fetch_stream(&Request::get(url), stream_limits(1 << 20, DEADLINE))).await,
    );
    assert!(matches!(e, HttpError::EgressDown(_)), "{e:?}");
    assert_eq!(egress.status().counters.kill_switch_refusals, 1);
    assert_eq!(server.conns.load(Ordering::SeqCst), 1);
    monitor.abort();
}

/// The endpoint policy applies to a streamed request as to any other.
#[tokio::test]
async fn a_streamed_request_keeps_the_endpoint_policy() {
    let server = serve(|_| ok("never")).await;
    let egress = factory(Scripted::new(&[]))
        .build("p", &Egress::Direct, public_only())
        .await;
    let e = stream_err(
        egress
            .fetch_stream(
                &Request::get(server.url("/")),
                stream_limits(1 << 20, DEADLINE),
            )
            .await,
    );
    assert!(
        matches!(&e, HttpError::Transport(t) if t.contains("endpoint policy")),
        "{e:?}"
    );
    assert_eq!(server.conns(), 0);
    assert_eq!(egress.status().counters.policy_refusals, 1);
}

/// With no monitor running and no other request, a body still being
/// read notices its route going down: it probes the route itself.
#[tokio::test]
async fn a_streamed_body_probes_its_route_without_a_monitor() {
    let mut first = open_head();
    first.extend_from_slice(b"first");
    let server = serve(move |_| Reply::ThenStall(first.clone())).await;
    let switch = Arc::new(Switch(AtomicBool::new(true)));
    let client = GuardedClient::with_route(
        NetPolicy::download_client(),
        DEADLINE * 3,
        Scripted::new(&[]),
        &Route::Direct,
    )
    .unwrap();
    let egress = PluginEgress::new(
        "p",
        RouteKind::Interface,
        Ok(client),
        Arc::clone(&switch) as Arc<dyn RouteProbe>,
        lan(),
    );
    let mut res = within(egress.fetch_stream(
        &Request::get(server.url("/")),
        stream_limits(1 << 20, DEADLINE * 3),
    ))
    .await
    .unwrap();
    assert_eq!(within(res.next_chunk()).await.unwrap().unwrap(), b"first");
    switch.0.store(false, Ordering::SeqCst);
    let e = within(res.next_chunk()).await.unwrap_err();
    assert!(
        matches!(&e, HttpError::EgressDown(t) if t.contains("went down in flight")),
        "{e:?}"
    );
    // Fused: the same answer again, without reading.
    assert_eq!(res.next_chunk().await.unwrap_err(), e);
    assert_eq!(egress.status().counters.kill_switch_aborts, 1);
}

/// A refusal while the route is down is the kill switch's answer even
/// when it takes longer than the plugin's idle wait: the host isn't
/// blamed for a stall and isn't backed off.
#[tokio::test]
async fn a_slow_refusal_is_not_a_stall() {
    let switch = Arc::new(Switch(AtomicBool::new(false)));
    let client = GuardedClient::with_route(
        NetPolicy::download_client(),
        DEADLINE,
        Scripted::new(&[]),
        &Route::Direct,
    )
    .unwrap();
    let egress: Arc<dyn Fetch> = Arc::new(PluginEgress::new(
        "p",
        RouteKind::Interface,
        Ok(client),
        Arc::clone(&switch) as Arc<dyn RouteProbe>,
        lan(),
    ));
    let client = Client::new(egress, HostLimiter::with_seed(fast_limits(), 1));
    let url = format!("http://{}/", unused_port_addr());
    let limits = StreamLimits {
        max_bytes: 1 << 20,
        timeout: DEADLINE,
        idle: Duration::from_secs(1),
    };
    // The second refusal in a row is paced for 2 s, past the 1 s idle.
    for unit in ["a", "b"] {
        let started = std::time::Instant::now();
        let e = within(client.poll_stream(unit, &Request::get(url.clone()), limits))
            .await
            .unwrap_err();
        assert!(matches!(e, HttpError::EgressDown(_)), "{unit}: {e:?}");
        if unit == "b" {
            assert!(started.elapsed() >= Duration::from_millis(1500));
        }
    }
    let host = host_key(&url).unwrap();
    assert_eq!(
        client.limiter().state(&host, Instant::now()),
        crate::http::HostState::Ready
    );
}

/// Headers that don't come within the idle wait after admission fail as
/// a stall.
#[tokio::test]
async fn headers_past_the_idle_wait_are_a_stall() {
    let server = serve(|_| Reply::Stall).await;
    let egress = factory(Scripted::new(&[]))
        .build("p", &Egress::Direct, lan())
        .await;
    let e = stream_err(
        within(egress.fetch_stream(
            &Request::get(server.url("/")),
            StreamLimits {
                max_bytes: 1 << 20,
                timeout: DEADLINE,
                idle: Duration::from_secs(1),
            },
        ))
        .await,
    );
    assert!(
        matches!(&e, HttpError::Transport(t) if t.contains("stalled")),
        "{e:?}"
    );
    assert_eq!(egress.status().counters.transport_failures, 1);
}

/// A refusal paced longer than the plugin's whole timeout still comes
/// back as the kill switch's answer, within half of it, so the kit's
/// deadline never fires first and the host is never backed off.
#[tokio::test(start_paused = true)]
async fn a_refusal_comes_back_within_the_streams_timeout() {
    let switch = Arc::new(Switch(AtomicBool::new(false)));
    let client = GuardedClient::with_route(
        NetPolicy::download_client(),
        DEADLINE,
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
    // Five refusals in a row: the next is paced 30 s.
    egress.down_streak.store(5, Ordering::Relaxed);
    let fetch: Arc<dyn Fetch> = egress;
    let client = Client::new(fetch, HostLimiter::with_seed(fast_limits(), 1));
    let url = "http://192.168.7.7/x".to_owned();
    let t0 = Instant::now();
    let e = client
        .poll_stream(
            "a",
            &Request::get(url.clone()),
            StreamLimits {
                max_bytes: 1 << 20,
                timeout: Duration::from_secs(4),
                idle: Duration::from_secs(4),
            },
        )
        .await
        .unwrap_err();
    assert!(
        matches!(&e, HttpError::EgressDown(t) if !t.contains("plugin's timeout")),
        "{e:?}"
    );
    assert_eq!(Instant::now() - t0, Duration::from_secs(2));
    assert_eq!(
        client
            .limiter()
            .state(&host_key(&url).unwrap(), Instant::now()),
        crate::http::HostState::Ready
    );
}

// ---------------------------------------------------------------- source state

/// A store of source states a test switches: `on`, or unreadable.
struct States {
    on: AtomicBool,
    broken: AtomicBool,
}

impl EgressCredentials for States {
    fn socks5<'a>(
        &'a self,
        _: &'a str,
    ) -> BoxFuture<'a, Result<Option<SocksAuth>, CredentialError>> {
        Box::pin(async { Ok(None) })
    }

    fn source_enabled<'a>(&'a self, _: &'a str) -> BoxFuture<'a, Result<bool, CredentialError>> {
        let r = if self.broken.load(Ordering::SeqCst) {
            Err(CredentialError)
        } else {
            Ok(self.on.load(Ordering::SeqCst))
        };
        Box::pin(async move { r })
    }
}

/// Defence in depth behind stopping a source when it's switched off
/// (X-tcache-recheck): a client the factory built sends nothing while the
/// store says its source is off, or can't say, whether the request is
/// buffered or streamed. Nothing connects.
#[tokio::test]
async fn a_source_switched_off_sends_nothing() {
    let server = serve(|_| ok("hello")).await;
    let states = Arc::new(States {
        on: AtomicBool::new(true),
        broken: AtomicBool::new(false),
    });
    let egress = EgressFactory::with_resolver(states.clone(), Scripted::new(&[]))
        .with_timeout(DEADLINE)
        .build("p", &Egress::Direct, lan())
        .await;
    let req = Request::get(server.url("/"));
    assert_eq!(egress.fetch(&req).await.unwrap().status, 200);
    assert_eq!(server.conns(), 1);

    states.on.store(false, Ordering::SeqCst);
    assert!(matches!(egress.fetch(&req).await, Err(HttpError::NoEgress)));
    let limits = StreamLimits {
        max_bytes: 1024,
        timeout: Duration::from_secs(10),
        idle: Duration::from_secs(5),
    };
    assert!(matches!(
        egress.fetch_stream(&req, limits).await,
        Err(HttpError::NoEgress)
    ));
    states.on.store(true, Ordering::SeqCst);
    states.broken.store(true, Ordering::SeqCst);
    assert!(matches!(egress.fetch(&req).await, Err(HttpError::NoEgress)));
    assert_eq!(server.conns(), 1, "a switched-off source connected");
    assert_eq!(egress.status().counters.policy_refusals, 3);
}

/// A database file of its own, removed when dropped.
struct TempDb(std::path::PathBuf);

impl TempDb {
    fn new(tag: &str) -> TempDb {
        static N: AtomicUsize = AtomicUsize::new(0);
        let n = N.fetch_add(1, Ordering::SeqCst);
        let dir =
            std::env::temp_dir().join(format!("lodestar-egress-{tag}-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        TempDb(dir)
    }

    fn path(&self) -> std::path::PathBuf {
        self.0.join("lodestar.db")
    }
}

impl Drop for TempDb {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// The host's wiring (X-egress-state-watch): the shared writer store
/// behind a mutex, with plugin `p` registered Enabled, and source states
/// read on a read-only connection of their own.
fn store_states(
    db: &TempDb,
) -> (
    Arc<std::sync::Mutex<lodestar_store::Store>>,
    lodestar_store::SourceId,
    crate::consent::SourceCredentials,
) {
    use lodestar_store::{SourceState, Store, StoreConfig};
    let mut writer = Store::open(db.path(), StoreConfig::default()).unwrap();
    let id = writer
        .register_source_as("p", "1", SourceState::Disabled, 0)
        .unwrap();
    writer.enable_source(id, None).unwrap();
    let reader = Store::open_reader(db.path(), StoreConfig::default()).unwrap();
    let shared = Arc::new(std::sync::Mutex::new(writer));
    let keys = Arc::new(crate::consent::SecretStore::from_key(&[5; 32]));
    let creds = crate::consent::SourceCredentials::with_keys(shared.clone(), keys)
        .reading_states_from(Arc::new(std::sync::Mutex::new(reader)));
    (shared, id, creds)
}

/// Holds `store`'s lock on a thread of its own, as a long API or Control
/// operation would, until the returned sender is dropped or sent to.
fn hold_lock(
    store: &Arc<std::sync::Mutex<lodestar_store::Store>>,
) -> (std::sync::mpsc::Sender<()>, std::thread::JoinHandle<()>) {
    let store = store.clone();
    let (held_tx, held_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
    let t = std::thread::spawn(move || {
        let _guard = store.lock().unwrap();
        held_tx.send(()).unwrap();
        let _ = release_rx.recv();
    });
    held_rx
        .recv_timeout(Duration::from_secs(10))
        .expect("the lock wasn't taken");
    (release_tx, t)
}

/// While the shared store's lock is held past `STATE_WAIT`, an enabled
/// source's requests still go out: the check reads its own connection
/// (X-egress-state-watch). The same check on the shared lock refuses,
/// which is what this slice fixes.
#[tokio::test]
async fn a_held_store_lock_does_not_hold_back_an_enabled_source() {
    let server = serve(|_| ok("hello")).await;
    let db = TempDb::new("held");
    let (shared, _, creds) = store_states(&db);
    let egress = EgressFactory::with_resolver(Arc::new(creds), Scripted::new(&[]))
        .with_timeout(DEADLINE)
        .build("p", &Egress::Direct, lan())
        .await;
    let req = Request::get(server.url("/"));

    let (release, holder) = hold_lock(&shared);
    let held_since = Instant::now();
    for _ in 0..3 {
        assert_eq!(egress.fetch(&req).await.unwrap().status, 200);
    }
    // The old wiring, the check on the shared lock, for contrast: it
    // waits out `STATE_WAIT` and refuses.
    let keys = Arc::new(crate::consent::SecretStore::from_key(&[5; 32]));
    let on_shared = EgressFactory::with_resolver(
        Arc::new(crate::consent::SourceCredentials::with_keys(
            shared.clone(),
            keys,
        )),
        Scripted::new(&[]),
    )
    .with_timeout(DEADLINE)
    .build("p", &Egress::Direct, lan())
    .await;
    assert!(matches!(
        on_shared.fetch(&req).await,
        Err(HttpError::NoEgress)
    ));
    let held = held_since.elapsed();
    assert!(held > STATE_WAIT, "the lock was held for only {held:?}");
    // Still held, and still answered.
    assert_eq!(egress.fetch(&req).await.unwrap().status, 200);
    drop(release);
    holder.join().unwrap();
    assert_eq!(server.conns(), 4);
    assert_eq!(egress.status().counters.policy_refusals, 0);
}

/// A state any writer commits on the shared store is read by the very
/// next request: off is refused at the transport and nothing connects;
/// on again sends. A missing row refuses too (X-egress-state-watch).
#[tokio::test]
async fn the_states_reader_sees_each_commit_at_once() {
    use lodestar_store::SourceState;
    let server = serve(|_| ok("hello")).await;
    let db = TempDb::new("commit");
    let (shared, id, creds) = store_states(&db);
    // Credentials are still read from the shared store.
    assert!(matches!(creds.socks5_now("p"), Ok(None)));
    let creds = Arc::new(creds);
    let egress = EgressFactory::with_resolver(creds.clone(), Scripted::new(&[]))
        .with_timeout(DEADLINE)
        .build("p", &Egress::Direct, lan())
        .await;
    let req = Request::get(server.url("/"));
    assert_eq!(egress.fetch(&req).await.unwrap().status, 200);

    for state in [
        SourceState::Disabled,
        SourceState::Paused,
        SourceState::Purged,
    ] {
        shared.lock().unwrap().set_source_state(id, state).unwrap();
        assert!(
            matches!(egress.fetch(&req).await, Err(HttpError::NoEgress)),
            "{state:?}"
        );
        shared.lock().unwrap().enable_source(id, None).unwrap();
        assert_eq!(egress.fetch(&req).await.unwrap().status, 200, "{state:?}");
    }
    assert_eq!(server.conns(), 4, "a switched-off source connected");

    // No row for the plugin: refused.
    let nobody = EgressFactory::with_resolver(creds, Scripted::new(&[]))
        .with_timeout(DEADLINE)
        .build("nobody", &Egress::Direct, lan())
        .await;
    assert!(matches!(nobody.fetch(&req).await, Err(HttpError::NoEgress)));
    assert_eq!(server.conns(), 4);
    assert_eq!(egress.status().counters.policy_refusals, 3);
    assert_eq!(nobody.status().counters.policy_refusals, 1);
}

/// A states connection that can't be read refuses every request
/// (X-egress-state-watch keeps #525's fail-closed rule): a row the
/// reader can't decode, then a reader whose lock is poisoned.
#[tokio::test]
async fn an_unreadable_states_reader_sends_nothing() {
    use lodestar_store::{Store, StoreConfig};
    let server = serve(|_| ok("hello")).await;
    let db = TempDb::new("broken");
    let (shared, id, creds) = store_states(&db);
    let egress = EgressFactory::with_resolver(Arc::new(creds), Scripted::new(&[]))
        .with_timeout(DEADLINE)
        .build("p", &Egress::Direct, lan())
        .await;
    let req = Request::get(server.url("/"));

    // A state no build knows: the reader's read fails, not just a lock.
    shared
        .lock()
        .unwrap()
        .connection()
        .execute(
            "UPDATE sources SET state = 99 WHERE id = ?1",
            [i64::from(id.get())],
        )
        .unwrap();
    assert!(matches!(egress.fetch(&req).await, Err(HttpError::NoEgress)));
    assert_eq!(server.conns(), 0);
    assert_eq!(egress.status().counters.policy_refusals, 1);

    // A reader poisoned by a panic while it was held can't be read.
    let reader = Arc::new(std::sync::Mutex::new(
        Store::open_reader(db.path(), StoreConfig::default()).unwrap(),
    ));
    let poison = reader.clone();
    let _ = std::thread::spawn(move || {
        let _g = poison.lock().unwrap();
        panic!("poisons the states reader");
    })
    .join();
    assert!(reader.is_poisoned());
    let keys = Arc::new(crate::consent::SecretStore::from_key(&[5; 32]));
    let creds =
        crate::consent::SourceCredentials::with_keys(shared, keys).reading_states_from(reader);
    let poisoned = EgressFactory::with_resolver(Arc::new(creds), Scripted::new(&[]))
        .with_timeout(DEADLINE)
        .build("p", &Egress::Direct, lan())
        .await;
    assert!(matches!(
        poisoned.fetch(&req).await,
        Err(HttpError::NoEgress)
    ));
    assert_eq!(server.conns(), 0);
    assert_eq!(poisoned.status().counters.policy_refusals, 1);
}
