//! Routed, guarded raw TCP connections (`net::connect_tcp_routed`, NOS-2a):
//! one test per route, the guard refusing a LAN address on every route,
//! malformed SOCKS5 answers, and the kill-switch before and during a
//! connection. Every wait has a deadline of at least 10 s; nothing sleeps
//! for a fixed time.

use std::collections::HashMap;
use std::io;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use lodestar_integrations::net::socks::SocksError;
use lodestar_integrations::net::{KillSwitch, TcpRouteError, connect_tcp_routed};
use lodestar_integrations::{
    AddrClass, BoxFuture, NetError, NetPolicy, Resolve, Route, RouteError, Secret, Socks5Route,
    SocksAuth, SocksDns,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, watch};

/// Long enough for the slowest CI runner; no test waits this long unless
/// it fails.
const DEADLINE: Duration = Duration::from_secs(20);

/// A resolver from a fixed table, counting lookups.
#[derive(Default)]
struct Table {
    names: HashMap<&'static str, Vec<IpAddr>>,
    lookups: AtomicUsize,
}

impl Table {
    fn with(entries: &[(&'static str, &[IpAddr])]) -> Arc<Table> {
        Arc::new(Table {
            names: entries.iter().map(|(n, a)| (*n, a.to_vec())).collect(),
            lookups: AtomicUsize::new(0),
        })
    }
}

impl Resolve for Table {
    fn resolve<'a>(&'a self, host: &'a str) -> BoxFuture<'a, io::Result<Vec<IpAddr>>> {
        self.lookups.fetch_add(1, Ordering::SeqCst);
        let answer = self.names.get(host).cloned();
        Box::pin(async move { answer.ok_or_else(|| io::Error::other("no such name")) })
    }
}

const LO: IpAddr = IpAddr::V4(Ipv4Addr::LOCALHOST);

/// A listener that counts accepts and reports each peer address, then
/// writes `greeting` and holds the connection open.
struct Server {
    addr: SocketAddr,
    accepts: Arc<AtomicUsize>,
    peers: mpsc::UnboundedReceiver<SocketAddr>,
}

async fn server(greeting: &'static [u8]) -> Server {
    let listener = TcpListener::bind((LO, 0)).await.unwrap();
    let addr = listener.local_addr().unwrap();
    let accepts = Arc::new(AtomicUsize::new(0));
    let (tx, peers) = mpsc::unbounded_channel();
    let count = Arc::clone(&accepts);
    tokio::spawn(async move {
        while let Ok((mut s, peer)) = listener.accept().await {
            count.fetch_add(1, Ordering::SeqCst);
            let _ = tx.send(peer);
            tokio::spawn(async move {
                let _ = s.write_all(greeting).await;
                let mut sink = [0u8; 64];
                while matches!(s.read(&mut sink).await, Ok(n) if n > 0) {}
            });
        }
    });
    Server {
        addr,
        accepts,
        peers,
    }
}

async fn read_greeting(s: &mut TcpStream, n: usize) -> Vec<u8> {
    let mut buf = vec![0u8; n];
    tokio::time::timeout(DEADLINE, s.read_exact(&mut buf))
        .await
        .expect("the greeting within the deadline")
        .unwrap();
    buf
}

async fn connect(
    resolver: &Table,
    host: &str,
    port: u16,
    policy: NetPolicy,
    route: &Route,
    kill: Option<&dyn KillSwitch>,
) -> Result<TcpStream, TcpRouteError> {
    tokio::time::timeout(
        DEADLINE,
        connect_tcp_routed(resolver, host, port, policy, route, kill, DEADLINE),
    )
    .await
    .expect("an answer within the deadline")
}

// ── Direct ─────────────────────────────────────────────────────────────

#[tokio::test]
async fn direct_route_connects_to_a_checked_address() {
    let mut srv = server(b"hi").await;
    let dns = Table::with(&[("irc.test", &[LO])]);
    let mut s = connect(
        &dns,
        "irc.test",
        srv.addr.port(),
        NetPolicy::download_client(),
        &Route::Direct,
        None,
    )
    .await
    .unwrap();
    assert_eq!(read_greeting(&mut s, 2).await, b"hi");
    assert_eq!(srv.peers.recv().await.unwrap().ip(), LO);
    assert_eq!(dns.lookups.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn lan_and_loopback_addresses_are_refused_on_every_route_before_connecting() {
    let srv = server(b"").await;
    let port = srv.addr.port();
    let lan: IpAddr = "192.168.1.20".parse().unwrap();
    // A mixed answer: one public address, one LAN address. Refused whole.
    let dns = Table::with(&[
        ("lan.test", &[lan]),
        ("mixed.test", &["93.184.215.14".parse().unwrap(), lan]),
        ("lo.test", &[LO]),
    ]);
    let policy = NetPolicy::notification(false);
    let socks = |dns| {
        Route::Socks5(Socks5Route {
            proxy: srv.addr,
            dns,
            auth: None,
        })
    };
    let routes = [
        Route::Direct,
        Route::BindAddress(LO),
        socks(SocksDns::Local),
        socks(SocksDns::Proxy),
    ];
    for route in &routes {
        for (host, class) in [
            ("192.168.1.20", AddrClass::Private),
            ("10.0.0.1", AddrClass::Private),
            ("127.0.0.1", AddrClass::Loopback),
            ("169.254.169.254", AddrClass::Metadata),
            ("[::ffff:192.168.1.20]", AddrClass::Private),
        ] {
            let host = host.trim_start_matches('[').trim_end_matches(']');
            let got = connect(&dns, host, port, policy, route, None).await;
            assert_eq!(
                got.err(),
                Some(TcpRouteError::Net(NetError::Blocked(class))),
                "{route:?} {host}"
            );
        }
    }
    // Names are resolved and checked on every route but proxy-side DNS.
    for route in &routes[..3] {
        for (host, class) in [
            ("lan.test", AddrClass::Private),
            ("mixed.test", AddrClass::Private),
            ("lo.test", AddrClass::Loopback),
        ] {
            let got = connect(&dns, host, port, policy, route, None).await;
            assert_eq!(
                got.err(),
                Some(TcpRouteError::Net(NetError::Blocked(class))),
                "{route:?} {host}"
            );
        }
    }
    // Proxy-side DNS refuses plainly local names, as the HTTP client does.
    for (host, class) in [
        ("localhost", AddrClass::Loopback),
        ("nas.local", AddrClass::Private),
        ("router", AddrClass::Private),
        ("metadata.google.internal", AddrClass::Metadata),
    ] {
        let got = connect(&dns, host, port, policy, &routes[3], None).await;
        assert_eq!(
            got.err(),
            Some(TcpRouteError::Net(NetError::Blocked(class))),
            "{host}"
        );
    }
    assert_eq!(srv.accepts.load(Ordering::SeqCst), 0, "nothing connected");
}

// ── Bind address ───────────────────────────────────────────────────────

#[tokio::test]
async fn bind_address_route_binds_the_local_address_and_never_falls_back() {
    let mut srv = server(b"ok").await;
    let port = srv.addr.port();
    let dns = Table::with(&[("v6only.test", &["::1".parse().unwrap()])]);
    let policy = NetPolicy::download_client();
    let mut s = connect(
        &dns,
        "127.0.0.1",
        port,
        policy,
        &Route::BindAddress(LO),
        None,
    )
    .await
    .unwrap();
    assert_eq!(read_greeting(&mut s, 2).await, b"ok");
    assert_eq!(srv.peers.recv().await.unwrap().ip(), LO);

    // A destination of the other family has nothing to bind to.
    let got = connect(
        &dns,
        "v6only.test",
        port,
        policy,
        &Route::BindAddress(LO),
        None,
    )
    .await;
    assert_eq!(got.err(), Some(TcpRouteError::Net(NetError::Connect)));

    // An address this machine doesn't have (TEST-NET-1): binding fails and
    // nothing connects directly instead.
    let gone = Route::BindAddress("192.0.2.1".parse().unwrap());
    let got = connect(&dns, "127.0.0.1", port, policy, &gone, None).await;
    assert_eq!(got.err(), Some(TcpRouteError::Net(NetError::Connect)));
    assert_eq!(srv.accepts.load(Ordering::SeqCst), 1);
}

// ── Interface ──────────────────────────────────────────────────────────

#[cfg(target_os = "linux")]
#[tokio::test]
async fn interface_route_binds_the_device_and_never_falls_back() {
    let srv = server(b"if").await;
    let port = srv.addr.port();
    let dns = Table::with(&[]);
    let policy = NetPolicy::download_client();
    let lo = Route::Interface("lo".into());
    match connect(&dns, "127.0.0.1", port, policy, &lo, None).await {
        Ok(mut s) => assert_eq!(read_greeting(&mut s, 2).await, b"if"),
        // Kernels before 5.7 want CAP_NET_RAW for SO_BINDTODEVICE; the
        // refusal must still be a plain connect failure, not a fallback.
        Err(e) => {
            assert_eq!(e, TcpRouteError::Net(NetError::Connect));
            assert_eq!(srv.accepts.load(Ordering::SeqCst), 0);
        }
    }
    let before = srv.accepts.load(Ordering::SeqCst);
    let missing = Route::Interface("nosuchif0".into());
    let got = connect(&dns, "127.0.0.1", port, policy, &missing, None).await;
    assert_eq!(got.err(), Some(TcpRouteError::Net(NetError::Connect)));
    assert_eq!(srv.accepts.load(Ordering::SeqCst), before);
}

#[tokio::test]
async fn bad_interface_names_fail_before_any_lookup() {
    let dns = Table::with(&[("irc.test", &[LO])]);
    let bad = Route::Interface("eth0/../x".into());
    let got = connect(
        &dns,
        "irc.test",
        1,
        NetPolicy::download_client(),
        &bad,
        None,
    )
    .await;
    assert_eq!(
        got.err(),
        Some(TcpRouteError::Route(RouteError::InvalidInterface))
    );
    assert_eq!(dns.lookups.load(Ordering::SeqCst), 0);
    #[cfg(not(target_os = "linux"))]
    {
        let lo = Route::Interface("lo".into());
        let got = connect(&dns, "irc.test", 1, NetPolicy::download_client(), &lo, None).await;
        assert!(matches!(
            got.err(),
            Some(TcpRouteError::Route(RouteError::Unsupported(_)))
        ));
    }
}

// ── SOCKS5 ─────────────────────────────────────────────────────────────

/// What a stub proxy saw: the login and the `CONNECT` destination
/// (address type, address bytes, port).
#[derive(Debug, Clone, PartialEq, Eq)]
struct Seen {
    login: Option<(Vec<u8>, Vec<u8>)>,
    atyp: u8,
    addr: Vec<u8>,
    port: u16,
}

/// How a stub proxy answers.
#[derive(Clone)]
struct Script {
    /// The method it picks.
    method: u8,
    /// Its RFC 1929 status.
    auth_status: u8,
    /// Its `CONNECT` reply, sent with `after` in one write.
    reply: Vec<u8>,
    /// Bytes of the tunnelled stream, sent right behind the reply.
    after: &'static [u8],
    /// Accept and never answer.
    silent: bool,
    /// Close the connection right after the reply.
    close: bool,
}

const OK_V4: &[u8] = &[5, 0, 0, 1, 10, 0, 0, 1, 0x1f, 0x90];

impl Script {
    fn ok() -> Script {
        Script {
            method: 0,
            auth_status: 0,
            reply: OK_V4.to_vec(),
            after: b"",
            silent: false,
            close: false,
        }
    }
}

struct Proxy {
    addr: SocketAddr,
    accepts: Arc<AtomicUsize>,
    seen: mpsc::UnboundedReceiver<Seen>,
}

async fn proxy(script: Script) -> Proxy {
    let listener = TcpListener::bind((LO, 0)).await.unwrap();
    let addr = listener.local_addr().unwrap();
    let accepts = Arc::new(AtomicUsize::new(0));
    let (tx, seen) = mpsc::unbounded_channel();
    let count = Arc::clone(&accepts);
    tokio::spawn(async move {
        while let Ok((s, _)) = listener.accept().await {
            count.fetch_add(1, Ordering::SeqCst);
            let (tx, script) = (tx.clone(), script.clone());
            tokio::spawn(async move {
                let _ = serve(s, script, tx).await;
            });
        }
    });
    Proxy {
        addr,
        accepts,
        seen,
    }
}

async fn serve(mut s: TcpStream, sc: Script, tx: mpsc::UnboundedSender<Seen>) -> io::Result<()> {
    if sc.silent {
        let mut sink = [0u8; 64];
        while s.read(&mut sink).await? > 0 {}
        return Ok(());
    }
    let mut head = [0u8; 2];
    s.read_exact(&mut head).await?;
    let mut methods = vec![0u8; usize::from(head[1])];
    s.read_exact(&mut methods).await?;
    s.write_all(&[5, sc.method]).await?;
    let mut login = None;
    if sc.method == 2 {
        let ver = s.read_u8().await?;
        assert_eq!(ver, 1);
        let mut user = vec![0u8; usize::from(s.read_u8().await?)];
        s.read_exact(&mut user).await?;
        let mut pass = vec![0u8; usize::from(s.read_u8().await?)];
        s.read_exact(&mut pass).await?;
        s.write_all(&[1, sc.auth_status]).await?;
        if sc.auth_status != 0 {
            return Ok(());
        }
        login = Some((user, pass));
    } else if sc.method == 0xff {
        return Ok(());
    }
    let mut req = [0u8; 4];
    s.read_exact(&mut req).await?;
    let addr = match req[3] {
        1 => {
            let mut a = vec![0u8; 4];
            s.read_exact(&mut a).await?;
            a
        }
        4 => {
            let mut a = vec![0u8; 16];
            s.read_exact(&mut a).await?;
            a
        }
        _ => {
            let mut a = vec![0u8; usize::from(s.read_u8().await?)];
            s.read_exact(&mut a).await?;
            a
        }
    };
    let port = s.read_u16().await?;
    let _ = tx.send(Seen {
        login,
        atyp: req[3],
        addr,
        port,
    });
    let mut out = sc.reply.clone();
    out.extend_from_slice(sc.after);
    s.write_all(&out).await?;
    if sc.close {
        return Ok(());
    }
    let mut sink = [0u8; 64];
    while s.read(&mut sink).await? > 0 {}
    Ok(())
}

fn socks(p: &Proxy, dns: SocksDns, auth: Option<(&str, &str)>) -> Route {
    Route::Socks5(Socks5Route {
        proxy: p.addr,
        dns,
        auth: auth.map(|(u, pw)| SocksAuth {
            username: Secret::new(u),
            password: Secret::new(pw),
        }),
    })
}

#[tokio::test]
async fn socks_with_proxy_dns_sends_the_normalised_name_and_hands_over_the_stream() {
    let mut p = proxy(Script {
        after: b"tunnel",
        ..Script::ok()
    })
    .await;
    let dns = Table::with(&[]);
    let route = socks(&p, SocksDns::Proxy, None);
    let mut s = connect(
        &dns,
        "IRC.Example.ORG",
        6697,
        NetPolicy::notification(false),
        &route,
        None,
    )
    .await
    .unwrap();
    // The bytes right behind the reply belong to the tunnel: the
    // handshake must not have consumed them.
    assert_eq!(read_greeting(&mut s, 6).await, b"tunnel");
    let seen = p.seen.recv().await.unwrap();
    assert_eq!(seen.atyp, 3);
    assert_eq!(seen.addr, b"irc.example.org");
    assert_eq!(seen.port, 6697);
    assert_eq!(seen.login, None);
    assert_eq!(dns.lookups.load(Ordering::SeqCst), 0, "no local lookup");
}

#[tokio::test]
async fn socks_with_local_dns_sends_a_checked_address() {
    let mut p = proxy(Script::ok()).await;
    let public: IpAddr = "93.184.215.14".parse().unwrap();
    let dns = Table::with(&[("irc.test", &[public])]);
    let route = socks(&p, SocksDns::Local, None);
    connect(
        &dns,
        "irc.test",
        6697,
        NetPolicy::notification(false),
        &route,
        None,
    )
    .await
    .unwrap();
    let seen = p.seen.recv().await.unwrap();
    assert_eq!(
        (seen.atyp, seen.addr, seen.port),
        (1, vec![93, 184, 215, 14], 6697)
    );
    assert_eq!(dns.lookups.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn socks_login_is_sent_and_a_refusal_or_downgrade_fails() {
    let policy = NetPolicy::notification(false);
    let dns = Table::with(&[]);

    let mut p = proxy(Script {
        method: 2,
        ..Script::ok()
    })
    .await;
    let route = socks(&p, SocksDns::Proxy, Some(("alice", "s3cret")));
    connect(&dns, "irc.example.org", 6697, policy, &route, None)
        .await
        .unwrap();
    let seen = p.seen.recv().await.unwrap();
    assert_eq!(seen.login, Some((b"alice".to_vec(), b"s3cret".to_vec())));

    let p = proxy(Script {
        method: 2,
        auth_status: 1,
        ..Script::ok()
    })
    .await;
    let route = socks(&p, SocksDns::Proxy, Some(("alice", "wrong")));
    let got = connect(&dns, "irc.example.org", 6697, policy, &route, None).await;
    assert_eq!(
        got.err(),
        Some(TcpRouteError::Socks(SocksError::AuthRejected))
    );
    let msg = TcpRouteError::Socks(SocksError::AuthRejected).to_string();
    assert!(!msg.contains("alice") && !msg.contains("wrong"), "{msg}");

    // A login is configured but the proxy picks "no authentication".
    let p = proxy(Script::ok()).await;
    let route = socks(&p, SocksDns::Proxy, Some(("alice", "s3cret")));
    let got = connect(&dns, "irc.example.org", 6697, policy, &route, None).await;
    assert_eq!(
        got.err(),
        Some(TcpRouteError::Socks(SocksError::NoAcceptableMethod))
    );

    // Logins RFC 1929 can't carry fail before anything is sent.
    let route = socks(&p, SocksDns::Proxy, Some(("", "pw")));
    let got = connect(&dns, "irc.example.org", 6697, policy, &route, None).await;
    assert_eq!(
        got.err(),
        Some(TcpRouteError::Route(RouteError::InvalidCredentials))
    );
    assert_eq!(p.accepts.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn malformed_or_refusing_proxy_replies_fail_cleanly() {
    let dns = Table::with(&[]);
    let policy = NetPolicy::notification(false);
    // `None` means "malformed".
    let cases: &[(&[u8], Option<SocksError>)] = &[
        (&[4, 0, 0, 1, 0, 0, 0, 0, 0, 0], None),
        (&[5, 0, 7, 1, 0, 0, 0, 0, 0, 0], None),
        (&[5, 0, 0, 9, 0, 0, 0, 0, 0, 0], None),
        (&[5, 0, 0, 3, 0, 0, 0], None),
        (&[5, 0, 0, 3, 3, b'a', 0, b'c', 0, 0], None),
        // Truncated, then the proxy closes.
        (&[5, 0, 0, 4, 0, 0, 0], None),
        (
            &[5, 5, 0, 1, 0, 0, 0, 0, 0, 0],
            Some(SocksError::Refused(5)),
        ),
    ];
    for (reply, want) in cases {
        let p = proxy(Script {
            reply: reply.to_vec(),
            close: true,
            ..Script::ok()
        })
        .await;
        let route = socks(&p, SocksDns::Proxy, None);
        let got = connect(&dns, "irc.example.org", 6697, policy, &route, None).await;
        match got {
            Err(TcpRouteError::Socks(e)) => match want {
                Some(w) => assert_eq!(e, *w, "{reply:?}"),
                None => assert!(matches!(e, SocksError::Malformed(_)), "{reply:?}: {e:?}"),
            },
            other => panic!("{reply:?}: {other:?}"),
        }
    }
    // A proxy that accepts none of the offered methods.
    let p = proxy(Script {
        method: 0xff,
        ..Script::ok()
    })
    .await;
    let route = socks(&p, SocksDns::Proxy, None);
    let got = connect(&dns, "irc.example.org", 6697, policy, &route, None).await;
    assert_eq!(
        got.err(),
        Some(TcpRouteError::Socks(SocksError::NoAcceptableMethod))
    );
}

#[tokio::test]
async fn an_unreachable_proxy_fails_without_a_direct_fallback() {
    let srv = server(b"").await;
    // Bind and drop a listener so its port is (very likely) closed.
    let closed = TcpListener::bind((LO, 0))
        .await
        .unwrap()
        .local_addr()
        .unwrap();
    let route = Route::Socks5(Socks5Route {
        proxy: closed,
        dns: SocksDns::Local,
        auth: None,
    });
    let dns = Table::with(&[]);
    let got = connect(
        &dns,
        "127.0.0.1",
        srv.addr.port(),
        NetPolicy::download_client(),
        &route,
        None,
    )
    .await;
    assert_eq!(got.err(), Some(TcpRouteError::Net(NetError::Connect)));
    assert_eq!(srv.accepts.load(Ordering::SeqCst), 0);
}

// ── Kill-switch ────────────────────────────────────────────────────────

#[tokio::test]
async fn a_down_kill_switch_refuses_before_any_lookup_or_connection() {
    let p = proxy(Script::ok()).await;
    let dns = Table::with(&[("irc.test", &[LO])]);
    let (_tx, rx) = watch::channel(false);
    for route in [Route::Direct, socks(&p, SocksDns::Local, None)] {
        let got = connect(
            &dns,
            "irc.test",
            p.addr.port(),
            NetPolicy::download_client(),
            &route,
            Some(&rx),
        )
        .await;
        assert_eq!(got.err(), Some(TcpRouteError::KillSwitch));
    }
    // A switch whose sender is gone can't come back up: it fails closed.
    let (tx, rx) = watch::channel(true);
    drop(tx);
    let got = connect(
        &dns,
        "irc.test",
        p.addr.port(),
        NetPolicy::download_client(),
        &Route::Direct,
        Some(&rx),
    )
    .await;
    assert_eq!(got.err(), Some(TcpRouteError::KillSwitch));
    assert_eq!(dns.lookups.load(Ordering::SeqCst), 0);
    assert_eq!(p.accepts.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn the_kill_switch_abandons_a_connection_in_flight() {
    let p = proxy(Script {
        silent: true,
        ..Script::ok()
    })
    .await;
    let dns = Table::with(&[]);
    let (tx, rx) = watch::channel(true);
    let route = socks(&p, SocksDns::Proxy, None);
    let accepts = Arc::clone(&p.accepts);
    let flip = tokio::spawn(async move {
        // Wait (with a deadline) until the proxy has the connection, then
        // take the route down mid-handshake.
        let start = tokio::time::Instant::now();
        while accepts.load(Ordering::SeqCst) == 0 {
            assert!(
                start.elapsed() < DEADLINE,
                "the proxy never saw a connection"
            );
            tokio::task::yield_now().await;
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        tx.send(false).unwrap();
        tx
    });
    // The handshake would wait out this long timeout; the switch must end
    // it well before.
    let started = std::time::Instant::now();
    let got = tokio::time::timeout(
        DEADLINE,
        connect_tcp_routed(
            &*dns,
            "irc.example.org",
            6697,
            NetPolicy::notification(false),
            &route,
            Some(&rx),
            Duration::from_secs(600),
        ),
    )
    .await
    .expect("the kill-switch ends the attempt");
    assert_eq!(got.err(), Some(TcpRouteError::KillSwitch));
    assert!(started.elapsed() < DEADLINE);
    let _tx = flip.await.unwrap();
}
