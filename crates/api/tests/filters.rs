//! The filters API through the router (§20.7, §20.11): strengthening
//! changes apply at once, weakening ones need the password every time,
//! the floor stays locked, and Undo goes back one step.

use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use lodestar_api::filters::FiltersApi;
use lodestar_api::{ApiState, Auth, router};
use lodestar_bencode::{OwnedValue, to_vec};
use lodestar_classify::ContentType;
use lodestar_filter::lists::SharedIps;
use lodestar_filter::{Action, Core, CsList, Engine, Handle, HeldSet, Lists, Sealer, Switches};
use lodestar_metainfo::Info;
use lodestar_store::{Filtered, Held, NewTorrent, SourceId, Store, StoreConfig, WriteBatch};
use serde_json::{Value, json};
use tower::ServiceExt;

const PASSWORD: &str = "correct horse battery";

struct Fixture {
    app: Router,
    auth: Arc<Auth>,
    api: Arc<FiltersApi>,
    _dir: tempfile::TempDir,
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("t.db");
    let store = Store::open(&db, StoreConfig::default()).unwrap();
    let reader = Store::open_reader(&db, StoreConfig::default()).unwrap();
    let engine = Handle::new(Engine::new(Core::new(
        Lists::shipped().unwrap(),
        CsList::default(),
        Switches::default(),
    )));
    let api = Arc::new(FiltersApi {
        store: Arc::new(Mutex::new(store)),
        engine: Arc::new(engine),
        held: Arc::new(HeldSet::default()),
        sealer: Some(Arc::new(Sealer::new(&[7; 32]))),
        ips: Arc::new(SharedIps::default()),
    });
    let auth = Arc::new(Auth::load(&dir.path().join("auth.json"), false).unwrap());
    let app = Router::new().nest(
        "/api/v1",
        router(ApiState {
            auth: auth.clone(),
            index: None,
            reader: Arc::new(Mutex::new(reader)),
            filters: Some(api.clone()),
        }),
    );
    Fixture {
        app,
        auth,
        api,
        _dir: dir,
    }
}

async fn call(app: &Router, req: Request<Body>) -> (StatusCode, Option<String>, Value) {
    let resp = app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let cookie = resp
        .headers()
        .get(header::SET_COOKIE)
        .map(|v| v.to_str().unwrap().split(';').next().unwrap().to_string());
    let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20)
        .await
        .unwrap();
    let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, cookie, body)
}

fn get(uri: &str, cookie: &str) -> Request<Body> {
    Request::get(uri)
        .header(header::COOKIE, cookie)
        .body(Body::empty())
        .unwrap()
}

fn post(uri: &str, cookie: Option<&str>, body: Value) -> Request<Body> {
    let mut b = Request::post(uri).header(header::CONTENT_TYPE, "application/json");
    if let Some(c) = cookie {
        b = b.header(header::COOKIE, c);
    }
    b.body(Body::from(body.to_string())).unwrap()
}

async fn logged_in(f: &Fixture) -> String {
    let token = f.auth.setup_token().unwrap();
    let (status, cookie, _) = call(
        &f.app,
        post(
            "/api/v1/setup",
            None,
            json!({ "token": token, "password": PASSWORD }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    cookie.unwrap()
}

async fn apply(
    f: &Fixture,
    cookie: &str,
    change: Value,
    password: Option<&str>,
) -> (StatusCode, Value) {
    let (status, _, body) = call(
        &f.app,
        post(
            "/api/v1/filters/apply",
            Some(cookie),
            json!({ "change": change, "password": password }),
        ),
    )
    .await;
    (status, body)
}

async fn overview(f: &Fixture, cookie: &str) -> Value {
    let (status, _, body) = call(&f.app, get("/api/v1/filters", cookie)).await;
    assert_eq!(status, StatusCode::OK);
    body
}

fn core_on(overview: &Value, rule: &str) -> Value {
    overview["core"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["id"] == rule)
        .unwrap()["on"]
        .clone()
}

#[tokio::test]
async fn needs_a_session() {
    let f = fixture();
    let (status, _, _) = call(
        &f.app,
        Request::get("/api/v1/filters").body(Body::empty()).unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn strengthening_applies_without_the_password() {
    let f = fixture();
    let c = logged_in(&f).await;
    let before = f.api.engine.current().version;
    let (status, body) = apply(
        &f,
        &c,
        json!({ "op": "add_rule", "query": "ext:exe", "action": "hide" }),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["assessment"]["confirm"], json!(false));
    assert!(f.api.engine.current().version > before);
    assert_eq!(f.api.engine.current().rules.len(), 1);
    let o = overview(&f, &c).await;
    assert_eq!(o["rules"][0]["query"], json!("ext:exe"));
    assert_eq!(o["rules"][0]["action"], json!("hide"));

    // Invalid input is refused with a reason, not stored.
    let (status, body) = apply(
        &f,
        &c,
        json!({ "op": "add_rule", "query": "ext:", "action": "hide" }),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    let (status, _) = apply(
        &f,
        &c,
        json!({ "op": "add_rule", "query": "ext:exe", "action": "explode" }),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(overview(&f, &c).await["rules"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn weakening_needs_the_password_every_time() {
    let f = fixture();
    let c = logged_in(&f).await;
    let change = json!({ "op": "core_switch", "rule": "CS-3a", "on": false });

    // Preview: the warning, nothing applied, no password asked.
    let (status, _, body) = call(
        &f.app,
        post(
            "/api/v1/filters/preview",
            Some(&c),
            json!({ "change": change }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["weakening"], json!(true));
    assert_eq!(body["confirm"], json!(true));
    let warning = body["warning"].as_str().unwrap();
    assert!(
        warning.contains("CS-3a") && warning.contains("Safer"),
        "{warning}"
    );
    assert_eq!(core_on(&overview(&f, &c).await, "CS-3a"), json!(true));

    let (status, body) = apply(&f, &c, change.clone(), None).await;
    assert_eq!(status, StatusCode::PRECONDITION_REQUIRED);
    assert_eq!(body["assessment"]["warning"].as_str(), Some(warning));
    let (status, _) = apply(&f, &c, change.clone(), Some("wrong password")).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, body) = apply(&f, &c, change.clone(), Some(PASSWORD)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(core_on(&overview(&f, &c).await, "CS-3a"), json!(false));

    // Switching it back on strengthens: no password.
    let (status, _) = apply(
        &f,
        &c,
        json!({ "op": "core_switch", "rule": "CS-3a", "on": true }),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // The next weakening change asks again.
    let (status, _) = apply(&f, &c, change, None).await;
    assert_eq!(status, StatusCode::PRECONDITION_REQUIRED);
}

#[tokio::test]
async fn the_floor_is_locked() {
    let f = fixture();
    let c = logged_in(&f).await;
    for rule in ["CS-1a", "CS-1b", "CS-8a"] {
        let change = json!({ "op": "core_switch", "rule": rule, "on": false });
        let (status, _) = apply(&f, &c, change, Some(PASSWORD)).await;
        assert_eq!(status, StatusCode::CONFLICT, "{rule}");
    }
    // No exception to the floor, with or without the password.
    let (status, body) = apply(
        &f,
        &c,
        json!({ "op": "add_exception", "rule": "CS-1a", "hashes": ["aa".repeat(20)] }),
        Some(PASSWORD),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    // A keyword override can't skip it either: the engine never lets one
    // reach the floor, so the override is accepted with its warning.
    let (status, body) = apply(
        &f,
        &c,
        json!({ "op": "add_override", "keyword": "harmless", "scope": "type:movie" }),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::PRECONDITION_REQUIRED);
    let w = body["assessment"]["warning"].as_str().unwrap();
    assert!(
        w.contains("isn't locked") && w.contains("Neutralize term"),
        "{w}"
    );
    assert!(body["assessment"]["neutralize"].is_object());
}

#[tokio::test]
async fn undo_goes_back_one_step() {
    let f = fixture();
    let c = logged_in(&f).await;
    let (_, body) = apply(
        &f,
        &c,
        json!({ "op": "add_rule", "query": "ext:scr", "action": "drop" }),
        None,
    )
    .await;
    let id = body["id"].as_i64().unwrap();
    let (status, _) = apply(&f, &c, json!({ "op": "remove_rule", "id": id }), None).await;
    assert_eq!(status, StatusCode::PRECONDITION_REQUIRED);
    let (status, _) = apply(
        &f,
        &c,
        json!({ "op": "remove_rule", "id": id }),
        Some(PASSWORD),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(f.api.engine.current().rules.is_empty());

    // A refused change leaves the Undo step alone.
    let (status, _) = apply(
        &f,
        &c,
        json!({ "op": "remove_rule", "id": id + 100 }),
        Some(PASSWORD),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // Undo can also weaken (it reverts whatever came since): password.
    let (status, _) = apply(&f, &c, json!({ "op": "undo" }), None).await;
    assert_eq!(status, StatusCode::PRECONDITION_REQUIRED);
    let (status, body) = apply(&f, &c, json!({ "op": "undo" }), Some(PASSWORD)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let o = overview(&f, &c).await;
    assert_eq!(o["rules"][0]["query"], json!("ext:scr"));
    assert_eq!(o["rules"][0]["action"], json!("drop"));
    assert_eq!(f.api.engine.current().rules.len(), 1);

    // Undo again redoes the removal.
    let (status, _) = apply(&f, &c, json!({ "op": "undo" }), Some(PASSWORD)).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        overview(&f, &c).await["rules"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn quarantine_review_never_shows_names_unasked() {
    let f = fixture();
    let c = logged_in(&f).await;
    let hash = [0x42u8; 20];
    let sealed = f
        .api
        .sealer
        .as_ref()
        .unwrap()
        .seal("Some.Held.Name.2024")
        .unwrap();
    f.api
        .store
        .lock()
        .unwrap()
        .write(&WriteBatch {
            filtered: &[Filtered {
                hashes: &[hash],
                rule: "CS-3a",
                action: Action::Quarantine.code(),
                held: Some(Held::Quarantine),
                sealed: Some(&sealed),
                evidence: &[],
                at: lodestar_api_now(),
                expires_at: lodestar_api_now() + 86_400,
            }],
            ..WriteBatch::default()
        })
        .unwrap();
    f.api.held.insert(hash);

    let (status, _, q) = call(&f.app, get("/api/v1/filters/queue", &c)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(q["quarantine"][0]["hash"], json!("42".repeat(20)));
    assert_eq!(q["quarantine"][0]["sealed"], json!(true));
    assert!(!q.to_string().contains("Held.Name"), "{q}");
    assert_eq!(q["decisions_30d"][0]["rule"], json!("CS-3a"));
    assert_eq!(q["decisions_30d"][0]["count"], json!(1));

    // Turning its rule off would release it: the preview counts it.
    let (_, _, a) = call(
        &f.app,
        post(
            "/api/v1/filters/preview",
            Some(&c),
            json!({ "change": { "op": "core_switch", "rule": "CS-3a", "on": false } }),
        ),
    )
    .await;
    assert_eq!(a["preview"]["quarantine"], json!(1));
    assert_eq!(a["preview"]["caught_30d"], json!(1));
    assert_eq!(a["preview"]["core_caught_30d"], json!(1));

    // Revealing the name is a confirmed action.
    let reveal = json!({ "op": "reveal_name", "hash": "42".repeat(20) });
    let (status, _) = apply(&f, &c, reveal.clone(), None).await;
    assert_eq!(status, StatusCode::PRECONDITION_REQUIRED);
    let (status, body) = apply(&f, &c, reveal, Some(PASSWORD)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["name"], json!("Some.Held.Name.2024"));

    // Release: out of the queue and out of the held set.
    let release = json!({ "op": "release", "hash": "42".repeat(20) });
    let (status, body) = apply(&f, &c, release.clone(), Some(PASSWORD)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(!f.api.held.contains(&hash));
    let (_, _, q) = call(&f.app, get("/api/v1/filters/queue", &c)).await;
    assert!(q["quarantine"].as_array().unwrap().is_empty());
    let (status, _) = apply(&f, &c, release, Some(PASSWORD)).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn reported_hashes_join_cs1_and_floor_lists_need_the_password() {
    let f = fixture();
    let c = logged_in(&f).await;
    // A stored torrent, reported: refused from now on and deleted.
    let raw = to_vec(&OwnedValue::Dict(
        [
            (b"length".to_vec(), OwnedValue::Int(5)),
            (b"name".to_vec(), OwnedValue::Bytes(b"x".to_vec())),
            (b"piece length".to_vec(), OwnedValue::Int(16_384)),
            (b"pieces".to_vec(), OwnedValue::Bytes(vec![0; 20])),
        ]
        .into_iter()
        .collect(),
    ));
    let info = Info::parse(&raw).unwrap();
    let hash = info.primary_hash();
    let id = f
        .api
        .store
        .lock()
        .unwrap()
        .insert(&[NewTorrent {
            info: &info,
            raw_info: Some(&raw),
            discovered_at: 1_700_000_000,
            metadata_at: 1_700_000_000,
            extra_flags: 0,
            source: SourceId::new(0).unwrap(),
        }])
        .unwrap()[0]
        .id();
    let h: String = hash.0.iter().map(|b| format!("{b:02x}")).collect();
    let (status, body) = apply(&f, &c, json!({ "op": "report_abuse", "hashes": [h] }), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["deleted"], json!(1));
    assert_eq!(overview(&f, &c).await["cs1_entries"], json!(1));
    assert!(f.api.engine.current().blocked_hash(&[hash.0]).is_some());
    assert!(f.api.store.lock().unwrap().torrent(id).unwrap().is_none());

    let sub = json!({
        "op": "subscribe", "url": "https://example.org/hashes.txt",
        "kind": "content", "floor": true,
    });
    let (status, body) = apply(&f, &c, sub.clone(), None).await;
    assert_eq!(status, StatusCode::PRECONDITION_REQUIRED);
    assert!(
        body["assessment"]["warning"]
            .as_str()
            .unwrap()
            .contains("locked floor")
    );
    let (status, _) = apply(&f, &c, sub, Some(PASSWORD)).await;
    assert_eq!(status, StatusCode::OK);
    // An ordinary list is a strengthening change.
    let (status, _) = apply(
        &f,
        &c,
        json!({ "op": "subscribe", "url": "https://example.org/ips.p2p", "kind": "ip" }),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let subs = overview(&f, &c).await["subscriptions"].clone();
    assert_eq!(subs.as_array().unwrap().len(), 2);
    // Only content lists can join the floor.
    let (status, _) = apply(
        &f,
        &c,
        json!({ "op": "subscribe", "url": "https://example.org/x", "kind": "ip", "floor": true }),
        Some(PASSWORD),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

fn lodestar_api_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

#[tokio::test]
async fn false_positives_become_exceptions_and_hashed_fixtures() {
    let f = fixture();
    let c = logged_in(&f).await;
    let hash = [0x24u8; 20];
    let sealed = f
        .api
        .sealer
        .as_ref()
        .unwrap()
        .seal("Harmless.Documentary.2021")
        .unwrap();
    f.api
        .store
        .lock()
        .unwrap()
        .write(&WriteBatch {
            filtered: &[Filtered {
                hashes: &[hash],
                rule: "CS-3a",
                action: Action::Quarantine.code(),
                held: Some(Held::Quarantine),
                sealed: Some(&sealed),
                evidence: &[],
                at: lodestar_api_now(),
                expires_at: lodestar_api_now() + 86_400,
            }],
            ..WriteBatch::default()
        })
        .unwrap();
    f.api.held.insert(hash);

    let change = json!({ "op": "false_positive", "hash": "24".repeat(20) });
    let (status, body) = apply(&f, &c, change.clone(), None).await;
    assert_eq!(status, StatusCode::PRECONDITION_REQUIRED);
    let w = body["assessment"]["warning"].as_str().unwrap();
    assert!(w.contains("that rule only"), "{w}");
    assert_eq!(body["assessment"]["preview"]["quarantine"], json!(1));
    let (status, body) = apply(&f, &c, change.clone(), Some(PASSWORD)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(!f.api.held.contains(&hash));
    let o = overview(&f, &c).await;
    assert_eq!(o["exceptions"][0]["rule"], json!("CS-3a"));
    assert_eq!(o["exceptions"][0]["hashes"], json!(["24".repeat(20)]));
    assert_eq!(o["quarantine"], json!(0));
    // Gone from the queue: a second report has nothing to act on.
    let (status, _) = apply(&f, &c, change, Some(PASSWORD)).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // The example is kept as hashed tokens only.
    {
        let store = f.api.store.lock().unwrap();
        let stored = store.fixtures("CS-3a").unwrap();
        assert_eq!(stored.len(), 1);
        assert!(!stored[0].1, "must not match");
        let text = String::from_utf8_lossy(&stored[0].2).to_lowercase();
        assert!(!text.contains("harmless") && !text.contains("documentary"));
        assert!(lodestar_filter::Fixture::decode(&stored[0].2).is_some());
    }
    // A must-match example the rule doesn't catch is reported failing.
    let miss = lodestar_filter::Fixture::of("plain words", &[], ContentType::Other);
    f.api
        .store
        .lock()
        .unwrap()
        .add_fixture("CS-3a", true, &miss.encode())
        .unwrap();
    let (status, _, fx) = call(&f.app, get("/api/v1/filters/fixtures", &c)).await;
    assert_eq!(status, StatusCode::OK);
    let cs3a = fx["core"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["rule"] == "CS-3a")
        .unwrap()
        .clone();
    assert_eq!(
        cs3a,
        json!({ "rule": "CS-3a", "must_match": 1, "must_not_match": 1, "failing": 1 })
    );
}

#[tokio::test]
async fn rule_tests_try_a_draft_on_examples_without_storing_them() {
    let f = fixture();
    let c = logged_in(&f).await;
    apply(
        &f,
        &c,
        json!({ "op": "add_rule", "query": "ext:scr", "action": "drop" }),
        None,
    )
    .await;
    let samples = json!([
        { "name": "Tool.Pack", "files": [
            { "path": "setup.exe", "size": 50_000_000 },
            { "path": "readme.txt", "size": 2_000 },
        ] },
        { "name": "Some.Movie.2020", "files": ["movie.mkv", "trailer.exe"], "type": "movie" },
        { "name": "Screens", "files": ["saver.scr"] },
    ]);
    let (status, _, body) = call(
        &f.app,
        post(
            "/api/v1/filters/test",
            Some(&c),
            json!({ "query": "ext:exe", "unless": "type:movie", "samples": samples }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let r = body["results"].as_array().unwrap();
    assert_eq!(r[0]["draft"], json!(true));
    assert_eq!(r[1]["draft"], json!(false), "unless: type:movie");
    assert_eq!(r[2]["draft"], json!(false));
    // The current rules decide too: U-1 drops the screensaver.
    assert_eq!(r[0]["decision"], Value::Null);
    assert_eq!(r[2]["decision"]["rule"], json!("U-1"));
    assert_eq!(r[2]["decision"]["action"], json!("drop"));
    // Without a draft, only the current rules.
    let (status, _, body) = call(
        &f.app,
        post(
            "/api/v1/filters/test",
            Some(&c),
            json!({ "samples": [{ "name": "x", "files": ["a.scr"] }] }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["results"][0]["draft"], Value::Null);
    // Bad input is refused.
    let (status, _, _) = call(
        &f.app,
        post(
            "/api/v1/filters/test",
            Some(&c),
            json!({ "query": "ext:", "samples": [] }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let many: Vec<Value> = (0..51).map(|_| json!({ "name": "x" })).collect();
    let (status, _, _) = call(
        &f.app,
        post("/api/v1/filters/test", Some(&c), json!({ "samples": many })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    // Nothing was stored.
    assert!(
        f.api
            .store
            .lock()
            .unwrap()
            .fixtures("U-1")
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn classification_corrections_reclassify_the_stored_torrent() {
    let f = fixture();
    let c = logged_in(&f).await;
    let raw = to_vec(&OwnedValue::Dict(
        [
            (b"length".to_vec(), OwnedValue::Int(8 << 30)),
            (
                b"name".to_vec(),
                OwnedValue::Bytes(b"The.Matrix.1999.1080p.BluRay.x264-GRP.mkv".to_vec()),
            ),
            (b"piece length".to_vec(), OwnedValue::Int(1 << 20)),
            (b"pieces".to_vec(), OwnedValue::Bytes(vec![0; 20])),
        ]
        .into_iter()
        .collect(),
    ));
    let info = Info::parse(&raw).unwrap();
    let id = f
        .api
        .store
        .lock()
        .unwrap()
        .insert(&[NewTorrent {
            info: &info,
            raw_info: Some(&raw),
            discovered_at: 1_700_000_000,
            metadata_at: 1_700_000_000,
            extra_flags: 0,
            source: SourceId::new(0).unwrap(),
        }])
        .unwrap()[0]
        .id();
    let h: String = info
        .primary_hash()
        .0
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let correct = |body: Value| post("/api/v1/classify/correct", Some(&c), body);

    let (status, _, body) = call(&f.app, correct(json!({ "hash": h, "adult": true }))).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["type"], json!("adult"));
    assert_eq!(body["adult_kind"], json!("movie"));
    {
        let store = f.api.store.lock().unwrap();
        let e = store.enrichment(id).unwrap().unwrap();
        assert_eq!(
            ContentType::from_code(e.content_type),
            Some(ContentType::Adult)
        );
        // Logged for the indexer.
        assert!(
            store
                .reindex_after(0, 100)
                .unwrap()
                .iter()
                .any(|(_, t)| *t == id)
        );
    }
    // Clearing the correction classifies it as before.
    let (status, _, body) = call(&f.app, correct(json!({ "hash": h, "adult": null }))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["type"], json!("movie"));
    assert_eq!(f.api.store.lock().unwrap().correction(id).unwrap(), None);

    let (status, _, _) = call(
        &f.app,
        correct(json!({ "hash": "00".repeat(20), "adult": false })),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}
