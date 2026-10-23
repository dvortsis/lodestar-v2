//! `/logs` end to end through the router, over a fake [`LogSource`]: the
//! stream over a real socket (headers, `retry:`, cursors and resyncs,
//! backpressure, purges, caps, the session ending, shutdown), and
//! history's parameters and read limit.
//! The real source's semantics (the ring, masks, files) are tested in
//! `crates/lodestar/src/logging`.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use axum::{Extension, Router};
use lodestar_api::auth::ProxyConfig;
use lodestar_api::logs::{
    HistoryQuery, LineFilter, LogComponent, LogDataKind, LogHistory, LogLevel, LogLine, LogPurge,
    LogSource, LogStreamConfig, LogsError, RingRead, Transport,
};
use lodestar_api::{ApiState, Auth, router};
use lodestar_log::{Component, Level};
use lodestar_store::{Store, StoreConfig};
use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::watch;
use tower::ServiceExt;

const EPOCH: &str = "0123456789abcdef";

/// A ring of numbered lines.
#[derive(Debug)]
struct Fake {
    transport: Transport,
    lines: Mutex<Vec<(u64, Level, String)>>,
    cap: usize,
    head: Mutex<u64>,
    wake: watch::Sender<u64>,
    purges: Mutex<Vec<LogPurge>>,
    history_delay: Duration,
    reads: AtomicUsize,
}

impl Fake {
    fn new(config: LogStreamConfig, cap: usize) -> Arc<Fake> {
        Arc::new(Fake {
            transport: Transport::new(config),
            lines: Mutex::default(),
            cap,
            head: Mutex::new(0),
            wake: watch::channel(0).0,
            purges: Mutex::default(),
            history_delay: Duration::from_millis(400),
            reads: AtomicUsize::new(0),
        })
    }

    fn push(&self, level: Level, msg: &str) -> u64 {
        let mut head = self.head.lock().unwrap();
        *head += 1;
        let mut lines = self.lines.lock().unwrap();
        lines.push((*head, level, msg.to_string()));
        if lines.len() > self.cap {
            lines.remove(0);
        }
        self.wake.send_replace(*head);
        *head
    }

    fn purge(&self, through: u64) {
        self.purges.lock().unwrap().push(LogPurge {
            kinds: vec![LogDataKind::Name],
            through: format!("{EPOCH}-{through}"),
        });
        self.wake.send_replace(through);
    }
}

fn line(seq: u64, level: Level, msg: &str) -> LogLine {
    LogLine {
        component: LogComponent::Fetch,
        event: Some("fake.line".into()),
        fields: vec![],
        id: Some(format!("{EPOCH}-{seq}")),
        level: match level {
            Level::Error => LogLevel::Error,
            Level::Warn => LogLevel::Warn,
            Level::Info => LogLevel::Info,
            Level::Debug => LogLevel::Debug,
            Level::Trace => LogLevel::Trace,
        },
        msg: msg.to_string(),
        opt_in: None,
        repeated: None,
        target: None,
        truncated: false,
        ts: "2026-10-23T10:15:00.000Z".into(),
    }
}

impl LogSource for Fake {
    fn transport(&self) -> &Transport {
        &self.transport
    }
    fn epoch(&self) -> &str {
        EPOCH
    }
    fn bounds(&self) -> (u64, u64) {
        let head = *self.head.lock().unwrap();
        let oldest = self.lines.lock().unwrap().first().map_or(head + 1, |l| l.0);
        (head, oldest)
    }
    fn listen(&self) -> (Box<dyn Send + Sync>, watch::Receiver<u64>) {
        (Box::new(()), self.wake.subscribe())
    }
    fn after(&self, cursor: u64, filter: &LineFilter, max: usize) -> RingRead {
        let lines = self.lines.lock().unwrap();
        let head = *self.head.lock().unwrap();
        let oldest = lines.first().map_or(head + 1, |l| l.0);
        let mut out = RingRead {
            evicted: (oldest - 1).saturating_sub(cursor),
            head,
            ..RingRead::default()
        };
        for (seq, level, msg) in lines.iter().filter(|l| l.0 > cursor) {
            let ok = filter.level_ok(*level)
                && filter.component_ok(Component::Fetch)
                && filter
                    .text
                    .as_ref()
                    .is_none_or(|t| msg.to_lowercase().contains(t));
            if ok {
                out.matching += 1;
                if out.lines.len() < max {
                    out.lines.push((*seq, line(*seq, *level, msg)));
                }
            }
        }
        out
    }
    fn generation(&self) -> u64 {
        self.purges.lock().unwrap().len() as u64
    }
    fn purges_since(&self, generation: u64) -> (u64, Vec<LogPurge>) {
        let p = self.purges.lock().unwrap();
        (
            p.len() as u64,
            p.iter().skip(generation as usize).cloned().collect(),
        )
    }
    fn purges_covering(&self, seq: u64) -> Vec<LogPurge> {
        self.purges
            .lock()
            .unwrap()
            .iter()
            .filter(|p| {
                p.through
                    .rsplit('-')
                    .next()
                    .and_then(|n| n.parse::<u64>().ok())
                    .is_some_and(|n| n >= seq)
            })
            .cloned()
            .collect()
    }
    fn history(&self, _: &HistoryQuery) -> Result<LogHistory, LogsError> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        std::thread::sleep(self.history_delay);
        Ok(LogHistory {
            lines: vec![],
            more: false,
            next_cursor: None,
            unreadable: 0,
        })
    }
}

struct Fixture {
    app: Router,
    auth: Arc<Auth>,
    _dir: tempfile::TempDir,
}

fn fixture(source: Option<Arc<Fake>>) -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("t.db");
    drop(Store::open(&db, StoreConfig::default()).unwrap());
    let reader = Store::open_reader(&db, StoreConfig::default()).unwrap();
    let auth = Arc::new(Auth::load(&dir.path().join("auth.json"), false).unwrap());
    let mut app = Router::new().nest(
        "/api/v1",
        router(ApiState {
            auth: auth.clone(),
            index: None,
            reader: Arc::new(Mutex::new(reader)),
            filters: None,
            settings: None,
            ops: None,
            user_data: None,
            adult: Default::default(),
            insights: None,
            hot: None,
            events: None,
        }),
    );
    if let Some(s) = source {
        app = app.layer(Extension(s as Arc<dyn LogSource>));
    }
    Fixture {
        app,
        auth,
        _dir: dir,
    }
}

fn config() -> LogStreamConfig {
    LogStreamConfig {
        heartbeat: Duration::from_millis(100),
        max_streams: 3,
        ..LogStreamConfig::default()
    }
}

const PASSWORD: &str = "correct horse battery";

fn session(auth: &Auth) -> String {
    let set = match auth.setup_token() {
        Some(token) => auth.setup([127, 0, 0, 1].into(), &token, PASSWORD).unwrap(),
        None => auth.login([127, 0, 0, 1].into(), PASSWORD).unwrap(),
    };
    set.split(';').next().unwrap().to_string()
}

async fn serve(app: Router) -> SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        .unwrap();
    });
    addr
}

struct Conn {
    stream: TcpStream,
    text: String,
}

impl Conn {
    async fn open(addr: SocketAddr, path: &str, headers: &[(&str, &str)]) -> Conn {
        let mut stream = TcpStream::connect(addr).await.unwrap();
        let mut req = format!("GET {path} HTTP/1.1\r\nHost: {addr}\r\n");
        for (k, v) in headers {
            req.push_str(&format!("{k}: {v}\r\n"));
        }
        req.push_str("\r\n");
        stream.write_all(req.as_bytes()).await.unwrap();
        Conn {
            stream,
            text: String::new(),
        }
    }

    async fn until(&mut self, needle: &str, limit: Duration) {
        let start = Instant::now();
        let mut buf = [0u8; 8192];
        while !self.text.contains(needle) {
            let left = limit.saturating_sub(start.elapsed());
            let n = tokio::time::timeout(left, self.stream.read(&mut buf))
                .await
                .unwrap_or_else(|_| panic!("no {needle:?} within {limit:?}; got {:?}", self.text))
                .unwrap();
            assert!(n > 0, "closed before {needle:?}; got {:?}", self.text);
            self.text.push_str(&String::from_utf8_lossy(&buf[..n]));
        }
    }

    /// Reads until the server ends the stream.
    async fn until_closed(&mut self, limit: Duration) {
        let mut buf = [0u8; 8192];
        let deadline = Instant::now() + limit;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let n = tokio::time::timeout(left, self.stream.read(&mut buf))
                .await
                .expect("the stream didn't end")
                .unwrap_or(0);
            if n == 0 {
                return;
            }
            self.text.push_str(&String::from_utf8_lossy(&buf[..n]));
            if self.text.ends_with("0\r\n\r\n") {
                return;
            }
        }
    }

    /// Reads whatever comes within `d`.
    async fn drain(&mut self, d: Duration) {
        let mut buf = [0u8; 65536];
        let deadline = Instant::now() + d;
        while let Ok(Ok(n)) = tokio::time::timeout(
            deadline.saturating_duration_since(Instant::now()),
            self.stream.read(&mut buf),
        )
        .await
        {
            if n == 0 {
                return;
            }
            self.text.push_str(&String::from_utf8_lossy(&buf[..n]));
        }
    }

    fn ids(&self) -> Vec<String> {
        self.text
            .lines()
            .filter_map(|l| l.strip_prefix("id: "))
            .map(str::to_string)
            .collect()
    }

    fn data(&self, event: &str) -> Vec<Value> {
        let mut out = Vec::new();
        let mut lines = self.text.lines();
        while let Some(l) = lines.next() {
            if l == format!("event: {event}") {
                let d = lines.next().unwrap().strip_prefix("data: ").unwrap();
                out.push(serde_json::from_str(d).unwrap());
            }
        }
        out
    }
}

async fn call(app: &Router, req: Request<Body>) -> (StatusCode, axum::http::HeaderMap, Vec<u8>) {
    let resp = app.clone().oneshot(req).await.unwrap();
    let (status, headers) = (resp.status(), resp.headers().clone());
    let bytes = axum::body::to_bytes(resp.into_body(), 1 << 22)
        .await
        .unwrap();
    (status, headers, bytes.to_vec())
}

fn get(uri: &str, cookie: &str) -> Request<Body> {
    Request::get(uri)
        .header(header::COOKIE, cookie)
        .body(Body::empty())
        .unwrap()
}

fn code(body: &[u8]) -> (String, String) {
    let v: Value = serde_json::from_slice(body).unwrap_or(Value::Null);
    (
        v["error"]["code"].as_str().unwrap_or("").to_string(),
        v["error"]["field"].as_str().unwrap_or("").to_string(),
    )
}

const SOON: Duration = Duration::from_secs(5);

#[tokio::test]
async fn without_a_source_only_the_catalogue_answers() {
    let f = fixture(None);
    let c = session(&f.auth);
    for uri in ["/api/v1/logs", "/api/v1/logs/stream"] {
        let (status, _, body) = call(&f.app, get(uri, &c)).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{uri}");
        assert_eq!(code(&body).0, "logs_unavailable");
    }
    assert_eq!(
        call(&f.app, get("/api/v1/logs/catalog", &c)).await.0,
        StatusCode::OK
    );
}

#[tokio::test]
async fn the_stream_resumes_filters_and_purges() {
    let fake = Fake::new(config(), 1000);
    let f = fixture(Some(fake.clone()));
    let c = session(&f.auth);
    let addr = serve(f.app.clone()).await;
    for i in 0..5 {
        fake.push(Level::Info, &format!("old {i}"));
    }
    let mut s = Conn::open(
        addr,
        "/api/v1/logs/stream?level=warn&text=WANTED",
        &[("cookie", &c)],
    )
    .await;
    s.until("retry: 5000", SOON).await;
    assert!(
        s.text.contains("content-type: text/event-stream"),
        "{}",
        s.text
    );
    assert!(s.text.contains("cache-control: no-store"));
    assert!(s.text.contains("x-accel-buffering: no"));
    fake.push(Level::Info, "wanted but info");
    fake.push(Level::Warn, "not this one");
    let seq = fake.push(Level::Warn, "the WANTED line");
    s.until("the WANTED line", SOON).await;
    assert_eq!(
        s.ids(),
        [format!("{EPOCH}-{seq}")],
        "nothing before the head, nothing filtered"
    );
    assert!(!s.text.contains("old 0") && !s.text.contains("not this one"));
    fake.purge(seq);
    s.until("event: purge", SOON).await;
    let p = &s.data("purge")[0];
    assert_eq!(p["kinds"], serde_json::json!(["name"]));
    assert_eq!(p["through"], format!("{EPOCH}-{seq}"));
    s.until(": keep-alive", SOON).await;
    drop(s);

    // Resume: `Last-Event-ID` wins over `after`, with no gap.
    let a = fake.push(Level::Error, "a");
    let b = fake.push(Level::Error, "b");
    let last = format!("{EPOCH}-{seq}");
    let mut s = Conn::open(
        addr,
        &format!("/api/v1/logs/stream?after={EPOCH}-{b}"),
        &[("cookie", &c), ("last-event-id", &last)],
    )
    .await;
    s.until("\"msg\":\"b\"", SOON).await;
    assert_eq!(s.ids(), [format!("{EPOCH}-{a}"), format!("{EPOCH}-{b}")]);
    drop(s);
    let mut s = Conn::open(
        addr,
        &format!("/api/v1/logs/stream?after={EPOCH}-{a}"),
        &[("cookie", &c)],
    )
    .await;
    s.until("\"msg\":\"b\"", SOON).await;
    assert_eq!(s.ids(), [format!("{EPOCH}-{b}")]);
    drop(s);
    // Restarted, expired (none: nothing evicted yet, so use the future),
    // malformed: each a resync, then live.
    for (cursor, reason) in [
        ("ffffffffffffffff-3".to_string(), "restarted"),
        (format!("{EPOCH}-999999"), "malformed"),
        ("junk".to_string(), "malformed"),
    ] {
        let mut s = Conn::open(
            addr,
            "/api/v1/logs/stream",
            &[("cookie", &c), ("last-event-id", &cursor)],
        )
        .await;
        s.until("event: resync", SOON).await;
        assert_eq!(s.data("resync")[0]["reason"], reason, "{cursor}");
        let n = fake.push(Level::Error, "live");
        s.until(&format!("id: {EPOCH}-{n}"), SOON).await;
    }
}

#[tokio::test]
async fn an_expired_cursor_and_a_lagging_client_resync() {
    let fake = Fake::new(config(), 100);
    let f = fixture(Some(fake.clone()));
    let c = session(&f.auth);
    let addr = serve(f.app.clone()).await;
    for i in 0..400 {
        fake.push(Level::Info, &format!("line {i}"));
    }
    let mut s = Conn::open(
        addr,
        &format!("/api/v1/logs/stream?after={EPOCH}-5"),
        &[("cookie", &c)],
    )
    .await;
    s.until("event: resync", SOON).await;
    assert_eq!(s.data("resync")[0]["reason"], "expired");
    drop(s);
    // More than 256 matching waiting at once: one `lagged`, then live.
    let fake = Fake::new(config(), 5000);
    let f = fixture(Some(fake.clone()));
    let c = session(&f.auth);
    let addr = serve(f.app.clone()).await;
    for i in 0..400 {
        fake.push(Level::Info, &format!("line {i}"));
    }
    let mut s = Conn::open(
        addr,
        &format!("/api/v1/logs/stream?after={EPOCH}-100"),
        &[("cookie", &c)],
    )
    .await;
    s.until("event: resync", SOON).await;
    let r = &s.data("resync")[0];
    assert_eq!(r["reason"], "lagged");
    assert_eq!(r["skipped"], 300);
    assert_eq!(r["evicted"], 0);
    let n = fake.push(Level::Info, "after");
    s.until(&format!("id: {EPOCH}-{n}"), SOON).await;
    assert_eq!(s.ids().len(), 1, "it carried on from the newest");
}

#[tokio::test]
async fn a_client_that_never_reads_slows_nothing_and_gets_one_resync() {
    let fake = Fake::new(config(), 5000);
    let f = fixture(Some(fake.clone()));
    let c = session(&f.auth);
    let addr = serve(f.app.clone()).await;
    let mut s = Conn::open(addr, "/api/v1/logs/stream", &[("cookie", &c)]).await;
    s.until("retry: 5000", SOON).await;
    let pad = "x".repeat(400);
    let start = Instant::now();
    for i in 0..40_000 {
        fake.push(Level::Info, &format!("{i} {pad}"));
    }
    assert!(
        start.elapsed() < Duration::from_secs(10),
        "the writer never waits"
    );
    tokio::time::sleep(Duration::from_millis(300)).await;
    s.drain(Duration::from_secs(3)).await;
    let resyncs = s.data("resync");
    assert!(!resyncs.is_empty(), "it fell behind");
    assert!(resyncs.iter().all(|r| r["reason"] == "lagged"));
    let lost: u64 = resyncs
        .iter()
        .map(|r| r["skipped"].as_u64().unwrap() + r["evicted"].as_u64().unwrap())
        .sum();
    let got = s.ids().len() as u64;
    assert!(got < 40_000, "{got}");
    assert!(got + lost >= 39_000, "{got} + {lost}");
}

#[tokio::test]
async fn the_caps_are_per_session_and_in_all() {
    let fake = Fake::new(config(), 100);
    let f = fixture(Some(fake.clone()));
    let c = session(&f.auth);
    let addr = serve(f.app.clone()).await;
    let mut held = Vec::new();
    for _ in 0..2 {
        let mut s = Conn::open(addr, "/api/v1/logs/stream", &[("cookie", &c)]).await;
        s.until("retry:", SOON).await;
        held.push(s);
    }
    let (status, headers, body) = call(&f.app, get("/api/v1/logs/stream", &c)).await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(headers[header::RETRY_AFTER], "5");
    assert_eq!(code(&body).0, "too_many_streams");
    // Another session gets the third and last slot.
    let other = {
        let set = f.auth.login([127, 0, 0, 1].into(), PASSWORD).unwrap();
        set.split(';').next().unwrap().to_string()
    };
    let mut s = Conn::open(addr, "/api/v1/logs/stream", &[("cookie", &other)]).await;
    s.until("retry:", SOON).await;
    held.push(s);
    let another = {
        let set = f.auth.login([127, 0, 0, 1].into(), PASSWORD).unwrap();
        set.split(';').next().unwrap().to_string()
    };
    assert_eq!(
        call(&f.app, get("/api/v1/logs/stream", &another)).await.0,
        StatusCode::TOO_MANY_REQUESTS
    );
    assert_eq!(fake.transport().streams(), 3);
    drop(held);
    let start = Instant::now();
    while fake.transport().streams() > 0 {
        assert!(start.elapsed() < SOON, "slots are released");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn a_stream_ends_with_its_session_and_on_close() {
    let fake = Fake::new(config(), 100);
    let f = fixture(Some(fake.clone()));
    let c = session(&f.auth);
    let addr = serve(f.app.clone()).await;
    let mut s = Conn::open(addr, "/api/v1/logs/stream", &[("cookie", &c)]).await;
    s.until(": keep-alive", SOON).await;
    f.auth
        .change_password([127, 0, 0, 1].into(), PASSWORD, "another long password")
        .unwrap();
    s.until_closed(Duration::from_secs(2)).await;

    // A trusted-proxy session stays up: the header, from the same peer.
    let set = f
        .auth
        .login([127, 0, 0, 1].into(), "another long password")
        .unwrap();
    let c2 = set.split(';').next().unwrap().to_string();
    f.auth
        .set_proxy(ProxyConfig {
            enabled: true,
            trusted: vec!["127.0.0.1".into()],
            ..ProxyConfig::default()
        })
        .unwrap();
    let mut p = Conn::open(addr, "/api/v1/logs/stream", &[("remote-user", "admin")]).await;
    p.until("retry:", SOON).await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    let n = fake.push(Level::Info, "still here");
    p.until(&format!("id: {EPOCH}-{n}"), SOON).await;
    assert!(p.text.matches(": keep-alive").count() >= 3, "{}", p.text);

    // Shutdown: every stream ends, new ones are refused.
    let mut s = Conn::open(addr, "/api/v1/logs/stream", &[("cookie", &c2)]).await;
    s.until("retry:", SOON).await;
    fake.close();
    s.until_closed(Duration::from_secs(2)).await;
    p.until_closed(Duration::from_secs(2)).await;
    assert_eq!(
        call(&f.app, get("/api/v1/logs/stream", &c2)).await.0,
        StatusCode::SERVICE_UNAVAILABLE
    );
}

#[tokio::test]
async fn parameters_are_checked_once_each() {
    let fake = Fake::new(config(), 100);
    let f = fixture(Some(fake.clone()));
    let c = session(&f.auth);
    let long = "x".repeat(201);
    let cursor = "A".repeat(513);
    for (q, want, field) in [
        ("level=loud", "bad_parameter", "level"),
        ("level=warn&level=info", "bad_parameter", "level"),
        ("components=dht,nope", "unknown_component", "components"),
        (&*format!("text={long}"), "bad_parameter", "text"),
        ("limit=0", "bad_parameter", "limit"),
        ("limit=1001", "bad_parameter", "limit"),
        ("since=yesterday", "bad_parameter", "since"),
        ("until=2026-10-23T10:15:00", "bad_parameter", "until"),
        (&*format!("cursor={cursor}"), "bad_cursor", "cursor"),
    ] {
        let (status, _, body) = call(&f.app, get(&format!("/api/v1/logs?{q}"), &c)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{q}");
        assert_eq!(code(&body), (want.to_string(), field.to_string()), "{q}");
    }
    let (status, _, body) = call(&f.app, get("/api/v1/logs/stream?components=nope", &c)).await;
    assert_eq!(
        (status, code(&body).0),
        (StatusCode::BAD_REQUEST, "unknown_component".into())
    );
    assert_eq!(fake.reads.load(Ordering::SeqCst), 0, "nothing read");
    let (status, headers, _) = call(
        &f.app,
        get("/api/v1/logs?limit=1000&since=2026-10-23T10:15:00Z", &c),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers[header::CACHE_CONTROL], "no-store");
}

#[tokio::test]
async fn at_most_two_file_reads_at_once() {
    let fake = Fake::new(config(), 100);
    let f = fixture(Some(fake.clone()));
    let c = session(&f.auth);
    let reqs = (0..3).map(|_| {
        let app = f.app.clone();
        let req = get("/api/v1/logs", &c);
        async move { call(&app, req).await }
    });
    let results = futures_join(reqs.collect()).await;
    let busy: Vec<_> = results
        .iter()
        .filter(|(s, _, _)| *s == StatusCode::TOO_MANY_REQUESTS)
        .collect();
    assert_eq!(
        busy.len(),
        1,
        "{:?}",
        results.iter().map(|r| r.0).collect::<Vec<_>>()
    );
    assert_eq!(busy[0].1[header::RETRY_AFTER], "2");
    assert_eq!(code(&busy[0].2).0, "logs_busy");
}

async fn futures_join<F: std::future::Future + Send + 'static>(fs: Vec<F>) -> Vec<F::Output>
where
    F::Output: Send + 'static,
{
    let handles: Vec<_> = fs.into_iter().map(tokio::spawn).collect();
    let mut out = Vec::new();
    for h in handles {
        out.push(h.await.unwrap());
    }
    out
}

#[tokio::test]
async fn a_steady_flow_of_lines_never_puts_off_the_session_check() {
    let fake = Fake::new(config(), 5000);
    let f = fixture(Some(fake.clone()));
    let c = session(&f.auth);
    let addr = serve(f.app.clone()).await;
    let mut s = Conn::open(addr, "/api/v1/logs/stream", &[("cookie", &c)]).await;
    s.until("retry:", SOON).await;
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let pusher = {
        let (fake, stop) = (fake.clone(), stop.clone());
        tokio::spawn(async move {
            while !stop.load(Ordering::Relaxed) {
                fake.push(Level::Info, "busy");
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        })
    };
    s.until("busy", SOON).await;
    // "Sign out other sessions" rotates the key: this one's cookie dies.
    f.auth.rotate_sessions().unwrap();
    s.until_closed(Duration::from_secs(2)).await;
    stop.store(true, Ordering::Relaxed);
    pusher.await.unwrap();
}

#[tokio::test]
async fn a_last_event_id_that_isnt_text_is_malformed_and_wins() {
    let fake = Fake::new(config(), 100);
    let f = fixture(Some(fake.clone()));
    let c = session(&f.auth);
    let addr = serve(f.app.clone()).await;
    let a = fake.push(Level::Info, "a");
    let mut stream = TcpStream::connect(addr).await.unwrap();
    let mut req = format!(
        "GET /api/v1/logs/stream?after={EPOCH}-{a} HTTP/1.1\r\nHost: {addr}\r\ncookie: {c}\r\nlast-event-id: "
    )
    .into_bytes();
    req.extend_from_slice(b"\xff\xfe\r\n\r\n");
    stream.write_all(&req).await.unwrap();
    let mut s = Conn {
        stream,
        text: String::new(),
    };
    s.until("event: resync", SOON).await;
    assert_eq!(s.data("resync")[0]["reason"], "malformed");
}

#[tokio::test]
async fn a_client_that_stopped_reading_doesnt_pin_its_slot() {
    let fake = Fake::new(config(), 5000);
    let f = fixture(Some(fake.clone()));
    let c = session(&f.auth);
    let addr = serve(f.app.clone()).await;
    let mut s = Conn::open(addr, "/api/v1/logs/stream", &[("cookie", &c)]).await;
    s.until("retry:", SOON).await;
    // Lines keep coming, enough to fill the queue and the socket's
    // buffers; the client never reads.
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let pusher = {
        let (fake, stop) = (fake.clone(), stop.clone());
        tokio::spawn(async move {
            let pad = "x".repeat(4000);
            let mut i = 0u64;
            while !stop.load(Ordering::Relaxed) {
                for _ in 0..50 {
                    fake.push(Level::Info, &format!("{i} {pad}"));
                    i += 1;
                }
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
        })
    };
    let start = Instant::now();
    while fake.transport().streams() > 0 {
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "a stalled stream let its slot go within a few heartbeats"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    stop.store(true, Ordering::Relaxed);
    pusher.await.unwrap();
    drop(s);
}

#[tokio::test]
async fn a_resumed_stream_gets_the_purges_it_missed() {
    let fake = Fake::new(config(), 100);
    let f = fixture(Some(fake.clone()));
    let c = session(&f.auth);
    let addr = serve(f.app.clone()).await;
    let a = fake.push(Level::Info, "a");
    let b = fake.push(Level::Info, "b");
    // A purge while it was away, reaching lines it already had.
    fake.purge(b);
    let mut s = Conn::open(
        addr,
        "/api/v1/logs/stream",
        &[("cookie", &c), ("last-event-id", &format!("{EPOCH}-{a}"))],
    )
    .await;
    s.until("\"msg\":\"b\"", SOON).await;
    let purge_at = s.text.find("event: purge").expect("the missed purge");
    assert!(
        purge_at < s.text.find("\"msg\":\"b\"").unwrap(),
        "before any line"
    );
    drop(s);
    // From the newest: nothing to replay.
    let mut s = Conn::open(
        addr,
        &format!("/api/v1/logs/stream?after={EPOCH}-{b}"),
        &[("cookie", &c)],
    )
    .await;
    s.until("retry:", SOON).await;
    // `through` is b, which it has: the purge reaches it, so it comes.
    s.until("event: purge", SOON).await;
}
