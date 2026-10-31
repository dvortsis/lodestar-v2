//! The filters API through the router (§20.7, §20.11): strengthening
//! changes apply at once, weakening ones need the password every time,
//! the floor stays locked, and Undo goes back one step.

use std::sync::atomic::AtomicBool;
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
use lodestar_search::SearchIndex;
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
    fixture_with(None)
}

fn fixture_with(index: Option<SearchIndex>) -> Fixture {
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
        index: index.map(Into::into),
        reenrich: Arc::new(AtomicBool::new(false)),
        jobs: Default::default(),
        quarantine_secs: 30 * 86_400,
        shutdown: Default::default(),
    });
    let auth = Arc::new(Auth::load(&dir.path().join("auth.json"), false).unwrap());
    let app = Router::new().nest(
        "/api/v1",
        router(ApiState {
            auth: auth.clone(),
            index: None,
            reader: Arc::new(Mutex::new(reader)),
            filters: Some(api.clone()),
            hot: None,
            events: None,
            settings: None,
            ops: None,
            user_data: None,
            adult: Default::default(),
            insights: None,
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

/// A release checks the store before intake may fetch again
/// (`2026-10-31-B-held-exact.md`): a hash two rules hold stays refused
/// when one is turned off, and goes when the other is too. The held set
/// equals what a restart loads from the store throughout.
#[tokio::test]
async fn releasing_one_of_two_holds_keeps_the_hash_refused() {
    let f = fixture();
    let c = logged_in(&f).await;
    let hash = [0x43u8; 20];
    let sealed = f.api.sealer.as_ref().unwrap().seal("Held.Twice").unwrap();
    let now = lodestar_api_now();
    let hold = |rule, held, sealed| Filtered {
        hashes: std::slice::from_ref(&hash),
        rule,
        action: Action::Quarantine.code(),
        held: Some(held),
        sealed,
        evidence: &[],
        at: now,
        expires_at: now + 86_400,
        adult_hold: false,
    };
    f.api
        .store
        .lock()
        .unwrap()
        .write(&WriteBatch {
            filtered: &[
                hold("CS-3a", Held::Quarantine, Some(&sealed[..])),
                hold("CS-3b", Held::Tombstone, None),
            ],
            ..WriteBatch::default()
        })
        .unwrap();
    f.api.held.insert(hash);
    let agrees = || {
        let held: std::collections::HashSet<_> = f
            .api
            .store
            .lock()
            .unwrap()
            .filter_state()
            .unwrap()
            .held
            .into_iter()
            .map(|(h, _)| h)
            .collect();
        assert_eq!(f.api.held.snapshot(), held);
    };
    for (rule, still) in [("CS-3a", true), ("CS-3b", false)] {
        let change = json!({ "op": "core_switch", "rule": rule, "on": false });
        let (status, body) = apply(&f, &c, change, Some(PASSWORD)).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(f.api.held.contains(&hash), still, "{rule} off");
        agrees();
    }
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
                adult_hold: false,
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
                adult_hold: false,
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

#[tokio::test]
async fn every_filters_route_needs_a_session_and_errors_use_the_envelope() {
    let f = fixture();
    // Without a session, every filters route is a 401 in the envelope,
    // whatever the method.
    for (method, uri) in [
        ("GET", "/api/v1/filters"),
        ("GET", "/api/v1/filters/queue"),
        ("GET", "/api/v1/filters/fixtures"),
        ("POST", "/api/v1/filters/test"),
        ("POST", "/api/v1/filters/preview"),
        ("POST", "/api/v1/filters/apply"),
        ("POST", "/api/v1/filters/strictness"),
        ("POST", "/api/v1/classify/correct"),
        ("GET", "/api/v1/classify"),
        ("POST", "/api/v1/classify/rules"),
        ("POST", "/api/v1/classify/rules/remove"),
        ("POST", "/api/v1/filters/preview/jobs"),
        ("GET", "/api/v1/filters/preview/jobs/1"),
        ("POST", "/api/v1/filters/preview/jobs/1/cancel"),
        ("DELETE", "/api/v1/filters/apply"),
    ] {
        let req = Request::builder()
            .method(method)
            .uri(uri)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from("{}"))
            .unwrap();
        let (status, _, body) = call(&f.app, req).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{method} {uri}");
        assert_eq!(body["error"]["code"], json!("login_required"), "{uri}");
    }

    let c = logged_in(&f).await;
    // A refused change: a stable code, a sentence and the field.
    let (status, body) = apply(
        &f,
        &c,
        json!({ "op": "add_rule", "query": "ext:exe", "action": "explode" }),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(
        body["error"],
        json!({
            "code": "unknown_action",
            "message": "The action must be flag, demote, hide, quarantine or drop.",
            "field": "action",
        })
    );
    // A bad draft query keeps its span.
    let (_, body) = apply(
        &f,
        &c,
        json!({ "op": "add_rule", "query": "sise:1", "action": "hide" }),
        None,
    )
    .await;
    assert_eq!(body["error"]["code"], json!("query_syntax"));
    assert_eq!(
        (&body["error"]["start"], &body["error"]["end"]),
        (&json!(0), &json!(5))
    );

    // A weakening change: first the confirmation request, with the
    // assessment beside the error, then a wrong password.
    let change = json!({ "op": "core_switch", "rule": "CS-3a", "on": false });
    let (status, body) = apply(&f, &c, change.clone(), None).await;
    assert_eq!(status, StatusCode::PRECONDITION_REQUIRED);
    assert_eq!(
        (&body["error"]["code"], &body["error"]["field"]),
        (&json!("confirm_required"), &json!("password"))
    );
    assert!(body["assessment"]["warning"].is_string());
    let (status, body) = apply(&f, &c, change, Some("not the password")).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(
        body,
        json!({ "error": {
            "code": "bad_password",
            "message": "That password is wrong.",
            "field": "password",
        } })
    );
}

/// Stores a single-file torrent and returns its id and hash (hex).
fn store_torrent(f: &Fixture, name: &str, size: i64) -> (i64, String) {
    let raw = to_vec(&OwnedValue::Dict(
        [
            (b"length".to_vec(), OwnedValue::Int(size)),
            (
                b"name".to_vec(),
                OwnedValue::Bytes(name.as_bytes().to_vec()),
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
    let hash = info
        .primary_hash()
        .0
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    (id, hash)
}

#[tokio::test]
async fn classification_rules_requeue_matching_torrents_and_corrections_are_fixtures() {
    use std::sync::atomic::Ordering;
    let f = fixture_with(Some(SearchIndex::in_memory().unwrap()));
    let c = logged_in(&f).await;
    let (a, _) = store_torrent(&f, "Holiday.Clips.2023.Lexb.mp4", 900 << 20);
    let (_, _) = store_torrent(&f, "Holiday.Clips.2023.Other.mp4", 900 << 20);
    let (_, mhash) = store_torrent(&f, "The.Matrix.1999.1080p.BluRay.x264-GRP.mkv", 8 << 30);
    {
        let mut store = f.api.store.lock().unwrap();
        enrich_all(&mut store);
        let index = f.api.index.as_ref().unwrap().index();
        let mut w = index.writer(15 << 20).unwrap();
        w.catch_up(&mut store, 10).unwrap();
        w.commit().unwrap();
    }
    let rules = |body: Value| post("/api/v1/classify/rules", Some(&c), body);

    // The query can't test what the rule decides.
    for q in ["type:movie lexb", "adult:scene", "-type:adult"] {
        let (status, _, body) = call(&f.app, rules(json!({ "query": q, "adult": true }))).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{q}: {body}");
        assert_eq!(body["error"]["code"], json!("class_rule_tests_type"), "{q}");
        assert_eq!(body["error"]["field"], json!("query"), "{q}");
    }
    let (status, _, body) = call(&f.app, rules(json!({ "query": "", "adult": true }))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["error"]["code"], json!("class_rule_needs_query"));

    let (status, _, body) = call(&f.app, rules(json!({ "query": "lexb", "adult": true }))).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["queued"], json!(1), "one stored torrent matches");
    let rule = body["id"].as_i64().unwrap();
    assert!(
        f.api.reenrich.swap(false, Ordering::AcqRel),
        "pipeline told"
    );
    assert_eq!(f.api.engine.current().class_rules.len(), 1);
    {
        let store = f.api.store.lock().unwrap();
        assert!(
            store
                .unenriched(0, lodestar_classify::ENRICHMENT_VERSION, 10)
                .unwrap()
                .contains(&a)
        );
    }

    // A per-torrent correction detection disagrees with is listed.
    let (status, _, _) = call(
        &f.app,
        post(
            "/api/v1/classify/correct",
            Some(&c),
            json!({ "hash": mhash, "adult": true }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _, body) = call(&f.app, get("/api/v1/classify", &c)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body["rules"],
        json!([{ "id": rule, "query": "lexb", "adult": true }])
    );
    assert_eq!(body["corrections"], json!(1));
    assert_eq!(body["detection"]["checked"], json!(1));
    assert_eq!(body["detection"]["disagree"], json!(1));
    assert_eq!(body["detection"]["examples"][0]["hash"], json!(mhash));

    // Removing the rule re-queues what it matched.
    let remove = |id: i64| {
        post(
            "/api/v1/classify/rules/remove",
            Some(&c),
            json!({ "id": id }),
        )
    };
    let (status, _, body) = call(&f.app, remove(rule)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["queued"], json!(1));
    assert!(f.api.engine.current().class_rules.is_empty());
    let (status, _, body) = call(&f.app, remove(rule)).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"]["code"], json!("class_rule_not_found"));
}

/// A shutdown during a rule's re-queue leaves it pending: the next start
/// redoes it, so the rule's existing matches end up marked.
#[tokio::test]
async fn a_requeue_cut_short_by_shutdown_is_redone_at_the_next_start() {
    let f = fixture_with(Some(SearchIndex::in_memory().unwrap()));
    let c = logged_in(&f).await;
    let (a, _) = store_torrent(&f, "Holiday.Clips.2023.Lexb.mp4", 900 << 20);
    store_torrent(&f, "The.Matrix.1999.1080p.BluRay.x264-GRP.mkv", 8 << 30);
    {
        let mut store = f.api.store.lock().unwrap();
        enrich_all(&mut store);
        let index = f.api.index.as_ref().unwrap().index();
        let mut w = index.writer(15 << 20).unwrap();
        w.catch_up(&mut store, 10).unwrap();
        w.commit().unwrap();
    }
    let marked = |api: &FiltersApi| {
        api.store
            .lock()
            .unwrap()
            .unenriched(0, lodestar_classify::ENRICHMENT_VERSION, 10)
            .unwrap()
    };
    assert!(marked(&f.api).is_empty());

    // The server is stopping: the rule is stored, its re-queue isn't run.
    f.api.shutdown.cancel();
    let (status, _, body) = call(
        &f.app,
        post(
            "/api/v1/classify/rules",
            Some(&c),
            json!({ "query": "lexb", "adult": true }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
    assert_eq!(body["error"]["code"], json!("server_stopping"));
    assert_eq!(f.api.engine.current().class_rules.len(), 1, "rule kept");
    assert!(marked(&f.api).is_empty(), "nothing marked yet");
    // Stopped again at once on resume: it stays pending.
    assert!(f.api.resume_requeues(0).is_err());
    assert_eq!(pending(&f.api), ["lexb"]);

    // A start with search off can't do it: it stays for one with search on.
    let off = restart(&f, None);
    assert_eq!(off.resume_requeues(0).unwrap(), 0);
    assert_eq!(pending(&off), ["lexb"]);
    drop(off);

    // The next start with search on: a fresh store handle on the same file
    // and a new shutdown token.
    let restarted = restart(&f, f.api.index.clone());
    assert_eq!(restarted.resume_requeues(0).unwrap(), 1);
    assert_eq!(marked(&restarted), vec![a], "the rule's match is marked");
    assert!(
        restarted
            .reenrich
            .load(std::sync::atomic::Ordering::Acquire)
    );
    // Done, so cleared: another start redoes nothing.
    assert_eq!(restarted.resume_requeues(0).unwrap(), 0);
    assert!(pending(&restarted).is_empty());
}

/// A rule added while search is off stays pending, once however often it
/// changes, and the first start with search on marks its existing matches.
#[tokio::test]
async fn a_rule_added_with_search_off_is_requeued_once_search_is_on() {
    let f = fixture();
    let c = logged_in(&f).await;
    let (a, _) = store_torrent(&f, "Holiday.Clips.2023.Lexb.mp4", 900 << 20);
    store_torrent(&f, "The.Matrix.1999.1080p.BluRay.x264-GRP.mkv", 8 << 30);
    enrich_all(&mut f.api.store.lock().unwrap());
    let rules = |body: Value| post("/api/v1/classify/rules", Some(&c), body);
    let (status, _, body) = call(&f.app, rules(json!({ "query": "lexb", "adult": true }))).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["queued"], Value::Null, "no index to search");
    let rule = body["id"].as_i64().unwrap();
    // Removed and added again: still one entry.
    let (status, _, body) = call(
        &f.app,
        post(
            "/api/v1/classify/rules/remove",
            Some(&c),
            json!({ "id": rule }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, _, body) = call(&f.app, rules(json!({ "query": "lexb", "adult": true }))).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(pending(&f.api), ["lexb"]);
    // Starts with search off keep it.
    assert_eq!(f.api.resume_requeues(0).unwrap(), 0);
    assert_eq!(pending(&f.api), ["lexb"]);

    // Search turned on (a restart): a new, caught-up index.
    let index = SearchIndex::in_memory().unwrap();
    {
        let mut store = f.api.store.lock().unwrap();
        let mut w = index.writer(15 << 20).unwrap();
        w.catch_up(&mut store, 10).unwrap();
        w.commit().unwrap();
    }
    let on = restart(&f, Some(index.into()));
    assert_eq!(on.resume_requeues(0).unwrap(), 1);
    let marked = on
        .store
        .lock()
        .unwrap()
        .unenriched(0, lodestar_classify::ENRICHMENT_VERSION, 10)
        .unwrap();
    assert_eq!(marked, vec![a], "the rule's existing match is marked");
    assert!(pending(&on).is_empty());
}

/// The pending re-queues, as stored.
fn pending(api: &FiltersApi) -> Vec<String> {
    api.store
        .lock()
        .unwrap()
        .setting_text("classify.requeue_pending")
        .unwrap()
        .map(|t| serde_json::from_str(&t).unwrap())
        .unwrap_or_default()
}

/// The next start: a fresh store handle on the same file, a new shutdown
/// token, and `index` (`None` with search off).
fn restart(f: &Fixture, index: Option<lodestar_search::SearchHandle>) -> FiltersApi {
    let db = f._dir.path().join("t.db");
    FiltersApi {
        store: Arc::new(Mutex::new(
            Store::open(&db, StoreConfig::default()).unwrap(),
        )),
        engine: f.api.engine.clone(),
        held: f.api.held.clone(),
        sealer: f.api.sealer.clone(),
        ips: f.api.ips.clone(),
        index,
        reenrich: Arc::new(AtomicBool::new(false)),
        jobs: Default::default(),
        quarantine_secs: f.api.quarantine_secs,
        shutdown: Default::default(),
    }
}

/// Enriches every stored torrent, as the pipeline's background pass does.
fn enrich_all(store: &mut Store) {
    let ids = store
        .unenriched(0, lodestar_classify::ENRICHMENT_VERSION, 100)
        .unwrap();
    let mut batch = Vec::new();
    for id in ids {
        let row = store.torrent(id).unwrap().unwrap();
        let tree = store.files(id).unwrap().unwrap();
        let files = lodestar_classify::file_infos(&tree.files, tree.encoding);
        let (e, _) = lodestar_filter::reclassify::enrich_corrected(
            &row.name,
            &files,
            lodestar_classify::Correction::default(),
        );
        batch.push((id, e));
    }
    store.set_enrichments(&batch).unwrap();
}

/// Quarantines `n` torrents under CS-3a with sealed names.
fn quarantine(f: &Fixture, n: u8) {
    let sealer = f.api.sealer.clone().unwrap();
    let sealed: Vec<Vec<u8>> = (0..n)
        .map(|i| sealer.seal(&format!("Held.Name.{i}")).unwrap())
        .collect();
    let hashes: Vec<[u8; 20]> = (0..n).map(|i| [i.wrapping_add(0x60); 20]).collect();
    let filtered: Vec<Filtered<'_>> = (0..n as usize)
        .map(|i| Filtered {
            hashes: std::slice::from_ref(&hashes[i]),
            rule: "CS-3a",
            action: Action::Quarantine.code(),
            held: Some(Held::Quarantine),
            sealed: Some(&sealed[i]),
            evidence: &[],
            at: lodestar_api_now(),
            expires_at: lodestar_api_now() + 86_400,
            adult_hold: false,
        })
        .collect();
    f.api
        .store
        .lock()
        .unwrap()
        .write(&WriteBatch {
            filtered: &filtered,
            ..WriteBatch::default()
        })
        .unwrap();
}

#[tokio::test]
async fn previews_run_as_background_jobs_with_progress() {
    let f = fixture();
    let c = logged_in(&f).await;
    quarantine(&f, 20);
    // Releases two of the twenty by exact hash.
    let change = json!({
        "op": "add_exception", "rule": "CS-3a",
        "hashes": ["60".repeat(20), "61".repeat(20)],
    });
    let (status, _, body) = call(
        &f.app,
        post(
            "/api/v1/filters/preview/jobs",
            Some(&c),
            json!({ "change": change }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    let job = body["job"].as_u64().unwrap();
    let uri = format!("/api/v1/filters/preview/jobs/{job}");
    let mut done = Value::Null;
    let mut last = Value::Null;
    for _ in 0..1500 {
        let (status, _, body) = call(&f.app, get(&uri, &c)).await;
        assert_eq!(status, StatusCode::OK);
        if body["state"] == "done" {
            done = body;
            break;
        }
        assert_eq!(body["state"], json!("running"));
        last = body;
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert!(!done.is_null(), "the job never finished; last saw {last}");
    assert_eq!(done["progress"]["phase"], json!("done"));
    assert_eq!(done["assessment"]["preview"]["quarantine"], json!(2));
    assert_eq!(done["assessment"]["weakening"], json!(true));
    // The same as the inline preview.
    let (_, _, inline) = call(
        &f.app,
        post(
            "/api/v1/filters/preview",
            Some(&c),
            json!({ "change": change }),
        ),
    )
    .await;
    assert_eq!(done["assessment"], inline);

    // A failing preview reports its error.
    let bad = json!({ "op": "core_switch", "rule": "CS-1a", "on": false });
    let (_, _, body) = call(
        &f.app,
        post(
            "/api/v1/filters/preview/jobs",
            Some(&c),
            json!({ "change": bad }),
        ),
    )
    .await;
    let uri = format!("/api/v1/filters/preview/jobs/{}", body["job"]);
    let mut state = Value::Null;
    for _ in 0..1500 {
        let (_, _, body) = call(&f.app, get(&uri, &c)).await;
        if body["state"] != "running" {
            state = body;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert!(!state.is_null(), "the job never left running");
    assert_eq!(state["state"], json!("failed"));
    assert_eq!(state["status"], json!(409));
    assert!(state["error"]["code"].is_string(), "{state}");
    assert!(state["error"]["message"].is_string(), "{state}");

    // Unknown jobs, and cancelling.
    let (status, _, body) = call(&f.app, get("/api/v1/filters/preview/jobs/999", &c)).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"]["code"], json!("preview_not_found"));
    let (status, _, _) = call(
        &f.app,
        post(
            &format!("/api/v1/filters/preview/jobs/{job}/cancel"),
            Some(&c),
            json!({}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn a_cancelled_preview_stops() {
    let f = fixture();
    quarantine(&f, 5);
    let change: lodestar_api::filters::Change = serde_json::from_value(json!({
        "op": "add_exception", "rule": "CS-3a", "hashes": ["60".repeat(20)],
    }))
    .unwrap();
    let progress = lodestar_api::filters::Progress::default();
    progress.cancel();
    let err = f
        .api
        .assess_tracked(&change, lodestar_api_now(), &progress)
        .unwrap_err();
    assert!(format!("{err:?}").contains("cancelled"), "{err:?}");
    // Not cancelled: it completes.
    let a = f
        .api
        .assess_tracked(
            &change,
            lodestar_api_now(),
            &lodestar_api::filters::Progress::default(),
        )
        .unwrap();
    assert_eq!(a.preview.quarantine, 1);
}

/// Owner's decision §2.7: what a config file dropped stays in the floor,
/// waiting; removing it needs the effect warning and the password.
#[tokio::test]
async fn floor_entries_a_file_dropped_leave_only_with_the_password() {
    let f = fixture();
    let cookie = logged_in(&f).await;
    let (a, b) = ([0xa1u8; 20], [0xb2u8; 20]);
    let hex = |h: &[u8; 20]| h.iter().map(|b| format!("{b:02x}")).collect::<String>();
    {
        let mut s = f.api.store.lock().unwrap();
        s.sync_floor_file("/config/blocked.txt", &[a, b], 1)
            .unwrap();
        // `b` deleted from the file, then a restart.
        assert_eq!(
            s.sync_floor_file("/config/blocked.txt", &[a], 2).unwrap(),
            1
        );
        let state = s.filter_state().unwrap();
        f.api.engine.update(|c| {
            for (h, _) in &state.cs_hashes {
                c.cs.add_hash(*h, lodestar_filter::Origin::Imported);
            }
        });
    }
    assert!(f.api.engine.current().blocked_hash(&[b]).is_some());
    let o = overview(&f, &cookie).await;
    let p = &o["pending_floor_removals"];
    assert_eq!(p.as_array().unwrap().len(), 1, "{p}");
    assert_eq!(p[0]["source"], "file:/config/blocked.txt");
    assert_eq!(p[0]["kind"], "file");
    assert_eq!(p[0]["entries"], 1);
    assert_eq!(p[0]["hashes"][0], hex(&b));

    let change = json!({ "op": "resolve_floor_removal", "source": "file:/config/blocked.txt",
        "remove": true });
    let (status, _, body) = call(
        &f.app,
        post(
            "/api/v1/filters/preview",
            Some(&cookie),
            json!({ "change": change }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["weakening"], true);
    assert_eq!(body["preview"]["removed_from_floor"], 1);
    assert_eq!(body["floor_entries"], json!([hex(&b)]));
    assert!(
        body["warning"]
            .as_str()
            .unwrap()
            .contains("/config/blocked.txt no longer lists them"),
        "{body}"
    );
    let (status, body) = apply(&f, &cookie, change.clone(), None).await;
    assert_eq!(status, StatusCode::PRECONDITION_REQUIRED, "{body}");
    assert!(f.api.engine.current().blocked_hash(&[b]).is_some());
    // The password confirms the entries the warning listed, so a removal
    // names them; without them it's refused before the password is read.
    let (status, body) = apply(&f, &cookie, change.clone(), Some(PASSWORD)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["error"]["code"], "floor_removal_needs_hashes");
    assert!(f.api.engine.current().blocked_hash(&[b]).is_some());
    let confirm = json!({ "op": "resolve_floor_removal", "source": "file:/config/blocked.txt",
        "hashes": [hex(&b)], "remove": true });
    // A wrong password changes nothing: still blocked, still waiting.
    let (status, body) = apply(&f, &cookie, confirm.clone(), Some("wrong password")).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
    assert!(f.api.engine.current().blocked_hash(&[b]).is_some());
    let o = overview(&f, &cookie).await;
    assert_eq!(o["pending_floor_removals"][0]["entries"], 1);
    let (status, body) = apply(&f, &cookie, confirm.clone(), Some(PASSWORD)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["settled"], 1);
    assert_eq!(body["assessment"]["floor_entries"], json!([hex(&b)]));
    let e = f.api.engine.current();
    assert!(e.blocked_hash(&[b]).is_none());
    assert!(e.blocked_hash(&[a]).is_some(), "the file still lists it");
    let o = overview(&f, &cookie).await;
    assert!(o["pending_floor_removals"].as_array().unwrap().is_empty());

    // Nothing left to settle.
    let (status, _) = apply(&f, &cookie, confirm, Some(PASSWORD)).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    // At most 10,000 named at a time.
    let many: Vec<String> = (0..10_001u32).map(|i| format!("{i:040x}")).collect();
    let (status, body) = apply(
        &f,
        &cookie,
        json!({ "op": "resolve_floor_removal", "source": "file:/config/blocked.txt",
            "hashes": many }),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["code"], "too_many_hashes");
    let (status, _) = apply(
        &f,
        &cookie,
        json!({ "op": "resolve_floor_removal", "source": "elsewhere" }),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    // The entries a warning counted are what a confirmation settles,
    // even if more are pending by then.
    let c = [0xc3u8; 20];
    let sync = |hs: &[[u8; 20]], at| {
        let mut s = f.api.store.lock().unwrap();
        s.sync_floor_file("/config/blocked.txt", hs, at).unwrap();
        for h in hs {
            let h = *h;
            f.api
                .engine
                .update(|e| e.cs.add_hash(h, lodestar_filter::Origin::Imported));
        }
    };
    sync(&[a, c], 3);
    sync(&[c], 4);
    let all = json!({ "op": "resolve_floor_removal", "source": "file:/config/blocked.txt",
        "remove": true });
    let (_, body) = apply(&f, &cookie, all, None).await;
    let shown = body["assessment"]["floor_entries"].clone();
    assert_eq!(shown, json!([hex(&a)]), "{body}");
    sync(&[], 5);
    let confirm = json!({ "op": "resolve_floor_removal", "source": "file:/config/blocked.txt",
        "hashes": shown, "remove": true });
    let (status, body) = apply(&f, &cookie, confirm, Some(PASSWORD)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["settled"], 1);
    assert!(
        f.api.engine.current().blocked_hash(&[c]).is_some(),
        "not in the warning"
    );
    let o = overview(&f, &cookie).await;
    assert_eq!(o["pending_floor_removals"][0]["hashes"], json!([hex(&c)]));
}

/// An entry another config file lists stays blocked when it's removed
/// through the file that dropped it: in the store and in the running
/// engine (§20.7).
#[tokio::test]
async fn removing_an_entry_another_file_lists_leaves_it_blocked() {
    let f = fixture();
    let cookie = logged_in(&f).await;
    let h = [0xd4u8; 20];
    {
        let mut s = f.api.store.lock().unwrap();
        // Start 1: a.txt lists it. Start 2: a.txt doesn't, b.txt does.
        s.sync_floor_file("/config/a.txt", &[h], 1).unwrap();
        assert_eq!(s.sync_floor_file("/config/a.txt", &[], 2).unwrap(), 1);
        s.sync_floor_file("/config/b.txt", &[h], 2).unwrap();
        let state = s.filter_state().unwrap();
        f.api.engine.update(|c| {
            for (h, _) in &state.cs_hashes {
                c.cs.add_hash(*h, lodestar_filter::Origin::Imported);
            }
        });
    }
    let hex = |h: &[u8; 20]| h.iter().map(|b| format!("{b:02x}")).collect::<String>();
    let change = json!({ "op": "resolve_floor_removal", "source": "file:/config/a.txt",
        "hashes": [hex(&h)], "remove": true });
    let (status, body) = apply(&f, &cookie, change, Some(PASSWORD)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["settled"], 1);
    let kept = f
        .api
        .store
        .lock()
        .unwrap()
        .filter_state()
        .unwrap()
        .cs_hashes
        .iter()
        .any(|(x, _)| *x == h);
    assert!(kept, "the store keeps it");
    assert!(
        f.api.engine.current().blocked_hash(&[h]).is_some(),
        "the engine still blocks it"
    );
}

/// Keeping without naming entries settles what the assessment counted,
/// even if more are pending by the time it's applied.
#[tokio::test]
async fn keeping_settles_what_was_counted() {
    let f = fixture();
    let (a, b) = ([0xa1u8; 20], [0xb2u8; 20]);
    let sync = |hs: &[[u8; 20]], at| {
        f.api
            .store
            .lock()
            .unwrap()
            .sync_floor_file("/config/blocked.txt", hs, at)
            .unwrap()
    };
    sync(&[a, b], 1);
    sync(&[b], 2);
    let change: lodestar_api::filters::Change = serde_json::from_value(json!({
        "op": "resolve_floor_removal", "source": "file:/config/blocked.txt" }))
    .unwrap();
    let now = lodestar_api_now();
    let assessment = f.api.assess(&change, now).unwrap();
    assert_eq!(assessment.floor_entries.as_ref().unwrap().len(), 1);
    // `b` drops out too before the change runs.
    sync(&[], 3);
    let done = f.api.apply(&change, assessment, now).unwrap();
    assert_eq!(done.settled, Some(1));
    let pending = f
        .api
        .store
        .lock()
        .unwrap()
        .pending_floor_of(&lodestar_store::FloorSource::File(
            "/config/blocked.txt".into(),
        ))
        .unwrap();
    assert_eq!(pending, [b], "not counted, so still waiting");
}

/// A floor-marked list's upstream removals wait too. Keeping them is
/// strengthening, so it needs no password, and they stay blocked.
#[tokio::test]
async fn floor_entries_a_list_dropped_can_be_kept_without_the_password() {
    let f = fixture();
    let cookie = logged_in(&f).await;
    let (a, b) = ([0xa1u8; 20], [0xb2u8; 20]);
    let id = {
        let mut s = f.api.store.lock().unwrap();
        let id = s
            .add_subscription(&lodestar_store::NewSubscription {
                url: "https://example.org/cs.txt",
                name: "Vetted list",
                kind: 0,
                format: Some("hashes"),
                action: Action::Drop.code(),
                floor: true,
                interval_secs: 86_400,
            })
            .unwrap();
        for data in [[a, b].concat(), a.to_vec()] {
            s.record_refresh(
                id,
                1,
                &lodestar_store::Refresh::Updated {
                    data: &data,
                    entries: (data.len() / 20) as i64,
                    etag: None,
                    last_modified: None,
                },
            )
            .unwrap();
        }
        lodestar_filter::lists::apply(&s, &f.api.engine, &f.api.ips).unwrap();
        id
    };
    assert!(f.api.engine.current().blocked_hash(&[b]).is_some());
    let o = overview(&f, &cookie).await;
    let p = &o["pending_floor_removals"][0];
    assert_eq!(p["source"], format!("list:{id}"));
    assert_eq!(p["name"], "Vetted list");
    let keep = json!({ "op": "resolve_floor_removal", "source": format!("list:{id}") });
    let (status, body) = apply(&f, &cookie, keep, None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(f.api.engine.current().blocked_hash(&[b]).is_some());
    let o = overview(&f, &cookie).await;
    assert!(o["pending_floor_removals"].as_array().unwrap().is_empty());
}

/// Removing a floor list's upstream removals is weakening: the password,
/// and nothing changes on a wrong one.
#[tokio::test]
async fn floor_entries_a_list_dropped_leave_only_with_the_password() {
    let f = fixture();
    let cookie = logged_in(&f).await;
    let (a, b, c) = ([0xa1u8; 20], [0xb2u8; 20], [0xc3u8; 20]);
    let id = {
        let mut s = f.api.store.lock().unwrap();
        let id = s
            .add_subscription(&lodestar_store::NewSubscription {
                url: "https://example.org/cs.txt",
                name: "Vetted list",
                kind: 0,
                format: Some("hashes"),
                action: Action::Drop.code(),
                floor: true,
                interval_secs: 86_400,
            })
            .unwrap();
        // The publisher drops `b` and `c`.
        for data in [[a, b, c].concat(), a.to_vec()] {
            s.record_refresh(
                id,
                1,
                &lodestar_store::Refresh::Updated {
                    data: &data,
                    entries: (data.len() / 20) as i64,
                    etag: None,
                    last_modified: None,
                },
            )
            .unwrap();
        }
        lodestar_filter::lists::apply(&s, &f.api.engine, &f.api.ips).unwrap();
        id
    };
    let blocked = |h: &[u8; 20]| f.api.engine.current().blocked_hash(&[*h]).is_some();
    assert!(blocked(&b) && blocked(&c));
    let hex = |h: &[u8; 20]| h.iter().map(|b| format!("{b:02x}")).collect::<String>();
    // Only `b`, by hash.
    let change = json!({ "op": "resolve_floor_removal", "source": format!("list:{id}"),
        "hashes": [hex(&b)], "remove": true });
    let (status, body) = apply(&f, &cookie, change.clone(), None).await;
    assert_eq!(status, StatusCode::PRECONDITION_REQUIRED, "{body}");
    let (status, body) = apply(&f, &cookie, change.clone(), Some("wrong password")).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
    assert!(
        blocked(&b) && blocked(&c),
        "a wrong password changes nothing"
    );
    assert_eq!(
        overview(&f, &cookie).await["pending_floor_removals"][0]["entries"],
        2
    );
    let (status, body) = apply(&f, &cookie, change, Some(PASSWORD)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["settled"], 1);
    assert!(!blocked(&b), "removed");
    assert!(blocked(&c), "still waiting");
    assert!(blocked(&a), "the list still has it");
    let o = overview(&f, &cookie).await;
    assert_eq!(o["pending_floor_removals"][0]["entries"], 1);
    assert_eq!(o["pending_floor_removals"][0]["hashes"][0], hex(&c));
}

/// Owner's decision §2.9: the strictness of a candidate set of lists, for
/// the wizard (catalog lists, nothing fetched) and Settings (subscribed
/// lists, measured from their data). Nothing is stored or connected.
#[tokio::test]
async fn strictness_of_candidate_lists() {
    let f = fixture();
    let cookie = logged_in(&f).await;
    let assess = |body: Value| {
        let (app, cookie) = (f.app.clone(), cookie.clone());
        async move {
            call(
                &app,
                post("/api/v1/filters/strictness", Some(&cookie), body),
            )
            .await
        }
    };
    let (status, _, s) = assess(json!({ "catalog": ["bt-blocklists"] })).await;
    assert_eq!(status, StatusCode::OK, "{s}");
    assert_eq!(s["level"], "severe");
    assert!((s["ipv4_share"].as_f64().unwrap() - 0.49).abs() < 0.01);
    assert_eq!(s["estimate"], true);
    assert_eq!(s["lists"][0]["measured"], true);
    assert!(
        s["text"]
            .as_str()
            .unwrap()
            .contains("out of the crawler's reach")
    );
    assert!(
        s["categories"]
            .as_array()
            .unwrap()
            .contains(&json!("anti_p2p"))
    );
    let (_, _, s) = assess(json!({ "catalog": ["firehol-level1"] })).await;
    assert_eq!(s["level"], "none");
    let (status, _, _) = assess(json!({ "catalog": ["nope"] })).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(
        f.api
            .store
            .lock()
            .unwrap()
            .subscriptions()
            .unwrap()
            .is_empty(),
        "assessing connects nothing"
    );

    // Subscribed lists are measured from their data: a fetched IP list
    // covering 64 /8s of public space, a content list, and a floor list.
    let ids = {
        let mut st = f.api.store.lock().unwrap();
        let mut add = |url: &str, kind: u8, floor: bool, data: Vec<u8>| {
            let id = st
                .add_subscription(&lodestar_store::NewSubscription {
                    url,
                    name: url,
                    kind,
                    format: None,
                    action: Action::Drop.code(),
                    floor,
                    interval_secs: 86_400,
                })
                .unwrap();
            st.record_refresh(
                id,
                1,
                &lodestar_store::Refresh::Updated {
                    data: &data,
                    entries: 1,
                    etag: None,
                    last_modified: None,
                },
            )
            .unwrap();
            id
        };
        let ips = lodestar_filter::lists::IpSet::new([lodestar_filter::lists::IpRange::V4(
            11 << 24,
            (75 << 24) - 1,
        )]);
        [
            add("https://example.org/ips", 1, false, ips.encode()),
            add(
                "https://example.org/hashes",
                0,
                false,
                [[1u8; 20], [2; 20]].concat(),
            ),
            add("https://example.org/vetted", 0, true, [3u8; 20].to_vec()),
        ]
    };
    let (status, _, s) = assess(json!({ "subscriptions": [ids[0]] })).await;
    assert_eq!(status, StatusCode::OK, "{s}");
    assert_eq!(s["level"], "severe");
    assert_eq!(s["estimate"], false);
    let (_, _, s) = assess(json!({ "subscriptions": [ids[1], ids[2]] })).await;
    assert_eq!(s["level"], "strong", "{s}");
    assert_eq!(
        (s["hashes"].as_u64(), s["floor_hashes"].as_u64()),
        (Some(2), Some(1))
    );
    assert!(
        s["text"]
            .as_str()
            .unwrap()
            .contains("joins the locked floor")
    );
    let (status, _, _) = assess(json!({ "subscriptions": [999] })).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn class_rules_are_capped_and_unindexed_torrents_are_requeued() {
    let f = fixture_with(Some(SearchIndex::in_memory().unwrap()));
    let c = logged_in(&f).await;
    let (indexed, _) = store_torrent(&f, "Indexed.Clip.mp4", 900 << 20);
    {
        let mut store = f.api.store.lock().unwrap();
        enrich_all(&mut store);
        let index = f.api.index.as_ref().unwrap().index();
        let mut w = index.writer(15 << 20).unwrap();
        w.catch_up(&mut store, 10).unwrap();
        w.commit().unwrap();
    }
    // Stored after the index's last commit: not searchable yet.
    let (late, _) = store_torrent(&f, "Late.Clip.mp4", 900 << 20);
    enrich_all(&mut f.api.store.lock().unwrap());
    let rules = |body: Value| post("/api/v1/classify/rules", Some(&c), body);
    let (status, _, body) = call(&f.app, rules(json!({ "query": "nomatch", "adult": true }))).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["queued"], json!(1), "only the unindexed one");
    assert_eq!(body["truncated"], json!(false));
    let unenriched = f
        .api
        .store
        .lock()
        .unwrap()
        .unenriched(0, lodestar_classify::ENRICHMENT_VERSION, 10)
        .unwrap();
    assert_eq!(unenriched, vec![late]);
    assert!(!unenriched.contains(&indexed));
    let generation = f.api.store.lock().unwrap().class_gen().unwrap();
    assert_eq!(generation, 1, "a rule change moves the generation");

    // At most 1,000 rules.
    {
        let mut store = f.api.store.lock().unwrap();
        for i in 1..lodestar_filter::MAX_CLASS_RULES {
            store.add_class_rule(&format!("word{i}"), true).unwrap();
        }
    }
    let (status, _, body) = call(&f.app, rules(json!({ "query": "more", "adult": true }))).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(body["error"]["code"], json!("too_many_class_rules"));
}

#[tokio::test]
async fn the_corrections_report_reads_in_chunks() {
    let f = fixture();
    let c = logged_in(&f).await;
    // More than one chunk (500) of corrections, all Is adult on a movie.
    {
        for i in 0..520 {
            let (id, _) = store_torrent(&f, &format!("Some.Film.{i}.1999.1080p.mkv"), 2 << 30);
            f.api
                .store
                .lock()
                .unwrap()
                .set_correction(
                    id,
                    lodestar_store::ClassCorrection {
                        adult: Some(true),
                        not_junk: false,
                    },
                )
                .unwrap();
        }
    }
    let (status, _, body) = call(&f.app, get("/api/v1/classify", &c)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["corrections"], json!(520));
    assert_eq!(body["detection"]["checked"], json!(520));
    assert_eq!(body["detection"]["disagree"], json!(520));
    assert_eq!(body["detection"]["examples"].as_array().unwrap().len(), 50);
}

#[tokio::test]
async fn a_correction_relabelling_a_torrent_adult_meets_the_adult_only_core() {
    use lodestar_filter::TermSet;
    let f = fixture();
    let c = logged_in(&f).await;
    // Stand-in terms (`lexa` sexual, `rolea` child role), ten words apart:
    // only the Adult-only CS-5a can catch them.
    f.api.engine.update_engine(|e| {
        *e = Engine::new(Core::new(
            Lists {
                sexual: TermSet::of(&["lexa"]),
                child_roles: TermSet::of(&["rolea"]),
                codes: TermSet::of(&["codex"]),
            },
            CsList::default(),
            Switches::default(),
        ))
    });
    let name = "Holiday Rolea one two three four five six seven eight nine Lexa.mp4";
    let (id, hash) = store_torrent(&f, name, 900 << 20);
    enrich_all(&mut f.api.store.lock().unwrap());
    let (status, _, body) = call(
        &f.app,
        post(
            "/api/v1/classify/correct",
            Some(&c),
            json!({ "hash": hash, "adult": true }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["type"], json!("adult"));
    assert_eq!(body["held_by"], json!("CS-5a"));
    {
        let store = f.api.store.lock().unwrap();
        assert!(store.torrent(id).unwrap().is_none(), "no longer stored");
        let q = store.quarantined(10).unwrap();
        assert_eq!(q.len(), 1);
        assert_eq!(q[0].rule, "CS-5a");
        assert!(q[0].sealed.is_some());
    }
    let mut raw = [0u8; 20];
    for (i, b) in raw.iter_mut().enumerate() {
        *b = u8::from_str_radix(&hash[2 * i..2 * i + 2], 16).unwrap();
    }
    assert!(f.api.held.contains(&raw), "refused at intake");

    // An adult hold: held by the Adult-only core only because you said
    // it's adult.
    let (_, _, q) = call(&f.app, get("/api/v1/filters/queue", &c)).await;
    assert_eq!(q["quarantine"][0]["adult_hold"], json!(true), "{q}");

    // The 30-day purge deletes its name and leaves a tombstone, so the
    // hash stays refused and is never fetched again (owner, 2026-10-22).
    let released = {
        let mut store = f.api.store.lock().unwrap();
        let (released, _) = store.purge_filter(4_000_000_000, 0).unwrap();
        assert!(store.quarantined(10).unwrap().is_empty(), "no name kept");
        let t = store.tombstones(10).unwrap();
        assert_eq!(t.len(), 1);
        assert_eq!((t[0].hash, t[0].rule.as_str()), (raw, "CS-5a"));
        assert!(t[0].adult_hold);
        let state = store.filter_state().unwrap();
        assert!(state.held.contains(&(raw, Held::Tombstone)));
        released
    };
    assert!(released.is_empty());
    assert!(f.api.held.contains(&raw), "still refused at intake");

    // Releasing it says plainly that the Adult-only checks won't see it
    // again, and needs the password.
    let release = json!({ "op": "release", "hash": hash });
    let (_, _, a) = call(
        &f.app,
        post(
            "/api/v1/filters/preview",
            Some(&c),
            json!({ "change": release.clone() }),
        ),
    )
    .await;
    let warning = a["warning"].as_str().unwrap();
    for part in [
        "your correction or an Is adult classification rule made it adult",
        "A correction is deleted with the torrent",
        "if no Is adult rule matches it when it's next seen",
        "the Adult-only child-safety checks won't look at it",
    ] {
        assert!(warning.contains(part), "{part}: {warning}");
    }
    assert_eq!(a["preview"]["tombstones"], json!(1));
    let (status, _) = apply(&f, &c, release.clone(), None).await;
    assert_eq!(status, StatusCode::PRECONDITION_REQUIRED);
    let (status, body) = apply(&f, &c, release, Some(PASSWORD)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(!f.api.held.contains(&raw));
    assert!(
        f.api
            .store
            .lock()
            .unwrap()
            .tombstones(10)
            .unwrap()
            .is_empty()
    );
}

/// Holds `hash` under `rule`, as a quarantine or a tombstone.
fn hold(f: &Fixture, hash: [u8; 20], rule: &str, held: Held, at: i64) {
    hold_as(f, hash, rule, held, at, false);
}

/// [`hold`], as an adult hold when `adult_hold`.
fn hold_as(f: &Fixture, hash: [u8; 20], rule: &str, held: Held, at: i64, adult_hold: bool) {
    f.api
        .store
        .lock()
        .unwrap()
        .write(&WriteBatch {
            filtered: &[Filtered {
                hashes: &[hash],
                rule,
                action: if held == Held::Quarantine {
                    Action::Quarantine.code()
                } else {
                    Action::Drop.code()
                },
                held: Some(held),
                sealed: None,
                evidence: &[],
                at,
                expires_at: at + 86_400,
                adult_hold,
            }],
            ..WriteBatch::default()
        })
        .unwrap();
    f.api.held.insert(hash);
}

#[tokio::test]
async fn the_review_queue_pages() {
    let f = fixture();
    let c = logged_in(&f).await;
    let now = lodestar_api_now();
    // Two share a time: the hash orders them.
    for (i, at) in [now, now, now - 10, now - 20, now - 30]
        .into_iter()
        .enumerate()
    {
        hold(&f, [i as u8 + 1; 20], "CS-3a", Held::Quarantine, at);
    }
    let mut seen = Vec::new();
    let mut uri = "/api/v1/filters/queue?limit=2".to_owned();
    let mut pages = 0;
    loop {
        let (status, _, q) = call(&f.app, get(&uri, &c)).await;
        assert_eq!(status, StatusCode::OK, "{q}");
        pages += 1;
        for e in q["quarantine"].as_array().unwrap() {
            seen.push(e["hash"].as_str().unwrap().to_owned());
        }
        match q["next_cursor"].as_str() {
            Some(next) => uri = format!("/api/v1/filters/queue?limit=2&cursor={next}"),
            None => break,
        }
    }
    assert_eq!(pages, 3);
    let want: Vec<String> = [1u8, 2, 3, 4, 5]
        .iter()
        .map(|b| format!("{b:02x}").repeat(20))
        .collect();
    assert_eq!(seen, want, "newest first, each once");

    for bad in ["?limit=0", "?limit=1001", "?cursor=nope", "?cursor=5.zz"] {
        let (status, _, body) = call(&f.app, get(&format!("/api/v1/filters/queue{bad}"), &c)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{bad}: {body}");
    }
}

#[tokio::test]
async fn removing_a_rule_or_a_pack_releases_what_it_holds() {
    let f = fixture();
    let c = logged_in(&f).await;
    let now = lodestar_api_now();
    let (status, body) = apply(
        &f,
        &c,
        json!({ "op": "add_rule", "query": "ext:scr", "action": "quarantine" }),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let id = body["id"].as_i64().unwrap();
    hold(&f, [0x11; 20], &format!("U-{id}"), Held::Quarantine, now);
    hold(&f, [0x12; 20], &format!("U-{id}"), Held::Tombstone, now);
    hold(&f, [0x13; 20], "FK-4", Held::Tombstone, now);
    hold(&f, [0x14; 20], "CS-3a", Held::Quarantine, now);
    // Held by the rule and, as a tombstone, by the pack.
    hold(&f, [0x15; 20], &format!("U-{id}"), Held::Quarantine, now);
    hold(&f, [0x15; 20], "FK-4", Held::Tombstone, now);

    let (status, body) = apply(
        &f,
        &c,
        json!({ "op": "remove_rule", "id": id }),
        Some(PASSWORD),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["released"], json!(2));
    for h in [[0x11; 20], [0x12; 20]] {
        assert!(!f.api.held.contains(&h));
    }
    assert!(f.api.held.contains(&[0x15; 20]), "the pack still holds it");

    let pack = |on: bool| json!({ "op": "pack_switch", "id": "fakes", "on": on });
    let (status, body) = apply(&f, &c, pack(true), Some(PASSWORD)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = apply(&f, &c, pack(false), Some(PASSWORD)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["released"], json!(2));
    assert!(!f.api.held.contains(&[0x13; 20]));
    assert!(!f.api.held.contains(&[0x15; 20]));
    // Another rule's hold stays.
    assert!(f.api.held.contains(&[0x14; 20]));
    assert_eq!(
        f.api.store.lock().unwrap().quarantined(10).unwrap().len(),
        1
    );
}

/// End to end (§20.9): an Is adult rule is added, the torrents it matches
/// are re-queued, the writer's background pass relabels one adult and the
/// Adult-only core holds it, and search no longer finds it.
#[tokio::test]
async fn an_is_adult_rule_takes_a_held_torrent_out_of_search() {
    use lodestar_filter::TermSet;
    use lodestar_pipeline::enrich::{Backfill, enrich_stored_with};
    use std::sync::atomic::Ordering;
    let f = fixture_with(Some(SearchIndex::in_memory().unwrap()));
    let c = logged_in(&f).await;
    f.api.engine.update_engine(|e| {
        *e = Engine::new(Core::new(
            Lists {
                sexual: TermSet::of(&["lexa"]),
                child_roles: TermSet::of(&["rolea"]),
                codes: TermSet::of(&["codex"]),
            },
            CsList::default(),
            Switches::default(),
        ))
    });
    // Ten words apart: only the Adult-only CS-5a can catch them.
    let name = "Holiday Rolea one two three four five six seven eight nine Lexa.mp4";
    store_torrent(&f, name, 900 << 20);
    store_torrent(&f, "Holiday Postcards.mp4", 900 << 20);
    let index = f.api.index.as_ref().unwrap().index();
    let catch_up = |store: &mut Store| {
        let mut w = index.writer(15 << 20).unwrap();
        w.catch_up(store, 100).unwrap();
        w.commit().unwrap();
        index.refresh().unwrap();
    };
    {
        let mut store = f.api.store.lock().unwrap();
        enrich_all(&mut store);
        catch_up(&mut store);
    }
    let found = |q: &str| {
        let node = lodestar_search::parse(q).unwrap();
        let hits = index.search(&index.query(&node, 0), 50).unwrap().hits;
        let mut names = index.names(&hits).unwrap();
        names.sort();
        names
    };
    assert_eq!(found("rolea").len(), 1, "stored and searchable");

    let (status, _, body) = call(
        &f.app,
        post(
            "/api/v1/classify/rules",
            Some(&c),
            json!({ "query": "rolea", "adult": true }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["queued"], json!(1));
    assert!(
        f.api.reenrich.swap(false, Ordering::AcqRel),
        "pipeline told"
    );

    // The writer's background pass, as it runs it.
    {
        let mut store = f.api.store.lock().unwrap();
        let ctx = Backfill {
            filter: Some(&f.api.engine),
            sealer: f.api.sealer.as_deref(),
            quarantine_secs: f.api.quarantine_secs,
        };
        let b = enrich_stored_with(&mut store, 0, 100, &ctx)
            .unwrap()
            .unwrap();
        assert!(!b.rejected);
        assert_eq!(b.held.len(), 1);
        let q = store.quarantined(10).unwrap();
        assert_eq!(q.len(), 1);
        assert_eq!(q[0].rule, "CS-5a");
        catch_up(&mut store);
    }
    assert!(found("rolea").is_empty(), "held, so not found");
    assert_eq!(found("holiday"), vec!["Holiday Postcards.mp4".to_string()]);
}

#[tokio::test]
async fn switching_a_core_rule_off_releases_what_it_holds() {
    let f = fixture();
    let c = logged_in(&f).await;
    let now = lodestar_api_now();
    hold_as(&f, [0x21; 20], "CS-5a", Held::Tombstone, now, true);
    hold_as(&f, [0x22; 20], "CS-5a", Held::Quarantine, now, true);
    hold(&f, [0x23; 20], "CS-5a", Held::Quarantine, now);
    hold(&f, [0x24; 20], "CS-1a", Held::Tombstone, now);
    hold(&f, [0x25; 20], "CS-3a", Held::Quarantine, now);
    let off = json!({ "op": "core_switch", "rule": "CS-5a", "on": false });

    // The preview counts every hold of the rule, adult or not.
    let (_, _, a) = call(
        &f.app,
        post(
            "/api/v1/filters/preview",
            Some(&c),
            json!({ "change": off.clone() }),
        ),
    )
    .await;
    assert_eq!(a["preview"]["quarantine"], json!(2), "{a}");
    assert_eq!(a["preview"]["tombstones"], json!(1), "{a}");

    let (status, _) = apply(&f, &c, off.clone(), None).await;
    assert_eq!(status, StatusCode::PRECONDITION_REQUIRED);
    let (status, body) = apply(&f, &c, off.clone(), Some(PASSWORD)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["released"], json!(3));
    for h in [[0x21; 20], [0x22; 20], [0x23; 20]] {
        assert!(!f.api.held.contains(&h));
    }
    // The floor and other rules keep theirs.
    for h in [[0x24; 20], [0x25; 20]] {
        assert!(f.api.held.contains(&h));
    }
    {
        let store = f.api.store.lock().unwrap();
        assert_eq!(store.tombstones(10).unwrap().len(), 1);
        assert_eq!(store.quarantined(10).unwrap().len(), 1);
    }

    // Already off: nothing more is released.
    hold(&f, [0x26; 20], "CS-5a", Held::Quarantine, now);
    let (status, body) = apply(&f, &c, off, Some(PASSWORD)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["released"], Value::Null);
    assert!(f.api.held.contains(&[0x26; 20]));
}

#[tokio::test]
async fn adult_tombstones_can_be_listed_and_released() {
    let f = fixture();
    let c = logged_in(&f).await;
    let now = lodestar_api_now();
    for i in 0..3u8 {
        hold_as(
            &f,
            [0x31 + i; 20],
            "CS-5a",
            Held::Tombstone,
            now - i64::from(i),
            true,
        );
    }
    hold(&f, [0x3f; 20], "CS-3a", Held::Tombstone, now);
    hold_as(&f, [0x40; 20], "CS-5b", Held::Quarantine, now, true);

    let (status, _, all) = call(&f.app, get("/api/v1/filters/tombstones", &c)).await;
    assert_eq!(status, StatusCode::OK, "{all}");
    assert_eq!(all["tombstones"].as_array().unwrap().len(), 4);
    let mut seen = Vec::new();
    let mut uri = "/api/v1/filters/tombstones?adult=true&limit=2".to_owned();
    loop {
        let (status, _, page) = call(&f.app, get(&uri, &c)).await;
        assert_eq!(status, StatusCode::OK, "{page}");
        for t in page["tombstones"].as_array().unwrap() {
            assert_eq!(t["adult_hold"], json!(true));
            assert_eq!(t["rule"], json!("CS-5a"));
            seen.push(t["hash"].as_str().unwrap().to_owned());
        }
        match page["next_cursor"].as_str() {
            Some(n) => uri = format!("/api/v1/filters/tombstones?adult=true&limit=2&cursor={n}"),
            None => break,
        }
    }
    assert_eq!(seen, ["31", "32", "33"].map(|b| b.repeat(20)));

    // Reporting a false positive on an adult hold carries the same note.
    let fp = json!({ "op": "false_positive", "hash": "40".repeat(20) });
    let (_, _, a) = call(
        &f.app,
        post("/api/v1/filters/preview", Some(&c), json!({ "change": fp })),
    )
    .await;
    let warning = a["warning"].as_str().unwrap();
    assert!(
        warning.contains("A correction is deleted with the torrent"),
        "{warning}"
    );

    // Released through the usual flow.
    let release = json!({ "op": "release", "hash": seen[0] });
    let (status, body) = apply(&f, &c, release, Some(PASSWORD)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(!f.api.held.contains(&[0x31; 20]));
}

/// A change previewed while it didn't weaken filtering, which weakens it
/// by the time it applies, is refused rather than applied without the
/// password (runner's probe, #70).
#[tokio::test]
async fn a_change_that_became_weakening_since_its_preview_is_refused() {
    use lodestar_api::filters::Change;
    let f = fixture();
    let c = logged_in(&f).await;
    let now = lodestar_api_now();
    let off = json!({ "op": "core_switch", "rule": "CS-3a", "on": false });
    let (status, body) = apply(&f, &c, off.clone(), Some(PASSWORD)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    hold(&f, [0x51; 20], "CS-3a", Held::Quarantine, now);

    // Previewed while CS-3a is off: no password asked.
    let change: Change = serde_json::from_value(off).unwrap();
    let stale = f.api.assess(&change, now).unwrap();
    assert!(!stale.confirm);
    // Switched back on meanwhile (strengthening, no password).
    let on = json!({ "op": "core_switch", "rule": "CS-3a", "on": true });
    let (status, body) = apply(&f, &c, on, None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let err = f.api.apply(&change, stale, now).unwrap_err();
    assert_eq!(err.status(), StatusCode::CONFLICT);
    assert_eq!(err.code(), "changed_since_preview");
    assert!(f.api.held.contains(&[0x51; 20]), "nothing released");
    assert_eq!(core_on(&overview(&f, &c).await, "CS-3a"), json!(true));

    // Accepting a held list version: previewed before one shrank.
    let id = f
        .api
        .store
        .lock()
        .unwrap()
        .add_subscription(&lodestar_store::NewSubscription {
            url: "https://lists.example/a.txt",
            name: "a",
            kind: 0,
            format: Some("hashes"),
            action: Action::Drop.code(),
            floor: false,
            interval_secs: 3600,
        })
        .unwrap();
    let accept: Change =
        serde_json::from_value(json!({ "op": "resolve_held", "id": id, "accept": true })).unwrap();
    let stale = f.api.assess(&accept, now).unwrap();
    assert!(!stale.confirm);
    {
        let mut store = f.api.store.lock().unwrap();
        let data = [[1u8; 20], [2; 20], [3; 20]].concat();
        fn refresh(data: &[u8], held: bool) -> lodestar_store::Refresh<'_> {
            if held {
                lodestar_store::Refresh::Held {
                    data,
                    entries: (data.len() / 20) as i64,
                    etag: None,
                    last_modified: None,
                    why: "shrank",
                }
            } else {
                lodestar_store::Refresh::Updated {
                    data,
                    entries: (data.len() / 20) as i64,
                    etag: None,
                    last_modified: None,
                }
            }
        }
        store.record_refresh(id, 1, &refresh(&data, false)).unwrap();
        store
            .record_refresh(id, 2, &refresh(&data[..20], true))
            .unwrap();
    }
    let err = f.api.apply(&accept, stale, now).unwrap_err();
    assert_eq!(err.code(), "changed_since_preview");
    let fresh = f.api.assess(&accept, now).unwrap();
    assert!(fresh.confirm, "a fresh preview asks for the password");
}

/// Whether a core switch-off weakens filtering is read from the store,
/// not the engine, which can lag it after an apply that failed part-way.
#[tokio::test]
async fn a_core_switch_off_is_judged_by_the_stored_switch() {
    use lodestar_api::filters::Change;
    let f = fixture();
    let c = logged_in(&f).await;
    let now = lodestar_api_now();
    hold(&f, [0x61; 20], "CS-3a", Held::Quarantine, now);
    // The engine says off, the store says on: the divergence a failed
    // apply can leave.
    f.api.engine.update_engine(|e| {
        e.core
            .switches
            .set(lodestar_filter::CoreRule::Cs3a, false)
            .unwrap();
    });
    assert!(f.api.store.lock().unwrap().core_switch("CS-3a").unwrap());
    let off = json!({ "op": "core_switch", "rule": "CS-3a", "on": false });
    let change: Change = serde_json::from_value(off.clone()).unwrap();
    let a = f.api.assess(&change, now).unwrap();
    assert!(a.confirm, "weakening, by the store");
    let (status, _) = apply(&f, &c, off, None).await;
    assert_eq!(status, StatusCode::PRECONDITION_REQUIRED);
    // An assessment that asked for nothing doesn't get past apply either.
    let mut unconfirmed = a;
    unconfirmed.confirm = false;
    unconfirmed.weakening = false;
    let err = f.api.apply(&change, unconfirmed, now).unwrap_err();
    assert_eq!(err.code(), "changed_since_preview");
    assert!(f.api.held.contains(&[0x61; 20]));
    assert!(f.api.store.lock().unwrap().core_switch("CS-3a").unwrap());
}
