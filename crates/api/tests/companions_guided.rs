//! The guided `/companions/…` routes through the router, over a fake of the
//! binary's `CompanionsGuidedApi`: the session guard on every route, `503`
//! without the binary's half, the paste's bounds, the plan's `429` with
//! `Retry-After`, confirmation and forget mismatches, the binary's
//! refusals passed through with their codes, and that a pasted value
//! reaches no response, error, log line or the OpenAPI document.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex, Once};

use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Request, header};
use axum::{Extension, Router};
use lodestar_api::companions::CompanionsError;
use lodestar_api::companions_guided::{CompanionsGuidedApi, GuidedError, PASTE_MAX};
use lodestar_api::dto::companions::{Companion, TemplateKind};
use lodestar_api::dto::companions_guided::{
    CompanionForgotten, CompanionTarget, DetectedCompanion,
};
use lodestar_api::ops::BoxFuture;
use lodestar_api::{ApiState, Auth, router};
use lodestar_log::{Mask, Record, Sink};
use lodestar_store::{Store, StoreConfig};
use serde_json::{Value, json};
use tower::ServiceExt;

const PASSWORD: &str = "correct horse battery";
/// What a test pastes, which must reach the fake and nothing else.
const PASTED: &str = "Tq7pasteDvX3kR";
const PLAN: [&str; 2] = ["webui.password", "downloads.save_path"];

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

/// Records each call; `refuse` makes every action answer that refusal.
#[derive(Debug, Default)]
struct Fake {
    calls: Mutex<Vec<String>>,
    refuse: Mutex<Option<GuidedError>>,
}

impl Fake {
    fn call(&self, what: String) -> Result<(), GuidedError> {
        self.calls.lock().unwrap().push(what);
        match self.refuse.lock().unwrap().clone() {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }
    fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }
    fn refuse(&self, e: GuidedError) {
        *self.refuse.lock().unwrap() = Some(e);
    }
}

fn detected() -> DetectedCompanion {
    DetectedCompanion {
        id: "qbittorrent".into(),
        target: CompanionTarget::Loopback { port: 8080 },
        version: Some("5.1.2".into()),
    }
}

impl CompanionsGuidedApi for Fake {
    fn detect(&self) -> BoxFuture<'_, Result<Vec<DetectedCompanion>, GuidedError>> {
        Box::pin(async move {
            self.call("detect".into())?;
            Ok(vec![detected()])
        })
    }
    fn set_target(
        &self,
        id: String,
        target: CompanionTarget,
    ) -> BoxFuture<'_, Result<DetectedCompanion, GuidedError>> {
        Box::pin(async move {
            self.call(format!("target {id} {target:?}"))?;
            Ok(detected())
        })
    }
    fn paste(&self, id: String, value: String) -> BoxFuture<'_, Result<(), GuidedError>> {
        Box::pin(async move {
            // The value reaches the binary, and only the binary.
            self.call(format!("paste {id} {}", value == PASTED))
        })
    }
    fn plan(&self, id: String) -> BoxFuture<'_, Result<Vec<String>, GuidedError>> {
        Box::pin(async move {
            self.call(format!("plan {id}"))?;
            Ok(PLAN.iter().map(|s| (*s).to_owned()).collect())
        })
    }
    fn confirm(
        &self,
        id: String,
        settings: Vec<String>,
    ) -> BoxFuture<'_, Result<Companion, GuidedError>> {
        Box::pin(async move {
            self.call(format!("confirm {id} {}", settings.join(",")))?;
            Ok(Companion {
                available: Some("5.1.2".into()),
                credentials: vec!["webui_password".into()],
                health: None,
                id,
                installed: None,
                licence: "GPL-2.0-or-later".into(),
                name: "qBittorrent".into(),
                opt_in: false,
                refusal: None,
                templates: vec![TemplateKind::Compose],
            })
        })
    }
    fn forget(&self, id: String) -> BoxFuture<'_, Result<CompanionForgotten, GuidedError>> {
        Box::pin(async move {
            self.call(format!("forget {id}"))?;
            Ok(CompanionForgotten {
                credential_in_use: true,
                deleted: vec![format!("companions.{id}.target")],
                id,
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
    let mut app = Router::new().nest("/api/v1", api);
    if layer {
        app = app.layer(Extension(fake.clone() as Arc<dyn CompanionsGuidedApi>));
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
    retry_after: Option<String>,
}

impl Reply {
    fn code(&self) -> &str {
        self.body["error"]["code"].as_str().unwrap_or("")
    }
}

async fn send(f: &Fixture, req: Request<Body>) -> Reply {
    let resp = f.app.clone().oneshot(req).await.unwrap();
    let status = resp.status().as_u16();
    let retry_after = resp
        .headers()
        .get(header::RETRY_AFTER)
        .map(|v| v.to_str().unwrap().to_string());
    let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20)
        .await
        .unwrap();
    let text = String::from_utf8(bytes.to_vec()).unwrap();
    Reply {
        status,
        body: serde_json::from_str(&text).unwrap_or(Value::Null),
        text,
        retry_after,
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
        ("POST", "/api/v1/companions/detect", None),
        (
            "POST",
            "/api/v1/companions/qbittorrent/target",
            Some(json!({ "kind": "loopback", "port": 8080 })),
        ),
        (
            "POST",
            "/api/v1/companions/qbittorrent/paste",
            Some(json!({ "value": PASTED })),
        ),
        ("POST", "/api/v1/companions/qbittorrent/plan", None),
        (
            "POST",
            "/api/v1/companions/qbittorrent/confirm",
            Some(json!({ "settings": PLAN })),
        ),
        (
            "POST",
            "/api/v1/companions/qbittorrent/guided/forget",
            Some(json!({ "confirm": "qbittorrent" })),
        ),
    ]
}

#[tokio::test]
async fn every_route_refuses_callers_who_arent_the_admin() {
    let f = fixture();
    let cookie = session(&f);
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
        // A session, but from another site's page.
        let mut q = req(method, path, Some(&cookie), body.clone());
        q.headers_mut()
            .insert(header::ORIGIN, "https://evil.example".parse().unwrap());
        let r = send(&f, q).await;
        assert_eq!(
            (r.status, r.code()),
            (403, "cross_origin"),
            "{method} {path}"
        );
    }
    assert!(f.fake.calls().is_empty(), "{:?}", f.fake.calls());
}

#[tokio::test]
async fn without_the_binarys_half_every_route_is_unavailable() {
    let f = fixture_with(false);
    let cookie = session(&f);
    for (method, path, body) in every_route() {
        let r = send(&f, req(method, path, Some(&cookie), body)).await;
        assert_eq!(
            (r.status, r.code()),
            (503, "companions_unavailable"),
            "{method} {path}: {}",
            r.text
        );
    }
}

#[tokio::test]
async fn each_route_reaches_the_binary_with_what_it_was_given() {
    let f = fixture();
    let cookie = session(&f);
    let mut statuses = Vec::new();
    for (method, path, body) in every_route() {
        let r = send(&f, req(method, path, Some(&cookie), body)).await;
        statuses.push(r.status);
        assert!(!r.text.contains(PASTED), "{path}: {}", r.text);
        match path {
            "/api/v1/companions/detect" => {
                assert_eq!(
                    r.body["found"][0]["target"],
                    json!({"kind":"loopback","port":8080})
                );
            }
            "/api/v1/companions/qbittorrent/plan" => {
                assert_eq!(r.body, json!({ "id": "qbittorrent", "settings": PLAN }));
            }
            "/api/v1/companions/qbittorrent/guided/forget" => {
                assert_eq!(
                    r.body,
                    json!({
                        "id": "qbittorrent",
                        "credential_in_use": true,
                        "deleted": ["companions.qbittorrent.target"]
                    })
                );
            }
            _ => {}
        }
    }
    assert_eq!(statuses, [200, 200, 204, 200, 200, 200]);
    assert_eq!(
        f.fake.calls(),
        [
            "detect",
            "target qbittorrent Loopback { port: 8080 }",
            "paste qbittorrent true",
            "plan qbittorrent",
            "confirm qbittorrent webui.password,downloads.save_path",
            "forget qbittorrent",
        ]
    );
}

#[tokio::test]
async fn targets_are_closed_and_ids_checked() {
    let f = fixture();
    let cookie = session(&f);
    let path = "/api/v1/companions/qbittorrent/target";
    for body in [
        json!({ "kind": "lan", "port": 1 }),
        json!({ "kind": "loopback", "port": 8080, "host": "10.0.0.1" }),
        json!({ "kind": "loopback", "port": 70000 }),
        json!({ "kind": "url" }),
        json!({ "kind": "url", "url": "http://x", "port": 1 }),
    ] {
        let r = send(&f, req("POST", path, Some(&cookie), Some(body.clone()))).await;
        assert_eq!(
            (r.status, r.code()),
            (422, "bad_body"),
            "{body}: {}",
            r.text
        );
    }
    for path in [
        "/api/v1/companions/Q/target",
        "/api/v1/companions/..%2Fetc/target",
        "/api/v1/companions/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa/target",
    ] {
        let r = send(
            &f,
            req(
                "POST",
                path,
                Some(&cookie),
                Some(json!({ "kind": "loopback", "port": 8080 })),
            ),
        )
        .await;
        assert_eq!((r.status, r.code()), (404, "unknown_companion"), "{path}");
    }
    assert!(f.fake.calls().is_empty(), "{:?}", f.fake.calls());
}

/// The bound is the API's: empty, over 128 bytes, or any control
/// character is refused before the binary sees it, and the refusal never
/// carries the value.
#[tokio::test]
async fn a_paste_is_bounded_before_the_binary_sees_it() {
    let f = fixture();
    let cookie = session(&f);
    let path = "/api/v1/companions/qbittorrent/paste";
    let long = format!("{PASTED}{}", "x".repeat(PASTE_MAX));
    for value in [
        String::new(),
        long.clone(),
        format!("{PASTED}\n"),
        format!("{PASTED}\u{0}"),
        format!("\u{1b}[31m{PASTED}"),
        format!("{PASTED}\u{7f}"),
        format!("{PASTED}\u{85}"),
    ] {
        let r = send(
            &f,
            req("POST", path, Some(&cookie), Some(json!({ "value": value }))),
        )
        .await;
        assert_eq!(
            (r.status, r.code(), r.body["error"]["field"].as_str()),
            (400, "bad_value", Some("value")),
            "{value:?}: {}",
            r.text
        );
        assert!(!r.text.contains(PASTED), "{}", r.text);
    }
    // Exactly the bound is accepted; multi-byte characters count as bytes.
    let at = "x".repeat(PASTE_MAX);
    let r = send(
        &f,
        req("POST", path, Some(&cookie), Some(json!({ "value": at }))),
    )
    .await;
    assert_eq!(r.status, 204, "{}", r.text);
    let over = "é".repeat(PASTE_MAX / 2 + 1);
    let r = send(
        &f,
        req("POST", path, Some(&cookie), Some(json!({ "value": over }))),
    )
    .await;
    assert_eq!(r.code(), "bad_value");
    // A body that isn't the shape says so without the value.
    for body in [
        json!({ "value": PASTED, "also": 1 }),
        json!({ "value": [PASTED] }),
        json!({ "secret": PASTED }),
    ] {
        let r = send(&f, req("POST", path, Some(&cookie), Some(body.clone()))).await;
        assert_eq!((r.status, r.code()), (422, "bad_body"), "{body}");
        assert!(!r.text.contains(PASTED), "{}", r.text);
    }
    assert_eq!(f.fake.calls(), ["paste qbittorrent false"]);
}

#[tokio::test]
async fn a_second_plan_within_the_minute_is_rate_limited_with_retry_after() {
    let f = fixture();
    let cookie = session(&f);
    f.fake.refuse(GuidedError::RateLimited { retry_after: 42 });
    let r = send(
        &f,
        req(
            "POST",
            "/api/v1/companions/qbittorrent/plan",
            Some(&cookie),
            None,
        ),
    )
    .await;
    assert_eq!((r.status, r.code()), (429, "rate_limited"), "{}", r.text);
    assert_eq!(r.retry_after.as_deref(), Some("42"));
    // Never `Retry-After: 0`, which would invite an immediate retry.
    f.fake.refuse(GuidedError::RateLimited { retry_after: 0 });
    let r = send(
        &f,
        req(
            "POST",
            "/api/v1/companions/qbittorrent/plan",
            Some(&cookie),
            None,
        ),
    )
    .await;
    assert_eq!(r.retry_after.as_deref(), Some("1"));
}

#[tokio::test]
async fn the_binarys_refusals_keep_their_codes() {
    let f = fixture();
    let cookie = session(&f);
    for (refusal, status, code) in [
        (
            CompanionsError::Refused("handover_later"),
            409,
            "handover_later",
        ),
        (
            CompanionsError::Refused("address_refused"),
            409,
            "address_refused",
        ),
        (
            CompanionsError::Refused("not_detected"),
            409,
            "not_detected",
        ),
        (CompanionsError::Refused("no_target"), 409, "no_target"),
        (CompanionsError::Refused("installed"), 409, "installed"),
        (CompanionsError::Refused("busy"), 409, "busy"),
        (CompanionsError::Mismatch, 409, "confirmation_mismatch"),
        (CompanionsError::Unknown, 404, "unknown_companion"),
        (
            CompanionsError::BadInput {
                field: Some("url"),
                code: "bad_template_input",
            },
            400,
            "bad_template_input",
        ),
    ] {
        f.fake.refuse(refusal.clone().into());
        for (method, path, body) in every_route() {
            let r = send(&f, req(method, path, Some(&cookie), body)).await;
            assert_eq!(
                (r.status, r.code()),
                (status, code),
                "{refusal:?} {path}: {}",
                r.text
            );
            assert!(!r.text.contains(PASTED), "{}", r.text);
        }
    }
}

#[tokio::test]
async fn forget_needs_the_companion_named() {
    let f = fixture();
    let cookie = session(&f);
    let path = "/api/v1/companions/qbittorrent/guided/forget";
    for body in [
        json!({ "confirm": "prowlarr" }),
        json!({ "confirm": "QBITTORRENT" }),
        json!({ "confirm": "" }),
    ] {
        let r = send(&f, req("POST", path, Some(&cookie), Some(body.clone()))).await;
        assert_eq!(
            (r.status, r.code()),
            (409, "confirmation_mismatch"),
            "{body}"
        );
    }
    let r = send(&f, req("POST", path, Some(&cookie), Some(json!({})))).await;
    assert_eq!((r.status, r.code()), (422, "bad_body"));
    assert!(f.fake.calls().is_empty(), "{:?}", f.fake.calls());
}

/// The paste's value goes to the binary and nowhere else: no log line from
/// any of this file's tests, no OpenAPI example, and the request's own
/// `Debug` hides it.
#[tokio::test]
async fn a_pasted_value_reaches_no_log_or_document() {
    let f = fixture();
    let cookie = session(&f);
    let r = send(
        &f,
        req(
            "POST",
            "/api/v1/companions/qbittorrent/paste",
            Some(&cookie),
            Some(json!({ "value": PASTED })),
        ),
    )
    .await;
    assert_eq!((r.status, r.text.as_str()), (204, ""));
    let doc = send(&f, req("GET", "/api/v1/openapi.json", None, None)).await;
    assert_eq!(doc.status, 200);
    assert!(doc.text.contains("pasteCompanionCredential"));
    assert!(!doc.text.contains(PASTED));
    let lines = LINES.lock().unwrap_or_else(|e| e.into_inner()).clone();
    assert!(!lines.is_empty(), "the capture works");
    for line in lines {
        assert!(!line.contains(PASTED), "{line}");
    }
}
