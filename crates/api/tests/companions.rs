//! `/companions` through the router, over a fake of the binary's
//! `CompanionsApi`: the session guard on every route (no session, an API
//! key, a proxy header), confirmation mismatches refused before the
//! manager is asked, the manager's `not_wired` refusal passed through,
//! the password-gated reveal (Q5) with its audit line, and that the
//! credential appears in the reveal's body and nowhere else.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex, Once};

use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Request, header};
use axum::{Extension, Router};
use lodestar_api::companions::{CompanionsApi, CompanionsError, Revealed};
use lodestar_api::dto::companions::{
    Companion, CompanionRuns, CompanionTemplate, InstallOffer, RemovalItem, RemovalItemKind,
    RemovalPlan, TemplateKind, TemplateRequest,
};
use lodestar_api::ops::BoxFuture;
use lodestar_api::{ApiState, Auth, router};
use lodestar_log::{Mask, Record, Sink};
use lodestar_store::{Store, StoreConfig};
use serde_json::{Value, json};
use tower::ServiceExt;

const PASSWORD: &str = "correct horse battery";
const SECRET: &str = "Zq3vL9sXk2pW8mN4rT7yB1cD6fH0jA5eGuIoPlKwMnE";
const SHA: &str = "0f1e2d3c4b5a69788796a5b4c3d2e1f00f1e2d3c4b5a69788796a5b4c3d2e1f0";

/// Every log line, every kind shown.
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

/// The binary as it is until CMP-5g: every action is `not_wired`.
#[derive(Debug, Default)]
struct Fake {
    calls: Mutex<Vec<String>>,
}

fn companion(id: &str) -> Companion {
    Companion {
        available: Some("5.1.2".into()),
        credentials: vec!["webui_password".into()],
        egress: None,
        health: None,
        id: id.into(),
        installed: None,
        licence: "GPL-2.0-or-later".into(),
        name: "qBittorrent".into(),
        opt_in: false,
        refusal: None,
        runs: CompanionRuns::Lodestar,
        templates: vec![TemplateKind::Compose, TemplateKind::Unraid],
    }
}

impl Fake {
    fn call(&self, what: String) {
        self.calls.lock().unwrap().push(what);
    }
    fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }
    fn known(id: &str) -> Result<(), CompanionsError> {
        if id == "qbittorrent" {
            Ok(())
        } else {
            Err(CompanionsError::Unknown)
        }
    }
}

impl CompanionsApi for Fake {
    fn list(&self) -> BoxFuture<'_, Result<Vec<Companion>, CompanionsError>> {
        Box::pin(async { Ok(vec![companion("qbittorrent")]) })
    }
    fn get(&self, id: String) -> BoxFuture<'_, Result<Companion, CompanionsError>> {
        Box::pin(async move {
            Fake::known(&id)?;
            Ok(companion(&id))
        })
    }
    fn offer(&self, id: String) -> BoxFuture<'_, Result<InstallOffer, CompanionsError>> {
        Box::pin(async move {
            Fake::known(&id)?;
            Ok(InstallOffer {
                id,
                licence: "GPL-2.0-or-later".into(),
                name: "qBittorrent".into(),
                replaces: None,
                sha256: SHA.into(),
                size_bytes: 10,
                version: "5.1.2".into(),
            })
        })
    }
    fn install(
        &self,
        id: String,
        version: String,
        sha256: String,
    ) -> BoxFuture<'_, Result<Companion, CompanionsError>> {
        Box::pin(async move {
            self.call(format!("install {id} {version} {sha256}"));
            Err(CompanionsError::Refused("not_wired"))
        })
    }
    fn removal(&self, id: String) -> BoxFuture<'_, Result<RemovalPlan, CompanionsError>> {
        Box::pin(async move {
            Fake::known(&id)?;
            Ok(RemovalPlan {
                deletes: vec![
                    RemovalItem {
                        kind: RemovalItemKind::Folder,
                        name: format!("companions/{id}"),
                    },
                    RemovalItem {
                        kind: RemovalItemKind::Setting,
                        name: format!("companions.{id}.secret.webui_password"),
                    },
                ],
                id,
                removed: false,
            })
        })
    }
    fn remove(&self, id: String) -> BoxFuture<'_, Result<RemovalPlan, CompanionsError>> {
        Box::pin(async move {
            self.call(format!("remove {id}"));
            let mut p = self.removal(id).await?;
            p.removed = true;
            Ok(p)
        })
    }
    fn restart(&self, id: String) -> BoxFuture<'_, Result<(), CompanionsError>> {
        Box::pin(async move {
            self.call(format!("restart {id}"));
            Err(CompanionsError::Refused("not_wired"))
        })
    }
    fn logs(&self, id: String) -> BoxFuture<'_, Result<Vec<String>, CompanionsError>> {
        Box::pin(async move {
            Fake::known(&id)?;
            Ok(vec!["WebUI: listening".into()])
        })
    }
    fn template(
        &self,
        id: String,
        request: TemplateRequest,
    ) -> BoxFuture<'_, Result<CompanionTemplate, CompanionsError>> {
        Box::pin(async move {
            Fake::known(&id)?;
            if request.puid != "1000" {
                return Err(CompanionsError::BadInput {
                    field: Some("puid"),
                    code: "bad_template_input",
                });
            }
            Ok(CompanionTemplate {
                compose: Some("services: {}\n".into()),
                id,
                kind: request.kind,
                unraid: Vec::new(),
            })
        })
    }
    fn reveal(
        &self,
        id: String,
        field: String,
    ) -> BoxFuture<'_, Result<Revealed, CompanionsError>> {
        Box::pin(async move {
            self.call(format!("reveal {id} {field}"));
            Fake::known(&id)?;
            if field != "webui_password" {
                return Err(CompanionsError::NoCredential);
            }
            Ok(Revealed {
                id: "qbittorrent",
                field: "webui_password",
                value: SECRET.into(),
            })
        })
    }
    fn regenerate(&self, id: String, field: String) -> BoxFuture<'_, Result<(), CompanionsError>> {
        Box::pin(async move {
            self.call(format!("regenerate {id} {field}"));
            Err(CompanionsError::Refused("not_wired"))
        })
    }
}

struct Fixture {
    app: Router,
    auth: Arc<Auth>,
    fake: Arc<Fake>,
    _dir: tempfile::TempDir,
}

fn fixture_with(layer: bool) -> Fixture {
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
    let fake = Arc::new(Fake::default());
    // As `lodestar serve` layers it.
    let mut app = Router::new().nest("/api/v1", api);
    if layer {
        app = app.layer(Extension(fake.clone() as Arc<dyn CompanionsApi>));
    }
    Fixture {
        app,
        auth,
        fake,
        _dir: dir,
    }
}

fn fixture() -> Fixture {
    fixture_with(true)
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
    req.extensions_mut()
        .insert(ConnectInfo(SocketAddr::from(([10, 9, 0, 7], 1))));
    req
}

struct Reply {
    status: u16,
    text: String,
    body: Value,
    cache: Option<String>,
}

impl Reply {
    fn code(&self) -> &str {
        self.body["error"]["code"].as_str().unwrap_or("")
    }
}

async fn send(f: &Fixture, req: Request<Body>) -> Reply {
    let resp = f.app.clone().oneshot(req).await.unwrap();
    let status = resp.status().as_u16();
    let cache = resp
        .headers()
        .get(header::CACHE_CONTROL)
        .map(|v| v.to_str().unwrap().to_string());
    let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20)
        .await
        .unwrap();
    let text = String::from_utf8(bytes.to_vec()).unwrap();
    Reply {
        status,
        body: serde_json::from_str(&text).unwrap_or(Value::Null),
        text,
        cache,
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

/// Every route, with a body that would succeed past the guard.
fn every_route() -> Vec<(&'static str, &'static str, Option<Value>)> {
    vec![
        ("GET", "/api/v1/companions", None),
        ("GET", "/api/v1/companions/qbittorrent", None),
        ("GET", "/api/v1/companions/qbittorrent/install", None),
        (
            "POST",
            "/api/v1/companions/qbittorrent/install",
            Some(json!({ "version": "5.1.2", "sha256": SHA })),
        ),
        ("GET", "/api/v1/companions/qbittorrent/remove", None),
        (
            "POST",
            "/api/v1/companions/qbittorrent/remove",
            Some(json!({ "confirm": "qbittorrent" })),
        ),
        ("POST", "/api/v1/companions/qbittorrent/restart", None),
        ("GET", "/api/v1/companions/qbittorrent/logs", None),
        (
            "POST",
            "/api/v1/companions/qbittorrent/template",
            Some(json!({ "kind": "compose" })),
        ),
        (
            "POST",
            "/api/v1/companions/qbittorrent/credentials/webui_password/reveal",
            Some(json!({ "password": PASSWORD })),
        ),
        (
            "POST",
            "/api/v1/companions/qbittorrent/credentials/webui_password/regenerate",
            None,
        ),
    ]
}

#[tokio::test]
async fn every_route_refuses_callers_who_arent_the_admin() {
    let f = fixture();
    let cookie = session(&f);
    // A real API key, every scope: still not the admin.
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
    for (method, path, body) in every_route() {
        let r = send(&f, req(method, path, None, body.clone())).await;
        assert_eq!(r.status, 401, "{method} {path}: {}", r.text);
        for (name, value) in [
            ("x-api-key", secret.as_str()),
            ("authorization", &format!("Bearer {secret}")),
            ("remote-user", "admin"),
        ] {
            let mut q = req(method, path, None, body.clone());
            q.headers_mut().insert(name, value.parse().unwrap());
            let r = send(&f, q).await;
            assert_eq!(r.status, 401, "{method} {path} with {name}: {}", r.text);
        }
        let mut q = req(method, path, None, body.clone());
        *q.uri_mut() = format!("{path}?apikey={secret}").parse().unwrap();
        let r = send(&f, q).await;
        assert_eq!(r.status, 401, "{method} {path} with ?apikey: {}", r.text);
    }
    assert!(f.fake.calls().is_empty());
}

#[tokio::test]
async fn without_the_manager_every_route_is_unavailable() {
    let f = fixture_with(false);
    let cookie = session(&f);
    for (method, path, body) in every_route() {
        let r = send(&f, req(method, path, Some(&cookie), body)).await;
        assert_eq!(r.status, 503, "{method} {path}: {}", r.text);
        assert_eq!(r.code(), "companions_unavailable");
    }
}

#[tokio::test]
async fn list_detail_and_unknown_ids() {
    let f = fixture();
    let cookie = session(&f);
    let r = send(&f, req("GET", "/api/v1/companions", Some(&cookie), None)).await;
    assert_eq!(r.status, 200, "{}", r.text);
    assert_eq!(r.body["companions"][0]["id"], "qbittorrent");
    assert_eq!(r.body["companions"][0]["installed"], Value::Null);
    assert_eq!(
        r.body["companions"][0]["templates"],
        json!(["compose", "unraid"])
    );
    let r = send(
        &f,
        req("GET", "/api/v1/companions/qbittorrent", Some(&cookie), None),
    )
    .await;
    assert_eq!(
        (r.status, r.body["available"].as_str()),
        (200, Some("5.1.2"))
    );
    for path in [
        "/api/v1/companions/prowlarr",
        "/api/v1/companions/Q",
        "/api/v1/companions/..%2Fetc",
    ] {
        let r = send(&f, req("GET", path, Some(&cookie), None)).await;
        assert_eq!((r.status, r.code()), (404, "unknown_companion"), "{path}");
    }
}

/// Install mirrors the CLI: the offer shows the pinned version and
/// SHA-256, and only a body echoing both reaches the manager, whose
/// `not_wired` refusal comes back as it is.
#[tokio::test]
async fn install_needs_the_shown_version_and_sha256() {
    let f = fixture();
    let cookie = session(&f);
    let path = "/api/v1/companions/qbittorrent/install";
    let offer = send(&f, req("GET", path, Some(&cookie), None)).await;
    assert_eq!(offer.status, 200, "{}", offer.text);
    assert_eq!(
        (
            offer.body["version"].as_str(),
            offer.body["sha256"].as_str()
        ),
        (Some("5.1.2"), Some(SHA))
    );
    for body in [
        json!({ "version": "5.1.2", "sha256": "ab".repeat(32) }),
        json!({ "version": "5.1.1", "sha256": SHA }),
        json!({ "version": "5.1.2", "sha256": "" }),
    ] {
        let r = send(&f, req("POST", path, Some(&cookie), Some(body.clone()))).await;
        assert_eq!(
            (r.status, r.code()),
            (409, "confirmation_mismatch"),
            "{body}"
        );
    }
    for body in [
        json!({ "version": "5.1.2" }),
        json!({ "version": "5.1.2", "sha256": SHA, "yes": true }),
    ] {
        let r = send(&f, req("POST", path, Some(&cookie), Some(body.clone()))).await;
        assert_eq!(
            (r.status, r.code()),
            (422, "bad_body"),
            "{body}: {}",
            r.text
        );
    }
    assert!(f.fake.calls().is_empty(), "{:?}", f.fake.calls());
    // Pasted in upper case, with stray spaces: still the shown hash.
    let r = send(
        &f,
        req(
            "POST",
            path,
            Some(&cookie),
            Some(json!({ "version": "5.1.2", "sha256": format!(" {} ", SHA.to_uppercase()) })),
        ),
    )
    .await;
    assert_eq!((r.status, r.code()), (409, "not_wired"), "{}", r.text);
    assert_eq!(
        f.fake.calls(),
        vec![format!("install qbittorrent 5.1.2 {SHA}")]
    );
    // Cross-site or not JSON: refused before anything is asked.
    let mut q = req(
        "POST",
        path,
        Some(&cookie),
        Some(json!({ "version": "5.1.2", "sha256": SHA })),
    );
    q.headers_mut()
        .insert(header::ORIGIN, "https://evil.example".parse().unwrap());
    assert_eq!(send(&f, q).await.status, 403);
    let mut q = req(
        "POST",
        path,
        Some(&cookie),
        Some(json!({ "version": "5.1.2", "sha256": SHA })),
    );
    q.headers_mut().remove(header::CONTENT_TYPE);
    assert_eq!(send(&f, q).await.status, 415);
    assert_eq!(f.fake.calls().len(), 1);
}

#[tokio::test]
async fn remove_shows_what_it_deletes_and_needs_the_id_typed() {
    let f = fixture();
    let cookie = session(&f);
    let path = "/api/v1/companions/qbittorrent/remove";
    let r = send(&f, req("GET", path, Some(&cookie), None)).await;
    assert_eq!(r.status, 200, "{}", r.text);
    assert_eq!(r.body["removed"], false);
    assert_eq!(
        r.body["deletes"][0],
        json!({ "kind": "folder", "name": "companions/qbittorrent" })
    );
    assert_eq!(r.body["deletes"][1]["kind"], "setting");
    for body in [
        json!({ "confirm": "prowlarr" }),
        json!({ "confirm": "" }),
        json!({ "confirm": "QBITTORRENT" }),
    ] {
        let r = send(&f, req("POST", path, Some(&cookie), Some(body.clone()))).await;
        assert_eq!(
            (r.status, r.code()),
            (409, "confirmation_mismatch"),
            "{body}"
        );
    }
    let r = send(&f, req("POST", path, Some(&cookie), Some(json!({})))).await;
    assert_eq!((r.status, r.code()), (422, "bad_body"), "{}", r.text);
    assert!(f.fake.calls().is_empty());
    let r = send(
        &f,
        req(
            "POST",
            path,
            Some(&cookie),
            Some(json!({ "confirm": "qbittorrent" })),
        ),
    )
    .await;
    assert_eq!(
        (r.status, r.body["removed"].as_bool()),
        (200, Some(true)),
        "{}",
        r.text
    );
    assert_eq!(f.fake.calls(), vec!["remove qbittorrent".to_owned()]);
}

#[tokio::test]
async fn actions_pass_the_managers_not_wired_refusal_through() {
    let f = fixture();
    let cookie = session(&f);
    for path in [
        "/api/v1/companions/qbittorrent/restart",
        "/api/v1/companions/qbittorrent/credentials/webui_password/regenerate",
    ] {
        let r = send(&f, req("POST", path, Some(&cookie), None)).await;
        assert_eq!(
            (r.status, r.code()),
            (409, "not_wired"),
            "{path}: {}",
            r.text
        );
        assert!(!r.body["error"]["message"].as_str().unwrap().is_empty());
    }
    let r = send(
        &f,
        req(
            "GET",
            "/api/v1/companions/qbittorrent/logs",
            Some(&cookie),
            None,
        ),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.text);
    assert_eq!(r.body["lines"], json!(["WebUI: listening"]));
    assert_eq!(r.cache.as_deref(), Some("no-store"));
}

#[tokio::test]
async fn templates_check_their_inputs() {
    let f = fixture();
    let cookie = session(&f);
    let path = "/api/v1/companions/qbittorrent/template";
    let r = send(
        &f,
        req(
            "POST",
            path,
            Some(&cookie),
            Some(json!({ "kind": "compose" })),
        ),
    )
    .await;
    assert_eq!(
        (r.status, r.body["kind"].as_str()),
        (200, Some("compose")),
        "{}",
        r.text
    );
    let r = send(
        &f,
        req(
            "POST",
            path,
            Some(&cookie),
            Some(json!({ "kind": "compose", "puid": "0x" })),
        ),
    )
    .await;
    assert_eq!((r.status, r.code()), (400, "bad_template_input"));
    assert_eq!(r.body["error"]["field"], "puid");
    let r = send(
        &f,
        req("POST", path, Some(&cookie), Some(json!({ "kind": "helm" }))),
    )
    .await;
    assert_eq!((r.status, r.code()), (422, "bad_body"), "{}", r.text);
}

/// Q5: the password is entered again for every reveal, the value comes
/// back once with `no-store`, the reveal is logged with names only, and
/// the value appears in no other response, error or log line.
#[tokio::test]
async fn reveal_needs_the_password_and_is_logged_without_the_value() {
    let f = fixture();
    let cookie = session(&f);
    let path = "/api/v1/companions/qbittorrent/credentials/webui_password/reveal";
    let r = send(&f, req("POST", path, Some(&cookie), Some(json!({})))).await;
    assert_eq!(
        (r.status, r.code()),
        (428, "confirm_required"),
        "{}",
        r.text
    );
    let r = send(
        &f,
        req(
            "POST",
            path,
            Some(&cookie),
            Some(json!({ "password": "wrong wrong wrong" })),
        ),
    )
    .await;
    assert_eq!((r.status, r.code()), (401, "bad_password"), "{}", r.text);
    // Cross-site: refused before the password is checked.
    let mut q = req(
        "POST",
        path,
        Some(&cookie),
        Some(json!({ "password": PASSWORD })),
    );
    q.headers_mut()
        .insert(header::ORIGIN, "https://evil.example".parse().unwrap());
    assert_eq!(send(&f, q).await.status, 403);
    assert!(f.fake.calls().is_empty(), "{:?}", f.fake.calls());

    let r = send(
        &f,
        req(
            "POST",
            path,
            Some(&cookie),
            Some(json!({ "password": PASSWORD })),
        ),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.text);
    assert_eq!(r.cache.as_deref(), Some("no-store"));
    assert_eq!(
        r.body,
        json!({ "field": "webui_password", "id": "qbittorrent", "value": SECRET })
    );
    // A field it doesn't have, or a companion it doesn't know.
    for (p, code) in [
        (
            "/api/v1/companions/qbittorrent/credentials/api_key/reveal",
            "no_credential",
        ),
        (
            "/api/v1/companions/qbittorrent/credentials/API/reveal",
            "no_credential",
        ),
        (
            "/api/v1/companions/prowlarr/credentials/api_key/reveal",
            "unknown_companion",
        ),
    ] {
        let r = send(
            &f,
            req(
                "POST",
                p,
                Some(&cookie),
                Some(json!({ "password": PASSWORD })),
            ),
        )
        .await;
        assert_eq!((r.status, r.code()), (404, code), "{p}: {}", r.text);
        assert!(!r.text.contains(SECRET));
    }

    let log = LINES.lock().unwrap().join("\n");
    assert!(!log.contains(SECRET) && !log.contains(PASSWORD), "{log}");
    let revealed: Vec<&str> = log
        .lines()
        .filter(|l| l.starts_with("companions.credential_revealed") && l.contains("10.9.0.7"))
        .collect();
    assert_eq!(revealed.len(), 1, "{log}");
    assert!(
        revealed[0].contains("qbittorrent") && revealed[0].contains("webui_password"),
        "{}",
        revealed[0]
    );
}

/// The password is checked before the id or field, so a guess learns
/// nothing about which companions or credentials exist, and wrong
/// passwords run into the login bucket.
#[tokio::test]
async fn reveal_checks_the_password_first_and_is_rate_limited() {
    let f = fixture();
    let cookie = session(&f);
    for path in [
        "/api/v1/companions/nope/credentials/api_key/reveal",
        "/api/v1/companions/qbittorrent/credentials/nope/reveal",
        "/api/v1/companions/qbittorrent/credentials/webui_password/reveal",
    ] {
        let r = send(
            &f,
            req(
                "POST",
                path,
                Some(&cookie),
                Some(json!({ "password": "wrong wrong wrong" })),
            ),
        )
        .await;
        assert_eq!(
            (r.status, r.code()),
            (401, "bad_password"),
            "{path}: {}",
            r.text
        );
    }
    let path = "/api/v1/companions/qbittorrent/credentials/webui_password/reveal";
    let mut limited = None;
    for i in 0..20 {
        let r = send(
            &f,
            req(
                "POST",
                path,
                Some(&cookie),
                Some(json!({ "password": "wrong wrong wrong" })),
            ),
        )
        .await;
        if r.status == 429 {
            assert_eq!(r.code(), "rate_limited");
            limited = Some(i);
            break;
        }
        assert_eq!(r.status, 401, "{}", r.text);
    }
    assert!(limited.is_some(), "never rate limited");
    // Limited, the right password is refused too.
    let r = send(
        &f,
        req(
            "POST",
            path,
            Some(&cookie),
            Some(json!({ "password": PASSWORD })),
        ),
    )
    .await;
    assert_eq!(r.status, 429, "{}", r.text);
    assert!(f.fake.calls().is_empty(), "{:?}", f.fake.calls());
}
