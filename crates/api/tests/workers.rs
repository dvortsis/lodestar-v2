//! `/workers` through the router, over a fake of the binary's
//! `WorkersApi`: every action, the session guard (no session, an API key,
//! a proxy header), id and range validation, the error envelope, and that
//! the join token appears in the create response and nowhere else: not in
//! any other route's body, not in an error, not in a log line.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex, Once};

use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Request, header};
use axum::{Extension, Router};
use lodestar_api::dto::workers::{
    DhtPin, Worker, WorkerBudget, WorkerIntensity, WorkerList, WorkerPatch, WorkerRevoked,
    WorkerStatus,
};
use lodestar_api::ops::BoxFuture;
use lodestar_api::workers::{NewToken, WorkersApi, WorkersError};
use lodestar_api::{ApiState, Auth, router};
use lodestar_log::{Mask, Record, Sink};
use lodestar_store::{Store, StoreConfig};
use serde_json::{Value, json};
use tower::ServiceExt;

const TOKEN: &str = "lsj1aaaabbbbccccddddeeeeffffgggghhhhiiiijjjjkkkkllllmmmmnnnnooooppppqqqqrrrrssssttttuuuuvvvvwwwwxxxxyyyyzzzz2222333344445555666677";
const PASSWORD: &str = "correct horse battery";

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

/// A fleet of one worker, id 3, as the binary would answer.
#[derive(Debug, Default)]
struct Fake {
    workers: Mutex<BTreeMap<u32, Worker>>,
    calls: Mutex<Vec<String>>,
}

fn worker(id: u32) -> Worker {
    Worker {
        budget: WorkerBudget {
            cpu_permille: 1000,
            down_kbps: 0,
            ram_mb: 0,
            up_kbps: 0,
        },
        claimed: Some(10),
        clock_offset_ms: Some(-12),
        clock_skewed: false,
        cpu_permille: Some(150),
        dht: DhtPin::Auto,
        external_ips: vec!["81.2.69.7".into()],
        fetched: Some(8),
        id,
        intensity: WorkerIntensity::Balanced,
        last_seen_at: Some(1_792_677_600),
        leases: 2,
        name: Some("nas".into()),
        pool_cap: 50_000,
        processed: Some(8),
        rss_mb: Some(120),
        spool_bytes: Some(0),
        status: WorkerStatus::Online,
    }
}

impl Fake {
    fn new() -> Fake {
        let f = Fake::default();
        f.workers.lock().unwrap().insert(3, worker(3));
        f
    }

    fn act(
        &self,
        what: &str,
        id: u32,
        f: impl FnOnce(&mut Worker),
    ) -> Result<Worker, WorkersError> {
        self.calls.lock().unwrap().push(format!("{what} {id}"));
        let mut all = self.workers.lock().unwrap();
        let w = all.get_mut(&id).ok_or(WorkersError::UnknownWorker)?;
        f(w);
        Ok(w.clone())
    }
}

impl WorkersApi for Fake {
    fn list(&self) -> BoxFuture<'_, Result<WorkerList, WorkersError>> {
        Box::pin(async move {
            Ok(WorkerList {
                allowed: true,
                workers: self.workers.lock().unwrap().values().cloned().collect(),
            })
        })
    }

    fn create_token(&self, name: String) -> BoxFuture<'_, Result<NewToken, WorkersError>> {
        Box::pin(async move {
            if name == "taken" {
                return Err(WorkersError::NameInUse);
            }
            Ok(NewToken {
                token: TOKEN.into(),
                name,
                expires_at: 1_792_678_500,
                port: 8091,
            })
        })
    }

    fn drain(&self, id: u32) -> BoxFuture<'_, Result<Worker, WorkersError>> {
        Box::pin(async move { self.act("drain", id, |w| w.status = WorkerStatus::Draining) })
    }

    fn resume(&self, id: u32) -> BoxFuture<'_, Result<Worker, WorkersError>> {
        Box::pin(async move { self.act("resume", id, |w| w.status = WorkerStatus::Online) })
    }

    fn pause(&self, id: u32) -> BoxFuture<'_, Result<Worker, WorkersError>> {
        Box::pin(async move { self.act("pause", id, |w| w.status = WorkerStatus::Paused) })
    }

    fn revoke(&self, id: u32) -> BoxFuture<'_, Result<WorkerRevoked, WorkersError>> {
        Box::pin(async move {
            self.calls.lock().unwrap().push(format!("revoke {id}"));
            let w = self
                .workers
                .lock()
                .unwrap()
                .remove(&id)
                .ok_or(WorkersError::UnknownWorker)?;
            Ok(WorkerRevoked {
                certificates: 1,
                gone_within_secs: 5,
                id,
                leases_returned: w.leases,
                name: w.name,
            })
        })
    }

    fn update(&self, id: u32, patch: WorkerPatch) -> BoxFuture<'_, Result<Worker, WorkersError>> {
        Box::pin(async move {
            self.act("update", id, |w| {
                if let Some(cap) = patch.pool_cap {
                    w.pool_cap = cap;
                }
                if let Some(p) = patch.dht {
                    w.dht = p;
                }
            })
        })
    }
}

struct Fixture {
    app: Router,
    auth: Arc<Auth>,
    fake: Option<Arc<Fake>>,
    _dir: tempfile::TempDir,
}

fn fixture_with(fake: bool) -> Fixture {
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
    let fake = fake.then(|| Arc::new(Fake::new()));
    // As `lodestar serve` layers it.
    let mut app = Router::new().nest("/api/v1", api);
    if let Some(f) = &fake {
        app = app.layer(Extension(f.clone() as Arc<dyn WorkersApi>));
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
        .insert(ConnectInfo(SocketAddr::from(([10, 9, 0, 1], 1))));
    req
}

struct Reply {
    status: u16,
    text: String,
    body: Value,
    cache: Option<String>,
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

/// Every route but the create, with a body where it takes one.
fn others() -> Vec<(&'static str, &'static str, Option<Value>)> {
    vec![
        ("GET", "/api/v1/workers", None),
        ("POST", "/api/v1/workers/3/drain", None),
        ("POST", "/api/v1/workers/3/pause", None),
        ("POST", "/api/v1/workers/3/resume", None),
        (
            "PATCH",
            "/api/v1/workers/3",
            Some(json!({ "pool_cap": 10 })),
        ),
        ("POST", "/api/v1/workers/3/revoke", None),
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
    let mut all = others();
    all.push((
        "POST",
        "/api/v1/workers/tokens",
        Some(json!({ "name": "w1" })),
    ));
    for (method, path, body) in all {
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
    }
    assert!(f.fake.as_ref().unwrap().calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn the_token_is_shown_once_with_its_commands() {
    let f = fixture();
    let cookie = session(&f);
    let r = send(
        &f,
        req(
            "POST",
            "/api/v1/workers/tokens",
            Some(&cookie),
            Some(json!({ "name": "w1" })),
        ),
    )
    .await;
    assert_eq!(r.status, 201, "{}", r.text);
    assert_eq!(r.cache.as_deref(), Some("no-store"));
    assert_eq!(r.body["token"], TOKEN);
    assert_eq!(r.body["name"], "w1");
    assert_eq!(r.body["address"], "https://nas.local:8091");
    assert_eq!(r.body["expires_at"], 1_792_678_500);
    let c = &r.body["commands"];
    assert_eq!(
        c["cli"],
        format!(
            "printf '%s\\n' '{TOKEN}' | lodestar worker --join https://nas.local:8091 --token-file -"
        )
    );
    assert!(
        c["docker_run"]
            .as_str()
            .unwrap()
            .contains(&format!("-e LODESTAR_TOKEN={TOKEN}"))
    );
    assert!(
        c["compose"]
            .as_str()
            .unwrap()
            .contains("LODESTAR_JOIN: \"https://nas.local:8091\"")
    );
    for k in ["unraid", "windows"] {
        assert!(c[k].as_str().unwrap().contains("https://nas.local:8091"));
    }

    // Every other route, and every refusal, is free of it.
    for (method, path, body) in others() {
        let r = send(&f, req(method, path, Some(&cookie), body)).await;
        assert!(r.status < 500, "{method} {path}: {}", r.text);
        assert!(
            !r.text.contains(TOKEN) && !r.text.contains("lsj1"),
            "{method} {path}: {}",
            r.text
        );
    }
    for body in [
        json!({ "name": "taken" }),
        json!({ "name": "Bad Name" }),
        json!({ "nope": 1 }),
    ] {
        let r = send(
            &f,
            req("POST", "/api/v1/workers/tokens", Some(&cookie), Some(body)),
        )
        .await;
        assert!(r.status >= 400, "{}", r.text);
        assert!(!r.text.contains("lsj1"), "{}", r.text);
    }
    let log = LINES.lock().unwrap().join("\n");
    assert!(
        !log.contains(TOKEN) && !log.contains(&TOKEN[4..60]),
        "{log}"
    );
}

#[tokio::test]
async fn token_refusals() {
    let f = fixture();
    let cookie = session(&f);
    let post = |body: Value| req("POST", "/api/v1/workers/tokens", Some(&cookie), Some(body));
    let r = send(&f, post(json!({ "name": "taken" }))).await;
    assert_eq!(
        (r.status, r.body["error"]["code"].as_str()),
        (409, Some("name_in_use"))
    );
    for bad in ["", "-a", "a-", "UPPER", "a b", &"x".repeat(64)] {
        let r = send(&f, post(json!({ "name": bad }))).await;
        assert_eq!(r.status, 400, "{bad:?}: {}", r.text);
        assert_eq!(r.body["error"]["code"], "bad_name");
        assert_eq!(r.body["error"]["field"], "name");
    }
    // Not JSON, or cross-site: refused before anything happens.
    let mut q = post(json!({ "name": "w2" }));
    q.headers_mut().remove(header::CONTENT_TYPE);
    assert_eq!(send(&f, q).await.status, 415);
    let mut q = post(json!({ "name": "w2" }));
    q.headers_mut()
        .insert(header::ORIGIN, "https://evil.example".parse().unwrap());
    assert_eq!(send(&f, q).await.status, 403);
    // No workers API in this process: workers are off.
    let off = fixture_with(false);
    let cookie = session(&off);
    let r = send(
        &off,
        req(
            "POST",
            "/api/v1/workers/tokens",
            Some(&cookie),
            Some(json!({ "name": "w1" })),
        ),
    )
    .await;
    assert_eq!(
        (r.status, r.body["error"]["code"].as_str()),
        (409, Some("workers_off"))
    );
}

#[tokio::test]
async fn each_action_reaches_the_binary() {
    let f = fixture();
    let cookie = session(&f);
    let post = |path: &str| req("POST", path, Some(&cookie), None);
    let list = send(&f, req("GET", "/api/v1/workers", Some(&cookie), None)).await;
    assert_eq!(list.status, 200);
    assert_eq!(list.body["allowed"], true);
    assert_eq!(list.body["workers"][0]["id"], 3);
    assert_eq!(
        list.body["workers"][0]["external_ips"],
        json!(["81.2.69.7"])
    );
    for (path, status) in [
        ("/api/v1/workers/3/drain", "draining"),
        ("/api/v1/workers/3/pause", "paused"),
        ("/api/v1/workers/3/resume", "online"),
    ] {
        let r = send(&f, post(path)).await;
        assert_eq!(
            (r.status, r.body["status"].as_str()),
            (200, Some(status)),
            "{path}"
        );
    }
    let r = send(
        &f,
        req(
            "PATCH",
            "/api/v1/workers/3",
            Some(&cookie),
            Some(json!({ "pool_cap": 1234, "dht": "off" })),
        ),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.text);
    assert_eq!(
        (r.body["pool_cap"].as_u64(), r.body["dht"].as_str()),
        (Some(1234), Some("off"))
    );
    let r = send(&f, post("/api/v1/workers/3/revoke")).await;
    assert_eq!(r.status, 200, "{}", r.text);
    assert_eq!(
        r.body,
        json!({ "certificates": 1, "gone_within_secs": 5, "id": 3, "leases_returned": 2, "name": "nas" })
    );
    assert_eq!(
        *f.fake.as_ref().unwrap().calls.lock().unwrap(),
        ["drain 3", "pause 3", "resume 3", "update 3", "revoke 3"]
    );
}

#[tokio::test]
async fn ids_are_checked_and_unknown_ones_are_404() {
    let f = fixture();
    let cookie = session(&f);
    for path in [
        "/api/v1/workers/9/drain",
        "/api/v1/workers/0/pause",
        "/api/v1/workers/9/revoke",
    ] {
        let r = send(&f, req("POST", path, Some(&cookie), None)).await;
        assert_eq!(
            (r.status, r.body["error"]["code"].as_str()),
            (404, Some("unknown_worker")),
            "{path}"
        );
    }
    for path in [
        "/api/v1/workers/abc/drain",
        "/api/v1/workers/-1/pause",
        "/api/v1/workers/99999999999/resume",
    ] {
        let r = send(&f, req("POST", path, Some(&cookie), None)).await;
        assert_eq!(r.status, 400, "{path}: {}", r.text);
    }
    // Worker 0 is the coordinator itself: never reached.
    let r = send(
        &f,
        req("PATCH", "/api/v1/workers/0", Some(&cookie), Some(json!({}))),
    )
    .await;
    assert_eq!(r.status, 404);
    assert!(
        f.fake
            .as_ref()
            .unwrap()
            .calls
            .lock()
            .unwrap()
            .iter()
            .all(|c| !c.ends_with(" 0"))
    );
    // Without a workers API (a single machine): an empty list, nothing to act on.
    let off = fixture_with(false);
    let cookie = session(&off);
    let r = send(&off, req("GET", "/api/v1/workers", Some(&cookie), None)).await;
    assert_eq!(
        (r.status, r.body.clone()),
        (200, json!({ "allowed": false, "workers": [] }))
    );
    let r = send(
        &off,
        req("POST", "/api/v1/workers/3/drain", Some(&cookie), None),
    )
    .await;
    assert_eq!(r.status, 404);
}

#[tokio::test]
async fn patch_ranges_are_enforced() {
    let f = fixture();
    let cookie = session(&f);
    let budget = |cpu: u32, ram: u64, up: u64, down: u64| json!({ "budget": { "cpu_permille": cpu, "ram_mb": ram, "up_kbps": up, "down_kbps": down } });
    for (body, field) in [
        (budget(0, 0, 0, 0), "budget.cpu_permille"),
        (budget(1001, 0, 0, 0), "budget.cpu_permille"),
        (budget(1, 1_048_577, 0, 0), "budget.ram_mb"),
        (budget(1, 0, 10_000_001, 0), "budget.up_kbps"),
        (budget(1, 0, 0, 10_000_001), "budget.down_kbps"),
        (json!({ "pool_cap": 0 }), "pool_cap"),
        (json!({ "pool_cap": 1_000_001 }), "pool_cap"),
    ] {
        let r = send(
            &f,
            req(
                "PATCH",
                "/api/v1/workers/3",
                Some(&cookie),
                Some(body.clone()),
            ),
        )
        .await;
        assert_eq!(r.status, 422, "{body}: {}", r.text);
        assert_eq!(r.body["error"]["code"], "out_of_range");
        assert_eq!(r.body["error"]["field"], field);
    }
    for body in [
        json!({ "intensity": "turbo" }),
        json!({ "dht": "on" }),
        json!({ "pool_cap": -1 }),
        json!({ "budget": { "cpu_permille": 70000, "ram_mb": 0, "up_kbps": 0, "down_kbps": 0 } }),
        json!({ "token": "x" }),
    ] {
        let r = send(
            &f,
            req(
                "PATCH",
                "/api/v1/workers/3",
                Some(&cookie),
                Some(body.clone()),
            ),
        )
        .await;
        assert_eq!(r.status, 422, "{body}: {}", r.text);
        assert_eq!(r.body["error"]["code"], "bad_body");
    }
    assert!(
        f.fake.as_ref().unwrap().calls.lock().unwrap().is_empty(),
        "nothing reached it"
    );
    let ok = budget(1000, 1_048_576, 10_000_000, 10_000_000);
    let r = send(
        &f,
        req("PATCH", "/api/v1/workers/3", Some(&cookie), Some(ok)),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.text);
}
