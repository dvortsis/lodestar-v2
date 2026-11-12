//! `POST /api/v1/torrents/upload` (UP-0, plan §5): the content type, the
//! cap, the guard, the same-origin rule and the rate bucket, end to end
//! through the router with a stand-in for the pipeline. The pipeline's
//! own judging is in `crates/pipeline/tests/local_torrent.rs`.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Method, Request, StatusCode, header};
use axum::{Extension, Router};
use lodestar_api::ops::BoxFuture;
use lodestar_api::upload::{self, Outcome, Sink, Unavailable, Uploader};
use lodestar_api::{ApiState, Auth, router};
use lodestar_store::{Store, StoreConfig};
use serde_json::{Value, json};
use tower::ServiceExt;

const PASSWORD: &str = "correct horse battery";
const HOST: &str = "lodestar.lan:8090";
const PROXY: [u8; 4] = [172, 17, 0, 2];

static NEXT_IP: AtomicU32 = AtomicU32::new(1);

fn fresh_ip() -> SocketAddr {
    let n = NEXT_IP.fetch_add(1, Ordering::Relaxed);
    SocketAddr::from(([10, 60, (n >> 8) as u8, n as u8], 40000))
}

/// Stands in for the pipeline: counts what reaches it and answers
/// `answer`.
#[derive(Debug)]
struct Fake {
    calls: AtomicUsize,
    last_len: AtomicUsize,
    answer: Mutex<Result<Outcome, Unavailable>>,
}

impl Sink for Fake {
    fn submit(&self, body: Vec<u8>) -> BoxFuture<'_, Result<Outcome, Unavailable>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.last_len.store(body.len(), Ordering::SeqCst);
        let a = *self.answer.lock().unwrap();
        Box::pin(async move { a })
    }
}

struct Fixture {
    app: Router,
    auth: Arc<Auth>,
    fake: Arc<Fake>,
    _dir: tempfile::TempDir,
}

const ACCEPTED: Outcome = Outcome::Accepted {
    info_hash: [0xab; 20],
    private: false,
};

/// `burst` uploads per address and no refill, so a slow runner can't
/// refill a bucket; `None` installs no [`Uploader`].
fn fixture(burst: Option<f64>) -> Fixture {
    fixture_with(burst, None)
}

/// [`fixture`] with `sink` in place of the [`Fake`] when it's given.
fn fixture_with(burst: Option<f64>, sink: Option<Arc<dyn Sink>>) -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("t.db");
    let store = Store::open(&db, StoreConfig::default()).unwrap();
    let reader = Store::open_reader(&db, StoreConfig::default()).unwrap();
    let auth = Arc::new(Auth::load(&dir.path().join("auth.json"), false).unwrap());
    let state = ApiState {
        auth: auth.clone(),
        index: None,
        reader: Arc::new(Mutex::new(reader)),
        filters: None,
        settings: None,
        ops: None,
        user_data: Some(Arc::new(Mutex::new(store))),
        adult: Default::default(),
        insights: None,
        hot: None,
        events: None,
    };
    let fake = Arc::new(Fake {
        calls: AtomicUsize::new(0),
        last_len: AtomicUsize::new(0),
        answer: Mutex::new(Ok(ACCEPTED)),
    });
    let mut app = Router::new().nest("/api/v1", router(state));
    if let Some(burst) = burst {
        let sink: Arc<dyn Sink> = sink.unwrap_or_else(|| fake.clone());
        app = app.layer(Extension(Uploader::with_rate(
            sink,
            burst,
            0.0,
            upload::RATE_MAX_IPS,
        )));
    }
    Fixture {
        app,
        auth,
        fake,
        _dir: dir,
    }
}

struct Reply {
    status: StatusCode,
    cookie: Option<String>,
    retry_after: Option<String>,
    body: Value,
    text: String,
}

async fn send(app: &Router, mut req: Request<Body>, from: SocketAddr) -> Reply {
    req.extensions_mut().insert(ConnectInfo(from));
    let resp = app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let cookie = resp
        .headers()
        .get(header::SET_COOKIE)
        .map(|v| v.to_str().unwrap().split(';').next().unwrap().to_string());
    let retry_after = resp
        .headers()
        .get(header::RETRY_AFTER)
        .map(|v| v.to_str().unwrap().to_string());
    let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20)
        .await
        .unwrap();
    Reply {
        status,
        cookie,
        retry_after,
        body: serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        text: String::from_utf8_lossy(&bytes).into_owned(),
    }
}

fn json_req(method: Method, uri: &str, cookie: Option<&str>, body: Value) -> Request<Body> {
    let mut b = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::CONTENT_TYPE, "application/json");
    if let Some(c) = cookie {
        b = b.header(header::COOKIE, c);
    }
    b.body(Body::from(body.to_string())).unwrap()
}

/// An upload as the UI sends it: the file as the body, from the page's
/// own origin.
fn upload_req(cookie: Option<&str>, body: Vec<u8>, extra: &[(&str, &str)]) -> Request<Body> {
    let mut b = Request::post("/api/v1/torrents/upload")
        .header(header::HOST, HOST)
        .header(header::ORIGIN, format!("http://{HOST}"))
        .header("sec-fetch-site", "same-origin");
    if let Some(c) = cookie {
        b = b.header(header::COOKIE, c);
    }
    let has_type = extra
        .iter()
        .any(|(k, _)| k.eq_ignore_ascii_case("content-type"));
    if !has_type {
        b = b.header(header::CONTENT_TYPE, upload::CONTENT_TYPE);
    }
    for (k, v) in extra {
        b = b.header(*k, *v);
    }
    b.body(Body::from(body)).unwrap()
}

fn a_file() -> Vec<u8> {
    b"d4:infod6:lengthi1e4:name4:Some12:piece lengthi16384e6:pieces20:aaaaaaaaaaaaaaaaaaaaee"
        .to_vec()
}

async fn signed_in(f: &Fixture) -> String {
    let token = f.auth.setup_token().unwrap();
    let r = send(
        &f.app,
        json_req(
            Method::POST,
            "/api/v1/setup",
            None,
            json!({ "token": token, "password": PASSWORD, "notice_version": lodestar_api::legal::NOTICE_VERSION }),
        ),
        fresh_ip(),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    r.cookie.unwrap()
}

fn code(r: &Reply) -> &str {
    r.body["error"]["code"].as_str().unwrap_or("")
}

#[test]
fn the_cap_is_the_pipelines() {
    assert_eq!(
        upload::MAX_BYTES,
        lodestar_pipeline::local_torrent::MAX_BYTES
    );
    assert_eq!(upload::MAX_BYTES, 8 << 20);
    assert_eq!((upload::RATE_BURST, upload::RATE_PER_SEC), (30.0, 1.0));
    assert_eq!(upload::RATE_MAX_IPS, 10_000);
}

#[tokio::test]
async fn an_accepted_upload_answers_its_hash() {
    let f = fixture(Some(30.0));
    let cookie = signed_in(&f).await;
    let r = send(&f.app, upload_req(Some(&cookie), a_file(), &[]), fresh_ip()).await;
    assert_eq!(r.status, StatusCode::ACCEPTED, "{}", r.text);
    assert_eq!(
        r.body,
        json!({ "info_hash": "ab".repeat(20), "private": false })
    );
    assert_eq!(f.fake.calls.load(Ordering::SeqCst), 1);
    assert_eq!(f.fake.last_len.load(Ordering::SeqCst), a_file().len());
}

#[tokio::test]
async fn every_other_content_type_is_415_unread() {
    let f = fixture(Some(100.0));
    let cookie = signed_in(&f).await;
    for t in [
        "multipart/form-data; boundary=----x",
        "application/x-www-form-urlencoded",
        "text/plain",
        "application/json",
        "application/octet-stream",
        "application/x-bittorrent; charset=utf-8",
        "application/x-bittorrent;name=a.torrent",
        "",
    ] {
        let r = send(
            &f.app,
            upload_req(Some(&cookie), a_file(), &[("content-type", t)]),
            fresh_ip(),
        )
        .await;
        assert_eq!(r.status, StatusCode::UNSUPPORTED_MEDIA_TYPE, "{t:?}");
        assert_eq!(code(&r), "unsupported_media_type");
    }
    // No content type at all.
    let mut req = upload_req(Some(&cookie), a_file(), &[]);
    req.headers_mut().remove(header::CONTENT_TYPE);
    let r = send(&f.app, req, fresh_ip()).await;
    assert_eq!(r.status, StatusCode::UNSUPPORTED_MEDIA_TYPE);
    // A multipart body under the right type is just a malformed file to
    // the pipeline: nothing parses multipart.
    assert_eq!(f.fake.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn the_cap_is_refused_before_anything_is_parsed() {
    let f = fixture(Some(100.0));
    let cookie = signed_in(&f).await;
    // A declared length over the cap: refused unread.
    let mut req = upload_req(Some(&cookie), Vec::new(), &[]);
    req.headers_mut().insert(
        header::CONTENT_LENGTH,
        (upload::MAX_BYTES + 1).to_string().parse().unwrap(),
    );
    let r = send(&f.app, req, fresh_ip()).await;
    assert_eq!(r.status, StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(code(&r), "body_too_large");

    // A body over the cap with no length, streamed in chunks: stopped at
    // the cap.
    let chunks: Vec<Result<Vec<u8>, std::io::Error>> =
        (0..9).map(|_| Ok(vec![b'x'; 1 << 20])).collect();
    let mut req = upload_req(Some(&cookie), Vec::new(), &[]);
    *req.body_mut() = Body::from_stream(futures_util_stream(chunks));
    let r = send(&f.app, req, fresh_ip()).await;
    assert_eq!(r.status, StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(f.fake.calls.load(Ordering::SeqCst), 0, "nothing parsed");

    // Exactly the cap is taken.
    let r = send(
        &f.app,
        upload_req(Some(&cookie), vec![b'x'; upload::MAX_BYTES], &[]),
        fresh_ip(),
    )
    .await;
    assert_eq!(r.status, StatusCode::ACCEPTED, "{}", r.text);
    assert_eq!(f.fake.last_len.load(Ordering::SeqCst), upload::MAX_BYTES);
}

/// A stream of `chunks` for `Body::from_stream`, without a dependency.
fn futures_util_stream(
    chunks: Vec<Result<Vec<u8>, std::io::Error>>,
) -> impl futures_core::Stream<Item = Result<Vec<u8>, std::io::Error>> + Send + 'static {
    struct Iter(std::vec::IntoIter<Result<Vec<u8>, std::io::Error>>);
    impl futures_core::Stream for Iter {
        type Item = Result<Vec<u8>, std::io::Error>;
        fn poll_next(
            mut self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Option<Self::Item>> {
            std::task::Poll::Ready(self.0.next())
        }
    }
    Iter(chunks.into_iter())
}

#[tokio::test]
async fn no_session_cross_site_and_api_keys_are_refused() {
    let f = fixture(Some(100.0));
    let cookie = signed_in(&f).await;

    // No session.
    let r = send(&f.app, upload_req(None, a_file(), &[]), fresh_ip()).await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    assert_eq!(code(&r), "login_required");

    // An API key with every scope: none grants the upload.
    let r = send(
        &f.app,
        json_req(
            Method::POST,
            "/api/v1/keys",
            Some(&cookie),
            json!({ "name": "Every scope", "kind": "api_key", "scopes": ["search", "torznab", "feeds"], "password": PASSWORD }),
        ),
        fresh_ip(),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    let key = r.body["secret"].as_str().unwrap().to_string();
    for presented in [
        ("authorization", format!("Bearer {key}")),
        ("x-api-key", key.clone()),
    ] {
        let r = send(
            &f.app,
            upload_req(None, a_file(), &[(presented.0, &presented.1)]),
            fresh_ip(),
        )
        .await;
        assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{}", presented.0);
    }
    let mut req = upload_req(None, a_file(), &[]);
    *req.uri_mut() = format!("/api/v1/torrents/upload?apikey={key}")
        .parse()
        .unwrap();
    assert_eq!(
        send(&f.app, req, fresh_ip()).await.status,
        StatusCode::UNAUTHORIZED
    );

    // Another site's page, by Origin or by Sec-Fetch-Site.
    let mut req = upload_req(Some(&cookie), a_file(), &[]);
    req.headers_mut()
        .insert(header::ORIGIN, "https://evil.example".parse().unwrap());
    let r = send(&f.app, req, fresh_ip()).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    assert_eq!(code(&r), "cross_origin");
    let mut req = upload_req(Some(&cookie), a_file(), &[]);
    req.headers_mut()
        .insert("sec-fetch-site", "cross-site".parse().unwrap());
    let r = send(&f.app, req, fresh_ip()).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);

    // No Origin at all, as for a JSON write: let through.
    let mut req = upload_req(Some(&cookie), a_file(), &[]);
    req.headers_mut().remove(header::ORIGIN);
    req.headers_mut().remove("sec-fetch-site");
    assert_eq!(
        send(&f.app, req, fresh_ip()).await.status,
        StatusCode::ACCEPTED
    );

    assert_eq!(f.fake.calls.load(Ordering::SeqCst), 1, "only the last one");
}

#[tokio::test]
async fn the_rate_bucket_is_its_own_not_logins() {
    let f = fixture(Some(2.0));
    let cookie = signed_in(&f).await;
    let ip = fresh_ip();
    for _ in 0..2 {
        let r = send(&f.app, upload_req(Some(&cookie), a_file(), &[]), ip).await;
        assert_eq!(r.status, StatusCode::ACCEPTED);
    }
    let r = send(&f.app, upload_req(Some(&cookie), a_file(), &[]), ip).await;
    assert_eq!(r.status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(code(&r), "rate_limited");
    assert!(r.retry_after.is_some());
    assert_eq!(f.fake.calls.load(Ordering::SeqCst), 2, "refused unread");

    // Login from the same address still has all its attempts.
    let bad = json!({ "password": "wrong wrong wrong" });
    for _ in 0..5 {
        let r = send(
            &f.app,
            json_req(Method::POST, "/api/v1/login", None, bad.clone()),
            ip,
        )
        .await;
        assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{}", r.text);
    }
    // And the other way: an address that's used up its logins can upload.
    let other = fresh_ip();
    for _ in 0..6 {
        send(
            &f.app,
            json_req(Method::POST, "/api/v1/login", None, bad.clone()),
            other,
        )
        .await;
    }
    let r = send(
        &f.app,
        json_req(Method::POST, "/api/v1/login", None, bad.clone()),
        other,
    )
    .await;
    assert_eq!(r.status, StatusCode::TOO_MANY_REQUESTS);
    let r = send(&f.app, upload_req(Some(&cookie), a_file(), &[]), other).await;
    assert_eq!(r.status, StatusCode::ACCEPTED);
}

/// Behind a trusted proxy the bucket is the forwarded client's, as the
/// login limiter's is, never the proxy's.
#[tokio::test]
async fn behind_a_trusted_proxy_the_bucket_is_the_clients() {
    let f = fixture(Some(1.0));
    let cookie = signed_in(&f).await;
    let r = send(
        &f.app,
        json_req(
            Method::PUT,
            "/api/v1/auth/proxy",
            Some(&cookie),
            json!({ "enabled": true, "trusted": ["172.17.0.0/16"], "allowed_users": [], "password": PASSWORD }),
        ),
        fresh_ip(),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    let proxy = SocketAddr::from((PROXY, 50000));
    let as_client = |xff: &str| {
        upload_req(
            None,
            a_file(),
            &[("remote-user", "ann"), ("x-forwarded-for", xff)],
        )
    };
    let r = send(&f.app, as_client("203.0.113.7"), proxy).await;
    assert_eq!(
        r.status,
        StatusCode::ACCEPTED,
        "proxy auth is a JSON write's guard: {}",
        r.text
    );
    let r = send(&f.app, as_client("203.0.113.7"), proxy).await;
    assert_eq!(r.status, StatusCode::TOO_MANY_REQUESTS);
    let r = send(&f.app, as_client("6.6.6.6, 203.0.113.7"), proxy).await;
    assert_eq!(
        r.status,
        StatusCode::TOO_MANY_REQUESTS,
        "a spoofed left entry"
    );
    let r = send(&f.app, as_client("203.0.113.8"), proxy).await;
    assert_eq!(r.status, StatusCode::ACCEPTED, "another client has its own");
}

#[tokio::test]
async fn each_outcome_names_its_cause_and_nothing_from_the_file() {
    let f = fixture(Some(100.0));
    let cookie = signed_in(&f).await;
    let file = b"d4:infod4:name17:Secret.Name.2024ee".to_vec();
    for (answer, status, want) in [
        (
            Ok(Outcome::Malformed),
            StatusCode::BAD_REQUEST,
            "malformed_torrent",
        ),
        (
            Ok(Outcome::Refused),
            StatusCode::UNPROCESSABLE_ENTITY,
            "refused",
        ),
        (Ok(Outcome::Held), StatusCode::UNPROCESSABLE_ENTITY, "held"),
        (
            Err(Unavailable),
            StatusCode::SERVICE_UNAVAILABLE,
            "uploads_unavailable",
        ),
    ] {
        *f.fake.answer.lock().unwrap() = answer;
        let r = send(
            &f.app,
            upload_req(Some(&cookie), file.clone(), &[]),
            fresh_ip(),
        )
        .await;
        assert_eq!((r.status, code(&r)), (status, want), "{}", r.text);
        assert!(!r.text.contains("Secret"), "{}", r.text);
    }
    *f.fake.answer.lock().unwrap() = Ok(Outcome::Accepted {
        info_hash: [1; 20],
        private: true,
    });
    let r = send(&f.app, upload_req(Some(&cookie), file, &[]), fresh_ip()).await;
    assert_eq!(r.status, StatusCode::ACCEPTED);
    assert_eq!(r.body["private"], true);
}

#[tokio::test]
async fn without_an_uploader_it_is_unavailable() {
    let f = fixture(None);
    let cookie = signed_in(&f).await;
    let r = send(&f.app, upload_req(Some(&cookie), a_file(), &[]), fresh_ip()).await;
    assert_eq!(r.status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(code(&r), "uploads_unavailable");
}

#[tokio::test]
async fn other_methods_and_paths_take_no_upload() {
    let f = fixture(Some(100.0));
    let cookie = signed_in(&f).await;
    for m in [Method::PUT, Method::GET, Method::PATCH] {
        let mut req = upload_req(Some(&cookie), a_file(), &[]);
        *req.method_mut() = m.clone();
        let r = send(&f.app, req, fresh_ip()).await;
        assert_eq!(r.status, StatusCode::METHOD_NOT_ALLOWED, "{m}");
    }
    let mut req = upload_req(Some(&cookie), a_file(), &[]);
    *req.uri_mut() = "/api/v1/torrents/upload/extra".parse().unwrap();
    assert_ne!(
        send(&f.app, req, fresh_ip()).await.status,
        StatusCode::ACCEPTED
    );
    assert_eq!(f.fake.calls.load(Ordering::SeqCst), 0);
}

/// A body that sends `first` and then stalls; `polled` turns true once
/// the route starts reading it.
fn stalled_body(first: Vec<u8>, polled: Arc<std::sync::atomic::AtomicBool>) -> Body {
    struct Stall(Option<Vec<u8>>, Arc<std::sync::atomic::AtomicBool>);
    impl futures_core::Stream for Stall {
        type Item = Result<Vec<u8>, std::io::Error>;
        fn poll_next(
            mut self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Option<Self::Item>> {
            self.1.store(true, Ordering::SeqCst);
            match self.0.take() {
                Some(b) => std::task::Poll::Ready(Some(Ok(b))),
                // Never ready again, and never wakes: a stalled client.
                None => std::task::Poll::Pending,
            }
        }
    }
    Body::from_stream(Stall(Some(first), polled))
}

/// A stalled client loses its turn after the read deadline: `408`, and
/// nothing reaches the pipeline. Paused time, so no real wait.
#[tokio::test]
async fn a_stalled_body_times_out() {
    let f = fixture(Some(100.0));
    let cookie = signed_in(&f).await;
    tokio::time::pause();
    let mut req = upload_req(Some(&cookie), Vec::new(), &[]);
    *req.body_mut() = stalled_body(b"d4:info".to_vec(), Arc::default());
    let r = send(&f.app, req, fresh_ip()).await;
    assert_eq!(r.status, StatusCode::REQUEST_TIMEOUT, "{}", r.text);
    assert_eq!(code(&r), "body_timeout");
    assert_eq!(f.fake.calls.load(Ordering::SeqCst), 0);
}

/// Holds every upload until the test lets one through.
#[derive(Debug)]
struct Gated {
    entered: AtomicUsize,
    gate: tokio::sync::Semaphore,
}

impl Sink for Gated {
    fn submit(&self, _: Vec<u8>) -> BoxFuture<'_, Result<Outcome, Unavailable>> {
        self.entered.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move {
            self.gate.acquire().await.unwrap().forget();
            Ok(ACCEPTED)
        })
    }
}

/// At most `IN_FLIGHT` requests are read or wait on the pipeline: one
/// more waits with its body unread until a turn frees up.
#[tokio::test]
async fn past_the_in_flight_bound_a_body_waits_unread() {
    let gated = Arc::new(Gated {
        entered: AtomicUsize::new(0),
        gate: tokio::sync::Semaphore::new(0),
    });
    let f = fixture_with(Some(100.0), Some(gated.clone()));
    let cookie = signed_in(&f).await;
    // Paused time: each `sleep` below returns only once every task is
    // idle, so what's asserted after it has had every chance to happen.
    tokio::time::pause();
    let mut tasks = Vec::new();
    for _ in 0..upload::IN_FLIGHT {
        let (app, req) = (f.app.clone(), upload_req(Some(&cookie), a_file(), &[]));
        tasks.push(tokio::spawn(
            async move { send(&app, req, fresh_ip()).await },
        ));
    }
    let polled = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let mut req = upload_req(Some(&cookie), Vec::new(), &[]);
    *req.body_mut() = stalled_body(a_file(), polled.clone());
    let app = f.app.clone();
    let fifth = tokio::spawn(async move { send(&app, req, fresh_ip()).await });
    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    assert_eq!(gated.entered.load(Ordering::SeqCst), upload::IN_FLIGHT);
    assert!(!polled.load(Ordering::SeqCst), "the fifth body was read");
    // One finishes: the fifth gets its turn and starts reading.
    gated.gate.add_permits(1);
    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    assert!(polled.load(Ordering::SeqCst), "the fifth never got a turn");
    gated.gate.add_permits(upload::IN_FLIGHT);
    for t in tasks {
        assert_eq!(t.await.unwrap().status, StatusCode::ACCEPTED);
    }
    // The fifth stalls after its first chunk, so its deadline answers it.
    assert_eq!(fifth.await.unwrap().status, StatusCode::REQUEST_TIMEOUT);
}
