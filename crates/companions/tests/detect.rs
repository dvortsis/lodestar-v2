//! CMP-8a: detection and health against recorded answers on a local
//! server, the SSRF guard and Q12's fixed probe set, and hostile answers.

use std::collections::HashMap;
use std::io;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU16, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use lodestar_companions::adapter::{
    AdapterError, BoxFuture, Detected, EnteredUrl, GuardedHttp, HealthStatus, Limits, ProbeRequest,
    ProbeResponse, RequestPath, Target,
};
use lodestar_companions::adapter::{HeaderName, HeaderValue};
use lodestar_companions::detect::{self, CompanionHttp, Runtime};
use lodestar_companions::inputs::{ContainerName, PortNumber};
use lodestar_companions::{CompanionId, builtin};
use lodestar_integrations::BoxFuture as NetFuture;
use lodestar_integrations::net::Resolve;
use lodestar_integrations::secret::Secret;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

// ─── A local server ─────────────────────────────────────────────────────

/// How the server answers one request.
enum Reply {
    Full {
        status: u16,
        headers: Vec<(String, String)>,
        body: Vec<u8>,
    },
    /// These bytes, then close.
    Raw(Vec<u8>),
    /// A head, then one body byte every 50 ms until the client leaves.
    Drip(Vec<u8>),
    /// Read the request and say nothing.
    Silent,
}

fn reply(status: u16, body: &[u8]) -> Reply {
    Reply::Full {
        status,
        headers: Vec::new(),
        body: body.to_vec(),
    }
}

/// One request as the server saw it.
#[derive(Debug, Clone)]
struct Seen {
    method: String,
    path: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

type Handler = Arc<dyn Fn(&str) -> Reply + Send + Sync>;

struct Server {
    addr: SocketAddr,
    seen: Arc<Mutex<Vec<Seen>>>,
}

impl Server {
    async fn start(handler: impl Fn(&str) -> Reply + Send + Sync + 'static) -> Server {
        Server::on(IpAddr::V4(Ipv4Addr::LOCALHOST), Arc::new(handler))
            .await
            .unwrap()
    }

    async fn on(ip: IpAddr, handler: Handler) -> io::Result<Server> {
        Server::serve(TcpListener::bind((ip, 0)).await?, handler)
    }

    fn serve(listener: TcpListener, handler: Handler) -> io::Result<Server> {
        let addr = listener.local_addr()?;
        let seen = Arc::new(Mutex::new(Vec::new()));
        let log = Arc::clone(&seen);
        tokio::spawn(async move {
            while let Ok((sock, _)) = listener.accept().await {
                tokio::spawn(serve(sock, Arc::clone(&handler), Arc::clone(&log)));
            }
        });
        Ok(Server { addr, seen })
    }

    fn port(&self) -> PortNumber {
        PortNumber::new(self.addr.port()).unwrap()
    }

    fn loopback(&self) -> Target {
        Target::Loopback(self.port())
    }

    fn entered(&self) -> Target {
        Target::Entered(EnteredUrl::parse(&format!("http://{}", self.addr)).unwrap())
    }

    fn paths(&self) -> Vec<String> {
        self.seen
            .lock()
            .unwrap()
            .iter()
            .map(|s| s.path.clone())
            .collect()
    }

    fn seen(&self) -> Vec<Seen> {
        self.seen.lock().unwrap().clone()
    }
}

async fn serve(mut sock: TcpStream, handler: Handler, log: Arc<Mutex<Vec<Seen>>>) {
    let mut head = Vec::new();
    let mut buf = [0u8; 1024];
    while !head.windows(4).any(|w| w == b"\r\n\r\n") {
        match sock.read(&mut buf).await {
            Ok(0) | Err(_) => return,
            Ok(n) => head.extend_from_slice(&buf[..n]),
        }
        if head.len() > 16 * 1024 {
            return;
        }
    }
    let end = head.windows(4).position(|w| w == b"\r\n\r\n").unwrap_or(0) + 4;
    let mut body = head.split_off(end);
    let text = String::from_utf8_lossy(&head).into_owned();
    let mut lines = text.split("\r\n");
    let mut request_line = lines.next().unwrap_or("").split(' ');
    let method = request_line.next().unwrap_or("").to_owned();
    let path = request_line.next().unwrap_or("").to_owned();
    let headers: Vec<(String, String)> = lines
        .take_while(|l| !l.is_empty())
        .filter_map(|l| l.split_once(':'))
        .map(|(n, v)| (n.trim().to_ascii_lowercase(), v.trim().to_owned()))
        .collect();
    let length = headers
        .iter()
        .find(|(n, _)| n == "content-length")
        .and_then(|(_, v)| v.parse::<usize>().ok())
        .unwrap_or(0)
        .min(64 * 1024);
    while body.len() < length {
        match sock.read(&mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(n) => body.extend_from_slice(&buf[..n]),
        }
    }
    log.lock().unwrap().push(Seen {
        method,
        path: path.clone(),
        headers,
        body,
    });
    match handler(&path) {
        Reply::Full {
            status,
            headers,
            body,
        } => {
            let mut out = format!("HTTP/1.1 {status} X\r\n");
            for (n, v) in headers {
                out.push_str(&format!("{n}: {v}\r\n"));
            }
            out.push_str(&format!(
                "Content-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            ));
            let mut bytes = out.into_bytes();
            bytes.extend_from_slice(&body);
            let _ = sock.write_all(&bytes).await;
        }
        Reply::Raw(bytes) => {
            let _ = sock.write_all(&bytes).await;
        }
        Reply::Drip(head) => {
            if sock.write_all(&head).await.is_err() {
                return;
            }
            // Paces the writer only; the test waits on the client's
            // deadline, not on this.
            let mut pace = tokio::time::interval(Duration::from_millis(50));
            loop {
                pace.tick().await;
                if sock.write_all(b"a").await.is_err() {
                    break;
                }
            }
        }
        Reply::Silent => {
            let _ = sock.read(&mut buf).await;
        }
    }
    let _ = sock.shutdown().await;
}

// ─── Recorded fixtures ──────────────────────────────────────────────────

struct Exchange {
    path: String,
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

fn fixture_dir(companion: &str, case: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures")
        .join(companion)
        .join(case)
}

fn load(companion: &str, case: &str) -> Vec<Exchange> {
    let dir = fixture_dir(companion, case);
    let mut out = Vec::new();
    for n in 1.. {
        let head = dir.join(format!("{n:03}.http"));
        let Ok(text) = std::fs::read_to_string(&head) else {
            break;
        };
        let mut lines = text.lines();
        let path = lines
            .next()
            .and_then(|l| l.strip_prefix("GET "))
            .unwrap()
            .to_owned();
        assert_eq!(lines.next(), Some(""), "{}", head.display());
        let status = lines
            .next()
            .and_then(|l| l.strip_prefix("HTTP "))
            .unwrap()
            .parse()
            .unwrap();
        let headers = lines
            .filter_map(|l| l.split_once(": "))
            .map(|(a, b)| (a.to_owned(), b.to_owned()))
            .collect();
        let body = std::fs::read(dir.join(format!("{n:03}.body"))).unwrap_or_default();
        out.push(Exchange {
            path,
            status,
            headers,
            body,
        });
    }
    assert!(!out.is_empty(), "no fixture at {}", dir.display());
    out
}

async fn replay(companion: &str, case: &str) -> (Server, Vec<String>) {
    let exchanges = load(companion, case);
    let paths = exchanges.iter().map(|e| e.path.clone()).collect();
    let by_path: HashMap<String, Exchange> =
        exchanges.into_iter().map(|e| (e.path.clone(), e)).collect();
    let server = Server::start(move |path| match by_path.get(path) {
        Some(e) => Reply::Full {
            status: e.status,
            headers: e.headers.clone(),
            body: e.body.clone(),
        },
        None => reply(599, b"no recording for this path"),
    })
    .await;
    (server, paths)
}

fn id(s: &str) -> CompanionId {
    builtin().unwrap().get(s).unwrap().id.clone()
}

fn http() -> CompanionHttp {
    CompanionHttp::new().unwrap()
}

/// What a fixture case should give.
enum Want {
    Found(Option<&'static str>),
    Absent,
}

#[tokio::test]
async fn recorded_answers() {
    use HealthStatus::{Healthy, Unhealthy};
    use Want::{Absent, Found};
    let cases: &[(&str, &str, Want, HealthStatus)] = &[
        ("qbittorrent", "found_bypass", Found(Some("5.0.3")), Healthy),
        ("qbittorrent", "auth_required", Found(None), Healthy),
        ("qbittorrent", "auth_required_v4", Found(None), Healthy),
        ("qbittorrent", "wrong_service", Absent, Unhealthy),
        // Health is asked only of a target detection found; a 403
        // `Forbidden` is how qBittorrent answers without a session.
        ("qbittorrent", "wrong_service_403", Absent, Healthy),
        ("prowlarr", "found", Found(None), Healthy),
        ("prowlarr", "database_error", Found(None), Unhealthy),
        // Sonarr's `/ping` is Prowlarr's; only the login page differs.
        ("prowlarr", "wrong_service_sonarr", Absent, Healthy),
        ("prowlarr", "auth_required_proxy", Absent, Unhealthy),
        ("gluetun", "found_open", Found(None), Healthy),
        ("gluetun", "vpn_stopped", Found(None), Unhealthy),
        ("gluetun", "auth_required", Found(None), Unhealthy),
        ("gluetun", "wrong_service", Absent, Unhealthy),
        // A released gluetun with authentication turned on for every
        // route answers like `wrong_service`: a documented false negative.
        ("gluetun", "release_auth_on", Absent, Unhealthy),
    ];
    let http = http();
    for (companion, case, want, health) in cases {
        let (server, paths) = replay(companion, case).await;
        let target = server.loopback();
        let detector = detect::detector(&id(companion)).unwrap();
        let got = detector.detect(&http, &target).await;
        match want {
            Found(version) => {
                let d = got.unwrap().unwrap_or_else(|| panic!("{companion}/{case}"));
                assert_eq!(d.target, target, "{companion}/{case}");
                assert_eq!(
                    d.version.as_ref().map(|v| v.as_str()),
                    *version,
                    "{companion}/{case}"
                );
            }
            Absent => assert_eq!(got, Ok(None), "{companion}/{case}"),
        }
        assert_eq!(server.paths(), paths, "{companion}/{case}: paths asked");
        let checker = detect::health(&id(companion)).unwrap();
        assert_eq!(
            checker.check(&http, &target).await,
            *health,
            "{companion}/{case}: health"
        );
        // An entered URL to the same server gives the same answer.
        let entered = server.entered();
        assert_eq!(
            detector.detect(&http, &entered).await.unwrap().is_some(),
            matches!(want, Found(_)),
            "{companion}/{case}: entered"
        );
        // Nothing identifying, and never a credential, in detection.
        for s in server.seen() {
            for (name, value) in &s.headers {
                assert!(
                    !matches!(
                        name.as_str(),
                        "authorization" | "cookie" | "x-api-key" | "referer" | "origin"
                    ),
                    "{companion}/{case}: sent {name}"
                );
                if name == "user-agent" {
                    assert_eq!(value, "Lodestar-Companions");
                }
            }
            assert!(
                !s.path.contains("initialize") && !s.path.contains("publicip"),
                "{}",
                s.path
            );
        }
    }
}

/// Whether `e` means the runner has no IPv6 loopback: no such address,
/// or no IPv6 sockets at all (EAFNOSUPPORT).
fn no_ipv6(e: &io::Error) -> bool {
    const EAFNOSUPPORT: i32 = if cfg!(windows) {
        10047
    } else if cfg!(target_os = "linux") {
        97
    } else {
        47 // macOS and the BSDs
    };
    matches!(
        e.kind(),
        io::ErrorKind::AddrNotAvailable | io::ErrorKind::Unsupported
    ) || e.raw_os_error() == Some(EAFNOSUPPORT)
}

/// Whether a bind failed because the port can't be had: taken, or on
/// Windows reserved (an excluded range answers `PermissionDenied`).
fn in_use(e: &io::Error) -> bool {
    matches!(
        e.kind(),
        io::ErrorKind::AddrInUse | io::ErrorKind::PermissionDenied
    )
}

/// The lowest ephemeral port on any target: Linux hands out 32768 and up
/// by default, macOS and Windows 49152 and up.
const EPHEMERAL_FLOOR: u16 = 32768;

/// A loopback port that belongs to this binary's tests: free on 127.0.0.1
/// when it's picked, and on ::1 too where `v6` is set (the runner has IPv6
/// loopback).
struct OwnPort {
    port: u16,
    v6: bool,
}

/// Picks a port no other socket in this binary can be handed. It comes from
/// below every target's ephemeral range, so `bind(.., 0)` never returns it,
/// and each one is handed out once. So a test can leave it unbound and it
/// stays the test's: a closed port is an unbound one. A port that's bound
/// but not listening won't do for one: macOS (as the BSDs) drops a
/// connection to it instead of refusing it, so the connection times out.
/// The range starts at a place set by the process id, so two test runs on
/// one machine rarely meet; a port another program holds is skipped.
fn own_port() -> OwnPort {
    static NEXT: OnceLock<AtomicU16> = OnceLock::new();
    let next = NEXT.get_or_init(|| AtomicU16::new(20_000 + (std::process::id() % 40) as u16 * 250));
    loop {
        let port = next.fetch_add(1, Ordering::Relaxed);
        assert!(port < EPHEMERAL_FLOOR, "out of this binary's own ports");
        match std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, port)) {
            Ok(_) => {}
            Err(e) if in_use(&e) => continue,
            Err(e) => panic!("binding 127.0.0.1:{port}: {e}"),
        }
        match std::net::TcpListener::bind((Ipv6Addr::LOCALHOST, port)) {
            Ok(_) => return OwnPort { port, v6: true },
            Err(e) if in_use(&e) => continue,
            Err(e) if no_ipv6(&e) => return OwnPort { port, v6: false },
            Err(e) => panic!("binding [::1]:{port}: {e}"),
        }
    }
}

/// A loopback port that nothing listens on, on 127.0.0.1 or ::1, and that
/// nothing in this binary can be handed while the test uses it
/// ([`own_port`]). A connection to it is refused on every target.
struct ClosedPort {
    port: u16,
}

impl ClosedPort {
    fn new() -> ClosedPort {
        ClosedPort {
            port: own_port().port,
        }
    }

    fn v4(&self) -> SocketAddr {
        (Ipv4Addr::LOCALHOST, self.port).into()
    }
}

/// What the tests below rely on: a closed port refuses connections, on
/// ::1 too where the runner has it, and isn't one the system hands out.
#[tokio::test]
async fn a_closed_port_refuses_and_is_never_handed_out() {
    let closed = ClosedPort::new();
    let refused = TcpStream::connect(closed.v4()).await.unwrap_err();
    assert_eq!(
        refused.kind(),
        io::ErrorKind::ConnectionRefused,
        "{refused}"
    );
    if let Err(e) = TcpStream::connect((Ipv6Addr::LOCALHOST, closed.port)).await {
        assert!(
            e.kind() == io::ErrorKind::ConnectionRefused || no_ipv6(&e),
            "[::1]:{}: {e}",
            closed.port
        );
    }
    assert!(closed.port < EPHEMERAL_FLOOR, "{}", closed.port);
    assert_ne!(
        ClosedPort::new().port,
        closed.port,
        "each is handed out once"
    );
}

#[tokio::test]
async fn nothing_listening_is_not_found_and_unreachable() {
    let closed = ClosedPort::new();
    let addr = closed.v4();
    let target = Target::Entered(EnteredUrl::parse(&format!("http://{addr}")).unwrap());
    let http = http();
    for c in ["qbittorrent", "prowlarr", "gluetun"] {
        // On Windows a refused connection takes about 2 s, so a loaded
        // runner may reach the probe's 5 s deadline first. At an entered
        // URL that's a timeout, which is a valid answer here.
        let got = detect::detector(&id(c))
            .unwrap()
            .detect(&http, &target)
            .await;
        assert!(
            matches!(got, Ok(None) | Err(AdapterError::Timeout)),
            "{c}: {got:?}"
        );
        assert_eq!(
            detect::health(&id(c)).unwrap().check(&http, &target).await,
            HealthStatus::Unreachable,
            "{c}"
        );
    }
}

#[tokio::test]
async fn nothing_on_a_loopback_port_is_not_found() {
    let closed = ClosedPort::new();
    let port = PortNumber::new(closed.port).unwrap();
    let http = http();
    for c in ["qbittorrent", "prowlarr", "gluetun"] {
        let got = detect::detector(&id(c))
            .unwrap()
            .detect(&http, &Target::Loopback(port))
            .await;
        assert_eq!(got, Ok(None), "{c}");
    }
}

/// A loopback probe whose IPv4 attempt is refused answers from ::1. On a
/// machine without IPv6 loopback only the refusal is checked, and on CI
/// that machine fails the test: the fallback is never passed untested.
#[tokio::test]
async fn loopback_falls_back_to_ipv6() {
    let version: Handler = Arc::new(|_: &str| reply(200, b"v5.0.3"));
    let limits = |secs| Limits::new(Duration::from_secs(secs), 1024).unwrap();
    let version_path = || RequestPath::parse("/api/v2/app/version").unwrap();
    let Some((server, _)) = ipv6_with_ipv4_closed(version).await else {
        // CI runners have IPv6 loopback: there (`CI` set to any value), this
        // is a failure, never a pass of the fallback.
        assert!(
            std::env::var_os("CI").is_none(),
            "::1 can't be bound here, so the IPv6 fallback is untested"
        );
        // Without ::1 a held port can only be found closed: check that.
        let closed = ClosedPort::new();
        let target = Target::Loopback(PortNumber::new(closed.port).unwrap());
        let got = http()
            .send(ProbeRequest::new(&target, version_path(), limits(15)))
            .await;
        assert!(
            matches!(got, Err(AdapterError::Unreachable)),
            "{:?}",
            got.map(|r| r.status())
        );
        eprintln!("::1 unavailable here: the IPv6 fallback wasn't exercised");
        return;
    };
    // The IPv4 attempt is refused first (about 2 s on Windows), so this
    // uses a 15 s deadline rather than the probe's 5 s.
    let got = http()
        .send(ProbeRequest::new(
            &server.loopback(),
            version_path(),
            limits(15),
        ))
        .await;
    let r = got.unwrap();
    assert_eq!((r.status(), r.body()), (200, &b"v5.0.3"[..]));
    assert_eq!(server.paths(), ["/api/v2/app/version"]);
}

/// A request that carries anything (a header, so a key or a cookie, or a
/// body) goes only to the loopback address a bare probe found the companion
/// on. Before any probe that is 127.0.0.1 alone; a companion a probe found
/// on ::1 gets its requests there.
#[tokio::test]
async fn only_a_bare_probe_looks_on_ipv6_loopback() {
    let version: Handler = Arc::new(|_: &str| reply(200, b"v5.0.3"));
    let limits = || Limits::new(Duration::from_secs(15), 1024).unwrap();
    let path = || RequestPath::parse("/api/v2/app/version").unwrap();
    let Some((server, _)) = ipv6_with_ipv4_closed(version).await else {
        assert!(
            std::env::var_os("CI").is_none(),
            "::1 can't be bound here, so the loopback rule is untested"
        );
        eprintln!("::1 unavailable here: the loopback rule wasn't exercised");
        return;
    };
    let target = server.loopback();
    let key = HeaderName::new("X-Api-Key").unwrap();
    let cookie = HeaderName::new("Cookie").unwrap();
    let carrying = |target| {
        [
            ProbeRequest::new(target, path(), limits())
                .header(key, HeaderValue::secret(Secret::new("k")).unwrap()),
            ProbeRequest::new(target, path(), limits())
                .header(cookie, HeaderValue::plain("SID=abc").unwrap()),
            ProbeRequest::new(target, path(), limits()).post(b"password=x".to_vec()),
            ProbeRequest::new(target, path(), limits()).put(b"{}".to_vec()),
        ]
    };
    // Nothing has probed this port: each stops at 127.0.0.1.
    let client = http();
    for req in carrying(&target) {
        let got = client.send(req).await;
        assert!(
            matches!(got, Err(AdapterError::Unreachable)),
            "{:?}",
            got.map(|r| r.status())
        );
    }
    assert!(server.seen().is_empty(), "nothing reached ::1");
    // A bare probe finds the companion on ::1, and from then on the same
    // client sends this port's requests there.
    let got = client
        .send(ProbeRequest::new(&target, path(), limits()))
        .await
        .unwrap();
    assert_eq!(got.status(), 200);
    for req in carrying(&target) {
        assert_eq!(client.send(req).await.unwrap().status(), 200);
    }
    assert_eq!(server.seen().len(), 5);
    // Another client has found nothing yet.
    let got = http()
        .send(ProbeRequest::new(&target, path(), limits()).post(b"x".to_vec()))
        .await;
    assert!(matches!(got, Err(AdapterError::Unreachable)));
    assert_eq!(server.seen().len(), 5);
}

/// Both loopback addresses of one of this binary's ports ([`own_port`]):
/// `::1` served by `other`, 127.0.0.1 listening and handed back for the
/// test to serve. `None` where ::1 can't be bound.
async fn both_loopbacks(other: Handler) -> Option<(TcpListener, Server)> {
    let OwnPort { port, v6 } = own_port();
    if !v6 {
        return None;
    }
    let v4 = TcpListener::bind((Ipv4Addr::LOCALHOST, port))
        .await
        .unwrap();
    let v6 = TcpListener::bind((Ipv6Addr::LOCALHOST, port))
        .await
        .unwrap();
    Some((v4, Server::serve(v6, other).unwrap()))
}

/// Once 127.0.0.1 has answered on a port, nothing for that port goes to
/// ::1, a bare probe included, even when 127.0.0.1 has stopped listening
/// and something still listens on ::1.
#[tokio::test]
async fn ipv4_loopback_keeps_the_port_once_it_has_answered() {
    let limits = || Limits::new(Duration::from_secs(15), 1024).unwrap();
    let path = || RequestPath::parse("/api/v2/app/version").unwrap();
    let other: Handler = Arc::new(|_: &str| reply(200, b"other"));
    let Some((v4, v6)) = both_loopbacks(other).await else {
        assert!(
            std::env::var_os("CI").is_none(),
            "::1 can't be bound here, so the loopback rule is untested"
        );
        eprintln!("::1 unavailable here: the loopback rule wasn't exercised");
        return;
    };
    let target = v6.loopback();
    let port = target_port(&v6);
    // 127.0.0.1 answers one connection and stops listening.
    let first: Handler = Arc::new(|_: &str| reply(200, b"first"));
    let served = tokio::spawn(async move {
        let (sock, _) = v4.accept().await.unwrap();
        drop(v4);
        serve(sock, first, Arc::new(Mutex::new(Vec::new()))).await;
    });
    let client = http();
    let got = client
        .send(ProbeRequest::new(&target, path(), limits()))
        .await
        .unwrap();
    assert_eq!(got.body(), b"first");
    served.await.unwrap();
    // Closed from here, and nothing else can be handed it ([`own_port`]).
    wait_closed(port).await;
    for req in [
        ProbeRequest::new(&target, path(), limits()),
        ProbeRequest::new(&target, path(), limits()).post(b"password=x".to_vec()),
    ] {
        let got = client.send(req).await;
        assert!(
            matches!(got, Err(AdapterError::Unreachable)),
            "{:?}",
            got.map(|r| r.status())
        );
    }
    assert!(v6.seen().is_empty(), "nothing reached ::1");
}

fn target_port(server: &Server) -> u16 {
    server.addr.port()
}

/// Waits until connections to 127.0.0.1:`port` are refused, once the
/// listener that had it is gone. The port is one of this binary's own
/// ([`own_port`]), so nothing else is handed it meanwhile; it isn't bound
/// to hold it, because a port bound and not listening drops connections on
/// macOS instead of refusing them.
async fn wait_closed(port: u16) {
    let deadline = Instant::now() + Duration::from_secs(20);
    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    loop {
        match TcpStream::connect(addr).await {
            Err(e) if e.kind() == io::ErrorKind::ConnectionRefused => return,
            other => {
                assert!(
                    Instant::now() < deadline,
                    "127.0.0.1:{port} still answers: {:?}",
                    other.map(drop)
                );
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        }
    }
}

/// A port is closed once its listener is gone, on a machine with no IPv6
/// too: the helper the test above rests on, run everywhere.
#[tokio::test]
async fn a_port_is_closed_once_its_listener_is_gone() {
    let port = own_port().port;
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, port))
        .await
        .unwrap();
    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let mut conn = TcpStream::connect(addr).await.unwrap();
    let (mut sock, _) = listener.accept().await.unwrap();
    drop(listener);
    sock.write_all(b"x").await.unwrap();
    drop(sock);
    let mut got = Vec::new();
    conn.read_to_end(&mut got).await.unwrap();
    drop(conn);
    wait_closed(port).await;
    let again = TcpStream::connect(addr).await;
    assert_eq!(
        again.err().map(|e| e.kind()),
        Some(io::ErrorKind::ConnectionRefused)
    );
}

/// A port a bare probe found on ::1 isn't kept there for good: once
/// 127.0.0.1 listens, the next request finds it, whatever it carries, and
/// from then on the port's requests go to 127.0.0.1 alone.
#[tokio::test]
async fn ipv4_loopback_takes_a_port_found_on_ipv6() {
    let limits = || Limits::new(Duration::from_secs(15), 1024).unwrap();
    let path = || RequestPath::parse("/api/v2/app/version").unwrap();
    let other: Handler = Arc::new(|_: &str| reply(200, b"other"));
    let Some((v6, port)) = ipv6_with_ipv4_closed(other).await else {
        assert!(
            std::env::var_os("CI").is_none(),
            "::1 can't be bound here, so the loopback rule is untested"
        );
        eprintln!("::1 unavailable here: the loopback rule wasn't exercised");
        return;
    };
    let target = v6.loopback();
    let client = http();
    let carrying = || ProbeRequest::new(&target, path(), limits()).post(b"password=x".to_vec());
    // Nothing on 127.0.0.1 yet: a bare probe finds ::1, and what carries
    // something follows it there.
    let got = client
        .send(ProbeRequest::new(&target, path(), limits()))
        .await
        .unwrap();
    assert_eq!(got.body(), b"other");
    assert_eq!(client.send(carrying()).await.unwrap().body(), b"other");
    assert_eq!(v6.seen().len(), 2);
    // 127.0.0.1 starts listening on the port, which was this test's all
    // along ([`own_port`]). The very next request finds it, though it
    // carries something.
    let first: Handler = Arc::new(|_: &str| reply(200, b"first"));
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, port))
        .await
        .unwrap();
    let v4 = Server::serve(listener, first).unwrap();
    assert_eq!(client.send(carrying()).await.unwrap().body(), b"first");
    // From here everything goes to 127.0.0.1.
    let got = client
        .send(ProbeRequest::new(&target, path(), limits()))
        .await
        .unwrap();
    assert_eq!(got.body(), b"first");
    assert_eq!(client.send(carrying()).await.unwrap().body(), b"first");
    assert_eq!((v6.seen().len(), v4.seen().len()), (2, 3));
}

/// A server on ::1, with 127.0.0.1 closed on its port, so the probe's IPv4
/// attempt is always refused and never answered by another program: the
/// port is one of this binary's own ([`own_port`]). Returns the port too.
/// `None` where ::1 can't be bound.
async fn ipv6_with_ipv4_closed(handler: Handler) -> Option<(Server, u16)> {
    let OwnPort { port, v6 } = own_port();
    if !v6 {
        return None;
    }
    let listener = TcpListener::bind((Ipv6Addr::LOCALHOST, port))
        .await
        .unwrap();
    Some((Server::serve(listener, handler).unwrap(), port))
}

#[tokio::test]
async fn request_headers_and_bodies_arrive_as_built() {
    let server = Server::start(|_| reply(200, b"Ok.")).await;
    let target = server.entered();
    let key = HeaderName::new("X-Api-Key").unwrap();
    let kind = HeaderName::new("Content-Type").unwrap();
    let req = ProbeRequest::new(
        &target,
        RequestPath::parse("/api/v2/app/setPreferences").unwrap(),
        Limits::PROBE,
    )
    .post(b"json={\"a\":1}".to_vec())
    .header(key, HeaderValue::secret(Secret::new("hunter2")).unwrap())
    .header(
        kind,
        HeaderValue::plain("application/x-www-form-urlencoded").unwrap(),
    );
    let r = http().send(req).await.unwrap();
    assert_eq!((r.status(), r.body()), (200, &b"Ok."[..]));
    let seen = server.seen();
    assert_eq!(seen.len(), 1);
    let s = &seen[0];
    assert_eq!(
        (s.method.as_str(), s.path.as_str()),
        ("POST", "/api/v2/app/setPreferences")
    );
    let header = |n: &str| {
        s.headers
            .iter()
            .find(|(k, _)| k == n)
            .map(|(_, v)| v.as_str())
    };
    assert_eq!(header("x-api-key"), Some("hunter2"));
    assert_eq!(
        header("content-type"),
        Some("application/x-www-form-urlencoded")
    );
    assert_eq!(header("user-agent"), Some("Lodestar-Companions"));
    assert_eq!(s.body, b"json={\"a\":1}");
}

// ─── Q12: the fixed probe set ───────────────────────────────────────────

#[test]
fn auto_detect_probes_exactly_the_fixed_list() {
    let c = builtin().unwrap();
    let got: Vec<(String, Target)> = detect::auto_probes(c, Runtime::Container)
        .into_iter()
        .map(|(id, t)| (id.as_str().to_owned(), t))
        .collect();
    let lo = |p| Target::Loopback(PortNumber::new(p).unwrap());
    let ct = |n, p| {
        Target::Container(
            ContainerName::parse(n).unwrap(),
            PortNumber::new(p).unwrap(),
        )
    };
    let want = vec![
        ("qbittorrent".to_owned(), lo(8080)),
        ("qbittorrent".to_owned(), ct("qbittorrent", 8080)),
        ("prowlarr".to_owned(), lo(9696)),
        ("prowlarr".to_owned(), ct("prowlarr", 9696)),
        ("gluetun".to_owned(), lo(8000)),
        ("gluetun".to_owned(), ct("gluetun", 8000)),
    ];
    assert_eq!(got, want);
    // A native install probes loopback only: a single-label name outside
    // Docker would be asked of the LAN's broadcast name services.
    let native: Vec<Target> = detect::auto_probes(c, Runtime::Native)
        .into_iter()
        .map(|(_, t)| t)
        .collect();
    assert_eq!(native, vec![lo(8080), lo(9696), lo(8000)]);
}

/// Records every request; nothing answers.
#[derive(Default)]
struct Recorder(Mutex<Vec<(Target, String)>>);

impl GuardedHttp for Recorder {
    fn send<'a>(
        &'a self,
        request: ProbeRequest<'a>,
    ) -> BoxFuture<'a, Result<ProbeResponse, AdapterError>> {
        self.0
            .lock()
            .unwrap()
            .push((request.target.clone(), request.path.as_str().to_owned()));
        Box::pin(async { Err(AdapterError::Unreachable) })
    }
}

#[tokio::test]
async fn auto_detect_sends_only_to_the_fixed_list() {
    let c = builtin().unwrap();
    let rec = Recorder::default();
    assert!(
        detect::auto_detect(&rec, c, Runtime::Container)
            .await
            .is_empty()
    );
    let fixed: Vec<Target> = detect::auto_probes(c, Runtime::Container)
        .into_iter()
        .map(|(_, t)| t)
        .collect();
    let sent = rec.0.lock().unwrap();
    assert_eq!(
        sent.iter().map(|(t, _)| t.clone()).collect::<Vec<_>>(),
        fixed
    );
    assert!(sent.iter().all(|(t, _)| !matches!(t, Target::Entered(_))));
}

// ─── The SSRF guard ─────────────────────────────────────────────────────

/// Answers from a table; any other name fails to resolve.
struct Table(Vec<(&'static str, IpAddr)>);

impl Resolve for Table {
    fn resolve<'a>(&'a self, host: &'a str) -> NetFuture<'a, io::Result<Vec<IpAddr>>> {
        let got: Vec<IpAddr> = self
            .0
            .iter()
            .filter(|(h, _)| *h == host)
            .map(|(_, ip)| *ip)
            .collect();
        Box::pin(async move {
            if got.is_empty() {
                Err(io::Error::other("no such name"))
            } else {
                Ok(got)
            }
        })
    }
}

#[tokio::test]
async fn entered_link_local_and_metadata_are_refused_before_connecting() {
    // A live server behind a name that also resolves to link-local: the
    // whole answer is refused, so the server must see nothing.
    let live = Server::start(|_| reply(200, b"v5.0.3")).await;
    let http = CompanionHttp::with_resolver(Arc::new(Table(vec![
        ("metadata.lan", "169.254.169.254".parse().unwrap()),
        ("mixed.lan", "127.0.0.1".parse().unwrap()),
        ("mixed.lan", "fe80::1".parse().unwrap()),
    ])))
    .unwrap();
    let mixed = format!("http://mixed.lan:{}", live.addr.port());
    for url in [
        "http://169.254.169.254/",
        "http://169.254.169.254:8080/latest",
        "http://[fe80::1]:8080",
        "http://[::ffff:169.254.169.254]/",
        "http://[fd00:ec2::254]/",
        "http://100.100.100.200/",
        "http://0.0.0.0:8080",
        "http://metadata.lan:9696",
        mixed.as_str(),
    ] {
        let target = Target::Entered(EnteredUrl::parse(url).unwrap());
        for c in ["qbittorrent", "prowlarr", "gluetun"] {
            assert_eq!(
                detect::detector(&id(c))
                    .unwrap()
                    .detect(&http, &target)
                    .await,
                Err(AdapterError::Blocked),
                "{c} {url}"
            );
            assert_eq!(
                detect::health(&id(c)).unwrap().check(&http, &target).await,
                HealthStatus::Unreachable
            );
        }
    }
    assert!(live.paths().is_empty(), "a refused name was connected to");
}

#[tokio::test]
async fn container_names_resolve_only_to_local_addresses() {
    let server = Server::start(|_| reply(200, b"v5.0.3")).await;
    let port = server.port();
    let http = CompanionHttp::with_resolver(Arc::new(Table(vec![
        ("qbittorrent", "127.0.0.1".parse().unwrap()),
        ("prowlarr", "8.8.8.8".parse().unwrap()),
        ("gluetun", "169.254.169.254".parse().unwrap()),
    ])))
    .unwrap();
    let ct = |n| Target::Container(ContainerName::parse(n).unwrap(), port);
    let q = detect::detector(&id("qbittorrent")).unwrap();
    let found = q.detect(&http, &ct("qbittorrent")).await.unwrap().unwrap();
    assert_eq!(found.version.unwrap().as_str(), "5.0.3");
    // A container name that resolves anywhere but Docker's network or
    // loopback isn't reached.
    for name in ["prowlarr", "gluetun"] {
        assert_eq!(q.detect(&http, &ct(name)).await, Ok(None), "{name}");
    }
    assert_eq!(server.paths().len(), 1);
}

#[tokio::test]
async fn redirects_are_never_followed() {
    let elsewhere = Server::start(|_| reply(200, b"v5.0.3")).await;
    let to = format!("http://{}/api/v2/app/version", elsewhere.addr);
    let server = Server::start(move |path| match path {
        "/api/v2/app/version" => Reply::Full {
            status: 307,
            headers: vec![("Location".into(), to.clone())],
            body: Vec::new(),
        },
        _ => Reply::Full {
            status: 302,
            headers: vec![("Location".into(), "http://169.254.169.254/".into())],
            body: Vec::new(),
        },
    })
    .await;
    let http = http();
    let q = detect::detector(&id("qbittorrent")).unwrap();
    assert_eq!(q.detect(&http, &server.loopback()).await, Ok(None));
    assert_eq!(q.detect(&http, &server.entered()).await, Ok(None));
    let p = detect::detector(&id("prowlarr")).unwrap();
    assert_eq!(p.detect(&http, &server.entered()).await, Ok(None));
    assert!(elsewhere.paths().is_empty(), "a redirect was followed");
    // The client hands back the 3xx itself.
    let target = server.loopback();
    let r = http
        .send(ProbeRequest::new(
            &target,
            RequestPath::parse("/api/v2/app/version").unwrap(),
            Limits::PROBE,
        ))
        .await
        .unwrap();
    assert_eq!(r.status(), 307);
}

// ─── Hostile answers ────────────────────────────────────────────────────

#[tokio::test]
async fn oversized_answers_are_cut_off() {
    let cap = Limits::PROBE.body_cap();
    let big = Server::start(move |_| reply(200, &vec![b'v'; cap + 1])).await;
    // Chunked, with no length to refuse up front.
    let chunked = Server::start(move |_| {
        let mut out = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n".to_vec();
        for _ in 0..(cap / 1000 + 2) {
            out.extend_from_slice(b"3e8\r\n");
            out.extend_from_slice(&[b'{'; 1000]);
            out.extend_from_slice(b"\r\n");
        }
        out.extend_from_slice(b"0\r\n\r\n");
        Reply::Raw(out)
    })
    .await;
    let http = http();
    for server in [&big, &chunked] {
        for c in ["qbittorrent", "prowlarr", "gluetun"] {
            let t = server.loopback();
            assert_eq!(
                detect::detector(&id(c)).unwrap().detect(&http, &t).await,
                Err(AdapterError::TooLarge),
                "{c}"
            );
            assert_eq!(
                detect::health(&id(c)).unwrap().check(&http, &t).await,
                HealthStatus::Unhealthy
            );
        }
    }
}

#[tokio::test]
async fn slow_and_silent_answers_hit_one_deadline() {
    let drip = Server::start(|_| {
        Reply::Drip(b"HTTP/1.1 200 OK\r\nContent-Length: 1000\r\n\r\nv5.".to_vec())
    })
    .await;
    let silent = Server::start(|_| Reply::Silent).await;
    let http = http();
    let limits = Limits::new(Duration::from_millis(500), 64 * 1024).unwrap();
    for server in [&drip, &silent] {
        let t = server.loopback();
        let started = Instant::now();
        let r = http
            .send(ProbeRequest::new(
                &t,
                RequestPath::parse("/api/v2/app/version").unwrap(),
                limits,
            ))
            .await;
        assert_eq!(r, Err(AdapterError::Timeout));
        assert!(started.elapsed() < Duration::from_secs(10));
    }
    // Through an adapter, at the probe's own limits.
    let q = detect::health(&id("qbittorrent")).unwrap();
    assert_eq!(
        q.check(&http, &drip.loopback()).await,
        HealthStatus::Unreachable
    );
}

#[tokio::test]
async fn hostile_bodies_and_headers_carry_nothing_out() {
    let ten_kb = format!("v{}", "5.".repeat(5_000));
    let bodies: Vec<(u16, Vec<u8>)> = vec![
        (200, b"v5.0.3\x1b]0;owned\x07".to_vec()),
        (200, b"v5.0.3\r\nSet-Cookie: x=1".to_vec()),
        (200, ten_kb.into_bytes()),
        (200, b"v\xff\xfe5.0.3".to_vec()),
        (200, b"{\"status\":\"OK\"".to_vec()),
        (
            200,
            b"{\"status\":\"running\",\"status\":\"running\"}".to_vec(),
        ),
        (200, b"<html>v5.0.3</html>".to_vec()),
        (200, Vec::new()),
    ];
    let http = http();
    for (status, body) in bodies {
        let b = body.clone();
        let server = Server::start(move |_| reply(status, &b)).await;
        for c in ["qbittorrent", "prowlarr", "gluetun"] {
            let got = detect::detector(&id(c))
                .unwrap()
                .detect(&http, &server.loopback())
                .await;
            assert_eq!(got, Ok(None), "{c} {:?}", String::from_utf8_lossy(&body));
        }
    }
    // A hostile `Server` header and a wrong content type change nothing:
    // only the body's version comes back, in its strict form.
    let server = Server::start(|_| Reply::Full {
        status: 200,
        headers: vec![
            (
                "Server".into(),
                format!("<script>{}</script>", "x".repeat(8_000)),
            ),
            ("Content-Type".into(), "image/png".into()),
        ],
        body: b"v4.6.3".to_vec(),
    })
    .await;
    let d: Detected = detect::detector(&id("qbittorrent"))
        .unwrap()
        .detect(&http, &server.loopback())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(d.version.unwrap().as_str(), "4.6.3");
}

#[tokio::test]
async fn errors_carry_no_url_or_body() {
    let server = Server::start(|_| reply(200, &vec![b'x'; 70_000])).await;
    let url = format!("http://{}/secret-path", server.addr);
    let target = Target::Entered(EnteredUrl::parse(&url).unwrap());
    let e = detect::detector(&id("prowlarr"))
        .unwrap()
        .detect(&http(), &target)
        .await
        .unwrap_err();
    for shown in [e.to_string(), format!("{e:?}")] {
        assert!(!shown.contains("secret") && !shown.contains("127.0.0.1"));
        assert!(!shown.contains("xxx"));
    }
}

// ─── Companions Lodestar runs (X-cmp-supervisor-bind) ───────────────────

mod bound {
    use super::*;
    use lodestar_companions::supervise::bind::Bindings;
    use lodestar_companions::supervise::{State, Status};
    use tokio::sync::watch;

    fn status(state: State, run: u64) -> Status {
        let up = matches!(state, State::Running | State::Unhealthy { .. });
        Status {
            state,
            restarts: 0,
            last_exit: None,
            run,
            up,
            pid: None,
        }
    }

    fn limits() -> Limits {
        Limits::new(Duration::from_secs(15), 1024).unwrap()
    }

    fn path() -> RequestPath {
        RequestPath::parse("/api/v2/app/version").unwrap()
    }

    /// A request carrying a key, a cookie, a body and a query: each is
    /// "not bare".
    fn carrying(target: &Target) -> Vec<ProbeRequest<'_>> {
        let key = HeaderName::new("X-Api-Key").unwrap();
        let cookie = HeaderName::new("Cookie").unwrap();
        vec![
            ProbeRequest::new(target, path(), limits())
                .header(key, HeaderValue::secret(Secret::new("k")).unwrap()),
            ProbeRequest::new(target, path(), limits())
                .header(cookie, HeaderValue::plain("SID=abc").unwrap()),
            ProbeRequest::new(target, path(), limits()).post(b"password=x".to_vec()),
            ProbeRequest::new(
                target,
                RequestPath::parse("/api/v2/app/version?apikey=k").unwrap(),
                limits(),
            ),
        ]
    }

    /// Refused with nothing sent: unreachable, or (at a managed port that
    /// isn't ready) still starting or not ready.
    async fn refused(client: &CompanionHttp, req: ProbeRequest<'_>) {
        let got = client.send(req).await;
        assert!(
            matches!(
                got,
                Err(AdapterError::Unreachable | AdapterError::Starting | AdapterError::NotReady)
            ),
            "{:?}",
            got.map(|r| r.status())
        );
    }

    /// X-companion-starting-steps: a managed port that isn't ready is
    /// "starting" while a start holds it and "not ready" once none does,
    /// for what carries something and for a bare probe that finds nothing
    /// there; detection there says so rather than "not here". A port
    /// nothing manages still reads "not here", and a ready one is asked.
    #[tokio::test]
    async fn a_managed_port_that_isnt_ready_says_starting() {
        let closed = ClosedPort::new();
        let target = Target::Loopback(PortNumber::new(closed.port).unwrap());
        let qbt = detect::detector(&id("qbittorrent")).unwrap();
        // Nothing manages the port: nothing there is "not here".
        let unmanaged = bound(&Arc::new(Bindings::new()));
        assert_eq!(qbt.detect(&unmanaged, &target).await, Ok(None));

        let bindings = Arc::new(Bindings::new());
        let (tx, rx) = watch::channel(status(State::Starting, 0));
        bindings.hold(closed.port, rx);
        let client = bound(&bindings);
        for req in carrying(&target) {
            assert_eq!(client.send(req).await.err(), Some(AdapterError::Starting));
        }
        let bare = client
            .send(ProbeRequest::new(&target, path(), limits()))
            .await;
        assert_eq!(bare.err(), Some(AdapterError::Starting));
        assert_eq!(
            qbt.detect(&client, &target).await,
            Err(AdapterError::Starting)
        );
        // Waiting for you after a crash loop: not starting, so not ready.
        tx.send_replace(status(
            State::NeedsAttention(lodestar_companions::supervise::Attention::CrashLoop),
            1,
        ));
        assert_eq!(
            qbt.detect(&client, &target).await,
            Err(AdapterError::NotReady)
        );
        tx.send_replace(status(State::Starting, 2));
        // Stopped (or removed, or a failed start): not ready.
        bindings.release(closed.port);
        for req in carrying(&target) {
            assert_eq!(client.send(req).await.err(), Some(AdapterError::NotReady));
        }
        assert_eq!(
            qbt.detect(&client, &target).await,
            Err(AdapterError::NotReady)
        );
    }

    /// Review O2: a bare probe at a managed port that's starting, which
    /// accepts the connection but never answers, ends at its deadline as
    /// starting too (not a bare timeout, which detection reads as "not
    /// here"), whichever deadline fires first.
    #[tokio::test]
    async fn a_silent_starting_port_past_its_deadline_says_starting() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let held = Arc::new(Mutex::new(Vec::new()));
        let keep = Arc::clone(&held);
        tokio::spawn(async move {
            while let Ok((sock, _)) = listener.accept().await {
                keep.lock().unwrap().push(sock);
            }
        });
        let target = Target::Loopback(PortNumber::new(port).unwrap());
        let bindings = Arc::new(Bindings::new());
        let (_tx, rx) = watch::channel(status(State::Starting, 0));
        bindings.hold(port, rx);
        let client = bound(&bindings);
        let short = Limits::new(Duration::from_millis(200), 1024).unwrap();
        let got = client.send(ProbeRequest::new(&target, path(), short)).await;
        assert_eq!(got.err(), Some(AdapterError::Starting));
        let qbt = detect::detector(&id("qbittorrent")).unwrap();
        assert_ne!(qbt.detect(&client, &target).await, Ok(None));
    }

    /// A ready companion is asked as before: what carries something goes,
    /// and detection finds it.
    #[tokio::test]
    async fn a_ready_managed_port_is_asked_as_before() {
        let server = Server::start(|_| reply(200, b"v5.0.3")).await;
        let target = server.loopback();
        let port = target_port(&server);
        let bindings = Arc::new(Bindings::new());
        let (_tx, rx) = watch::channel(status(State::Running, 1));
        bindings.hold(port, rx);
        bindings.mark_ready(port, 1);
        let client = bound(&bindings);
        for req in carrying(&target) {
            assert_eq!(client.send(req).await.unwrap().status(), 200);
        }
        assert_eq!(server.seen().len(), carrying(&target).len());
    }

    fn bound(bindings: &Arc<Bindings>) -> CompanionHttp {
        http().with_bindings(Arc::clone(bindings))
    }

    /// A managed port is never asked on `::1`, in any state: not by a
    /// request that carries something, and not by a bare probe either.
    #[tokio::test]
    async fn a_managed_port_is_never_asked_on_ipv6() {
        let v6: Handler = Arc::new(|_: &str| reply(200, b"v6"));
        let Some((server, _)) = ipv6_with_ipv4_closed(v6).await else {
            assert!(
                std::env::var_os("CI").is_none(),
                "::1 can't be bound here, so the managed-port rule is untested"
            );
            eprintln!("::1 unavailable here: the managed-port rule wasn't exercised");
            return;
        };
        let target = server.loopback();
        let port = target_port(&server);
        let bindings = Arc::new(Bindings::new());
        let (tx, rx) = watch::channel(status(State::Starting, 0));
        bindings.hold(port, rx);
        let client = bound(&bindings);
        for req in carrying(&target) {
            refused(&client, req).await;
        }
        refused(&client, ProbeRequest::new(&target, path(), limits())).await;
        // Running, but not found ready: still nothing goes to ::1.
        tx.send_replace(status(State::Running, 1));
        refused(&client, ProbeRequest::new(&target, path(), limits())).await;
        for req in carrying(&target) {
            refused(&client, req).await;
        }
        // A readiness that can't be had from ::1 changes nothing either.
        bindings.mark_ready(port, 1);
        refused(&client, ProbeRequest::new(&target, path(), limits())).await;
        assert!(server.seen().is_empty(), "nothing reached ::1");
    }

    /// A request that carries something goes to a managed port only while
    /// the child is running in the run found ready. A restart holds them
    /// until the new run is found ready, and a stop holds them for good.
    #[tokio::test]
    async fn a_restart_holds_credentialed_requests_until_ready_again() {
        let server = Server::start(|_| reply(200, b"v5.0.3")).await;
        let target = server.loopback();
        let port = target_port(&server);
        let bindings = Arc::new(Bindings::new());
        let (tx, rx) = watch::channel(status(State::Running, 1));
        bindings.hold(port, rx);
        let client = bound(&bindings);
        // Running but not yet found ready: only a bare probe goes out.
        for req in carrying(&target) {
            refused(&client, req).await;
        }
        assert!(server.seen().is_empty());
        let got = client
            .send(ProbeRequest::new(&target, path(), limits()))
            .await
            .unwrap();
        assert_eq!(got.status(), 200);
        bindings.mark_ready(port, 1);
        for req in carrying(&target) {
            assert_eq!(client.send(req).await.unwrap().status(), 200);
        }
        assert_eq!(server.seen().len(), 5);
        // The child is found gone: held at once, before its state moves
        // on, though something still answers on the port.
        tx.send_modify(|s| s.up = false);
        for req in carrying(&target) {
            refused(&client, req).await;
        }
        // It starts again: held until the new run is found ready.
        tx.send_replace(status(
            State::BackingOff {
                delay: Duration::from_secs(1),
            },
            1,
        ));
        for req in carrying(&target) {
            refused(&client, req).await;
        }
        tx.send_replace(status(State::Running, 2));
        for req in carrying(&target) {
            refused(&client, req).await;
        }
        assert_eq!(server.seen().len(), 5);
        bindings.mark_ready(port, 1);
        refused(&client, carrying(&target).remove(0)).await;
        bindings.mark_ready(port, 2);
        for req in carrying(&target) {
            assert_eq!(client.send(req).await.unwrap().status(), 200);
        }
        assert_eq!(server.seen().len(), 9);
        // Stopped: held, and its port is never looked for on ::1.
        bindings.release(port);
        for req in carrying(&target) {
            refused(&client, req).await;
        }
        assert_eq!(server.seen().len(), 9);
    }

    /// A companion you run yourself (the guided path) is reached as after
    /// #867 through the shared client: a bare probe finds it on ::1, and
    /// what carries something goes there from then on.
    #[tokio::test]
    async fn a_hand_run_companion_is_reached_as_before() {
        let version: Handler = Arc::new(|_: &str| reply(200, b"v5.0.3"));
        let Some((server, _)) = ipv6_with_ipv4_closed(version).await else {
            assert!(
                std::env::var_os("CI").is_none(),
                "::1 can't be bound here, so the loopback rule is untested"
            );
            eprintln!("::1 unavailable here: the loopback rule wasn't exercised");
            return;
        };
        let target = server.loopback();
        // Another port is managed; this one isn't.
        let bindings = Arc::new(Bindings::new());
        let (_tx, rx) = watch::channel(status(State::Running, 1));
        bindings.hold(target_port(&server).wrapping_add(1).max(1), rx);
        let client = bound(&bindings);
        for req in carrying(&target) {
            refused(&client, req).await;
        }
        assert!(server.seen().is_empty());
        let got = client
            .send(ProbeRequest::new(&target, path(), limits()))
            .await
            .unwrap();
        assert_eq!(got.status(), 200);
        for req in carrying(&target) {
            assert_eq!(client.send(req).await.unwrap().status(), 200);
        }
        assert_eq!(server.seen().len(), 5);
    }

    /// An entered URL naming `localhost` reaches 127.0.0.1 alone, whatever
    /// the system resolver says of it.
    #[tokio::test]
    async fn an_entered_localhost_is_ipv4_loopback() {
        let server = Server::start(|_| reply(200, b"v4")).await;
        let port = target_port(&server);
        let resolver = Arc::new(Table(vec![
            ("localhost", IpAddr::V6(Ipv6Addr::LOCALHOST)),
            ("LocalHost.", IpAddr::V6(Ipv6Addr::LOCALHOST)),
        ]));
        let client = CompanionHttp::with_resolver(resolver).unwrap();
        for host in ["localhost", "LocalHost."] {
            let target =
                Target::Entered(EnteredUrl::parse(&format!("http://{host}:{port}")).unwrap());
            let got = client
                .send(ProbeRequest::new(&target, path(), limits()).post(b"x".to_vec()))
                .await
                .unwrap();
            assert_eq!(got.body(), b"v4");
        }
        assert_eq!(server.seen().len(), 2);
    }
}
