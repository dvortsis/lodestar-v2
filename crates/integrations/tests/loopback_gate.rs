//! The loopback gate (X-cmp09-fix-2). Its own test binary: the gate is
//! set once per process, so everything runs in one test, in order.

use std::collections::{HashMap, HashSet};
use std::io;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use lodestar_integrations::BoxFuture;
use lodestar_integrations::net::{
    GuardedClient, LoopbackGate, NetError, NetPolicy, Resolve, loopback_gate_is_set, resolve_tcp,
    set_loopback_gate,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

const TIMEOUT: Duration = Duration::from_secs(15);

/// Refuses the ports in its set.
#[derive(Default)]
struct Refusing(Mutex<HashSet<u16>>);

impl LoopbackGate for Refusing {
    fn allow(&self, port: u16) -> bool {
        !self.0.lock().unwrap().contains(&port)
    }
}

/// Fixed answers per name, the `n`th lookup of a name taking the `n`th
/// answer (the last repeats).
struct Answers {
    names: HashMap<&'static str, Vec<IpAddr>>,
    seen: Mutex<HashMap<String, usize>>,
}

impl Answers {
    fn new(names: &[(&'static str, Vec<IpAddr>)]) -> Arc<Answers> {
        Arc::new(Answers {
            names: names.iter().cloned().collect(),
            seen: Mutex::new(HashMap::new()),
        })
    }
}

impl Resolve for Answers {
    fn resolve<'a>(&'a self, host: &'a str) -> BoxFuture<'a, io::Result<Vec<IpAddr>>> {
        let mut seen = self.seen.lock().unwrap();
        let n = seen.entry(host.to_owned()).or_default();
        let got = self.names.get(host).map(|a| vec![a[(*n).min(a.len() - 1)]]);
        *n += 1;
        Box::pin(async move { got.ok_or_else(|| io::Error::other("no such name")) })
    }
}

/// An HTTP server on 127.0.0.1 counting requests. `/hop` answers 307 to
/// `location`; anything else answers 200.
struct Server {
    port: u16,
    hits: Arc<AtomicUsize>,
}

async fn server(location: Option<String>) -> Server {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let hits = Arc::new(AtomicUsize::new(0));
    let count = Arc::clone(&hits);
    tokio::spawn(async move {
        while let Ok((mut sock, _)) = listener.accept().await {
            let (count, location) = (Arc::clone(&count), location.clone());
            tokio::spawn(async move {
                let mut buf = vec![0u8; 4096];
                let n = sock.read(&mut buf).await.unwrap_or(0);
                count.fetch_add(1, Ordering::SeqCst);
                let reply = match location {
                    Some(to) if buf[..n].starts_with(b"GET /hop ") => format!(
                        "HTTP/1.1 307 Temporary Redirect\r\nLocation: {to}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                    ),
                    _ => "HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok"
                        .to_owned(),
                };
                let _ = sock.write_all(reply.as_bytes()).await;
            });
        }
    });
    Server { port, hits }
}

fn client(resolver: Arc<dyn Resolve>) -> GuardedClient {
    GuardedClient::with_resolver(NetPolicy::download_client(), TIMEOUT, resolver).unwrap()
}

async fn get(c: &GuardedClient, url: &str) -> Result<u16, NetError> {
    c.exchange("GET", url, &[], Vec::new(), 1024)
        .await
        .map(|e| e.status)
}

async fn follow(c: &GuardedClient, url: &str) -> Result<u16, NetError> {
    let req = reqwest::Request::new(reqwest::Method::GET, url.parse().unwrap());
    c.send_following(req).await.map(|r| r.status().as_u16())
}

#[tokio::test(flavor = "multi_thread")]
async fn the_loopback_gate_refuses_a_port_until_allowed() {
    let managed = server(None).await;
    let p = managed.port;
    let other = server(Some(format!("http://127.0.0.1:{p}/"))).await;
    let q = other.port;
    let lo = IpAddr::V4(Ipv4Addr::LOCALHOST);
    let lan = IpAddr::V4(Ipv4Addr::new(192, 168, 77, 9));
    let names = Answers::new(&[
        ("svc.test", vec![lo]),
        ("localhost", vec![lo]),
        // Answers a LAN address first, loopback after.
        ("turns.test", vec![lan, lo]),
    ]);
    let c = client(names.clone());

    // With no gate, nothing changes.
    assert!(!loopback_gate_is_set());
    assert_eq!(get(&c, &format!("http://127.0.0.1:{p}/")).await, Ok(200));

    let gate = Arc::new(Refusing::default());
    gate.0.lock().unwrap().insert(p);
    set_loopback_gate(gate.clone()).unwrap();
    assert!(loopback_gate_is_set());
    // Set once: a second gate is refused, and the first stays.
    assert!(set_loopback_gate(Arc::new(Refusing::default())).is_err());

    let before = managed.hits.load(Ordering::SeqCst);
    for url in [
        format!("http://127.0.0.1:{p}/"),
        format!("http://[::1]:{p}/"),
        format!("http://localhost:{p}/"),
        format!("http://svc.test:{p}/"),
        format!("http://127.0.0.2:{p}/"),
        // A name that doesn't resolve now, on a refused port.
        format!("http://nowhere.test:{p}/"),
    ] {
        assert_eq!(get(&c, &url).await, Err(NetError::NotReady), "{url}");
    }
    // A name that turns to loopback between the request's check and its
    // connection is refused at the connection.
    assert_eq!(
        get(&c, &format!("http://turns.test:{p}/")).await,
        Err(NetError::NotReady)
    );
    // A redirect hop to the port.
    assert_eq!(
        follow(&c, &format!("http://127.0.0.1:{q}/hop")).await,
        Err(NetError::NotReady)
    );
    assert_eq!(
        managed.hits.load(Ordering::SeqCst),
        before,
        "nothing reached it"
    );
    // Raw TCP names and literals alike.
    for host in ["127.0.0.1", "::1", "localhost"] {
        let got = resolve_tcp(&*names, host, p, NetPolicy::download_client()).await;
        assert_eq!(got, Err(NetError::NotReady), "{host}");
    }

    // Another loopback port is untouched, and so is the same port on
    // another address.
    assert_eq!(get(&c, &format!("http://127.0.0.1:{q}/")).await, Ok(200));
    let lan_tcp = resolve_tcp(&*names, "192.168.77.9", p, NetPolicy::download_client()).await;
    assert_eq!(lan_tcp.map(|a| a.len()), Ok(1));
    // The companions' own client, which applies its own rule.
    let own = client(names.clone()).without_loopback_gate();
    assert_eq!(get(&own, &format!("http://127.0.0.1:{p}/")).await, Ok(200));

    // Allowed: through again.
    gate.0.lock().unwrap().remove(&p);
    assert_eq!(get(&c, &format!("http://127.0.0.1:{p}/")).await, Ok(200));
    assert_eq!(
        follow(&c, &format!("http://127.0.0.1:{q}/hop")).await,
        Ok(200)
    );
    let v6 = resolve_tcp(&*names, "::1", p, NetPolicy::download_client()).await;
    assert_eq!(v6.map(|a| a[0].ip()), Ok(IpAddr::V6(Ipv6Addr::LOCALHOST)));
}

/// `without_loopback_gate` is for the companions' own client alone: no
/// other source file in the workspace names it, in any form of call.
#[test]
fn only_the_companions_client_opts_out_of_the_gate() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut callers = Vec::new();
    let mut stack = vec![root.join("crates"), root.join("fuzz"), root.join("tools")];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                if path
                    .file_name()
                    .is_some_and(|n| n == "target" || n == "node_modules")
                {
                    continue;
                }
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == "rs") {
                let text = std::fs::read_to_string(&path).unwrap_or_default();
                if text.contains("without_loopback_gate") {
                    let rel = path
                        .strip_prefix(&root)
                        .unwrap()
                        .to_string_lossy()
                        .replace('\\', "/");
                    callers.push(rel);
                }
            }
        }
    }
    callers.sort();
    assert_eq!(
        callers,
        [
            "crates/companions/src/detect/mod.rs",
            "crates/integrations/src/net.rs",
            "crates/integrations/tests/loopback_gate.rs"
        ],
        "a new caller of without_loopback_gate needs the runner's agreement"
    );
}
