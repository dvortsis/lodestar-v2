//! The Legal Notice over HTTP (owner decision §2.38, LEGAL-1r): `POST
//! /setup` needs the current version, a signed-in browser meets `409
//! notice_required` on every session route until it accepts, and the
//! auth limits stay as they were. The error envelopes are checked
//! byte-exact against `tests/golden/` (bless with `LODESTAR_BLESS=1`).

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Request, StatusCode, header};
use lodestar_api::legal::{NOTICE, NOTICE_VERSION};
use lodestar_api::{ApiState, Auth, router};
use lodestar_store::{Store, StoreConfig};
use serde_json::{Value, json};
use tower::ServiceExt;

const PASSWORD: &str = "correct horse battery";
static NEXT_IP: AtomicU32 = AtomicU32::new(1);

struct App {
    app: Router,
    auth: Arc<Auth>,
    store: Arc<Mutex<Store>>,
}

/// The API over `dir`, showing notice `version`.
fn app(dir: &Path, version: u32) -> App {
    let db = dir.join("t.db");
    let store = Arc::new(Mutex::new(
        Store::open(&db, StoreConfig::default()).unwrap(),
    ));
    let reader = Store::open_reader(&db, StoreConfig::default()).unwrap();
    let auth = Arc::new(
        Auth::load(&dir.join("auth.json"), false)
            .unwrap()
            .with_notice_version(version)
            .without_refill(),
    );
    let app = Router::new().nest(
        "/api/v1",
        router(ApiState {
            auth: auth.clone(),
            index: None,
            reader: Arc::new(Mutex::new(reader)),
            filters: None,
            hot: None,
            events: None,
            settings: None,
            ops: None,
            user_data: Some(store.clone()),
            adult: Default::default(),
            insights: None,
        }),
    );
    App { app, auth, store }
}

/// From its own address unless `ip` is given, so the per-IP login limit
/// only bites where a case means it to.
async fn send(a: &App, mut req: Request<Body>, ip: Option<[u8; 4]>) -> (StatusCode, Value, String) {
    let ip = ip.unwrap_or_else(|| {
        let n = NEXT_IP.fetch_add(1, Ordering::Relaxed);
        [10, 9, (n >> 8) as u8, n as u8]
    });
    req.extensions_mut()
        .insert(ConnectInfo(SocketAddr::from((ip, 1))));
    let resp = a.app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let cookie = resp
        .headers()
        .get(header::SET_COOKIE)
        .map(|v| v.to_str().unwrap().split(';').next().unwrap().to_string())
        .unwrap_or_default();
    let bytes = axum::body::to_bytes(resp.into_body(), 1 << 22)
        .await
        .unwrap();
    let body = String::from_utf8(bytes.to_vec()).unwrap();
    (
        status,
        serde_json::from_str(&body).unwrap_or(Value::Null),
        cookie,
    )
}

fn get(uri: &str, cookie: Option<&str>) -> Request<Body> {
    let mut b = Request::get(uri);
    if let Some(c) = cookie {
        b = b.header(header::COOKIE, c);
    }
    b.body(Body::empty()).unwrap()
}

fn post(uri: &str, cookie: Option<&str>, body: Value) -> Request<Body> {
    let mut b = Request::post(uri).header(header::CONTENT_TYPE, "application/json");
    if let Some(c) = cookie {
        b = b.header(header::COOKIE, c);
    }
    b.body(Body::from(body.to_string())).unwrap()
}

/// Checks the status and body against `tests/golden/{name}.txt`.
async fn golden(a: &App, name: &str, req: Request<Body>) -> (StatusCode, Value, String) {
    let (status, body, cookie) = send(a, req, None).await;
    let got = format!("{}\n{body}\n", status.as_u16());
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden")
        .join(format!("{name}.txt"));
    if std::env::var_os("LODESTAR_BLESS").is_some() {
        std::fs::write(&path, &got).unwrap();
    } else {
        let want = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("{}: {e} (bless with LODESTAR_BLESS=1)", path.display()));
        assert_eq!(
            got,
            want.replace("\r\n", "\n"),
            "{name}: the response changed"
        );
    }
    (status, body, cookie)
}

fn setup_body(a: &App, notice: Option<u32>) -> Value {
    let mut body = json!({ "token": a.auth.setup_token().unwrap(), "password": PASSWORD });
    if let Some(v) = notice {
        body["notice_version"] = json!(v);
    }
    body
}

async fn login(a: &App) -> String {
    let (status, body, cookie) = send(
        a,
        post("/api/v1/login", None, json!({ "password": PASSWORD })),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    cookie
}

async fn status(a: &App, cookie: &str) -> Value {
    send(a, get("/api/v1/auth", Some(cookie)), None).await.1
}

/// The notice is open, and its version is the build's.
#[tokio::test]
async fn the_notice_is_served_open() {
    let dir = tempfile::tempdir().unwrap();
    let a = app(dir.path(), NOTICE_VERSION);
    let (status, body, _) = send(&a, get("/api/v1/legal/notice", None), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["version"], NOTICE_VERSION);
    assert_eq!(body["text"], NOTICE);
}

/// `POST /setup` without the version, or with a stale one, is `409
/// notice_required` and claims nothing; the current one sets up and is
/// saved, so the new session meets no gate.
#[tokio::test]
async fn setup_needs_the_current_notice() {
    let dir = tempfile::tempdir().unwrap();
    let a = app(dir.path(), NOTICE_VERSION);
    golden(
        &a,
        "setup_notice_required",
        post("/api/v1/setup", None, setup_body(&a, None)),
    )
    .await;
    golden(
        &a,
        "setup_notice_stale",
        post(
            "/api/v1/setup",
            None,
            setup_body(&a, Some(NOTICE_VERSION - 1)),
        ),
    )
    .await;
    assert!(a.auth.needs_setup(), "nothing was claimed");
    let (status, _, cookie) = send(
        &a,
        post("/api/v1/setup", None, setup_body(&a, Some(NOTICE_VERSION))),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let file: Value =
        serde_json::from_str(&std::fs::read_to_string(dir.path().join("auth.json")).unwrap())
            .unwrap();
    assert_eq!(file["notice_version"], NOTICE_VERSION);
    assert_eq!(status_of(&a, &cookie).await, (true, false));
    let (status, _, _) = send(&a, get("/api/v1/keys", Some(&cookie)), None).await;
    assert_eq!(status, StatusCode::OK, "no gate after setup");
}

async fn status_of(a: &App, cookie: &str) -> (bool, bool) {
    let s = status(a, cookie).await;
    (
        s["logged_in"].as_bool().unwrap(),
        s["notice_required"].as_bool().unwrap(),
    )
}

/// Sets up `dir` on `version`, then restarts it on `then`.
async fn set_up_then(dir: &Path, version: u32, then: u32) -> App {
    let a = app(dir, version);
    let (status, body, _) = send(
        &a,
        post("/api/v1/setup", None, setup_body(&a, Some(version))),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    drop(a);
    app(dir, then)
}

/// Raising the version: sign-in still works, `GET /auth` says the
/// notice is required, every session route is `409 notice_required`
/// (the error golden), and accepting the current version opens them.
/// A stale acceptance is refused.
#[tokio::test]
async fn a_version_bump_gates_the_session_until_accepted() {
    let dir = tempfile::tempdir().unwrap();
    let v = NOTICE_VERSION;
    let a = set_up_then(dir.path(), v, v + 1).await;
    let cookie = login(&a).await;
    golden(
        &a,
        "auth_status_notice_required",
        get("/api/v1/auth", Some(&cookie)),
    )
    .await;
    golden(
        &a,
        "error_notice_required",
        get("/api/v1/keys", Some(&cookie)),
    )
    .await;
    // Session routes of every kind: plain, key-or-session, the auth ones.
    for (method, path) in [
        ("GET", "/api/v1/keys"),
        ("GET", "/api/v1/alerts/unread"),
        ("GET", "/api/v1/auth/proxy"),
        ("POST", "/api/v1/sessions/revoke-others"),
        ("POST", "/api/v1/password"),
    ] {
        let req = if method == "GET" {
            get(path, Some(&cookie))
        } else {
            post(path, Some(&cookie), json!({}))
        };
        let (status, body, _) = send(&a, req, None).await;
        assert_eq!(status, StatusCode::CONFLICT, "{method} {path}: {body}");
        assert_eq!(body["error"]["code"], "notice_required", "{method} {path}");
    }
    // Without a session it's still the 401, not the notice.
    let (status, body, _) = send(&a, get("/api/v1/keys", None), None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
    assert_eq!(
        status_of(&a, "lodestar_session=forged").await,
        (false, false)
    );
    let (status, _, _) = send(
        &a,
        post("/api/v1/legal/accept", None, json!({ "version": v + 1 })),
        None,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "accepting needs a session"
    );

    golden(
        &a,
        "legal_accept_stale",
        post(
            "/api/v1/legal/accept",
            Some(&cookie),
            json!({ "version": v }),
        ),
    )
    .await;
    golden(
        &a,
        "legal_accept_ok",
        post(
            "/api/v1/legal/accept",
            Some(&cookie),
            json!({ "version": v + 1 }),
        ),
    )
    .await;
    assert_eq!(status_of(&a, &cookie).await, (true, false));
    let (status, _, _) = send(&a, get("/api/v1/keys", Some(&cookie)), None).await;
    assert_eq!(status, StatusCode::OK);

    // Accepted for good: a restart on the same version (an editorial
    // change) doesn't show it again.
    drop(a);
    let a = app(dir.path(), v + 1);
    let cookie = login(&a).await;
    assert_eq!(status_of(&a, &cookie).await, (true, false));
}

/// An edit that leaves the version alone never shows the notice again.
#[tokio::test]
async fn an_editorial_change_does_not_gate() {
    let dir = tempfile::tempdir().unwrap();
    let a = set_up_then(dir.path(), NOTICE_VERSION, NOTICE_VERSION).await;
    let cookie = login(&a).await;
    assert_eq!(status_of(&a, &cookie).await, (true, false));
    let (status, _, _) = send(&a, get("/api/v1/keys", Some(&cookie)), None).await;
    assert_eq!(status, StatusCode::OK);
}

/// An install set up on rc.1 to rc.3 has no `notice_version`: its
/// existing session (a cookie from before the upgrade) meets the gate,
/// and accepting records the version.
#[tokio::test]
async fn an_install_from_before_the_notice_sees_it_once() {
    let dir = tempfile::tempdir().unwrap();
    let a = app(dir.path(), NOTICE_VERSION);
    let (_, _, cookie) = send(
        &a,
        post("/api/v1/setup", None, setup_body(&a, Some(NOTICE_VERSION))),
        None,
    )
    .await;
    drop(a);
    let path = dir.path().join("auth.json");
    let mut file: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    file.as_object_mut().unwrap().remove("notice_version");
    std::fs::write(&path, file.to_string()).unwrap();

    let a = app(dir.path(), NOTICE_VERSION);
    assert_eq!(status_of(&a, &cookie).await, (true, true), "the old cookie");
    let (status, body, _) = send(&a, get("/api/v1/keys", Some(&cookie)), None).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    let (status, _, _) = send(
        &a,
        post(
            "/api/v1/legal/accept",
            Some(&cookie),
            json!({ "version": NOTICE_VERSION }),
        ),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let file: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(file["notice_version"], NOTICE_VERSION);
    let (status, _, _) = send(&a, get("/api/v1/keys", Some(&cookie)), None).await;
    assert_eq!(status, StatusCode::OK);
}

/// API keys aren't browsers: a connected app keeps working through an
/// upgrade that raises the version, before the admin signs in.
#[tokio::test]
async fn api_keys_are_not_gated() {
    use lodestar_api::keys::{mint, scope};
    use lodestar_store::{KeyKind, NewApiKey};

    let dir = tempfile::tempdir().unwrap();
    let a = set_up_then(dir.path(), NOTICE_VERSION, NOTICE_VERSION + 1).await;
    let m = mint(KeyKind::ApiKey).unwrap();
    a.store
        .lock()
        .unwrap()
        .create_api_key(
            &NewApiKey {
                kind: KeyKind::ApiKey,
                name: "arr",
                prefix: &m.prefix,
                hash: m.hash,
                scopes: scope::ALL,
                saved_search_id: None,
                expires_at: None,
            },
            0,
        )
        .unwrap();
    // The one route keys reach on this branch: a feed (404, as no saved
    // search exists, rather than the gate's 409).
    let req = Request::get("/api/v1/feeds/1")
        .header(
            header::AUTHORIZATION,
            format!("Bearer {}", m.secret.expose()),
        )
        .body(Body::empty())
        .unwrap();
    let (status, body, _) = send(&a, req, None).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
}

/// The setup refusal spends a login attempt like any other, and the
/// login limit itself is unchanged: a burst of 5 per address, then 429,
/// gate or no gate.
#[tokio::test]
async fn the_rate_limit_is_unchanged() {
    let dir = tempfile::tempdir().unwrap();
    let a = app(dir.path(), NOTICE_VERSION);
    let ip = Some([10, 8, 0, 1]);
    for _ in 0..5 {
        let (status, body, _) =
            send(&a, post("/api/v1/setup", None, setup_body(&a, None)), ip).await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
    }
    let (status, body, _) = send(
        &a,
        post("/api/v1/setup", None, setup_body(&a, Some(NOTICE_VERSION))),
        ip,
    )
    .await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS, "{body}");
    assert_eq!(body["error"]["code"], "rate_limited");

    drop(a);
    let a = set_up_then(dir.path(), NOTICE_VERSION, NOTICE_VERSION + 1).await;
    let ip = Some([10, 8, 0, 2]);
    for _ in 0..5 {
        let (status, _, _) = send(
            &a,
            post(
                "/api/v1/login",
                None,
                json!({ "password": "wrong wrong wrong" }),
            ),
            ip,
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }
    let (status, _, _) = send(
        &a,
        post("/api/v1/login", None, json!({ "password": PASSWORD })),
        ip,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::TOO_MANY_REQUESTS,
        "pending notice or not"
    );
}
