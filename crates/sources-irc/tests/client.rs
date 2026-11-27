//! The client against a scripted IRC server on loopback, over the guarded
//! egress (NOS-2a's `connect_tcp_routed`), plaintext and TLS. Every wait
//! has a deadline; nothing sleeps for a fixed time.

use std::io;
use std::net::{IpAddr, Ipv4Addr};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use lodestar_integrations::net::TcpRouteError;
use lodestar_integrations::net::{KillSwitch, NetError, NetPolicy, Resolve, Route};
use lodestar_integrations::{BoxFuture, Secret};
use lodestar_sources_irc::client::{
    Auth, Channel, Config, ConfigError, Disconnect, Egress, Events, Handle, Roots, SendError,
    State, Stop, Transport, spawn,
};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::{mpsc, watch};
use tokio::time::Instant;

/// The deadline for every wait.
const WAIT: Duration = Duration::from_secs(20);

/// Every name resolves to loopback, so TLS can verify a real name.
struct Loopback;

impl Resolve for Loopback {
    fn resolve<'a>(&'a self, _: &'a str) -> BoxFuture<'a, io::Result<Vec<IpAddr>>> {
        Box::pin(async { Ok(vec![IpAddr::V4(Ipv4Addr::LOCALHOST)]) })
    }
}

/// The email tests' throwaway CA and `mail.test` leaf
/// (`crates/integrations/tests/fixtures/email/README.md`).
fn fixture(name: &str) -> Vec<u8> {
    let path = format!(
        "{}/../integrations/tests/fixtures/email/{name}",
        env!("CARGO_MANIFEST_DIR")
    );
    std::fs::read(&path).unwrap_or_else(|e| panic!("{path}: {e}"))
}

trait Io: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Io for T {}

struct Server {
    port: u16,
    conns: mpsc::Receiver<Conn>,
    accepted: Arc<AtomicUsize>,
}

impl Server {
    async fn start(tls: bool) -> Server {
        let acceptor = tls.then(|| {
            let provider = Arc::new(rustls::crypto::ring::default_provider());
            let config = rustls::ServerConfig::builder_with_provider(provider)
                .with_safe_default_protocol_versions()
                .unwrap()
                .with_no_client_auth()
                .with_single_cert(
                    vec![CertificateDer::from(fixture("server.der"))],
                    PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(fixture("server.key.der"))),
                )
                .unwrap();
            tokio_rustls::TlsAcceptor::from(Arc::new(config))
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let (tx, conns) = mpsc::channel(8);
        let accepted = Arc::new(AtomicUsize::new(0));
        let count = Arc::clone(&accepted);
        tokio::spawn(async move {
            while let Ok((tcp, _)) = listener.accept().await {
                count.fetch_add(1, Ordering::SeqCst);
                let io: Box<dyn Io> = match &acceptor {
                    Some(a) => match a.accept(tcp).await {
                        Ok(s) => Box::new(s),
                        Err(_) => continue,
                    },
                    None => Box::new(tcp),
                };
                let (r, w) = tokio::io::split(io);
                let conn = Conn {
                    r: BufReader::new(r),
                    w,
                };
                if tx.send(conn).await.is_err() {
                    return;
                }
            }
        });
        Server {
            port,
            conns,
            accepted,
        }
    }

    async fn accept(&mut self) -> Conn {
        tokio::time::timeout(WAIT, self.conns.recv())
            .await
            .expect("no connection within the deadline")
            .expect("the server stopped")
    }

    fn accepted(&self) -> usize {
        self.accepted.load(Ordering::SeqCst)
    }
}

struct Conn {
    r: BufReader<tokio::io::ReadHalf<Box<dyn Io>>>,
    w: tokio::io::WriteHalf<Box<dyn Io>>,
}

impl Conn {
    /// The next line from the client, without `CR LF`; `None` at EOF.
    async fn line(&mut self) -> Option<String> {
        let mut s = String::new();
        let n = tokio::time::timeout(WAIT, self.r.read_line(&mut s))
            .await
            .expect("no line within the deadline")
            .unwrap_or(0);
        (n > 0).then(|| s.trim_end_matches(['\r', '\n']).to_owned())
    }

    async fn expect(&mut self, want: &str) {
        assert_eq!(self.line().await.as_deref(), Some(want));
    }

    async fn send(&mut self, line: &str) {
        self.send_bytes(format!("{line}\r\n").as_bytes()).await;
    }

    async fn send_bytes(&mut self, bytes: &[u8]) {
        self.w.write_all(bytes).await.unwrap();
        self.w.flush().await.unwrap();
    }

    /// Reads until the client closes the connection.
    async fn closed(&mut self) {
        let mut sink = Vec::new();
        tokio::time::timeout(WAIT, self.r.read_to_end(&mut sink))
            .await
            .expect("the client didn't close within the deadline")
            .ok();
    }

    /// The plain registration, through the welcome.
    async fn register(&mut self) {
        self.expect("NICK lode").await;
        self.expect("USER lodestar 0 * Lodestar").await;
        self.send(":irc.test 001 lode :Welcome").await;
    }
}

fn config(port: u16, tls: bool) -> Config {
    let mut c = Config::new(if tls { "mail.test" } else { "irc.test" }, "lode");
    c.port = port;
    if tls {
        c.roots = Roots::Only(vec![CertificateDer::from(fixture("ca.der"))]);
    } else {
        c.transport = Transport::Plaintext;
    }
    c.flood.burst = 10;
    c.reconnect.base = Duration::from_secs(1);
    c.reconnect.cap = Duration::from_secs(2);
    c
}

fn egress() -> Egress {
    Egress::new(
        Arc::new(Loopback),
        NetPolicy::download_client(),
        Route::Direct,
        None,
    )
}

fn egress_with(kill: Arc<dyn KillSwitch>) -> Egress {
    Egress::new(
        Arc::new(Loopback),
        NetPolicy::download_client(),
        Route::Direct,
        Some(kill),
    )
}

async fn state(h: &Handle, what: &str, pred: impl Fn(&State) -> bool) -> State {
    let mut rx = h.state();
    let got = tokio::time::timeout(WAIT, rx.wait_for(|s| pred(s)))
        .await
        .map(|r| r.map(|s| s.clone()));
    match got {
        Ok(Ok(s)) => s,
        _ => panic!("no {what} within the deadline; last {:?}", *rx.borrow()),
    }
}

async fn ready(h: &Handle) {
    state(h, "Ready", |s| matches!(s, State::Ready { .. })).await;
}

async fn next_event(ev: &mut Events) -> lodestar_sources_irc::Message {
    tokio::time::timeout(WAIT, ev.recv())
        .await
        .expect("no event within the deadline")
        .expect("the client stopped")
}

#[tokio::test]
async fn registers_joins_answers_pings_and_streams_messages() {
    let mut srv = Server::start(false).await;
    let mut c = config(srv.port, false);
    c.channels = vec![Channel::new("#announce")];
    let (h, mut ev) = spawn(c, egress()).unwrap();
    let mut conn = srv.accept().await;
    conn.register().await;
    conn.expect("JOIN #announce").await;
    conn.send(":lode!u@h JOIN #announce").await;
    conn.send("PING :tok-1").await;
    conn.expect("PONG tok-1").await;
    conn.send(":n!u@h PRIVMSG lode :\u{1}VERSION\u{1}").await;
    conn.send(":Bot!b@h PRIVMSG #announce :New: Some.Release.2026 [1 GB]")
        .await;
    ready(&h).await;

    let mut seen = Vec::new();
    loop {
        let m = next_event(&mut ev).await;
        assert!(m.ctcp().is_none(), "CTCP reached the plugin");
        assert_ne!(m.command, "PING");
        seen.push(m.command.clone());
        if m.command == "PRIVMSG" {
            assert_eq!(m.nick(), Some("Bot"));
            assert_eq!(m.param(1), Some("New: Some.Release.2026 [1 GB]"));
            break;
        }
    }
    assert_eq!(seen, ["001", "JOIN", "PRIVMSG"]);

    h.privmsg("#announce", "hello there").unwrap();
    conn.expect("PRIVMSG #announce :hello there").await;
    assert_eq!(h.privmsg("#a,#b", "x"), Err(SendError::BadTarget));
    assert_eq!(h.privmsg("#a", "\u{1}PING"), Err(SendError::Ctcp));
    assert!(matches!(
        h.privmsg("#a", "x\r\nQUIT"),
        Err(SendError::Encode(_))
    ));
    // A channel key never goes out on plaintext.
    let key = Secret::new("chan-key");
    assert_eq!(h.join("#k", Some(&key)), Err(SendError::KeyNeedsTls));
    assert_eq!(
        h.privmsg_secret("Bot", "!invite ", &key),
        Err(SendError::KeyNeedsTls)
    );
    let stats = h.stats();
    assert_eq!(stats.ctcp_ignored, 1);
    assert_eq!(stats.connections, 1);

    h.shutdown();
    conn.expect("QUIT").await;
    state(&h, "Stopped", |s| *s == State::Stopped(Stop::Shutdown)).await;
}

#[tokio::test]
async fn sasl_plain_over_tls() {
    let mut srv = Server::start(true).await;
    let mut c = config(srv.port, true);
    c.auth = Auth::SaslPlain {
        account: "acct".into(),
        password: Secret::new("pw-test"),
    };
    c.channels = vec![Channel {
        name: "#private".into(),
        key: Some(Secret::new("chan-key")),
    }];
    let (h, _ev) = spawn(c, egress()).unwrap();
    let mut conn = srv.accept().await;
    conn.expect("CAP LS 302").await;
    conn.expect("NICK lode").await;
    conn.expect("USER lodestar 0 * Lodestar").await;
    conn.send(":irc.test CAP * LS :multi-prefix sasl=PLAIN")
        .await;
    conn.expect("CAP REQ sasl").await;
    conn.send(":irc.test CAP * ACK :sasl").await;
    conn.expect("AUTHENTICATE PLAIN").await;
    conn.send("AUTHENTICATE +").await;
    // base64("acct\0acct\0pw-test")
    conn.expect("AUTHENTICATE YWNjdABhY2N0AHB3LXRlc3Q=").await;
    conn.send(":irc.test 900 lode lode!u@h acct :You are now logged in")
        .await;
    conn.send(":irc.test 903 lode :SASL authentication successful")
        .await;
    conn.expect("CAP END").await;
    conn.send(":irc.test 001 lode :Welcome").await;
    conn.expect("JOIN #private chan-key").await;
    ready(&h).await;
    h.privmsg_secret("Bot", "!invite ", &Secret::new("irc-key-test"))
        .unwrap();
    conn.expect("PRIVMSG Bot :!invite irc-key-test").await;
}

#[tokio::test]
async fn a_failed_login_stops_for_good() {
    let mut srv = Server::start(true).await;
    let mut c = config(srv.port, true);
    c.auth = Auth::SaslPlain {
        account: "acct".into(),
        password: Secret::new("pw-wrong"),
    };
    let (h, _ev) = spawn(c, egress()).unwrap();
    let mut conn = srv.accept().await;
    for _ in 0..3 {
        conn.line().await;
    }
    conn.send(":irc.test CAP * LS :sasl").await;
    conn.expect("CAP REQ sasl").await;
    conn.send(":irc.test CAP * ACK :sasl").await;
    conn.expect("AUTHENTICATE PLAIN").await;
    conn.send("AUTHENTICATE +").await;
    conn.line().await;
    conn.send(":irc.test 904 lode :SASL authentication failed")
        .await;
    state(&h, "Stopped", |s| *s == State::Stopped(Stop::AuthFailed)).await;
    conn.closed().await;
    // Stopped is final: the task has ended, so nothing reconnects.
    let deadline = Instant::now() + WAIT;
    while h.privmsg("#a", "x") != Err(SendError::Closed) {
        assert!(Instant::now() < deadline, "the client kept running");
        tokio::task::yield_now().await;
    }
    assert_eq!(srv.accepted(), 1);
}

#[tokio::test]
async fn a_certificate_for_another_name_stops_the_client() {
    let srv = Server::start(true).await;
    let mut c = config(srv.port, true);
    c.host = "other.test".into();
    let (h, _ev) = spawn(c, egress()).unwrap();
    state(&h, "Stopped", |s| *s == State::Stopped(Stop::Certificate)).await;

    // And the platform roots don't trust the test CA either.
    let mut c = config(srv.port, true);
    c.roots = Roots::Platform;
    let (h, _ev) = spawn(c, egress()).unwrap();
    state(&h, "Stopped", |s| *s == State::Stopped(Stop::Certificate)).await;
}

#[tokio::test]
async fn a_routed_client_verifies_with_its_own_roots() {
    // Over a bound address the default roots (`lodestar_integrations::
    // tls`, self-contained there) don't hold the test CA: the handshake
    // reaches the server over the route and the certificate is refused.
    let mut srv = Server::start(true).await;
    let mut c = config(srv.port, true);
    c.roots = Roots::Platform;
    let bound = Egress::new(
        Arc::new(Loopback),
        NetPolicy::download_client(),
        Route::BindAddress(IpAddr::V4(Ipv4Addr::LOCALHOST)),
        None,
    );
    let (h, _ev) = spawn(c, bound).unwrap();
    state(&h, "Stopped", |s| *s == State::Stopped(Stop::Certificate)).await;
    assert_eq!(srv.accepted(), 1);

    // With the test CA named, the same route registers.
    let (h, _ev) = spawn(
        config(srv.port, true),
        Egress::new(
            Arc::new(Loopback),
            NetPolicy::download_client(),
            Route::BindAddress(IpAddr::V4(Ipv4Addr::LOCALHOST)),
            None,
        ),
    )
    .unwrap();
    let mut conn = srv.accept().await;
    conn.register().await;
    state(&h, "Ready", |s| matches!(s, State::Ready { .. })).await;
}

#[tokio::test]
async fn credentials_are_refused_on_plaintext() {
    let mut c = config(1, false);
    c.auth = Auth::NickServ {
        account: "acct".into(),
        password: Secret::new("pw-test"),
        service: None,
    };
    assert_eq!(
        spawn(c, egress()).map(|_| ()),
        Err(ConfigError::CredentialsNeedTls)
    );
}

#[tokio::test]
async fn the_egress_guard_refuses_a_blocked_destination() {
    let srv = Server::start(false).await;
    let c = config(srv.port, false);
    // Public addresses only: loopback is refused before any connection.
    let public = Egress::new(
        Arc::new(Loopback),
        NetPolicy::notification(false),
        Route::Direct,
        None,
    );
    let (h, _ev) = spawn(c, public).unwrap();
    let s = state(&h, "Stopped", |s| matches!(s, State::Stopped(_))).await;
    assert!(
        matches!(
            s,
            State::Stopped(Stop::Refused(TcpRouteError::Net(NetError::Blocked(_))))
        ),
        "{s:?}"
    );
    assert_eq!(srv.accepted(), 0);
}

#[tokio::test]
async fn overlong_and_malformed_lines_are_dropped_not_buffered() {
    let mut srv = Server::start(false).await;
    let (h, mut ev) = spawn(config(srv.port, false), egress()).unwrap();
    let mut conn = srv.accept().await;
    conn.register().await;
    assert_eq!(next_event(&mut ev).await.command, "001");
    // 1 MiB without a line end, then its end, then a NUL line.
    let big = vec![b'x'; 1 << 20];
    conn.send_bytes(&big).await;
    conn.send_bytes(b"\r\nPRIVMSG #a :bad\0byte\r\n").await;
    conn.send(":n!u@h PRIVMSG #a :after").await;
    let m = next_event(&mut ev).await;
    assert_eq!(m.param(1), Some("after"));
    let stats = h.stats();
    assert_eq!(stats.overlong, 1);
    assert_eq!(stats.malformed, 1);
}

#[tokio::test]
async fn reconnects_with_backoff_after_the_server_closes() {
    let mut srv = Server::start(false).await;
    let (h, _ev) = spawn(config(srv.port, false), egress()).unwrap();
    let mut conn = srv.accept().await;
    conn.register().await;
    ready(&h).await;
    let closed_at = Instant::now();
    drop(conn);
    let s = state(&h, "Waiting", |s| matches!(s, State::Waiting { .. })).await;
    let State::Waiting {
        attempt,
        after,
        retry_in,
    } = s
    else {
        unreachable!()
    };
    assert_eq!((attempt, after), (1, Disconnect::Closed));
    // base 1 s, jittered into [0.5 s, 1 s].
    assert!(retry_in >= Duration::from_millis(500) && retry_in <= Duration::from_secs(1));
    let mut conn = srv.accept().await;
    assert!(closed_at.elapsed() >= Duration::from_millis(500));
    conn.register().await;
    ready(&h).await;
}

#[tokio::test]
async fn the_kill_switch_drops_the_connection_and_holds_reconnects() {
    let mut srv = Server::start(false).await;
    let (up, rx) = watch::channel(true);
    let kill: Arc<dyn KillSwitch> = Arc::new(rx);
    let (h, _ev) = spawn(config(srv.port, false), egress_with(kill)).unwrap();
    let mut conn = srv.accept().await;
    conn.register().await;
    ready(&h).await;
    up.send_replace(false);
    conn.closed().await;
    // A later attempt is refused by the switch before any connection.
    state(&h, "a refused retry", |s| {
        matches!(
            s,
            State::Waiting {
                attempt: 2..,
                after: Disconnect::KillSwitch,
                ..
            }
        )
    })
    .await;
    assert_eq!(srv.accepted(), 1);
    up.send_replace(true);
    let mut conn = srv.accept().await;
    conn.register().await;
    ready(&h).await;
}

#[tokio::test]
async fn flood_control_paces_lines() {
    let mut srv = Server::start(false).await;
    let mut c = config(srv.port, false);
    c.flood.burst = 2;
    c.flood.interval = Duration::from_millis(500);
    c.channels = vec![Channel::new("#announce")];
    // Before the client exists, so before its bucket starts.
    let first = Instant::now();
    let (h, _ev) = spawn(c, egress()).unwrap();
    let mut conn = srv.accept().await;
    conn.expect("NICK lode").await;
    conn.expect("USER lodestar 0 * Lodestar").await;
    conn.send(":irc.test 001 lode :Welcome").await;
    ready(&h).await;
    for i in 0..4 {
        h.privmsg("#announce", &format!("line {i}")).unwrap();
    }
    conn.expect("JOIN #announce").await;
    for i in 0..4 {
        conn.expect(&format!("PRIVMSG #announce :line {i}")).await;
    }
    // Two lines in the burst, then one per 500 ms: the fifth paced line
    // can't be sent before 2.5 s after the session started.
    assert!(
        first.elapsed() >= Duration::from_millis(2500),
        "{:?}",
        first.elapsed()
    );
}

#[tokio::test]
async fn silence_sends_a_ping_then_reconnects() {
    let mut srv = Server::start(false).await;
    let mut c = config(srv.port, false);
    c.timeouts.ping_after = Duration::from_secs(1);
    c.timeouts.ping_timeout = Duration::from_secs(1);
    // A long wait, so the short-lived Waiting state can't be missed.
    c.reconnect.base = Duration::from_secs(3);
    c.reconnect.cap = Duration::from_secs(3);
    let (h, _ev) = spawn(c, egress()).unwrap();
    let mut conn = srv.accept().await;
    conn.register().await;
    conn.expect("PING lodestar").await;
    conn.closed().await;
    state(&h, "Waiting", |s| {
        matches!(
            s,
            State::Waiting {
                after: Disconnect::PingTimeout,
                ..
            }
        )
    })
    .await;
    // Kept open until Ready is seen, so Ready isn't replaced first.
    let mut again = srv.accept().await;
    again.register().await;
    ready(&h).await;
}

/// After the welcome the server sends only garbage, over and over: lines
/// over the limit, malformed lines, and a line that never ends. None of it
/// counts as traffic, so the client still sends `PING` on time and then
/// reconnects.
#[tokio::test]
async fn garbage_is_still_silence() {
    let mut srv = Server::start(false).await;
    let mut c = config(srv.port, false);
    c.timeouts.ping_after = Duration::from_secs(1);
    c.timeouts.ping_timeout = Duration::from_secs(1);
    c.reconnect.base = Duration::from_secs(3);
    c.reconnect.cap = Duration::from_secs(3);
    let (h, _ev) = spawn(c, egress()).unwrap();
    let mut conn = srv.accept().await;
    conn.register().await;
    let Conn { mut r, mut w } = conn;
    // A writer paced well inside the 1 s idle limit, until the client
    // hangs up.
    tokio::spawn(async move {
        let mut overlong = vec![b'x'; 9000];
        overlong.push(b'\n');
        let mut tick = tokio::time::interval(Duration::from_millis(100));
        loop {
            tick.tick().await;
            for bytes in [
                &overlong[..],
                b"PRIVMSG #a :bad\0byte\r\n",
                b":n!u@h PRIVMSG #a :never ends",
            ] {
                if w.write_all(bytes).await.is_err() || w.flush().await.is_err() {
                    return;
                }
            }
        }
    });
    let mut line = String::new();
    tokio::time::timeout(WAIT, r.read_line(&mut line))
        .await
        .expect("no PING within the deadline")
        .unwrap();
    assert_eq!(line.trim_end(), "PING lodestar");
    state(&h, "Waiting", |s| {
        matches!(
            s,
            State::Waiting {
                after: Disconnect::PingTimeout,
                ..
            }
        )
    })
    .await;
    let stats = h.stats();
    assert!(stats.overlong > 0 && stats.malformed > 0, "{stats:?}");
}

/// Accepts a connection and ends it with `ERROR` before registering.
async fn refuse_with_error(srv: &mut Server) {
    let mut conn = srv.accept().await;
    conn.expect("NICK lode").await;
    conn.send("ERROR :Closing Link: lode (K-lined)").await;
    conn.closed().await;
}

/// A server that ends every connection with `ERROR` before registering
/// (a K-line) stops the client after three once the back-off is at its
/// cap, instead of being retried forever.
#[tokio::test]
async fn repeated_errors_at_the_cap_stop_the_client() {
    let mut srv = Server::start(false).await;
    let mut c = config(srv.port, false);
    c.reconnect.cap = c.reconnect.base;
    let (h, _ev) = spawn(c, egress()).unwrap();
    // The first attempt had no wait, so its ERROR doesn't count; every
    // later wait is the cap.
    for _ in 0..4 {
        refuse_with_error(&mut srv).await;
    }
    state(&h, "Stopped", |s| *s == State::Stopped(Stop::Banned)).await;
    let deadline = Instant::now() + WAIT;
    while h.privmsg("#a", "x") != Err(SendError::Closed) {
        assert!(Instant::now() < deadline, "the client kept running");
        tokio::task::yield_now().await;
    }
    assert_eq!(srv.accepted(), 4);
}

/// Quick `ERROR`s while the back-off is still below its cap are a
/// server's reconnect throttle ("too many connections"): the client keeps
/// trying, and registers once the server lets it.
#[tokio::test]
async fn quick_errors_below_the_cap_are_retried() {
    let mut srv = Server::start(false).await;
    let mut c = config(srv.port, false);
    c.reconnect.cap = Duration::from_secs(60);
    let (h, _ev) = spawn(c, egress()).unwrap();
    for _ in 0..3 {
        refuse_with_error(&mut srv).await;
    }
    let mut conn = srv.accept().await;
    conn.register().await;
    ready(&h).await;
}

/// An `ERROR` during the NickServ wait comes after the welcome, so it is
/// a registered connection: it ends the `ERROR` streak instead of
/// extending it.
#[tokio::test]
async fn an_error_while_identifying_ends_the_streak() {
    let mut srv = Server::start(true).await;
    let mut c = config(srv.port, true);
    c.reconnect.cap = c.reconnect.base;
    c.auth = Auth::NickServ {
        account: "acct".into(),
        password: Secret::new("pw-test"),
        service: None,
    };
    let (h, _ev) = spawn(c, egress()).unwrap();
    // One uncounted (no wait before it), then two at the cap.
    for _ in 0..3 {
        refuse_with_error(&mut srv).await;
    }
    let mut conn = srv.accept().await;
    conn.register().await;
    conn.expect("PRIVMSG NickServ :IDENTIFY acct pw-test").await;
    conn.send("ERROR :Closing Link: lode (Services restarting)")
        .await;
    conn.closed().await;
    // Not a ban: the next connection comes and registers.
    let mut conn = srv.accept().await;
    conn.register().await;
    conn.expect("PRIVMSG NickServ :IDENTIFY acct pw-test").await;
    conn.send(":irc.test 900 lode lode!u@h acct :You are now logged in")
        .await;
    ready(&h).await;
}

/// A resolver that never answers, so the connect never finishes.
struct Hang;

impl Resolve for Hang {
    fn resolve<'a>(&'a self, _: &'a str) -> BoxFuture<'a, io::Result<Vec<IpAddr>>> {
        Box::pin(std::future::pending())
    }
}

/// Dropping `Events` stops a client stuck connecting at once, not after
/// the connect timeout (set to its 10 min maximum here, well past every
/// wait's deadline).
#[tokio::test]
async fn dropping_events_stops_a_stuck_connect() {
    let mut c = config(6667, false);
    c.timeouts.connect = Duration::from_secs(600);
    let egress = Egress::new(
        Arc::new(Hang),
        NetPolicy::download_client(),
        Route::Direct,
        None,
    );
    let (h, ev) = spawn(c, egress).unwrap();
    state(&h, "Connecting", |s| matches!(s, State::Connecting { .. })).await;
    drop(ev);
    state(&h, "Stopped", |s| *s == State::Stopped(Stop::Shutdown)).await;
}

/// The same during a TLS handshake the server never answers.
#[tokio::test]
async fn dropping_events_stops_a_stuck_tls_handshake() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut c = config(listener.local_addr().unwrap().port(), true);
    c.timeouts.connect = Duration::from_secs(600);
    let (h, ev) = spawn(c, egress()).unwrap();
    let (mut silent, _) = tokio::time::timeout(WAIT, listener.accept())
        .await
        .expect("no connection within the deadline")
        .unwrap();
    // The ClientHello's first bytes: the client is in the handshake.
    let mut hello = [0u8; 1];
    tokio::time::timeout(WAIT, silent.read_exact(&mut hello))
        .await
        .expect("no ClientHello within the deadline")
        .unwrap();
    drop(ev);
    state(&h, "Stopped", |s| *s == State::Stopped(Stop::Shutdown)).await;
}
