//! `GET /api/v1/events` end to end: over a real socket through the
//! router (headers, `retry:`, prompt delivery, heartbeats, replay,
//! caps, shutdown), and its errors in the envelope.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Request, StatusCode, header};
use axum::{Extension, Router};
use lodestar_api::auth::ProxyConfig;
use lodestar_api::dto::events::{CrawlerEvent, Event, TorrentsEvent};
use lodestar_api::events::{EventHub, EventSources, EventsConfig};
use lodestar_api::{ApiState, Auth, router};
use lodestar_store::{Store, StoreConfig};
use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tower::ServiceExt;

struct Fixture {
    app: Router,
    hub: Arc<EventHub>,
    auth: Arc<Auth>,
    _dir: tempfile::TempDir,
}

fn config() -> EventsConfig {
    EventsConfig {
        heartbeat: Duration::from_millis(100),
        max_streams: 3,
        max_per_session: 2,
        ..EventsConfig::default()
    }
}

fn fixture(config: EventsConfig) -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("t.db");
    drop(Store::open(&db, StoreConfig::default()).unwrap());
    let reader = Store::open_reader(&db, StoreConfig::default()).unwrap();
    let auth = Arc::new(Auth::load(&dir.path().join("auth.json"), false).unwrap());
    let hub = EventHub::start(config, EventSources::default());
    // As `lodestar serve` mounts it.
    let app = Router::new()
        .nest(
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
            }),
        )
        .layer(Extension(hub.clone()));
    Fixture {
        app,
        hub,
        auth,
        _dir: dir,
    }
}

/// A session cookie (`name=value`).
fn session(auth: &Auth) -> String {
    let set = match auth.setup_token() {
        Some(token) => auth
            .setup([127, 0, 0, 1].into(), &token, "correct horse battery")
            .unwrap(),
        None => auth
            .login([127, 0, 0, 1].into(), "correct horse battery")
            .unwrap(),
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

/// A raw HTTP/1.1 connection, read as text.
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

    /// Reads until `needle` appears (and returns how long that took), or
    /// panics after `limit`.
    async fn until(&mut self, needle: &str, limit: Duration) -> Duration {
        let start = Instant::now();
        let mut buf = [0u8; 4096];
        while !self.text.contains(needle) {
            let left = limit.saturating_sub(start.elapsed());
            let n = tokio::time::timeout(left, self.stream.read(&mut buf))
                .await
                .unwrap_or_else(|_| panic!("no {needle:?} within {limit:?}; got {:?}", self.text))
                .unwrap();
            assert!(n > 0, "closed before {needle:?}; got {:?}", self.text);
            self.text.push_str(&String::from_utf8_lossy(&buf[..n]));
        }
        start.elapsed()
    }

    /// Reads until the server closes the stream (the last chunk).
    async fn until_closed(&mut self, limit: Duration) {
        let mut buf = [0u8; 4096];
        let deadline = Instant::now() + limit;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let n = tokio::time::timeout(left, self.stream.read(&mut buf))
                .await
                .expect("the stream ended")
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

    /// The `id:` lines received so far.
    fn ids(&self) -> Vec<String> {
        self.text
            .lines()
            .filter_map(|l| l.strip_prefix("id: "))
            .map(str::to_string)
            .collect()
    }
}

async fn call(app: &Router, req: Request<Body>) -> (StatusCode, axum::http::HeaderMap, Value) {
    let resp = app.clone().oneshot(req).await.unwrap();
    let (status, headers) = (resp.status(), resp.headers().clone());
    let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20)
        .await
        .unwrap();
    (
        status,
        headers,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

fn get(uri: &str, headers: &[(&str, &str)]) -> Request<Body> {
    let mut b = Request::get(uri);
    for (k, v) in headers {
        b = b.header(*k, *v);
    }
    b.body(Body::empty()).unwrap()
}

fn torrents(added: u64) -> Event {
    Event::Torrents(TorrentsEvent {
        added,
        max_id: Some(added as i64),
    })
}

fn crawler() -> Event {
    Event::Crawler(CrawlerEvent::default())
}

const SOON: Duration = Duration::from_secs(5);

#[tokio::test]
async fn events_stream_promptly_through_the_router() {
    let f = fixture(config());
    let cookie = session(&f.auth);
    let addr = serve(f.app.clone()).await;
    let mut c = Conn::open(
        addr,
        "/api/v1/events?topics=torrents",
        &[("Cookie", &cookie), ("Accept-Encoding", "gzip, br")],
    )
    .await;
    c.until("retry: 5000\n\n", SOON).await;
    let head = c.text.to_ascii_lowercase();
    assert!(head.starts_with("http/1.1 200"), "{head}");
    assert!(head.contains("content-type: text/event-stream"), "{head}");
    assert!(head.contains("cache-control: no-store"), "{head}");
    assert!(head.contains("x-accel-buffering: no"), "{head}");
    // Never compressed, so nothing holds frames back.
    assert!(!head.contains("content-encoding"), "{head}");

    // Another topic's event isn't sent; this one's arrives at once.
    f.hub.publish(&crawler());
    let id = f.hub.publish(&torrents(3)).unwrap();
    let took = c.until("data: {\"added\":3,\"max_id\":3}\n\n", SOON).await;
    assert!(took < Duration::from_millis(500), "took {took:?}");
    assert!(c.text.contains(&format!("id: {id}\nevent: torrents\n")));
    assert!(!c.text.contains("event: crawler"));
    assert_eq!(
        f.hub.listeners(lodestar_api::dto::events::Topic::Torrents),
        1
    );
}

#[tokio::test]
async fn heartbeats_keep_an_idle_stream_open() {
    let f = fixture(config());
    let cookie = session(&f.auth);
    let addr = serve(f.app.clone()).await;
    let mut c = Conn::open(addr, "/api/v1/events", &[("Cookie", &cookie)]).await;
    c.until("retry:", SOON).await;
    c.until(": keep-alive\n\n", SOON).await;
    let before = c.text.matches(": keep-alive").count();
    c.until_more(": keep-alive", before).await;
}

impl Conn {
    /// Reads until `needle` appears more than `n` times.
    async fn until_more(&mut self, needle: &str, n: usize) {
        let start = Instant::now();
        let mut buf = [0u8; 4096];
        while self.text.matches(needle).count() <= n {
            assert!(start.elapsed() < SOON, "{:?}", self.text);
            let read = tokio::time::timeout(SOON, self.stream.read(&mut buf))
                .await
                .unwrap()
                .unwrap();
            assert!(read > 0);
            self.text.push_str(&String::from_utf8_lossy(&buf[..read]));
        }
    }
}

#[tokio::test]
async fn last_event_id_replays_exactly_or_resyncs() {
    let f = fixture(config());
    let cookie = session(&f.auth);
    let addr = serve(f.app.clone()).await;
    let ids: Vec<String> = (1..=4)
        .map(|i| {
            f.hub
                .publish(&if i == 2 { crawler() } else { torrents(i) })
                .unwrap()
        })
        .collect();
    // After the first: the rest of its topics, in order, once.
    let mut c = Conn::open(
        addr,
        "/api/v1/events?topics=torrents",
        &[("Cookie", &cookie), ("Last-Event-ID", &ids[0])],
    )
    .await;
    c.until(&format!("id: {}\n", ids[3]), SOON).await;
    assert_eq!(c.ids(), [ids[2].clone(), ids[3].clone()]);
    let live = f.hub.publish(&torrents(5)).unwrap();
    c.until(&format!("id: {live}\n"), SOON).await;
    assert_eq!(c.ids(), [ids[2].clone(), ids[3].clone(), live.clone()]);
    assert!(!c.text.contains("resync"));
    drop(c);

    // Another process's id: a resync, then live.
    let other = format!("{:016x}-3", 1u64);
    let mut c = Conn::open(
        addr,
        "/api/v1/events?topics=torrents",
        &[("Cookie", &cookie), ("Last-Event-ID", &other)],
    )
    .await;
    c.until("event: resync\ndata: {\"reason\":\"restarted\"}\n\n", SOON)
        .await;
    assert!(c.ids().is_empty(), "a resync has no id");
    let next = f.hub.publish(&torrents(6)).unwrap();
    c.until(&format!("id: {next}\n"), SOON).await;
    assert_eq!(c.ids(), [next]);
}

#[tokio::test]
async fn caps_answer_429_and_free_on_disconnect() {
    let f = fixture(config());
    let a = session(&f.auth);
    let b = session(&f.auth);
    assert_ne!(a, b);
    let c = session(&f.auth);
    let addr = serve(f.app.clone()).await;
    let mut open = Vec::new();
    for cookie in [&a, &a, &b] {
        let mut c = Conn::open(addr, "/api/v1/events", &[("Cookie", cookie)]).await;
        c.until("retry:", SOON).await;
        open.push(c);
    }
    assert_eq!(f.hub.streams(), 3);
    // Past the session's cap, then past the global one.
    for (cookie, code) in [(&a, "too_many_streams"), (&c, "too_many_streams")] {
        let (status, headers, body) =
            call(&f.app, get("/api/v1/events", &[("cookie", cookie)])).await;
        assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(headers[header::RETRY_AFTER], "5");
        assert_eq!(body["error"]["code"], code);
    }
    // A client going away frees its slot.
    drop(open.remove(0));
    let start = Instant::now();
    while f.hub.streams() != 2 {
        assert!(start.elapsed() < SOON);
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let mut c = Conn::open(addr, "/api/v1/events", &[("Cookie", &a)]).await;
    c.until("retry:", SOON).await;
    drop(c);
    drop(open);
    let start = Instant::now();
    while f.hub.streams() != 0 {
        assert!(start.elapsed() < SOON);
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

#[tokio::test]
async fn a_thousand_connects_and_disconnects_leave_the_count_at_zero() {
    let f = fixture(config());
    let cookie = session(&f.auth);
    for _ in 0..1000 {
        let resp = f
            .app
            .clone()
            .oneshot(get("/api/v1/events", &[("cookie", &cookie)]))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        drop(resp);
    }
    assert_eq!(f.hub.streams(), 0);
    // The clients' tasks notice and let go of their subscriptions.
    let start = Instant::now();
    while lodestar_api::dto::events::Topic::ALL
        .iter()
        .any(|&t| f.hub.listeners(t) != 0)
    {
        assert!(start.elapsed() < SOON);
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

#[tokio::test]
async fn closing_the_hub_ends_open_streams() {
    let f = fixture(config());
    let cookie = session(&f.auth);
    let addr = serve(f.app.clone()).await;
    let mut c = Conn::open(addr, "/api/v1/events", &[("Cookie", &cookie)]).await;
    c.until("retry:", SOON).await;
    f.hub.close();
    c.until_closed(SOON).await;
    // And refuses new ones.
    let (status, _, body) = call(&f.app, get("/api/v1/events", &[("cookie", &cookie)])).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(body["error"]["code"], "events_unavailable");
}

#[tokio::test]
async fn bad_requests_get_the_envelope() {
    let f = fixture(config());
    // No session: 401, before anything else is looked at.
    let (status, _, body) = call(&f.app, get("/api/v1/events?topics=nope", &[])).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["error"]["code"], "login_required");
    let cookie = session(&f.auth);
    for q in ["nope", "", "torrents,", "torrents,bogus"] {
        let (status, _, body) = call(
            &f.app,
            get(
                &format!("/api/v1/events?topics={q}"),
                &[("cookie", &cookie)],
            ),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{q}");
        assert_eq!(body["error"]["code"], "unknown_topic");
        assert_eq!(body["error"]["field"], "topics");
    }
    let (status, _, body) = call(
        &f.app,
        get(
            "/api/v1/events?topics=torrents&topics=crawler",
            &[("cookie", &cookie)],
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["code"], "bad_parameter");
    assert_eq!(body["error"]["field"], "topics");
    assert_eq!(f.hub.streams(), 0);
}

#[tokio::test]
async fn a_stream_ends_with_its_session() {
    let f = fixture(config());
    let cookie = session(&f.auth);
    let addr = serve(f.app.clone()).await;
    let mut c = Conn::open(addr, "/api/v1/events", &[("Cookie", &cookie)]).await;
    c.until(": keep-alive", SOON).await;
    // A new password ends every other session, open streams included,
    // at the next heartbeat (100 ms here).
    f.auth
        .change_password(
            [127, 0, 0, 1].into(),
            "correct horse battery",
            "another fine password",
        )
        .unwrap();
    let start = Instant::now();
    c.until_closed(SOON).await;
    let took = start.elapsed();
    assert!(took < Duration::from_millis(500), "took {took:?}");
    let start = Instant::now();
    while f.hub.streams() != 0 {
        assert!(start.elapsed() < SOON);
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

#[tokio::test]
async fn a_proxy_signed_stream_lives_past_heartbeats_and_ends_with_the_proxy() {
    let f = fixture(config());
    // A password must exist before the proxy can sign anyone in.
    let _ = session(&f.auth);
    f.auth
        .set_proxy(ProxyConfig {
            enabled: true,
            trusted: vec!["127.0.0.1/32".into()],
            ..ProxyConfig::default()
        })
        .unwrap();
    let addr = serve(f.app.clone()).await;
    // No cookie: the trusted peer's user header is the session.
    let mut c = Conn::open(addr, "/api/v1/events", &[("Remote-User", "alice")]).await;
    c.until("retry:", SOON).await;
    // Several heartbeats re-check it (100 ms apart) and keep it open.
    c.until_more(": keep-alive", 3).await;
    // Turning proxy sign-in off ends it at the next heartbeat.
    f.auth.set_proxy(ProxyConfig::default()).unwrap();
    c.until_closed(SOON).await;
}

#[tokio::test]
async fn a_proxy_user_loses_the_stream_when_no_longer_allowed() {
    let f = fixture(config());
    let _ = session(&f.auth);
    let proxy = |users: Vec<String>| ProxyConfig {
        enabled: true,
        trusted: vec!["127.0.0.1/32".into()],
        allowed_users: users,
        ..ProxyConfig::default()
    };
    f.auth.set_proxy(proxy(vec!["alice".into()])).unwrap();
    let addr = serve(f.app.clone()).await;
    let mut c = Conn::open(addr, "/api/v1/events", &[("Remote-User", "alice")]).await;
    c.until(": keep-alive", SOON).await;
    f.auth.set_proxy(proxy(vec!["bob".into()])).unwrap();
    c.until_closed(SOON).await;
}

#[tokio::test]
async fn made_up_cookies_beside_a_proxy_user_share_one_cap() {
    // Two streams per session here.
    let f = fixture(config());
    let _ = session(&f.auth);
    f.auth
        .set_proxy(ProxyConfig {
            enabled: true,
            trusted: vec!["127.0.0.1/32".into()],
            ..ProxyConfig::default()
        })
        .unwrap();
    let addr = serve(f.app.clone()).await;
    let mut open = Vec::new();
    for i in 0..2 {
        let cookie = format!("lodestar=made-up-{i}");
        let mut c = Conn::open(
            addr,
            "/api/v1/events",
            &[("Remote-User", "alice"), ("Cookie", &cookie)],
        )
        .await;
        c.until("retry:", SOON).await;
        open.push(c);
    }
    // A third invented cookie is still alice's session: refused.
    let mut req = get(
        "/api/v1/events",
        &[("remote-user", "alice"), ("cookie", "lodestar=made-up-2")],
    );
    req.extensions_mut()
        .insert(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 1))));
    let (status, _, body) = call(&f.app, req).await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS, "{body}");
    // Another user from the same address has a bucket of their own.
    let mut bob = Conn::open(addr, "/api/v1/events", &[("Remote-User", "bob")]).await;
    bob.until("retry:", SOON).await;
}

#[tokio::test]
async fn cross_origin_requests_follow_the_get_rule() {
    let f = fixture(config());
    let cookie = session(&f.auth);
    let evil = ("origin", "https://evil.example");
    // A cross-site page has no cookie to send (SameSite=Strict): 401, as
    // for every other GET.
    let (events, _, _) = call(&f.app, get("/api/v1/events", &[evil])).await;
    let (search, _, _) = call(&f.app, get("/api/v1/parse?q=x", &[evil])).await;
    assert_eq!(events, StatusCode::UNAUTHORIZED);
    assert_eq!(events, search);
    // With one, a GET is answered as any other, and never with CORS
    // headers, so a browser won't let another origin read it.
    let resp = f
        .app
        .clone()
        .oneshot(get("/api/v1/events", &[evil, ("cookie", &cookie)]))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert!(
        resp.headers()
            .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
            .is_none()
    );
    let (parse, headers, _) = call(
        &f.app,
        get("/api/v1/parse?q=x", &[evil, ("cookie", &cookie)]),
    )
    .await;
    assert_eq!(parse, StatusCode::OK);
    assert!(headers.get(header::ACCESS_CONTROL_ALLOW_ORIGIN).is_none());
}

#[tokio::test]
async fn without_a_hub_it_is_unavailable() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("t.db");
    drop(Store::open(&db, StoreConfig::default()).unwrap());
    let reader = Store::open_reader(&db, StoreConfig::default()).unwrap();
    let auth = Arc::new(Auth::load(&dir.path().join("auth.json"), false).unwrap());
    let app = Router::new().nest(
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
        }),
    );
    let cookie = session(&auth);
    let (status, _, body) = call(&app, get("/api/v1/events", &[("cookie", &cookie)])).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(body["error"]["code"], "events_unavailable");
}

/// Stores `names` and classifies them, as the pipeline would, before any
/// indexer sees them.
fn add_classified(store: &mut Store, names: &[&str]) {
    use lodestar_bencode::{OwnedValue, to_vec};
    let b = |s: &str| OwnedValue::Bytes(s.as_bytes().to_vec());
    let raws: Vec<Vec<u8>> = names
        .iter()
        .map(|n| {
            to_vec(&OwnedValue::Dict(
                [
                    ("length", OwnedValue::Int(1 << 30)),
                    ("name", b(n)),
                    ("piece length", OwnedValue::Int(1 << 16)),
                    ("pieces", OwnedValue::Bytes(vec![0; 20])),
                ]
                .into_iter()
                .map(|(k, v)| (k.as_bytes().to_vec(), v))
                .collect(),
            ))
        })
        .collect();
    let infos: Vec<lodestar_metainfo::Info> = raws
        .iter()
        .map(|r| lodestar_metainfo::Info::parse(r).unwrap())
        .collect();
    let batch: Vec<lodestar_store::NewTorrent<'_>> = infos
        .iter()
        .zip(&raws)
        .map(|(info, raw)| lodestar_store::NewTorrent {
            info,
            raw_info: Some(raw),
            discovered_at: 1_700_000_000,
            metadata_at: 1_700_000_100,
            extra_flags: 0,
            source: lodestar_store::SourceId::new(0).unwrap(),
        })
        .collect();
    for t in store.insert(&batch).unwrap() {
        let id = t.id();
        let row = store.torrent(id).unwrap().unwrap();
        let tree = store.files(id).unwrap().unwrap();
        let files = lodestar_classify::file_infos(&tree.files, tree.encoding);
        let (e, _) = lodestar_filter::reclassify::enrich_corrected(
            &row.name,
            &files,
            lodestar_classify::Correction::default(),
        );
        store.set_enrichments(&[(id, e)]).unwrap();
    }
}

/// B4b end to end: the live indexer's commits reach the stream with
/// Adult left out while "Show adult content" is off, read live through
/// the same `AdultVisibility` the API state holds (as `lodestar serve`
/// wires it), and a commit of only hidden Adult sends nothing.
#[tokio::test]
async fn the_torrents_count_leaves_out_adult_while_it_is_hidden() {
    use lodestar_search::{CommitHook, LiveConfig, LiveIndexer, SearchIndex};
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("t.db");
    let mut store = Store::open(&db, StoreConfig::default()).unwrap();
    let index = SearchIndex::in_memory().unwrap();
    let (show, rx) = tokio::sync::watch::channel(false);
    let adult = lodestar_api::AdultVisibility::follow(move || *rx.borrow());
    let hook = Arc::new(CommitHook::default());
    let hub = EventHub::start(
        // Three streams from one session: the live one and two
        // reconnects.
        EventsConfig {
            max_per_session: 4,
            ..config()
        },
        EventSources {
            commits: Some(hook.clone()),
            adult: adult.clone(),
            ..EventSources::default()
        },
    );
    let auth = Arc::new(Auth::load(&dir.path().join("auth.json"), false).unwrap());
    let app = Router::new()
        .nest(
            "/api/v1",
            router(ApiState {
                auth: auth.clone(),
                index: Some(index.clone().into()),
                reader: Arc::new(Mutex::new(
                    Store::open_reader(&db, StoreConfig::default()).unwrap(),
                )),
                filters: None,
                settings: None,
                ops: None,
                user_data: None,
                adult,
                insights: None,
            }),
        )
        .layer(Extension(hub.clone()));
    let cookie = session(&auth);
    let addr = serve(app).await;
    let mut c = Conn::open(
        addr,
        "/api/v1/events?topics=torrents",
        &[("Cookie", &cookie)],
    )
    .await;
    c.until("retry: 5000\n\n", SOON).await;
    // One indexer run per step, so each step is one commit of torrents
    // classified before it saw them.
    let run = |names: &[&str], store: &mut Store| {
        add_classified(store, names);
        let live = LiveIndexer::spawn_with_stats(
            index.clone(),
            Store::open_reader(&db, StoreConfig::default()).unwrap(),
            LiveConfig {
                heap_bytes: 15 << 20,
                poll: Duration::from_millis(10),
                commit_every: Duration::from_secs(3600),
                commit_docs: names.len(),
                ..LiveConfig::default()
            },
            Arc::default(),
            hook.clone(),
        )
        .unwrap();
        let start = Instant::now();
        let want = store.ids_after(0, 1000).unwrap().len() as i64;
        while index.watermark().unwrap() < want {
            assert!(start.elapsed() < SOON, "not indexed");
            std::thread::sleep(Duration::from_millis(5));
        }
        live.stop().unwrap();
    };
    let mixed = [
        "Big.Movie.2019.1080p.mkv",
        "Studio.Scene.XXX.1080p.mp4",
        "Other.Movie.2018.720p.mkv",
        "Another.Clip.XXX.720p.mp4",
        "Third.Movie.2017.2160p.mkv",
    ];
    // Hidden (the default): 3 of 5, and no id (B4b-r2).
    run(&mixed, &mut store);
    c.until("data: {\"added\":3,\"max_id\":null}\n\n", SOON)
        .await;
    // Shown: the same mix counts 5, with the newest id.
    show.send_replace(true);
    let again: Vec<String> = mixed.iter().map(|n| n.replacen('.', ".Two.", 1)).collect();
    let again: Vec<&str> = again.iter().map(String::as_str).collect();
    run(&again, &mut store);
    c.until("data: {\"added\":5,\"max_id\":10}\n\n", SOON).await;
    // Hidden again: an all-Adult commit sends nothing, and later events
    // carry no id.
    show.send_replace(false);
    run(
        &["Third.Clip.XXX.1080p.mp4", "Fourth.Clip.XXX.720p.mp4"],
        &mut store,
    );
    run(&["Fourth.Movie.2016.1080p.mkv"], &mut store);
    // Each its own event: three complete `data:` frames so far.
    c.until_more("}\n\n", 2).await;
    run(
        &["Fifth.Movie.2015.1080p.mkv", "Fifth.Clip.XXX.1080p.mp4"],
        &mut store,
    );
    // Four complete `data:` frames.
    c.until_more("}\n\n", 3).await;
    let frames = |text: &str| -> Vec<String> {
        text.lines()
            .filter_map(|l| l.strip_prefix("data: "))
            .map(str::to_string)
            .collect()
    };
    let hidden_then = r#"{"added":1,"max_id":null}"#;
    let sent = [
        r#"{"added":3,"max_id":null}"#,
        r#"{"added":5,"max_id":10}"#,
        hidden_then,
        hidden_then,
    ];
    assert_eq!(frames(&c.text), sent, "{}", c.text);

    // A tab that reconnects from before the first event after Adult was
    // turned off: the hidden events as they were, a `resync` in place of
    // the one made while Adult was shown, and no frame with an id.
    let ids = c.ids();
    assert_eq!(ids.len(), 4, "{}", c.text);
    let epoch = ids[0].split('-').next().unwrap();
    let from_start = format!("{epoch}-0");
    let mut back = Conn::open(
        addr,
        "/api/v1/events?topics=torrents",
        &[("Cookie", &cookie), ("Last-Event-ID", &from_start)],
    )
    .await;
    back.until_more("}\n\n", 3).await;
    // The frames after `retry:`, without the chunked encoding's sizes.
    let replayed: String = back
        .text
        .split("retry: 5000\n\n")
        .nth(1)
        .unwrap()
        .split("\r\n")
        .filter(|part| !part.bytes().all(|b| b.is_ascii_hexdigit()))
        .collect();
    assert_eq!(
        replayed,
        format!(
            "id: {}\nevent: torrents\ndata: {}\n\n\
             event: resync\ndata: {{\"reason\":\"visibility\"}}\n\n\
             id: {}\nevent: torrents\ndata: {hidden_then}\n\n\
             id: {}\nevent: torrents\ndata: {hidden_then}\n\n",
            ids[0], sent[0], ids[2], ids[3]
        )
    );
    // Shown again, the same reconnect replays them as they were.
    show.send_replace(true);
    let mut shown = Conn::open(
        addr,
        "/api/v1/events?topics=torrents",
        &[("Cookie", &cookie), ("Last-Event-ID", &from_start)],
    )
    .await;
    shown.until_more("}\n\n", 3).await;
    assert_eq!(frames(&shown.text), sent, "{}", shown.text);
    assert!(!shown.text.contains("resync"), "{}", shown.text);
    hub.close();
}
