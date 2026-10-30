//! `/updates` through the router, over a fake update engine: the session
//! guard (API keys never reach it), the same-origin and JSON checks, the
//! `429` with `Retry-After`, the pause values, and the response bytes
//! against `golden/updates_<name>.txt`.
//! `LODESTAR_BLESS=1 cargo test -p lodestar-api --test updates` rewrites
//! the files; read the diff before committing it.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Request, header};
use axum::{Extension, Router};
use lodestar_api::ops::BoxFuture;
use lodestar_api::updates::{
    Blocked, Capability, CheckState, CheckStatus, HistoryEntry, Policy, ReleaseInfo, RollbackInfo,
    Running, UpdatesApi, UpdatesError, UpdatesStatus,
};
use lodestar_api::{ApiState, Auth, router};
use lodestar_store::{Store, StoreConfig};
use serde_json::{Value, json};
use tower::ServiceExt;

#[derive(Debug, Default)]
struct FakeEngine {
    checks: Mutex<u32>,
    skipped: Mutex<Vec<String>>,
    paused: Mutex<Option<u32>>,
    /// Saves fail (DUR-26): a pause or resume is `updates_not_saved`.
    fail_saves: Mutex<bool>,
    /// A failed pause's process-only hold.
    held: Mutex<bool>,
}

fn status(e: &FakeEngine) -> UpdatesStatus {
    let skipped = e.skipped.lock().unwrap().clone();
    let releases = if skipped.contains(&"0.6.0".to_string()) {
        vec![]
    } else {
        vec![ReleaseInfo {
            version: "0.6.0".into(),
            date: 1_792_600_000,
            level: "feature".into(),
            security: false,
            manual_steps: false,
            migrates: true,
            reindex: false,
            reindex_bytes: 0,
            notes: "New: the Updates screen.".into(),
            notes_url: None,
            eligible: false,
            blocked_by: vec![
                Blocked {
                    code: "level".into(),
                    until: None,
                    via: None,
                    reason: None,
                },
                Blocked {
                    code: "wait".into(),
                    until: Some(1_792_859_200),
                    via: None,
                    reason: None,
                },
            ],
            installs_at: None,
        }]
    };
    UpdatesStatus {
        unavailable: None,
        status: CheckStatus {
            state: CheckState::Ok,
            checked_at: Some(1_792_677_600),
            next_check_at: Some(1_792_700_400),
            error: None,
        },
        running: Running {
            version: "0.5.3".into(),
            channel: "stable".into(),
            image_version: Some("0.5.2".into()),
            overlay: true,
            revoked_key: false,
            withdrawn: false,
        },
        capability: Capability {
            can_install: false,
            reason: Some("not_built_yet".into()),
        },
        policy: Policy {
            mode: "notify".into(),
            level: "fixes".into(),
            security_fixes: false,
            wait_days: 3,
            window: "03:00-05:00".into(),
            window_zone: "Europe/Berlin".into(),
            next_window_at: Some(1_792_717_200),
            paused_until: e
                .paused
                .lock()
                .unwrap()
                .filter(|&d| d > 0)
                .map(|d| 1_792_677_600 + i64::from(d) * 86_400),
            held_in_process: *e.held.lock().unwrap(),
            allow_reindex: true,
        },
        releases,
        pending: None,
        rollback: RollbackInfo {
            available: false,
            to_version: Some("image".into()),
            lost_torrents: None,
        },
        notices: vec![],
        history: vec![HistoryEntry {
            version: "0.5.3".into(),
            from: "0.5.2".into(),
            level: Some("fix".into()),
            at: 1_792_500_000,
            result: "installed".into(),
            reason: None,
            lost_torrents: None,
        }],
    }
}

impl UpdatesApi for FakeEngine {
    fn status(&self) -> BoxFuture<'_, UpdatesStatus> {
        Box::pin(async move { status(self) })
    }

    fn check(&self) -> BoxFuture<'_, Result<UpdatesStatus, UpdatesError>> {
        Box::pin(async move {
            let mut n = self.checks.lock().unwrap();
            *n += 1;
            if *n > 1 {
                return Err(UpdatesError::RateLimited { retry_after: 42 });
            }
            drop(n);
            Ok(status(self))
        })
    }

    fn skip(&self, version: String) -> BoxFuture<'_, Result<UpdatesStatus, UpdatesError>> {
        Box::pin(async move {
            if version != "0.6.0" {
                return Err(UpdatesError::UnknownVersion);
            }
            self.skipped.lock().unwrap().push(version);
            Ok(status(self))
        })
    }

    fn pause(&self, days: u32) -> BoxFuture<'_, Result<UpdatesStatus, UpdatesError>> {
        Box::pin(async move {
            if *self.fail_saves.lock().unwrap() {
                if days > 0 {
                    *self.held.lock().unwrap() = true;
                }
                return Err(UpdatesError::NotSaved);
            }
            *self.paused.lock().unwrap() = Some(days);
            *self.held.lock().unwrap() = false;
            Ok(status(self))
        })
    }

    fn clear_history(&self) -> BoxFuture<'_, Result<UpdatesStatus, UpdatesError>> {
        Box::pin(async move {
            let mut s = status(self);
            s.history.clear();
            Ok(s)
        })
    }
}

struct Fixture {
    app: Router,
    auth: Arc<Auth>,
    engine: Option<Arc<FakeEngine>>,
    _dir: tempfile::TempDir,
}

fn fixture_with(engine: bool) -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("t.db");
    drop(Store::open(&db, StoreConfig::default()).unwrap());
    let reader = Store::open_reader(&db, StoreConfig::default()).unwrap();
    let auth = Arc::new(Auth::load(&dir.path().join("auth.json"), false).unwrap());
    let api = router(ApiState {
        auth: auth.clone(),
        index: None,
        reader: Arc::new(Mutex::new(reader)),
        filters: None,
        settings: None,
        ops: None,
        user_data: None,
        insights: None,
        adult: Default::default(),
        hot: None,
        events: None,
    });
    let mut app = Router::new().nest("/api/v1", api);
    let engine = engine.then(|| Arc::new(FakeEngine::default()));
    if let Some(e) = &engine {
        let e: Arc<dyn UpdatesApi> = e.clone();
        app = app.layer(Extension(e));
    }
    Fixture {
        app,
        auth,
        engine,
        _dir: dir,
    }
}

fn req(method: &str, uri: &str, cookie: Option<&str>, body: Option<Value>) -> Request<Body> {
    let mut b = Request::builder().method(method).uri(uri);
    if let Some(c) = cookie {
        b = b.header(header::COOKIE, c);
    }
    if method == "POST" {
        b = b.header(header::CONTENT_TYPE, "application/json");
    }
    let body = body.map_or_else(Body::empty, |v| Body::from(v.to_string()));
    let mut req = b.body(body).unwrap();
    req.extensions_mut()
        .insert(ConnectInfo(SocketAddr::from(([10, 9, 0, 1], 1))));
    req
}

async fn send(f: &Fixture, req: Request<Body>) -> (u16, Option<String>, String) {
    let resp = f.app.clone().oneshot(req).await.unwrap();
    let status = resp.status().as_u16();
    let retry = resp
        .headers()
        .get(header::RETRY_AFTER)
        .map(|v| v.to_str().unwrap().to_string());
    let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20)
        .await
        .unwrap();
    (status, retry, String::from_utf8(bytes.to_vec()).unwrap())
}

async fn golden(f: &Fixture, name: &str, req: Request<Body>) -> Value {
    let (status, _, body) = send(f, req).await;
    let got = format!("{status}\n{body}\n");
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden")
        .join(format!("updates_{name}.txt"));
    if std::env::var_os("LODESTAR_BLESS").is_some() {
        std::fs::write(&path, &got).unwrap();
    } else {
        let want = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("{}: {e} (bless with LODESTAR_BLESS=1)", path.display()));
        assert_eq!(
            got,
            want.replace("\r\n", "\n"),
            "{name}: the response bytes changed"
        );
    }
    serde_json::from_str(&body).unwrap_or(Value::Null)
}

fn session(f: &Fixture) -> String {
    let token = f.auth.setup_token().unwrap();
    let set = f
        .auth
        .setup([127, 0, 0, 1].into(), &token, "correct horse battery")
        .unwrap();
    set.split(';').next().unwrap().to_string()
}

const ROUTES: &[(&str, &str)] = &[
    ("GET", "/api/v1/updates"),
    ("POST", "/api/v1/updates/check"),
    ("POST", "/api/v1/updates/skip"),
    ("POST", "/api/v1/updates/pause"),
    ("POST", "/api/v1/updates/history/clear"),
];

/// A valid body for each write.
fn body_for(path: &str) -> Value {
    match path.rsplit('/').next() {
        Some("skip") => json!({"version": "0.6.0"}),
        Some("pause") => json!({"days": 1}),
        _ => json!({}),
    }
}

#[tokio::test]
async fn every_route_needs_a_session_and_never_an_api_key() {
    let f = fixture_with(true);
    for (method, path) in ROUTES {
        let (status, _, body) = send(&f, req(method, path, None, Some(json!({})))).await;
        assert_eq!(status, 401, "{method} {path}: {body}");
        for (name, value) in [
            (
                "x-api-key",
                "lsk_AAAAAAAA_AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
            ),
            ("remote-user", "admin"),
        ] {
            let mut r = req(method, path, None, Some(json!({})));
            r.headers_mut().insert(name, value.parse().unwrap());
            let (status, _, body) = send(&f, r).await;
            assert_eq!(status, 401, "{method} {path} with {name}: {body}");
        }
    }
}

#[tokio::test]
async fn writes_are_same_origin_json() {
    let f = fixture_with(true);
    let c = session(&f);
    for (method, path) in ROUTES.iter().filter(|(m, _)| *m == "POST") {
        let mut cross = req(method, path, Some(&c), Some(body_for(path)));
        cross
            .headers_mut()
            .insert(header::ORIGIN, "https://evil.example".parse().unwrap());
        let (status, _, body) = send(&f, cross).await;
        assert_eq!(status, 403, "{path}: {body}");
        let mut text = req(method, path, Some(&c), Some(body_for(path)));
        text.headers_mut()
            .insert(header::CONTENT_TYPE, "text/plain".parse().unwrap());
        let (status, _, body) = send(&f, text).await;
        assert_eq!(status, 415, "{path}: {body}");
    }
}

#[tokio::test]
async fn updates_golden() {
    let f = fixture_with(true);
    let c = session(&f);
    let s = golden(&f, "status", req("GET", "/api/v1/updates", Some(&c), None)).await;
    assert_eq!(s["releases"][0]["version"], "0.6.0");
    golden(
        &f,
        "check",
        req("POST", "/api/v1/updates/check", Some(&c), Some(json!({}))),
    )
    .await;
    let (status, retry, _) = send(
        &f,
        req("POST", "/api/v1/updates/check", Some(&c), Some(json!({}))),
    )
    .await;
    assert_eq!((status, retry.as_deref()), (429, Some("42")));
    golden(
        &f,
        "check_rate_limited",
        req("POST", "/api/v1/updates/check", Some(&c), Some(json!({}))),
    )
    .await;
    golden(
        &f,
        "skip_unknown",
        req(
            "POST",
            "/api/v1/updates/skip",
            Some(&c),
            Some(json!({"version": "9.9.9"})),
        ),
    )
    .await;
    let skipped = golden(
        &f,
        "skip",
        req(
            "POST",
            "/api/v1/updates/skip",
            Some(&c),
            Some(json!({"version": "0.6.0"})),
        ),
    )
    .await;
    assert_eq!(skipped["releases"], json!([]));
    golden(
        &f,
        "pause_bad",
        req(
            "POST",
            "/api/v1/updates/pause",
            Some(&c),
            Some(json!({"days": 3})),
        ),
    )
    .await;
    let paused = golden(
        &f,
        "pause",
        req(
            "POST",
            "/api/v1/updates/pause",
            Some(&c),
            Some(json!({"days": 7})),
        ),
    )
    .await;
    assert!(paused["policy"]["paused_until"].is_i64());
    let cleared = golden(
        &f,
        "history_clear",
        req(
            "POST",
            "/api/v1/updates/history/clear",
            Some(&c),
            Some(json!({})),
        ),
    )
    .await;
    assert_eq!(cleared["history"], json!([]));
}

/// DUR-26: a pause that couldn't be saved is `500 updates_not_saved`
/// with the fixed sentence, and `GET /updates` then reports the
/// process-only hold, never a saved pause; a saved resume ends the hold.
#[tokio::test]
async fn a_pause_that_isnt_saved_is_refused_and_held() {
    let f = fixture_with(true);
    let c = session(&f);
    let engine = f.engine.clone().unwrap();
    *engine.fail_saves.lock().unwrap() = true;
    let refused = golden(
        &f,
        "pause_not_saved",
        req(
            "POST",
            "/api/v1/updates/pause",
            Some(&c),
            Some(json!({"days": 7})),
        ),
    )
    .await;
    assert_eq!(refused["error"]["code"], "updates_not_saved");
    let s = golden(
        &f,
        "status_held",
        req("GET", "/api/v1/updates", Some(&c), None),
    )
    .await;
    assert_eq!(s["policy"]["held_in_process"], true);
    assert_eq!(s["policy"]["paused_until"], Value::Null);
    *engine.fail_saves.lock().unwrap() = false;
    let (status, _, body) = send(
        &f,
        req(
            "POST",
            "/api/v1/updates/pause",
            Some(&c),
            Some(json!({"days": 0})),
        ),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    let v: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["policy"]["held_in_process"], false);
}

#[tokio::test]
async fn without_an_engine_every_route_is_503() {
    let f = fixture_with(false);
    let c = session(&f);
    for (method, path) in ROUTES {
        let (status, _, body) = send(&f, req(method, path, Some(&c), Some(body_for(path)))).await;
        assert_eq!(status, 503, "{method} {path}: {body}");
        assert!(body.contains("updates_unavailable"), "{body}");
    }
}
