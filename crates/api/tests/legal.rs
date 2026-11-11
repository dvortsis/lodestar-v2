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
    state: ApiState,
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
    let state = ApiState {
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
    };
    let app = Router::new().nest("/api/v1", router(state.clone()));
    App {
        app,
        auth,
        store,
        state,
    }
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

/// The routes a session with the notice pending may still reach: the
/// open auth routes, the notice itself and accepting it, the API's own
/// description, and the token-authenticated webhook calls. Everything
/// else in `/openapi.json` answers `409 notice_required`.
const PENDING_ALLOWED: [(&str, &str); 8] = [
    ("get", "/auth"),
    ("post", "/setup"),
    ("post", "/login"),
    ("post", "/logout"),
    ("get", "/legal/notice"),
    ("post", "/legal/accept"),
    ("get", "/openapi.json"),
    ("post", "/webhooks/{integration}"),
];

/// Every documented path and method, with the notice pending: 409
/// `notice_required` unless the route is on [`PENDING_ALLOWED`]. A new
/// route mounted without `protected` (or the key guard) fails here. The
/// document is the router's own (`tests/openapi.rs` keeps the two in
/// step), so nothing can be left out of the walk.
#[tokio::test]
async fn every_route_but_the_allowed_ones_meets_the_gate() {
    let dir = tempfile::tempdir().unwrap();
    let a = set_up_then(dir.path(), NOTICE_VERSION, NOTICE_VERSION + 1).await;
    let cookie = login(&a).await;
    let (status, doc, _) = send(&a, get("/api/v1/openapi.json", None), None).await;
    assert_eq!(status, StatusCode::OK);
    let paths = doc["paths"].as_object().expect("paths");
    let mut walked = 0;
    let mut allowed_seen = Vec::new();
    for (path, item) in paths {
        for (method, _) in item.as_object().unwrap() {
            let m = method.as_str();
            if !matches!(m, "get" | "post" | "put" | "patch" | "delete") {
                continue;
            }
            walked += 1;
            // Any value fits a path parameter: the guard answers first.
            let uri = format!("/api/v1{}", fill_params(path));
            let mut req = Request::builder()
                .method(m.to_uppercase().as_str())
                .uri(&uri)
                .header(header::COOKIE, &cookie);
            if m != "get" && m != "delete" {
                req = req.header(header::CONTENT_TYPE, "application/json");
            }
            let body = if m == "get" {
                Body::empty()
            } else {
                Body::from("{}")
            };
            let (status, body, _) = send(&a, req.body(body).unwrap(), None).await;
            let gated =
                status == StatusCode::CONFLICT && body["error"]["code"] == "notice_required";
            if PENDING_ALLOWED.contains(&(m, path.as_str())) {
                allowed_seen.push((m, path.clone()));
                assert!(!gated, "{m} {path} must stay open with the notice pending");
            } else {
                assert!(gated, "{m} {path} isn't gated: {status} {body}");
            }
        }
    }
    assert!(walked > 100, "walked only {walked} routes");
    assert_eq!(
        allowed_seen.len(),
        PENDING_ALLOWED.len(),
        "every allowed route is documented: {allowed_seen:?}"
    );
}

/// `/torrents/{hash}` → `/torrents/1`.
fn fill_params(path: &str) -> String {
    let mut out = String::new();
    let mut in_param = false;
    for c in path.chars() {
        match c {
            '{' => {
                in_param = true;
                out.push('1');
            }
            '}' => in_param = false,
            _ if !in_param => out.push(c),
            _ => {}
        }
    }
    out
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

/// The upgrade from v0.5.1 (R-rc5-notice stored `notice_version: 3`): this
/// build's notice 5 shows once at sign-in, a stale accept of 3 is
/// refused, and after accepting 5 a fresh sign-in passes straight
/// through, after a restart too. (v0.5.2's stored 4 is the same path,
/// covered in `auth.rs`.)
#[tokio::test]
async fn a_v0_5_1_install_sees_the_new_notice_once_at_login() {
    assert_eq!(NOTICE_VERSION, 5);
    let dir = tempfile::tempdir().unwrap();
    let a = app(dir.path(), NOTICE_VERSION);
    let (status, body, _) = send(
        &a,
        post("/api/v1/setup", None, setup_body(&a, Some(NOTICE_VERSION))),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    drop(a);
    let path = dir.path().join("auth.json");
    let mut file: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    file["notice_version"] = json!(3);
    std::fs::write(&path, file.to_string()).unwrap();

    let a = app(dir.path(), NOTICE_VERSION);
    let cookie = login(&a).await;
    assert_eq!(status_of(&a, &cookie).await, (true, true));
    let (status, body, _) = send(&a, get("/api/v1/keys", Some(&cookie)), None).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    let (status, _, _) = send(
        &a,
        post(
            "/api/v1/legal/accept",
            Some(&cookie),
            json!({ "version": 3 }),
        ),
        None,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "the v0.5.1 copy accepts nothing"
    );
    let (status, _, _) = send(
        &a,
        post(
            "/api/v1/legal/accept",
            Some(&cookie),
            json!({ "version": 5 }),
        ),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    drop(a);

    let a = app(dir.path(), NOTICE_VERSION);
    let cookie = login(&a).await;
    assert_eq!(status_of(&a, &cookie).await, (true, false), "shown once");
    let (status, _, _) = send(&a, get("/api/v1/keys", Some(&cookie)), None).await;
    assert_eq!(status, StatusCode::OK);
    let file: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(file["notice_version"], 5);
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

/// The gate covers the trusted-proxy header's sessions and the event
/// stream too, and accepting is same-origin JSON like every write.
#[tokio::test]
async fn the_gate_covers_proxy_sessions_and_streams() {
    let dir = tempfile::tempdir().unwrap();
    let a = set_up_then(dir.path(), NOTICE_VERSION, NOTICE_VERSION + 1).await;
    a.auth
        .set_proxy(lodestar_api::auth::ProxyConfig {
            enabled: true,
            trusted: vec!["10.7.0.1/32".into()],
            ..Default::default()
        })
        .unwrap();
    let proxy = || {
        Request::get("/api/v1/keys")
            .header("Remote-User", "admin")
            .body(Body::empty())
            .unwrap()
    };
    let (status, body, _) = send(&a, proxy(), Some([10, 7, 0, 1])).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["error"]["code"], "notice_required");

    let cookie = login(&a).await;
    let (status, body, _) = send(&a, get("/api/v1/events", Some(&cookie)), None).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");

    let cross = Request::post("/api/v1/legal/accept")
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::COOKIE, &cookie)
        .header(header::ORIGIN, "https://evil.example")
        .header(header::HOST, "lodestar.local")
        .body(Body::from(
            json!({ "version": NOTICE_VERSION + 1 }).to_string(),
        ))
        .unwrap();
    let (status, body, _) = send(&a, cross, None).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    let form = Request::post("/api/v1/legal/accept")
        .header(header::CONTENT_TYPE, "text/plain")
        .header(header::COOKIE, &cookie)
        .body(Body::from(
            json!({ "version": NOTICE_VERSION + 1 }).to_string(),
        ))
        .unwrap();
    let (status, _, _) = send(&a, form, None).await;
    assert!(status.is_client_error(), "{status}");
    assert!(a.auth.notice_pending(), "neither was accepted");

    // Accepted from the proxy's session, it lifts for every session.
    let accept = Request::post("/api/v1/legal/accept")
        .header(header::CONTENT_TYPE, "application/json")
        .header("Remote-User", "admin")
        .body(Body::from(
            json!({ "version": NOTICE_VERSION + 1 }).to_string(),
        ))
        .unwrap();
    let (status, body, _) = send(&a, accept, Some([10, 7, 0, 1])).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, _, _) = send(&a, proxy(), Some([10, 7, 0, 1])).await;
    assert_eq!(status, StatusCode::OK);
}

/// The other two ways a browser session reaches the server: the feed
/// route's key-or-session guard with a cookie, and `require_session`,
/// which the binary puts on `/api/status`. Both meet the gate.
#[tokio::test]
async fn feeds_and_require_session_meet_the_gate() {
    use axum::routing::get as route_get;

    let dir = tempfile::tempdir().unwrap();
    let a = set_up_then(dir.path(), NOTICE_VERSION, NOTICE_VERSION + 1).await;
    let cookie = login(&a).await;
    let (status, body, _) = send(&a, get("/api/v1/feeds/1", Some(&cookie)), None).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["error"]["code"], "notice_required");

    let status_app = App {
        app: Router::new()
            .route("/api/status", route_get(|| async { "{}" }))
            .route_layer(axum::middleware::from_fn_with_state(
                a.state.clone(),
                lodestar_api::require_session,
            ))
            .with_state(a.state.clone()),
        auth: a.auth.clone(),
        store: a.store.clone(),
        state: a.state.clone(),
    };
    let (status, body, _) = send(&status_app, get("/api/status", Some(&cookie)), None).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    let (status, _, _) = send(&status_app, get("/api/status", None), None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    a.auth.accept_notice(NOTICE_VERSION + 1).unwrap();
    let (status, _, _) = send(&status_app, get("/api/status", Some(&cookie)), None).await;
    assert_eq!(status, StatusCode::OK);
    let (status, _, _) = send(&a, get("/api/v1/feeds/1", Some(&cookie)), None).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "past the gate: no such feed");
}

/// A hand-edited `notice_version` that isn't a version stops the server
/// at load, the way any malformed `auth.json` does, rather than loading
/// as accepted.
#[test]
fn a_malformed_notice_version_fails_closed() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("auth.json");
    let key = "ab".repeat(32);
    for bad in [r#""2""#, "-1", "2.5", "null"] {
        std::fs::write(
            &path,
            format!(r#"{{"password":null,"session_key":"{key}","notice_version":{bad}}}"#),
        )
        .unwrap();
        let loaded = Auth::load(&path, false);
        if bad == "null" {
            // JSON null is the field left out: not accepted.
            assert!(loaded.is_ok());
        } else {
            let e = loaded.expect_err(bad);
            assert_eq!(e.kind(), std::io::ErrorKind::InvalidData, "{bad}");
        }
    }
}

/// Sonarr and Radarr's webhook calls (SRC-11b) carry their own token,
/// like API keys: a raised version doesn't stop them, while the token
/// routes, which need a session, meet the gate.
#[tokio::test]
async fn webhooks_are_not_gated() {
    use lodestar_api::webhooks::{Delivered, Integration, WebhookSink, Webhooks};

    struct Found;
    impl WebhookSink for Found {
        fn deliver(&self, _: Integration, _: &[u8]) -> Delivered {
            Delivered::Found
        }
    }
    let with_hook = |a: &mut App| {
        a.app = Router::new().nest(
            "/api/v1",
            router(a.state.clone()).layer(Webhooks::new(Arc::new(Found)).layer()),
        );
    };

    let dir = tempfile::tempdir().unwrap();
    let mut a = app(dir.path(), NOTICE_VERSION);
    with_hook(&mut a);
    let (status, body, cookie) = send(
        &a,
        post("/api/v1/setup", None, setup_body(&a, Some(NOTICE_VERSION))),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body, _) = send(
        &a,
        post(
            "/api/v1/webhooks/sonarr/token",
            Some(&cookie),
            json!({ "password": PASSWORD }),
        ),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let token = body["token"].as_str().unwrap().to_owned();
    drop(a);

    let mut a = app(dir.path(), NOTICE_VERSION + 1);
    with_hook(&mut a);
    let call = Request::post("/api/v1/webhooks/sonarr")
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .body(Body::from(r#"{"eventType":"Grab"}"#))
        .unwrap();
    let (status, body, _) = send(&a, call, None).await;
    assert_eq!(status, StatusCode::OK, "a delivery isn't gated: {body}");

    let cookie = login(&a).await;
    let (status, body, _) = send(&a, get("/api/v1/webhooks", Some(&cookie)), None).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["error"]["code"], "notice_required");
}
