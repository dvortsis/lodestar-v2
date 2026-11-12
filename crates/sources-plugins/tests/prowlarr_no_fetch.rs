//! The Legal Notice (section 2) says Lodestar "never downloads a `.torrent`
//! file from any indexer" through Prowlarr or Jackett. These tests back
//! that sentence (X-prowlarr-register-followups):
//!
//! - **Behaviour:** the plugin polls a fake Prowlarr on loopback whose
//!   indexer list and RSS items point every URL (`link`, `enclosure`,
//!   `guid`, `comments`, the indexers' own addresses) at a second loopback
//!   listener, the trap. Every request the plugin makes goes out over a real
//!   connection to the host and port in its URL, so a fetched item link
//!   would reach the trap. The trap must see none.
//! - **Source text:** in `src/prowlarr.rs` every request is built from the
//!   configured base URL, and the plugin sends only the two requests it
//!   builds (the indexer list and an indexer's page).
//!
//! Both fake servers bind to 127.0.0.1 only and answer nothing but the
//! requests below. Every wait is a condition with a deadline of at least
//! 10 s; nothing sleeps for a fixed time.

use std::io::{Read, Write};
use std::net::{Ipv4Addr, Shutdown, TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use lodestar_sources::testing::{FakeCtx, wait_for};
use lodestar_sources::{BoxFuture, Fetch, HttpError, Request, Response};
use lodestar_sources_plugins::StaticCredentials;
use lodestar_sources_plugins::prowlarr::{Config, Manager, Prowlarr};
use lodestar_store::SourceId;

/// Socket reads and writes give up after this, so a broken exchange fails
/// the test instead of hanging it.
const IO_TIMEOUT: Duration = Duration::from_secs(10);
/// The largest request head or response either side reads.
const MAX_READ: usize = 1 << 20;

fn fixture(case: &str, name: &str) -> String {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures/prowlarr")
        .join(case)
        .join(name);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display()))
}

/// Every host the recorded bodies name, besides XML namespaces.
const RECORDED_HOSTS: [&str; 6] = [
    "http://prowlarr.example:9696",
    "http://jackett.example:9117",
    "https://public.example",
    "https://private.example",
    "https://semi.example",
    "https://wiki.servarr.com",
];

/// Points every URL the recorded bodies carry at the trap.
fn trapped(body: &str, trap: &str) -> String {
    RECORDED_HOSTS
        .iter()
        .fold(body.to_owned(), |b, host| b.replace(host, trap))
}

/// Reads until the end of the head (`\r\n\r\n`), or the peer closes.
fn read_head(s: &mut TcpStream) -> String {
    let mut buf = Vec::new();
    let mut byte = [0u8; 1];
    while !buf.ends_with(b"\r\n\r\n") && buf.len() < MAX_READ {
        match s.read(&mut byte) {
            Ok(1) => buf.push(byte[0]),
            _ => break,
        }
    }
    String::from_utf8_lossy(&buf).into_owned()
}

/// A loopback HTTP server on its own thread: answers each request line
/// with `answer`, counts connections and keeps the request lines.
struct Server {
    origin: String,
    conns: Arc<AtomicUsize>,
    lines: Arc<Mutex<Vec<String>>>,
}

impl Server {
    /// Bound to 127.0.0.1 only; `name` is the host its origin is written
    /// with (`127.0.0.1` or `localhost`).
    fn start(name: &str, answer: impl Fn(&str) -> (u16, String) + Send + 'static) -> Server {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let origin = format!("http://{name}:{port}");
        let conns = Arc::new(AtomicUsize::new(0));
        let lines = Arc::new(Mutex::new(Vec::new()));
        let (c, l) = (conns.clone(), lines.clone());
        // Ends with the test process; it only ever accepts loopback peers.
        std::thread::spawn(move || {
            for sock in listener.incoming() {
                let Ok(mut sock) = sock else { continue };
                c.fetch_add(1, Ordering::SeqCst);
                let _ = sock.set_read_timeout(Some(IO_TIMEOUT));
                let _ = sock.set_write_timeout(Some(IO_TIMEOUT));
                let head = read_head(&mut sock);
                let line = head.lines().next().unwrap_or_default().to_owned();
                l.lock().unwrap().push(line.clone());
                let (status, body) = answer(&line);
                let reply = format!(
                    "HTTP/1.1 {status} X\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = sock.write_all(reply.as_bytes());
                let _ = sock.shutdown(Shutdown::Both);
            }
        });
        Server {
            origin,
            conns,
            lines,
        }
    }

    fn lines(&self) -> Vec<String> {
        self.lines.lock().unwrap().clone()
    }
}

/// Sends each request over a real connection to the host and port in its
/// URL (loopback only), so the plugin can't reach anything this test
/// didn't start, and keeps every URL it was asked for.
#[derive(Default)]
struct Loopback {
    urls: Mutex<Vec<String>>,
}

fn exchange(req: &Request) -> Result<Response, String> {
    let rest = req.url.strip_prefix("http://").ok_or("not http")?;
    let (authority, path) = rest.split_at(rest.find('/').unwrap_or(rest.len()));
    // Loopback only, whatever the name: `127.0.0.1` is the manager and
    // `localhost` the trap (a host of its own, so the plugin's per-host
    // pacing never delays a stray request past the test's wait).
    let (host, port) = authority.rsplit_once(':').ok_or("no port")?;
    let port: u16 = port.parse().map_err(|_| "bad port")?;
    if !matches!(host, "127.0.0.1" | "localhost") {
        return Err(format!("not loopback: {authority}"));
    }
    let path = if path.is_empty() { "/" } else { path };
    let mut s = TcpStream::connect((Ipv4Addr::LOCALHOST, port)).map_err(|e| e.to_string())?;
    s.set_read_timeout(Some(IO_TIMEOUT))
        .map_err(|e| e.to_string())?;
    s.set_write_timeout(Some(IO_TIMEOUT))
        .map_err(|e| e.to_string())?;
    let mut head = format!("GET {path} HTTP/1.1\r\nHost: {authority}\r\nConnection: close\r\n");
    for (n, v) in &req.headers {
        head.push_str(&format!("{n}: {v}\r\n"));
    }
    head.push_str("\r\n");
    s.write_all(head.as_bytes()).map_err(|e| e.to_string())?;
    let mut raw = Vec::new();
    s.take(MAX_READ as u64)
        .read_to_end(&mut raw)
        .map_err(|e| e.to_string())?;
    let split = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or("no head")?;
    let head = String::from_utf8_lossy(&raw[..split]);
    let status = head
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .ok_or("no status")?;
    Ok(Response {
        status,
        headers: Vec::new(),
        body: raw[split + 4..].to_vec(),
    })
}

impl Fetch for Loopback {
    fn fetch<'a>(&'a self, req: &'a Request) -> BoxFuture<'a, Result<Response, HttpError>> {
        self.urls.lock().unwrap().push(req.url.clone());
        let req = req.clone();
        Box::pin(async move {
            tokio::task::spawn_blocking(move || exchange(&req))
                .await
                .map_err(|e| HttpError::Transport(e.to_string()))?
                .map_err(HttpError::Transport)
        })
    }
}

/// Runs one cycle of the plugin as `manager` against the fake manager
/// serving `pages` (request path, recorded body in `case`), with every URL
/// in those bodies pointing at the trap, and checks that nothing but those
/// paths was asked for and the trap was never reached.
async fn never_fetches_an_item(manager: Manager, case: &str, pages: &[(&str, &str)], finds: usize) {
    let trap = Server::start("localhost", |_| (200, "d4:infoe".into()));
    let t = trap.origin.clone();
    let served: Vec<(String, String)> = pages
        .iter()
        .map(|(path, file)| ((*path).to_owned(), trapped(&fixture(case, file), &t)))
        .collect();
    for (_, body) in &served {
        assert!(
            RECORDED_HOSTS.iter().all(|h| !body.contains(h)),
            "a URL escaped the trap: {body}"
        );
    }
    assert!(
        served
            .iter()
            .any(|(_, b)| b.contains(&format!("<link>{t}/"))),
        "the items link to the trap"
    );
    let answers = served.clone();
    let manager_server = Server::start("127.0.0.1", move |line| {
        let path = line.split_whitespace().nth(1).unwrap_or_default();
        match answers.iter().find(|(p, _)| p == path) {
            Some((_, body)) => (200, body.clone()),
            None => (404, String::new()),
        }
    });

    let p = Arc::new(Prowlarr::new(
        Config::new(&manager_server.origin, manager, Some(2)).unwrap(),
        Arc::new(StaticCredentials::new([("api_key", "REDACTED")])),
    ));
    let fetch = Arc::new(Loopback::default());
    let mut ctx = FakeCtx::with_fetch(&*p, SourceId::new(5).unwrap(), fetch.clone());
    let running = ctx.start(p.clone());

    // One full cycle: the list and each torrent indexer's page, and the
    // finds those pages hold (as `prowlarr.rs`'s own tests count them). A
    // request to the trap ends the wait at once, so it fails below by name.
    let mut got = Vec::new();
    wait_for(
        "the cycle's requests and finds, or a request to the trap",
        || {
            got.extend(ctx.drain());
            trap.conns.load(Ordering::SeqCst) > 0
                || (manager_server.lines().len() >= pages.len() && got.len() >= finds)
        },
    )
    .await;
    running.stop_within().await.unwrap();

    let ours = format!("{}/", manager_server.origin);
    let urls = fetch.urls.lock().unwrap().clone();
    assert!(
        urls.iter().all(|u| u.starts_with(&ours)),
        "the plugin asked for a URL outside the manager's address: {urls:#?}"
    );
    assert_eq!(
        trap.conns.load(Ordering::SeqCst),
        0,
        "the plugin fetched an item's URL: {:?}",
        trap.lines()
    );
    let mut lines = manager_server.lines();
    lines.sort();
    let mut want: Vec<String> = pages
        .iter()
        .map(|(p, _)| format!("GET {p} HTTP/1.1"))
        .collect();
    want.sort();
    assert_eq!(
        lines, want,
        "only the list and the pages, never a download path"
    );
    assert_eq!(got.len(), finds, "{got:#?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn prowlarr_never_requests_an_items_link() {
    never_fetches_an_item(
        Manager::Prowlarr,
        "prowlarr_rss",
        &[
            ("/api/v1/indexer", "001.body"),
            ("/1/api?t=search&extended=1&limit=100", "002.body"),
            ("/2/api?t=search&extended=1&limit=100", "003.body"),
        ],
        3,
    )
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn jackett_never_requests_an_items_link() {
    let api = "/api/v2.0/indexers";
    let page = "results/torznab/api?apikey=REDACTED&t=search&extended=1&limit=100";
    never_fetches_an_item(
        Manager::Jackett,
        "jackett_rss",
        &[
            (
                &format!(
                    "{api}/all/results/torznab/api?apikey=REDACTED&t=indexers&configured=true"
                ),
                "001.body",
            ),
            (&format!("{api}/examplepublic/{page}"), "002.body"),
            (&format!("{api}/example-semi/{page}"), "003.body"),
        ],
        2,
    )
    .await;
}

/// The text of `src/prowlarr.rs` before its unit tests, if it has any.
fn plugin_source() -> String {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/prowlarr.rs");
    let src = std::fs::read_to_string(&p)
        .unwrap_or_else(|e| panic!("{}: {e}", p.display()))
        .replace("\r\n", "\n");
    match src.find("\n#[cfg(test)]\nmod tests") {
        Some(at) => src[..at].to_owned(),
        None => src,
    }
}

/// Code lines only: whole-line `//` comments (doc comments included) are
/// dropped, so prose about requests never counts. A trailing comment or a
/// `/* */` block that mentions a request would still count; that can only
/// fail the check, never let a fetch through.
fn code_lines(src: &str) -> Vec<&str> {
    src.lines()
        .map(str::trim_start)
        .filter(|l| !l.starts_with("//"))
        .collect()
}

/// Every request the plugin builds starts at the configured base URL, and
/// it sends only the two it builds. A change that requests an item's link
/// (or any URL taken from a response) fails here until this test, and the
/// Legal Notice's section 2, say otherwise.
#[test]
fn every_request_is_built_from_the_configured_base() {
    let src = plugin_source();
    let code = code_lines(&src);
    // The exact patterns: a request is `Request::get(format!(` followed,
    // on the same line or the next, by a string starting `"{base}/`.
    let mut built = 0;
    for (i, line) in code.iter().enumerate() {
        if !line.contains("Request::") {
            continue;
        }
        built += 1;
        let next = code.get(i + 1).copied().unwrap_or_default();
        let from_base = line.contains("Request::get(format!(\"{base}/")
            || (line.ends_with("Request::get(format!(") && next.starts_with("\"{base}/"));
        assert!(
            from_base,
            "src/prowlarr.rs builds a request that doesn't start at the configured base URL \
             (Legal Notice section 2: never downloads a .torrent from any indexer): {line}"
        );
    }
    assert_eq!(
        built, 4,
        "src/prowlarr.rs builds a different number of requests (the list and an indexer's page, \
         for each manager); check each against Legal Notice section 2"
    );
    // `base` is the configured address, and nothing else: `Config` sets it
    // from the checked address, and the two builders read it from there.
    let bases: Vec<&&str> = code
        .iter()
        .filter(|l| l.contains("let base") || l.contains("let (base"))
        .collect();
    assert_eq!(
        bases,
        [
            &"let base = parsed.as_str().trim_end_matches('/').to_owned();",
            &"let base = &self.config.base;",
            &"let (base, id) = (&self.config.base, &ix.id);",
        ],
        "src/prowlarr.rs's `base` no longer comes from the configured address alone"
    );
    // No request is built field by field: the only `Request {` are the two
    // builders' signatures.
    let literal: Vec<&&str> = code.iter().filter(|l| l.contains("Request {")).collect();
    assert_eq!(
        literal,
        [
            &"fn list_request(&self) -> Request {",
            &"fn page_request(&self, ix: &Indexer) -> Request {",
        ],
        "src/prowlarr.rs builds a request without Request::get; check it against Legal Notice \
         section 2"
    );
    // It sends only the requests it built, through any client handle: every
    // `.http.`, `.send(`, `.poll(` and `.poll_stream(` (the last-but-one also
    // matches `Job::poll`, the indexer page's own step).
    let sends: Vec<&&str> = code
        .iter()
        .filter(|l| {
            [".http.", ".send(", ".poll(", ".poll_stream("]
                .iter()
                .any(|p| l.contains(p))
        })
        .collect();
    assert_eq!(
        sends,
        [
            &"let res = self.http.poll(\"indexers\", &self.list_request()).await;",
            &"let body = answer(self.http.poll(&unit, &self.page_request(ix)).await)?;",
            &"match job.poll(&ix).await {",
        ],
        "src/prowlarr.rs sends a request other than the indexer list and an indexer's page"
    );
}
