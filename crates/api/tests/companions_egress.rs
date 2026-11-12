//! `PUT /companions/{id}/egress` and the route fields of `GET
//! /companions/{id}` (X-cmp-egress-api), through the router over a fake of
//! the binary's `CompanionsApi`:
//!
//! - the route and where it runs, as the binary reports them;
//! - the session guard, same-origin JSON and the admin password, each
//!   refused before the binary is asked;
//! - the route checked as a source's is: every kind that isn't offered,
//!   every unusable proxy and anything that would turn its kill switch off
//!   refused, with nothing passed on;
//! - the binary's refusals passed through as their stable codes, and a
//!   saved route whose restart didn't begin reported as not applied;
//! - no password or proxy credential in a response or a log line.
//!
//! What the binary does with it (saved, then restarted; nothing saved
//! when refused) is tested in `crates/lodestar/src/companions.rs`.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex, Once};

use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Request, header};
use axum::{Extension, Router};
use lodestar_api::companions::{CompanionsApi, CompanionsError, Revealed};
use lodestar_api::dto::companions::{
    Companion, CompanionEgress, CompanionEgressChanged, CompanionRuns, CompanionTemplate,
    EgressFrom, InstallOffer, RemovalPlan, TemplateKind, TemplateRequest,
};
use lodestar_api::dto::ops::{EgressRoute, EgressSetting, SocksDns};
use lodestar_api::ops::BoxFuture;
use lodestar_api::{ApiState, Auth, router};
use lodestar_log::{Mask, Record, Sink};
use lodestar_store::{Store, StoreConfig};
use serde_json::{Value, json};
use tower::ServiceExt;

const PASSWORD: &str = "correct horse battery";
/// What a caller might try to slip in as a proxy credential.
const PROXY_SECRET: &str = "Hk7pQ2xW9vB4nM1sD8fJ";

static LINES: Mutex<Vec<String>> = Mutex::new(Vec::new());
struct Capture;
impl Sink for Capture {
    fn record(&self, r: &Record<'_>) {
        let line = format!("{} {}", r.event.id, r.text(Mask(u32::MAX), None));
        LINES.lock().unwrap_or_else(|e| e.into_inner()).push(line);
    }
}
static CAPTURE: Capture = Capture;

fn capture() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let _ = lodestar_log::set_sink(&CAPTURE);
    });
}

/// The binary's answers: qBittorrent is installed and takes a route,
/// gluetun takes none, Prowlarr isn't installed, FlareSolverr is busy.
#[derive(Debug)]
struct Fake {
    /// The routes `set_egress` was given.
    calls: Mutex<Vec<EgressSetting>>,
    /// What qBittorrent's restart answers.
    restart: Result<(), CompanionsError>,
}

impl Fake {
    fn new(restart: Result<(), CompanionsError>) -> Fake {
        Fake {
            calls: Mutex::new(Vec::new()),
            restart,
        }
    }
    fn calls(&self) -> Vec<EgressSetting> {
        self.calls.lock().unwrap().clone()
    }
}

fn socks5() -> EgressSetting {
    EgressSetting {
        route: EgressRoute::Socks5,
        proxy: Some("10.8.0.1:1080".into()),
        dns: Some(SocksDns::Proxy),
    }
}

fn companion(id: &str) -> Companion {
    Companion {
        available: None,
        credentials: Vec::new(),
        egress: Some(CompanionEgress {
            from: EgressFrom::Saved,
            setting: Some(socks5()),
        }),
        health: None,
        id: id.into(),
        installed: Some("5.1.2".into()),
        licence: "GPL-2.0-or-later".into(),
        name: "qBittorrent".into(),
        opt_in: false,
        refusal: None,
        runs: CompanionRuns::Lodestar,
        templates: vec![TemplateKind::Compose],
    }
}

fn unused<T: Send + 'static>() -> BoxFuture<'static, Result<T, CompanionsError>> {
    Box::pin(async { Err(CompanionsError::Refused("not_wired")) })
}

impl CompanionsApi for Fake {
    fn list(&self) -> BoxFuture<'_, Result<Vec<Companion>, CompanionsError>> {
        Box::pin(async { Ok(vec![companion("qbittorrent")]) })
    }
    fn get(&self, id: String) -> BoxFuture<'_, Result<Companion, CompanionsError>> {
        Box::pin(async move {
            match id.as_str() {
                "qbittorrent" => Ok(companion(&id)),
                "gluetun" => Ok(Companion {
                    egress: None,
                    runs: CompanionRuns::OwnContainer,
                    installed: None,
                    ..companion(&id)
                }),
                _ => Err(CompanionsError::Unknown),
            }
        })
    }
    fn offer(&self, _: String) -> BoxFuture<'_, Result<InstallOffer, CompanionsError>> {
        unused()
    }
    fn install(
        &self,
        _: String,
        _: String,
        _: String,
    ) -> BoxFuture<'_, Result<Companion, CompanionsError>> {
        unused()
    }
    fn removal(&self, _: String) -> BoxFuture<'_, Result<RemovalPlan, CompanionsError>> {
        unused()
    }
    fn remove(&self, _: String) -> BoxFuture<'_, Result<RemovalPlan, CompanionsError>> {
        unused()
    }
    fn restart(&self, _: String) -> BoxFuture<'_, Result<(), CompanionsError>> {
        unused()
    }
    fn logs(&self, _: String) -> BoxFuture<'_, Result<Vec<String>, CompanionsError>> {
        unused()
    }
    fn template(
        &self,
        _: String,
        _: TemplateRequest,
    ) -> BoxFuture<'_, Result<CompanionTemplate, CompanionsError>> {
        unused()
    }
    fn reveal(&self, _: String, _: String) -> BoxFuture<'_, Result<Revealed, CompanionsError>> {
        unused()
    }
    fn regenerate(&self, _: String, _: String) -> BoxFuture<'_, Result<(), CompanionsError>> {
        unused()
    }
    fn set_egress(
        &self,
        id: String,
        egress: EgressSetting,
    ) -> BoxFuture<'_, Result<CompanionEgressChanged, CompanionsError>> {
        Box::pin(async move {
            self.calls.lock().unwrap().push(egress.clone());
            match id.as_str() {
                "qbittorrent" => {}
                "gluetun" => return Err(CompanionsError::Refused("takes_no_route")),
                "prowlarr" => return Err(CompanionsError::Refused("not_installed")),
                "flaresolverr" => return Err(CompanionsError::Refused("busy")),
                _ => return Err(CompanionsError::Unknown),
            }
            let not_applied = match &self.restart {
                Ok(()) => None,
                Err(CompanionsError::Refused(code)) => Some((*code).to_owned()),
                Err(_) => Some("restart_failed".to_owned()),
            };
            Ok(CompanionEgressChanged {
                applied: not_applied.is_none(),
                egress: CompanionEgress {
                    from: EgressFrom::Saved,
                    setting: Some(egress),
                },
                id,
                not_applied,
            })
        })
    }
}

struct Fixture {
    app: Router,
    auth: Arc<Auth>,
    fake: Arc<Fake>,
    _dir: tempfile::TempDir,
}

fn fixture(restart: Result<(), CompanionsError>) -> Fixture {
    capture();
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("t.db");
    let store = Store::open(&db, StoreConfig::default()).unwrap();
    let reader = Store::open_reader(&db, StoreConfig::default()).unwrap();
    let auth = Arc::new(Auth::load(&dir.path().join("auth.json"), false).unwrap());
    let api = router(ApiState {
        auth: auth.clone(),
        index: None,
        reader: Arc::new(Mutex::new(reader)),
        filters: None,
        settings: None,
        ops: None,
        user_data: Some(Arc::new(Mutex::new(store))),
        insights: None,
        adult: Default::default(),
        hot: None,
        events: None,
    });
    let fake = Arc::new(Fake::new(restart));
    let app = Router::new()
        .nest("/api/v1", api)
        .layer(Extension(fake.clone() as Arc<dyn CompanionsApi>));
    Fixture {
        app,
        auth,
        fake,
        _dir: dir,
    }
}

fn req(method: &str, uri: &str, cookie: Option<&str>, body: Option<Value>) -> Request<Body> {
    let mut b = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::HOST, "nas.local:8190")
        .header(header::CONTENT_TYPE, "application/json");
    if let Some(c) = cookie {
        b = b.header(header::COOKIE, c);
    }
    let body = body.map_or(Body::empty(), |v| Body::from(v.to_string()));
    let mut req = b.body(body).unwrap();
    // Each request from its own address: the password check's bucket is
    // per address, and these tests send more than it allows from one.
    static NEXT: AtomicU32 = AtomicU32::new(0);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let [_, _, a, b] = n.to_be_bytes();
    req.extensions_mut()
        .insert(ConnectInfo(SocketAddr::from(([10, 9, a, b], 1))));
    req
}

struct Reply {
    status: u16,
    text: String,
    body: Value,
}

impl Reply {
    fn code(&self) -> &str {
        self.body["error"]["code"].as_str().unwrap_or("")
    }
}

async fn send(f: &Fixture, req: Request<Body>) -> Reply {
    let resp = f.app.clone().oneshot(req).await.unwrap();
    let status = resp.status().as_u16();
    let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20)
        .await
        .unwrap();
    let text = String::from_utf8(bytes.to_vec()).unwrap();
    Reply {
        status,
        body: serde_json::from_str(&text).unwrap_or(Value::Null),
        text,
    }
}

fn session(f: &Fixture) -> String {
    let token = f.auth.setup_token().unwrap();
    let set = f
        .auth
        .setup([127, 0, 0, 1].into(), &token, PASSWORD)
        .unwrap();
    set.split(';').next().unwrap().to_string()
}

const QBT: &str = "/api/v1/companions/qbittorrent/egress";

async fn put(f: &Fixture, cookie: &str, path: &str, body: Value) -> Reply {
    send(f, req("PUT", path, Some(cookie), Some(body))).await
}

fn with_password(egress: Value) -> Value {
    json!({ "egress": egress, "password": PASSWORD })
}

#[tokio::test]
async fn get_shows_the_saved_route_and_where_it_runs() {
    let f = fixture(Ok(()));
    let cookie = session(&f);
    let r = send(
        &f,
        req("GET", "/api/v1/companions/qbittorrent", Some(&cookie), None),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.text);
    assert_eq!(
        r.body["egress"],
        json!({ "from": "saved", "setting": { "route": "socks5", "proxy": "10.8.0.1:1080", "dns": "proxy" } })
    );
    assert_eq!(r.body["runs"], "lodestar");
    // A companion that takes no route shows null, not a direct route.
    let r = send(
        &f,
        req("GET", "/api/v1/companions/gluetun", Some(&cookie), None),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.text);
    assert!(r.body["egress"].is_null(), "{}", r.text);
    assert_eq!(r.body["runs"], "own_container");
}

#[tokio::test]
async fn a_valid_route_is_passed_on_and_its_restart_reported() {
    let f = fixture(Ok(()));
    let cookie = session(&f);
    let r = put(
        &f,
        &cookie,
        QBT,
        with_password(json!({ "route": "socks5", "proxy": "10.8.0.1:1080", "dns": "proxy" })),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.text);
    assert_eq!(
        r.body,
        json!({
            "applied": true,
            "egress": { "from": "saved", "setting": { "route": "socks5", "proxy": "10.8.0.1:1080", "dns": "proxy" } },
            "id": "qbittorrent",
            "not_applied": null,
        })
    );
    let r = put(
        &f,
        &cookie,
        QBT,
        with_password(json!({ "route": "direct" })),
    )
    .await;
    assert_eq!((r.status, r.body["applied"].as_bool()), (200, Some(true)));
    assert_eq!(
        f.fake.calls(),
        vec![
            socks5(),
            EgressSetting {
                route: EgressRoute::Direct,
                proxy: None,
                dns: None
            }
        ]
    );
}

/// The restart didn't begin: the route is saved, and the answer says it
/// isn't applied yet, never that it is.
#[tokio::test]
async fn a_failed_restart_is_saved_but_not_applied() {
    for (restart, code) in [
        (Err(CompanionsError::Refused("not_wired")), "not_wired"),
        (Err(CompanionsError::Internal("spawn")), "restart_failed"),
    ] {
        let f = fixture(restart);
        let cookie = session(&f);
        let r = put(
            &f,
            &cookie,
            QBT,
            with_password(json!({ "route": "direct" })),
        )
        .await;
        assert_eq!(r.status, 200, "{}", r.text);
        assert_eq!(r.body["applied"], false, "{}", r.text);
        assert_eq!(r.body["not_applied"], code, "{}", r.text);
        assert_eq!(r.body["egress"]["setting"]["route"], "direct");
        assert_eq!(f.fake.calls().len(), 1);
    }
}

/// Every route that isn't offered, every unusable proxy, and a direct
/// route with proxy options: refused before the binary is asked.
#[tokio::test]
async fn each_invalid_route_is_refused() {
    let f = fixture(Ok(()));
    let cookie = session(&f);
    let cases = [
        // Not offered: lookups would leave outside them.
        (
            json!({ "route": "interface", "proxy": "10.8.0.1:1080" }),
            409,
            "route_unavailable",
        ),
        (json!({ "route": "bind_address" }), 409, "route_unavailable"),
        // Not a kind at all.
        (json!({ "route": "wireguard" }), 422, "bad_body"),
        (json!({ "route": "" }), 422, "bad_body"),
        (json!({}), 422, "bad_body"),
        // Direct takes no proxy options.
        (
            json!({ "route": "direct", "proxy": "10.8.0.1:1080" }),
            400,
            "invalid_egress",
        ),
        (
            json!({ "route": "direct", "dns": "local" }),
            400,
            "invalid_egress",
        ),
        // SOCKS5 needs a usable host:port.
        (json!({ "route": "socks5" }), 400, "invalid_egress"),
        (
            json!({ "route": "socks5", "dns": "bogus", "proxy": "10.8.0.1:1080" }),
            422,
            "bad_body",
        ),
    ];
    for (egress, status, code) in cases {
        let r = put(&f, &cookie, QBT, with_password(egress.clone())).await;
        assert_eq!((r.status, r.code()), (status, code), "{egress}: {}", r.text);
    }
    for proxy in [
        "0.0.0.0:1080",
        "[::]:1080",
        "[::ffff:0.0.0.0]:1080",
        "224.0.0.1:1080",
        "255.255.255.255:1080",
        "10.8.0.1:0",
        "10.8.0.1",
        "10.8.0.1:99999",
        "127.1:1080",
        "0x7f000001:1080",
        "http://10.8.0.1:1080",
        "socks5://10.8.0.1:1080",
        "",
    ] {
        let r = put(
            &f,
            &cookie,
            QBT,
            with_password(json!({ "route": "socks5", "proxy": proxy })),
        )
        .await;
        assert_eq!(
            (r.status, r.code()),
            (400, "invalid_egress"),
            "{proxy}: {}",
            r.text
        );
        assert_eq!(r.body["error"]["field"], "egress.proxy", "{proxy}");
    }
    assert!(f.fake.calls().is_empty(), "{:?}", f.fake.calls());
}

/// A SOCKS5 route's kill switch is always on: nothing in the request can
/// turn it off or name a fallback, and the routes that would leave
/// outside the tunnel aren't offered.
#[tokio::test]
async fn a_kill_switch_bypass_is_refused() {
    let f = fixture(Ok(()));
    let cookie = session(&f);
    let base = || json!({ "route": "socks5", "proxy": "10.8.0.1:1080" });
    for (key, value) in [
        ("kill_switch", json!(false)),
        ("fallback", json!("direct")),
        ("bypass", json!(true)),
    ] {
        let mut egress = base();
        egress[key] = value.clone();
        let r = put(&f, &cookie, QBT, with_password(egress)).await;
        assert_eq!((r.status, r.code()), (422, "bad_body"), "{key}: {}", r.text);
        // Beside the route, too.
        let mut body = with_password(base());
        body[key] = value;
        let r = put(&f, &cookie, QBT, body).await;
        assert_eq!((r.status, r.code()), (422, "bad_body"), "{key}: {}", r.text);
    }
    for route in ["interface", "bind_address"] {
        let r = put(
            &f,
            &cookie,
            QBT,
            with_password(json!({ "route": route, "proxy": "10.8.0.1:1080" })),
        )
        .await;
        assert_eq!(
            (r.status, r.code()),
            (409, "route_unavailable"),
            "{route}: {}",
            r.text
        );
    }
    assert!(f.fake.calls().is_empty(), "{:?}", f.fake.calls());
}

#[tokio::test]
async fn the_binarys_refusals_pass_through() {
    let f = fixture(Ok(()));
    let cookie = session(&f);
    let direct = || with_password(json!({ "route": "direct" }));
    for (path, status, code) in [
        ("/api/v1/companions/nope/egress", 404, "unknown_companion"),
        ("/api/v1/companions/gluetun/egress", 409, "takes_no_route"),
        ("/api/v1/companions/prowlarr/egress", 409, "not_installed"),
        ("/api/v1/companions/flaresolverr/egress", 409, "busy"),
    ] {
        let r = put(&f, &cookie, path, direct()).await;
        assert_eq!((r.status, r.code()), (status, code), "{path}: {}", r.text);
        assert!(r.body.get("applied").is_none(), "{path}: {}", r.text);
    }
    // An id that isn't an id is unknown before the binary is asked.
    let before = f.fake.calls().len();
    let r = put(
        &f,
        &cookie,
        "/api/v1/companions/Q%2F..%2Fx/egress",
        direct(),
    )
    .await;
    assert_eq!(
        (r.status, r.code()),
        (404, "unknown_companion"),
        "{}",
        r.text
    );
    assert_eq!(f.fake.calls().len(), before);
    // Only PUT.
    let r = send(&f, req("POST", QBT, Some(&cookie), Some(direct()))).await;
    assert_eq!(r.status, 405, "{}", r.text);
}

/// No session, an API key, a cross-site request, a form post, no password
/// or a wrong one: refused, and the binary is never asked.
#[tokio::test]
async fn without_auth_csrf_or_the_password_it_is_refused() {
    let f = fixture(Ok(()));
    let cookie = session(&f);
    let body = || with_password(json!({ "route": "direct" }));
    let r = send(&f, req("PUT", QBT, None, Some(body()))).await;
    assert_eq!(r.status, 401, "{}", r.text);
    let key = send(
        &f,
        req(
            "POST",
            "/api/v1/keys",
            Some(&cookie),
            Some(json!({ "name": "arr", "kind": "api_key", "scopes": ["search", "torznab", "feeds"], "password": PASSWORD })),
        ),
    )
    .await;
    assert_eq!(key.status, 201, "{}", key.text);
    let secret = key.body["secret"].as_str().unwrap().to_string();
    for (name, value) in [
        ("x-api-key", secret.clone()),
        ("authorization", format!("Bearer {secret}")),
        ("remote-user", "admin".to_owned()),
    ] {
        let mut q = req("PUT", QBT, None, Some(body()));
        q.headers_mut().insert(name, value.parse().unwrap());
        assert_eq!(send(&f, q).await.status, 401, "{name}");
    }
    let mut q = req("PUT", QBT, Some(&cookie), Some(body()));
    q.headers_mut()
        .insert(header::ORIGIN, "https://evil.example".parse().unwrap());
    let r = send(&f, q).await;
    assert_eq!((r.status, r.code()), (403, "cross_origin"), "{}", r.text);
    let mut q = req("PUT", QBT, Some(&cookie), Some(body()));
    q.headers_mut().insert(
        header::CONTENT_TYPE,
        "application/x-www-form-urlencoded".parse().unwrap(),
    );
    assert_eq!(send(&f, q).await.status, 415);
    let r = put(&f, &cookie, QBT, json!({ "egress": { "route": "direct" } })).await;
    assert_eq!(
        (r.status, r.code()),
        (428, "confirm_required"),
        "{}",
        r.text
    );
    let r = put(
        &f,
        &cookie,
        QBT,
        json!({ "egress": { "route": "direct" }, "password": "wrong wrong wrong" }),
    )
    .await;
    assert_eq!((r.status, r.code()), (401, "bad_password"), "{}", r.text);
    // The password comes first: an unknown id or a bad route learns
    // nothing without it.
    let r = put(
        &f,
        &cookie,
        "/api/v1/companions/nope/egress",
        json!({ "egress": { "route": "interface" } }),
    )
    .await;
    assert_eq!(
        (r.status, r.code()),
        (428, "confirm_required"),
        "{}",
        r.text
    );
    assert!(f.fake.calls().is_empty(), "{:?}", f.fake.calls());
}

/// A proxy credential has nowhere to go: the route has no field for one,
/// a `user:pass@` proxy isn't a host and port, and neither it nor the
/// admin password is echoed in a refusal or written to a log line.
#[tokio::test]
async fn credentials_never_appear_in_a_response_or_a_log() {
    let f = fixture(Ok(()));
    let cookie = session(&f);
    let mut texts = Vec::new();
    for egress in [
        json!({ "route": "socks5", "proxy": format!("user:{PROXY_SECRET}@10.8.0.1:1080") }),
        json!({ "route": "socks5", "proxy": "10.8.0.1:1080", "password": PROXY_SECRET }),
        json!({ "route": "socks5", "proxy": "10.8.0.1:1080", "username": "u", "credentials": { "password": PROXY_SECRET } }),
        json!({ "route": PROXY_SECRET }),
    ] {
        let r = put(&f, &cookie, QBT, with_password(egress.clone())).await;
        assert!(r.status >= 400, "{egress}: {}", r.text);
        texts.push(r.text);
    }
    // Accepted: the answer carries the route, never the password.
    let r = put(
        &f,
        &cookie,
        QBT,
        with_password(json!({ "route": "socks5", "proxy": "10.8.0.1:1080" })),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.text);
    texts.push(r.text);
    let r = put(
        &f,
        &cookie,
        QBT,
        json!({ "egress": { "route": "direct" }, "password": PROXY_SECRET }),
    )
    .await;
    assert_eq!(r.status, 401, "{}", r.text);
    texts.push(r.text);
    for t in &texts {
        assert!(!t.contains(PROXY_SECRET) && !t.contains(PASSWORD), "{t}");
    }
    assert_eq!(f.fake.calls().len(), 1, "{:?}", f.fake.calls());
    let log = LINES.lock().unwrap().join("\n");
    assert!(
        !log.contains(PROXY_SECRET) && !log.contains(PASSWORD),
        "{log}"
    );
}
