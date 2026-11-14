//! CMP-8a: detection and health against recorded answers on a local
//! server, the SSRF guard and Q12's fixed probe set, and hostile answers.

use std::collections::HashMap;
use std::io;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
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
use tokio::net::{TcpListener, TcpSocket, TcpStream};

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

/// A loopback port that nothing listens on, held closed for as long as
/// this lives: bound on 127.0.0.1, and on ::1 where the runner has it,
/// but never listening. A connection to it is refused, and no other
/// socket (another test in this binary, say) can take the port meanwhile.
/// Dropping a listener and probing its port instead leaves a window in
/// which another test's listener can be handed the same port.
struct ClosedPort {
    port: u16,
    _v4: TcpSocket,
    _v6: Option<TcpSocket>,
}

impl ClosedPort {
    fn new() -> ClosedPort {
        // A port free on 127.0.0.1 may be taken on ::1 (the loopback
        // probe falls back to it): then another pair is chosen. Both are
        // held from the moment they're bound, so nothing is left to race.
        for _ in 0..64 {
            let v4 = TcpSocket::new_v4().unwrap();
            v4.bind((Ipv4Addr::LOCALHOST, 0).into()).unwrap();
            let port = v4.local_addr().unwrap().port();
            let v6 = TcpSocket::new_v6().and_then(|s| {
                s.bind((Ipv6Addr::LOCALHOST, port).into())?;
                Ok(s)
            });
            match v6 {
                Ok(v6) => {
                    return ClosedPort {
                        port,
                        _v4: v4,
                        _v6: Some(v6),
                    };
                }
                // Taken, or on Windows in an excluded port range (Hyper-V,
                // WinNAT: WSAEACCES): another pair. Nothing is left unheld.
                Err(e) if in_use(&e) => continue,
                // No IPv6 loopback here: the probe can't fall back to it.
                Err(e) if no_ipv6(&e) => {
                    return ClosedPort {
                        port,
                        _v4: v4,
                        _v6: None,
                    };
                }
                // Anything else would leave ::1 unheld: say so.
                Err(e) => panic!("holding [::1]:{port}: {e}"),
            }
        }
        panic!("no loopback port free on both 127.0.0.1 and ::1");
    }

    fn v4(&self) -> SocketAddr {
        (Ipv4Addr::LOCALHOST, self.port).into()
    }
}

/// What the two tests below rely on: a held port refuses connections, and
/// no listener can be bound on it while it's held.
#[tokio::test]
async fn a_closed_port_refuses_and_cant_be_taken() {
    let closed = ClosedPort::new();
    let refused = TcpStream::connect(closed.v4()).await.unwrap_err();
    assert_eq!(
        refused.kind(),
        io::ErrorKind::ConnectionRefused,
        "{refused}"
    );
    // Taken: the held socket doesn't set `SO_REUSEADDR`, so this is
    // `AddrInUse` on every target (an excluded-range `PermissionDenied` can't arise for a
    // port the system just handed out).
    let taken = TcpListener::bind(closed.v4()).await.unwrap_err();
    assert_eq!(taken.kind(), io::ErrorKind::AddrInUse, "{taken}");
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
    let Some((server, _v4)) = ipv6_with_ipv4_closed(version).await else {
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

/// A server on ::1, with 127.0.0.1 held closed on its port, so the probe's
/// IPv4 attempt is always refused and never answered by another program.
/// The port is picked on IPv4 first, as `ClosedPort` does. `None` where
/// ::1 can't be bound.
async fn ipv6_with_ipv4_closed(handler: Handler) -> Option<(Server, TcpSocket)> {
    for _ in 0..64 {
        let v4 = TcpSocket::new_v4().unwrap();
        v4.bind((Ipv4Addr::LOCALHOST, 0).into()).unwrap();
        let port = v4.local_addr().unwrap().port();
        let v6 = TcpSocket::new_v6().and_then(|s| {
            s.bind((Ipv6Addr::LOCALHOST, port).into())?;
            s.listen(16)
        });
        match v6 {
            Ok(listener) => {
                let server = Server::serve(listener, Arc::clone(&handler)).unwrap();
                return Some((server, v4));
            }
            Err(e) if in_use(&e) => continue,
            Err(e) if no_ipv6(&e) => return None,
            Err(e) => panic!("listening on [::1]:{port}: {e}"),
        }
    }
    panic!("no port free on both 127.0.0.1 and ::1");
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
