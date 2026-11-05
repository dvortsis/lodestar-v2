//! `/webhooks` (SRC-11b): the Sonarr and Radarr inbound route and its
//! tokens, with golden response bytes in `tests/golden/webhooks_*.txt`
//! (`LODESTAR_BLESS=1 cargo test -p lodestar-api --test webhooks`
//! rewrites them; review the diff before committing it). No fixed sleeps:
//! nothing here waits on time.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Method, Request, StatusCode, header};
use lodestar_api::auth::KEY_FAIL_BURST;
use lodestar_api::webhooks::{Delivered, Integration, MAX_BODY, RATE_BURST, WebhookSink, Webhooks};
use lodestar_api::{ApiState, Auth, router};
use lodestar_store::{Store, StoreConfig};
use serde_json::{Value, json};
use tower::ServiceExt;

const PASSWORD: &str = "correct horse battery";
static NEXT_IP: AtomicU32 = AtomicU32::new(1);

fn fresh_ip() -> SocketAddr {
    let n = NEXT_IP.fetch_add(1, Ordering::Relaxed);
    SocketAddr::from(([10, 9, (n >> 8) as u8, n as u8], 1))
}

/// A sink that judges bodies by `eventType` with `serde_json` (the real
/// parser is the plugin's, tested in `lodestar-sources-plugins`), and
/// answers `mode` when set.
#[derive(Default)]
struct Sink {
    calls: AtomicUsize,
    mode: Mutex<Option<Delivered>>,
    last: Mutex<Option<(Integration, usize)>>,
}

impl WebhookSink for Sink {
    fn deliver(&self, integration: Integration, body: &[u8]) -> Delivered {
        self.calls.fetch_add(1, Ordering::Relaxed);
        *self.last.lock().unwrap() = Some((integration, body.len()));
        if let Some(m) = *self.mode.lock().unwrap() {
            return m;
        }
        match serde_json::from_slice::<Value>(body) {
            Ok(v) if v["eventType"] == "Grab" => Delivered::Found,
            Ok(v) if v.is_object() => Delivered::Ignored,
            _ => Delivered::Malformed,
        }
    }
}

struct Fixture {
    app: Router,
    /// The same routes with no [`Webhooks`] layer.
    bare: Router,
    /// Wired, but with no user-data store to read tokens from.
    nostore: Router,
    auth: Arc<Auth>,
    sink: Arc<Sink>,
    _dir: tempfile::TempDir,
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("t.db");
    let store = Arc::new(Mutex::new(
        Store::open(&db, StoreConfig::default()).unwrap(),
    ));
    let reader = Store::open_reader(&db, StoreConfig::default()).unwrap();
    // No refills, so emptying a bucket doesn't depend on the machine's
    // speed.
    let auth = Arc::new(
        Auth::load(&dir.path().join("auth.json"), false)
            .unwrap()
            .without_refill(),
    );
    let state = ApiState {
        auth: auth.clone(),
        index: None,
        reader: Arc::new(Mutex::new(reader)),
        filters: None,
        hot: None,
        events: None,
        settings: None,
        ops: None,
        user_data: Some(store),
        adult: Default::default(),
        insights: None,
    };
    let sink = Arc::new(Sink::default());
    let hub = Webhooks::new(sink.clone());
    Fixture {
        app: Router::new().nest("/api/v1", router(state.clone()).layer(hub.layer())),
        nostore: Router::new().nest(
            "/api/v1",
            router(ApiState {
                user_data: None,
                ..state.clone()
            })
            .layer(Webhooks::new(sink.clone()).layer()),
        ),
        bare: Router::new().nest("/api/v1", router(state)),
        auth,
        sink,
        _dir: dir,
    }
}

struct Reply {
    status: StatusCode,
    retry_after: Option<String>,
    cache_control: Option<String>,
    text: String,
    body: Value,
}

async fn send_to(app: &Router, mut req: Request<Body>, from: SocketAddr) -> Reply {
    req.extensions_mut().insert(ConnectInfo(from));
    let resp = app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let h = |n| {
        resp.headers()
            .get(n)
            .map(|v: &header::HeaderValue| v.to_str().unwrap().to_owned())
    };
    let (retry_after, cache_control) = (h(header::RETRY_AFTER), h(header::CACHE_CONTROL));
    let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20)
        .await
        .unwrap();
    let text = String::from_utf8_lossy(&bytes).into_owned();
    Reply {
        status,
        retry_after,
        cache_control,
        body: serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        text,
    }
}

async fn send(f: &Fixture, req: Request<Body>) -> Reply {
    send_to(&f.app, req, fresh_ip()).await
}

/// Checks `r` against `tests/golden/<name>.txt`, `blank`'s string values
/// shown as `"…"`.
fn golden(name: &str, r: &Reply, blank: &[&str]) {
    let mut v = r.body.clone();
    if let Value::Object(m) = &mut v {
        for k in blank {
            if m.get(*k).is_some_and(Value::is_string) {
                m.insert((*k).to_owned(), json!("…"));
            }
        }
    }
    // Raw bytes unless something had to be blanked.
    let shown = if v.is_null() || blank.is_empty() {
        r.text.clone()
    } else {
        v.to_string()
    };
    let got = format!("{}\n{shown}\n", r.status.as_u16());
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden")
        .join(format!("{name}.txt"));
    if std::env::var_os("LODESTAR_BLESS").is_some() {
        std::fs::write(&path, &got).unwrap();
    } else {
        let want = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("{}: {e} (bless with LODESTAR_BLESS=1)", path.display()));
        assert_eq!(got, want.replace("\r\n", "\n"), "{name}");
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

async fn signed_in(f: &Fixture) -> String {
    let token = f.auth.setup_token().unwrap();
    let mut req = json_req(
        Method::POST,
        "/api/v1/setup",
        None,
        json!({ "token": token, "password": PASSWORD, "notice_version": lodestar_api::legal::NOTICE_VERSION }),
    );
    req.extensions_mut().insert(ConnectInfo(fresh_ip()));
    let resp = f.app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    resp.headers()[header::SET_COOKIE]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned()
}

async fn make_token(f: &Fixture, cookie: &str, integration: &str) -> String {
    let r = send(
        f,
        json_req(
            Method::POST,
            &format!("/api/v1/webhooks/{integration}/token"),
            Some(cookie),
            json!({ "password": PASSWORD }),
        ),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    r.body["token"].as_str().unwrap().to_owned()
}

fn basic(user: &str, pass: &str) -> String {
    const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let raw = format!("{user}:{pass}");
    let mut out = String::from("Basic ");
    for chunk in raw.as_bytes().chunks(3) {
        let n = chunk
            .iter()
            .enumerate()
            .fold(0u32, |acc, (i, &b)| acc | (u32::from(b) << (16 - 8 * i)));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(char::from(B64[((n >> (18 - 6 * i)) & 63) as usize]));
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// A call from the app: `auth` is the whole `Authorization` value.
fn call(integration: &str, auth: Option<&str>, body: impl Into<Body>) -> Request<Body> {
    let mut b = Request::post(format!("/api/v1/webhooks/{integration}"))
        .header(header::CONTENT_TYPE, "application/json");
    if let Some(a) = auth {
        b = b.header(header::AUTHORIZATION, a);
    }
    b.body(body.into()).unwrap()
}

const GRAB: &str =
    r#"{"eventType":"Grab","downloadId":"FB9FF1EF824D61BD83B24FB91811804DB71C0C53"}"#;

#[tokio::test]
async fn tokens_are_made_with_the_password_shown_once_and_listed_as_set() {
    let f = fixture();
    let cookie = signed_in(&f).await;
    let list = || json_req(Method::GET, "/api/v1/webhooks", Some(&cookie), json!({}));
    golden("webhooks_list_none", &send(&f, list()).await, &[]);

    // No password, or the wrong one: nothing is made.
    let r = send(
        &f,
        json_req(
            Method::POST,
            "/api/v1/webhooks/sonarr/token",
            Some(&cookie),
            json!({}),
        ),
    )
    .await;
    golden("webhooks_token_confirm_required", &r, &[]);
    let r = send(
        &f,
        json_req(
            Method::POST,
            "/api/v1/webhooks/lidarr/token",
            Some(&cookie),
            json!({ "password": PASSWORD }),
        ),
    )
    .await;
    golden("webhooks_token_unknown", &r, &[]);

    let r = send(
        &f,
        json_req(
            Method::POST,
            "/api/v1/webhooks/sonarr/token",
            Some(&cookie),
            json!({ "password": PASSWORD }),
        ),
    )
    .await;
    golden("webhooks_token_created", &r, &["token"]);
    assert_eq!(r.cache_control.as_deref(), Some("no-store"));
    let token = r.body["token"].as_str().unwrap();
    assert!(token.starts_with("lsw_") && token.len() == 47);

    let r = send(&f, list()).await;
    golden("webhooks_list_sonarr_set", &r, &[]);
    // The list never shows a token or its hash.
    assert!(!r.text.contains(token) && !r.text.contains(&token[4..20]));

    // The session is needed for every token route.
    for (method, uri) in [
        (Method::GET, "/api/v1/webhooks"),
        (Method::POST, "/api/v1/webhooks/sonarr/token"),
        (Method::DELETE, "/api/v1/webhooks/sonarr/token"),
    ] {
        let r = send(
            &f,
            json_req(method, uri, None, json!({ "password": PASSWORD })),
        )
        .await;
        assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{uri}");
    }
}

#[tokio::test]
async fn a_good_token_delivers_and_every_bad_one_gets_the_same_401() {
    let f = fixture();
    let cookie = signed_in(&f).await;
    let sonarr = make_token(&f, &cookie, "sonarr").await;
    let radarr = make_token(&f, &cookie, "radarr").await;

    // Basic auth's password (the apps' fields), or Bearer.
    let r = send(&f, call("sonarr", Some(&basic("sonarr", &sonarr)), GRAB)).await;
    golden("webhooks_receive_found", &r, &[]);
    assert_eq!(
        *f.sink.last.lock().unwrap(),
        Some((Integration::Sonarr, GRAB.len()))
    );
    let r = send(
        &f,
        call(
            "radarr",
            Some(&format!("Bearer {radarr}")),
            r#"{"eventType":"Test"}"#,
        ),
    )
    .await;
    golden("webhooks_receive_ignored", &r, &[]);
    assert_eq!(f.sink.calls.load(Ordering::Relaxed), 2);

    // Every way of failing: one answer, byte for byte, and the body is
    // never handed on.
    let wrong = format!("lsw_{}", "A".repeat(43));
    let fails: Vec<(&str, Option<String>)> = vec![
        ("sonarr", None),
        ("sonarr", Some(format!("Bearer {wrong}"))),
        ("sonarr", Some(basic("sonarr", &wrong))),
        ("sonarr", Some(format!("Bearer {radarr}"))),
        ("radarr", Some(basic("radarr", &sonarr))),
        ("sonarr", Some(format!("Bearer {}", &sonarr[..46]))),
        ("sonarr", Some(basic(&sonarr, ""))),
        ("sonarr", Some(format!("Token {sonarr}"))),
        ("lidarr", Some(format!("Bearer {sonarr}"))),
        ("sonarr", Some("Basic !!!!".to_owned())),
    ];
    let mut first = None;
    for (integration, auth) in &fails {
        let r = send(&f, call(integration, auth.as_deref(), GRAB)).await;
        assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{integration} {auth:?}");
        if first.is_none() {
            golden("webhooks_receive_unauthorized", &r, &[]);
        }
        let text = first.get_or_insert_with(|| r.text.clone());
        assert_eq!(&r.text, text, "{integration} {auth:?}");
        // Not the token, not any part of it.
        if let Some(a) = auth {
            assert!(!r.text.contains(a.split(' ').nth(1).unwrap_or("")));
        }
    }
    // A query-string token is never read.
    let r = send(
        &f,
        Request::post(format!("/api/v1/webhooks/sonarr?token={sonarr}"))
            .body(Body::from(GRAB))
            .unwrap(),
    )
    .await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    assert_eq!(f.sink.calls.load(Ordering::Relaxed), 2);
}

#[tokio::test]
async fn bodies_are_capped_before_and_while_reading_and_malformed_ones_counted() {
    let f = fixture();
    let cookie = signed_in(&f).await;
    let token = make_token(&f, &cookie, "sonarr").await;
    let auth = format!("Bearer {token}");

    let r = send(&f, call("sonarr", Some(&auth), "{not json")).await;
    golden("webhooks_receive_malformed", &r, &[]);

    // Declared too large: refused before reading.
    let r = send(
        &f,
        Request::post("/api/v1/webhooks/sonarr")
            .header(header::AUTHORIZATION, &auth)
            .header(header::CONTENT_LENGTH, (MAX_BODY + 1).to_string())
            .body(Body::from(vec![b' '; MAX_BODY + 1]))
            .unwrap(),
    )
    .await;
    golden("webhooks_receive_too_large", &r, &[]);
    // Not declared (a stream): refused once past the cap.
    let chunks: Vec<Result<Vec<u8>, std::io::Error>> = (0..=MAX_BODY / 4096)
        .map(|_| Ok(vec![b' '; 4096]))
        .collect();
    let stream = Body::from_stream(futures_util_stream(chunks));
    let r = send(&f, call("sonarr", Some(&auth), stream)).await;
    assert_eq!(r.status, StatusCode::PAYLOAD_TOO_LARGE, "{}", r.text);
    // Exactly the cap is read.
    let mut edge = GRAB.as_bytes().to_vec();
    edge.resize(MAX_BODY, b' ');
    let r = send(&f, call("sonarr", Some(&auth), edge)).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(
        f.sink.calls.load(Ordering::Relaxed),
        2,
        "too-large bodies never reach the sink"
    );

    let r = send(
        &f,
        json_req(Method::GET, "/api/v1/webhooks", Some(&cookie), json!({})),
    )
    .await;
    let s = &r.body["integrations"][0];
    assert_eq!(
        (&s["found"], &s["malformed"], &s["refused"]),
        (&json!(1), &json!(1), &json!(2))
    );
}

/// A body stream from chunks, without a `futures` dependency.
fn futures_util_stream(
    chunks: Vec<Result<Vec<u8>, std::io::Error>>,
) -> impl futures_core::Stream<Item = Result<Vec<u8>, std::io::Error>> + Send + 'static {
    struct S(std::vec::IntoIter<Result<Vec<u8>, std::io::Error>>);
    impl futures_core::Stream for S {
        type Item = Result<Vec<u8>, std::io::Error>;
        fn poll_next(
            mut self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Option<Self::Item>> {
            std::task::Poll::Ready(self.0.next())
        }
    }
    S(chunks.into_iter())
}

#[tokio::test]
async fn each_integration_is_rate_limited_on_its_own() {
    let f = fixture();
    let cookie = signed_in(&f).await;
    let sonarr = format!("Bearer {}", make_token(&f, &cookie, "sonarr").await);
    let radarr = format!("Bearer {}", make_token(&f, &cookie, "radarr").await);
    // Refills a call a second; a slow runner may earn a few more. A call
    // over the rate is dropped, but still answered 200, so the app
    // doesn't back its notifications off.
    let mut ok = 0;
    let limited = loop {
        let r = send(&f, call("sonarr", Some(&sonarr), GRAB)).await;
        assert_eq!(r.status, StatusCode::OK);
        if r.body["dropped"] == true {
            break r;
        }
        ok += 1;
        assert!(ok < 10 * RATE_BURST as usize, "never limited");
    };
    assert!(ok >= RATE_BURST as usize);
    golden("webhooks_receive_rate_limited", &limited, &[]);
    assert_eq!(
        f.sink.calls.load(Ordering::Relaxed),
        ok,
        "a dropped call isn't delivered"
    );
    let r = send(&f, call("radarr", Some(&radarr), GRAB)).await;
    assert_eq!(r.body, json!({ "found": true, "dropped": false }));
}

/// Wrong well-formed tokens spend the address's failed-check bucket that
/// API keys use; tokens of the wrong shape spend nothing (they can't be
/// guesses). A good token from another address still works.
#[tokio::test]
async fn failed_tokens_spend_the_per_address_bucket() {
    let f = fixture();
    let cookie = signed_in(&f).await;
    let token = format!("Bearer {}", make_token(&f, &cookie, "sonarr").await);
    let ip = fresh_ip();
    for _ in 0..50 {
        let r = send_to(&f.app, call("sonarr", Some("Bearer nope"), GRAB), ip).await;
        assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    }
    let wrong = format!("Bearer lsw_{}", "B".repeat(43));
    for _ in 0..KEY_FAIL_BURST as usize {
        let r = send_to(&f.app, call("sonarr", Some(&wrong), GRAB), ip).await;
        assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    }
    let r = send_to(&f.app, call("sonarr", Some(&wrong), GRAB), ip).await;
    golden("webhooks_receive_failures_limited", &r, &[]);
    assert!(r.retry_after.is_some());
    // Even the right token waits there; elsewhere it works.
    let r = send_to(&f.app, call("sonarr", Some(&token), GRAB), ip).await;
    assert_eq!(r.status, StatusCode::TOO_MANY_REQUESTS);
    let r = send(&f, call("sonarr", Some(&token), GRAB)).await;
    assert_eq!(r.status, StatusCode::OK);
}

#[tokio::test]
async fn a_new_token_replaces_the_old_and_a_removed_one_stops_working() {
    let f = fixture();
    let cookie = signed_in(&f).await;
    let old = format!("Bearer {}", make_token(&f, &cookie, "sonarr").await);
    let new = format!("Bearer {}", make_token(&f, &cookie, "sonarr").await);
    assert_ne!(old, new);
    let r = send(&f, call("sonarr", Some(&old), GRAB)).await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    let r = send(&f, call("sonarr", Some(&new), GRAB)).await;
    assert_eq!(r.status, StatusCode::OK);
    let r = send(
        &f,
        json_req(
            Method::DELETE,
            "/api/v1/webhooks/sonarr/token",
            Some(&cookie),
            json!({}),
        ),
    )
    .await;
    golden("webhooks_token_deleted", &r, &[]);
    let r = send(&f, call("sonarr", Some(&new), GRAB)).await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn dropped_calls_say_why_and_unwired_webhooks_read_nothing() {
    let f = fixture();
    let cookie = signed_in(&f).await;
    let token = format!("Bearer {}", make_token(&f, &cookie, "sonarr").await);
    // A real event dropped is still a 200; only the Test call errors.
    *f.sink.mode.lock().unwrap() = Some(Delivered::Off { test: false });
    let r = send(&f, call("sonarr", Some(&token), GRAB)).await;
    golden("webhooks_receive_dropped_off", &r, &[]);
    *f.sink.mode.lock().unwrap() = Some(Delivered::Off { test: true });
    let r = send(&f, call("sonarr", Some(&token), r#"{"eventType":"Test"}"#)).await;
    golden("webhooks_receive_source_off", &r, &[]);
    *f.sink.mode.lock().unwrap() = Some(Delivered::Busy);
    let r = send(&f, call("sonarr", Some(&token), GRAB)).await;
    golden("webhooks_receive_busy", &r, &[]);

    // Without the host's layer: still authenticated first, then 503.
    let calls = f.sink.calls.load(Ordering::Relaxed);
    let r = send_to(&f.bare, call("sonarr", None, GRAB), fresh_ip()).await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    let r = send_to(&f.bare, call("sonarr", Some(&token), GRAB), fresh_ip()).await;
    golden("webhooks_receive_unavailable", &r, &[]);
    assert_eq!(f.sink.calls.load(Ordering::Relaxed), calls);

    // No store to read tokens from: 503, not a 401, and nothing spent.
    let ip = fresh_ip();
    for _ in 0..2 * KEY_FAIL_BURST as usize {
        let r = send_to(&f.nostore, call("sonarr", Some(&token), GRAB), ip).await;
        assert_eq!(r.status, StatusCode::SERVICE_UNAVAILABLE, "{}", r.text);
    }
    assert_eq!(f.sink.calls.load(Ordering::Relaxed), calls);
}

/// A long Basic username still fits the header cap.
#[tokio::test]
async fn a_long_basic_username_is_fine() {
    let f = fixture();
    let cookie = signed_in(&f).await;
    let token = make_token(&f, &cookie, "radarr").await;
    let r = send(
        &f,
        call("radarr", Some(&basic(&"u".repeat(300), &token)), GRAB),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
}

/// The token routes keep the cached hashes current: a token made after
/// the first call works at once, and a removed one stops at once.
#[tokio::test]
async fn the_cache_follows_token_changes() {
    let f = fixture();
    let cookie = signed_in(&f).await;
    // The first call fills the cache: no token yet.
    let wrong = format!("Bearer lsw_{}", "C".repeat(43));
    let r = send(&f, call("sonarr", Some(&wrong), GRAB)).await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    let token = format!("Bearer {}", make_token(&f, &cookie, "sonarr").await);
    let r = send(&f, call("sonarr", Some(&token), GRAB)).await;
    assert_eq!(r.status, StatusCode::OK);
    let r = send(
        &f,
        json_req(
            Method::DELETE,
            "/api/v1/webhooks/sonarr/token",
            Some(&cookie),
            json!({}),
        ),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);
    let r = send(&f, call("sonarr", Some(&token), GRAB)).await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
}
