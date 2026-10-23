//! The library routes (`/user-items`, `/collections`, `/saved-searches`)
//! through the router: round trips, errors, what a hidden Adult torrent
//! never reveals, and paging past withheld rows. Goldens are in
//! `tests/golden/` (`LODESTAR_BLESS=1` rewrites them).

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use lodestar_api::{AdultVisibility, ApiState, Auth, router};
use lodestar_bencode::{OwnedValue, to_vec};
use lodestar_classify::Correction;
use lodestar_metainfo::Info;
use lodestar_search::SearchIndex;
use lodestar_store::{NewTorrent, SourceId, Store, StoreConfig, flags};
use serde_json::{Value, json};
use tower::ServiceExt;

const PASSWORD: &str = "correct horse battery";

/// One fixture torrent: a name, its files (`""` alone is single-file),
/// whether it's corrected to Adult, and its extra flags.
struct T<'a> {
    name: &'a str,
    files: Vec<(String, i64)>,
    adult: bool,
    flags: i64,
}

fn t<'a>(name: &'a str, files: &[(&str, i64)]) -> T<'a> {
    T {
        name,
        files: files.iter().map(|(p, l)| (p.to_string(), *l)).collect(),
        adult: false,
        flags: 0,
    }
}

fn metainfo(name: &str, files: &[(String, i64)]) -> Vec<u8> {
    let d = |e: Vec<(&str, OwnedValue)>| {
        OwnedValue::Dict(
            e.into_iter()
                .map(|(k, v)| (k.as_bytes().to_vec(), v))
                .collect(),
        )
    };
    let b = |s: &str| OwnedValue::Bytes(s.as_bytes().to_vec());
    let mut entries = vec![
        ("name", b(name)),
        ("piece length", OwnedValue::Int(1 << 16)),
        ("pieces", OwnedValue::Bytes(vec![0; 20])),
    ];
    if let [(single, len)] = files
        && single.is_empty()
    {
        entries.push(("length", OwnedValue::Int(*len)));
    } else {
        let list = files
            .iter()
            .map(|(p, len)| {
                d(vec![
                    ("length", OwnedValue::Int(*len)),
                    ("path", OwnedValue::List(p.split('/').map(b).collect())),
                ])
            })
            .collect();
        entries.push(("files", OwnedValue::List(list)));
    }
    to_vec(&d(entries))
}

struct Fixture {
    app: Router,
    auth: Arc<Auth>,
    /// "Show adult content", flipped live.
    adult: Arc<AtomicBool>,
    db: PathBuf,
    /// Info hashes in catalogue order.
    hashes: Vec<String>,
    _dir: tempfile::TempDir,
}

fn fixture(torrents: &[T<'_>], search: bool) -> Fixture {
    fixture_with(torrents, search, true)
}

/// `writable`: the API has its read-write connection.
fn fixture_with(torrents: &[T<'_>], search: bool, writable: bool) -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("t.db");
    let mut store = Store::open(&db, StoreConfig::default()).unwrap();
    let raws: Vec<Vec<u8>> = torrents
        .iter()
        .map(|t| metainfo(t.name, &t.files))
        .collect();
    let infos: Vec<Info> = raws.iter().map(|r| Info::parse(r).unwrap()).collect();
    let batch: Vec<NewTorrent<'_>> = infos
        .iter()
        .zip(&raws)
        .zip(torrents)
        .enumerate()
        .map(|(i, ((info, raw), t))| NewTorrent {
            info,
            raw_info: Some(raw),
            discovered_at: 1_700_000_000 + i as i64,
            metadata_at: 1_700_000_001 + i as i64,
            extra_flags: t.flags,
            source: SourceId::new(0).unwrap(),
        })
        .collect();
    let ids: Vec<i64> = store
        .insert(&batch)
        .unwrap()
        .into_iter()
        .map(|i| i.id())
        .collect();
    // Classified, as the pipeline would have; `adult` as an Is adult
    // correction would.
    let mut enriched = Vec::new();
    for (id, t) in ids.iter().zip(torrents) {
        let row = store.torrent(*id).unwrap().unwrap();
        let tree = store.files(*id).unwrap().unwrap();
        let files = lodestar_classify::file_infos(&tree.files, tree.encoding);
        let correction = Correction {
            adult: t.adult.then_some(true),
            not_junk: false,
        };
        let (e, _) = lodestar_filter::reclassify::enrich_corrected(&row.name, &files, correction);
        enriched.push((*id, e));
    }
    store.set_enrichments(&enriched).unwrap();
    let index = SearchIndex::in_memory().unwrap();
    let mut w = index.writer(15 << 20).unwrap();
    w.catch_up(&mut store, 1000).unwrap();
    w.commit().unwrap();
    let hashes: Vec<String> = ids
        .iter()
        .map(|id| {
            let row = store.torrent(*id).unwrap().unwrap();
            row.info_hash.0.iter().map(|b| format!("{b:02x}")).collect()
        })
        .collect();
    let adult = Arc::new(AtomicBool::new(false));
    let auth = Arc::new(Auth::load(&dir.path().join("auth.json"), false).unwrap());
    let reader = Store::open_reader(&db, StoreConfig::default()).unwrap();
    let user_data = Store::open(&db, StoreConfig::default()).unwrap();
    let shown = adult.clone();
    let app = Router::new().nest(
        "/api/v1",
        router(ApiState {
            auth: auth.clone(),
            index: search.then(|| index.into()),
            reader: Arc::new(Mutex::new(reader)),
            filters: None,
            settings: None,
            ops: None,
            user_data: writable.then(|| Arc::new(Mutex::new(user_data))),
            adult: AdultVisibility::follow(move || shown.load(Ordering::Relaxed)),
        }),
    );
    Fixture {
        app,
        auth,
        adult,
        db,
        hashes,
        _dir: dir,
    }
}

async fn call(app: &Router, req: Request<Body>) -> (StatusCode, String) {
    let resp = app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), 1 << 24)
        .await
        .unwrap();
    (status, String::from_utf8(bytes.to_vec()).unwrap())
}

async fn json_of(app: &Router, req: Request<Body>) -> (StatusCode, Value) {
    let (status, body) = call(app, req).await;
    (status, serde_json::from_str(&body).unwrap_or(Value::Null))
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

async fn session(f: &Fixture) -> String {
    let token = f.auth.setup_token().unwrap();
    let resp = f
        .app
        .clone()
        .oneshot(post(
            "/api/v1/setup",
            None,
            json!({ "token": token, "password": PASSWORD }),
        ))
        .await
        .unwrap();
    resp.headers()[header::SET_COOKIE]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_string()
}

/// Checks the status and body against `tests/golden/{name}.txt`.
async fn golden(f: &Fixture, name: &str, req: Request<Body>) -> Value {
    let (status, body) = call(&f.app, req).await;
    let got = format!("{}\n{}\n", status.as_u16(), blank_cursors(&body));
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
    serde_json::from_str(&body).unwrap_or(Value::Null)
}

/// Times and cursors vary between runs: they become `0` and `"…"`.
fn blank_cursors(body: &str) -> String {
    fn walk(v: &mut Value) {
        match v {
            Value::Object(m) => {
                for (k, v) in m.iter_mut() {
                    match k.as_str() {
                        "created_at" | "updated_at" | "added_at" | "last_alert_at"
                            if v.is_number() =>
                        {
                            *v = json!(0)
                        }
                        "next_cursor" if v.is_string() => *v = json!("…"),
                        _ => walk(v),
                    }
                }
            }
            Value::Array(a) => a.iter_mut().for_each(walk),
            _ => {}
        }
    }
    match serde_json::from_str::<Value>(body) {
        Ok(mut v) => {
            walk(&mut v);
            v.to_string()
        }
        Err(_) => body.to_string(),
    }
}

const GB: i64 = 1 << 30;

/// Discovered one second apart, in this order: five ordinary torrents
/// (0-4), then one a filter hid (5) and one that is Adult (6).
fn catalogue() -> Vec<T<'static>> {
    let mut hidden = t("Hidden.Movie.2020.1080p.BluRay.x264-HID", &[("h.mkv", GB)]);
    hidden.flags = flags::HIDDEN;
    let mut adult = t("Some.Feature.2021.1080p.WEB.x264-ADL", &[("a.mp4", GB)]);
    adult.adult = true;
    vec![
        t(
            "The.Dark.Knight.2008.1080p.BluRay.x264-GRP",
            &[("k.mkv", 8 * GB)],
        ),
        t("Show.Name.S01E02.1080p.WEB.x264-AAA", &[("s2.mkv", GB)]),
        t(
            "Batman.Begins.2005.720p.BluRay.x264-GRP",
            &[("b.mkv", 4 * GB)],
        ),
        t("Show.Name.S01E03.1080p.WEB.x264-AAA", &[("s3.mkv", GB)]),
        t(
            "Gotham.Soundtrack.2008.FLAC",
            &[("01 Intro.flac", 30 << 20)],
        ),
        hidden,
        adult,
    ]
}

const HIDDEN: usize = 5;
const ADULT: usize = 6;

fn send(method: &str, uri: &str, cookie: Option<&str>, body: Option<Value>) -> Request<Body> {
    let mut b = Request::builder().method(method).uri(uri);
    if let Some(c) = cookie {
        b = b.header(header::COOKIE, c);
    }
    // Every write says it is JSON, bodiless ones too.
    let b = b.header(header::CONTENT_TYPE, "application/json");
    match body {
        Some(v) => b.body(Body::from(v.to_string())),
        None => b.body(Body::empty()),
    }
    .unwrap()
}

fn patch(uri: &str, c: Option<&str>, body: Value) -> Request<Body> {
    send("PATCH", uri, c, Some(body))
}

fn put(uri: &str, c: Option<&str>, body: Value) -> Request<Body> {
    send("PUT", uri, c, Some(body))
}

fn delete(uri: &str, c: Option<&str>) -> Request<Body> {
    send("DELETE", uri, c, None)
}

fn code(body: &Value) -> &str {
    body["error"]["code"].as_str().unwrap_or("")
}

async fn ready() -> (Fixture, String) {
    let f = fixture(&catalogue(), true);
    let c = session(&f).await;
    (f, c)
}

fn url(f: &Fixture, i: usize, rest: &str) -> String {
    format!("/api/v1/user-items/{}{rest}", f.hashes[i])
}

fn hashes_of(f: &Fixture, ix: &[usize]) -> Value {
    json!({ "hashes": ix.iter().map(|&i| f.hashes[i].clone()).collect::<Vec<_>>() })
}

// ---- user items ------------------------------------------------------------

#[tokio::test]
async fn user_items_round_trip() {
    let (f, c) = ready().await;
    let c = Some(c.as_str());
    // No marks yet: all-false, never a 404.
    let b = golden(&f, "user_items_get_unmarked", get(&url(&f, 0, ""), c)).await;
    assert_eq!(b["favourite"], false);
    assert_eq!(b["labels"], json!([]));
    assert_eq!(b["collections"], json!([]));

    let body = json!({ "favourite": true, "labels": ["  4K  HDR ", "anime", "4K HDR"], "note": " Watch\r\nwith subs " });
    let b = golden(&f, "user_items_patch", patch(&url(&f, 0, ""), c, body)).await;
    assert_eq!(b["labels"], json!(["4K HDR", "anime"]));
    assert_eq!(b["note"], "Watch\nwith subs");

    // Unsent fields stay; `note: null` clears.
    let b = golden(
        &f,
        "user_items_patch_clear_note",
        patch(&url(&f, 0, ""), c, json!({ "hidden": true, "note": null })),
    )
    .await;
    assert_eq!(
        (b["favourite"].clone(), b["hidden"].clone()),
        (json!(true), json!(true))
    );
    assert_eq!(b["note"], Value::Null);
    assert_eq!(b["labels"], json!(["4K HDR", "anime"]));

    patch_ok(&f, c, 1, json!({ "favourite": true, "labels": ["anime"] })).await;
    let b = golden(&f, "user_items_list", get("/api/v1/user-items", c)).await;
    // Newest change first; each row carries its torrent.
    assert_eq!(b["items"][0]["hash"], f.hashes[1].as_str());
    assert_eq!(b["items"][0]["torrent"]["name"], catalogue()[1].name);
    assert_eq!(b["items"].as_array().unwrap().len(), 2);
    let b = json_of(&f.app, get("/api/v1/user-items?hidden_mark=true", c))
        .await
        .1;
    assert_eq!(b["items"].as_array().unwrap().len(), 1);
    let b = json_of(
        &f.app,
        get("/api/v1/user-items?label=anime&favourite=true", c),
    )
    .await
    .1;
    assert_eq!(b["items"].as_array().unwrap().len(), 2);
    let b = golden(&f, "user_items_labels", get("/api/v1/user-items/labels", c)).await;
    assert_eq!(
        b,
        json!([{ "count": 1, "label": "4K HDR" }, { "count": 2, "label": "anime" }])
    );

    // In collections.
    let (_, col) = json_of(
        &f.app,
        send(
            "POST",
            "/api/v1/collections",
            c,
            Some(json!({ "name": "Mine" })),
        ),
    )
    .await;
    let id = col["id"].as_i64().unwrap();
    let add = send(
        "POST",
        &format!("/api/v1/collections/{id}/items"),
        c,
        Some(hashes_of(&f, &[0])),
    );
    assert_eq!(json_of(&f.app, add).await.0, StatusCode::OK);
    let b = json_of(&f.app, get(&url(&f, 0, ""), c)).await.1;
    assert_eq!(b["collections"], json!([id]));

    // Clearing every mark.
    let (st, b) = json_of(&f.app, delete(&url(&f, 0, ""), c)).await;
    assert_eq!((st, b), (StatusCode::OK, json!({ "ok": true })));
    let b = json_of(&f.app, get(&url(&f, 0, ""), c)).await.1;
    assert_eq!(
        (b["favourite"].clone(), b["updated_at"].clone()),
        (json!(false), Value::Null)
    );
}

async fn patch_ok(f: &Fixture, c: Option<&str>, i: usize, body: Value) -> Value {
    let (st, b) = json_of(&f.app, patch(&url(f, i, ""), c, body)).await;
    assert_eq!(st, StatusCode::OK, "{b}");
    b
}

#[tokio::test]
async fn user_item_errors() {
    let (f, c) = ready().await;
    let c = Some(c.as_str());
    let unknown = format!("/api/v1/user-items/{}", "ab".repeat(20));
    let b = golden(&f, "user_items_unknown_hash", get(&unknown, c)).await;
    assert_eq!(code(&b), "torrent_not_found");
    golden(&f, "user_items_bad_hash", get("/api/v1/user-items/xyz", c)).await;
    let b = golden(
        &f,
        "user_items_bad_label",
        patch(&url(&f, 0, ""), c, json!({ "labels": ["ok", "   "] })),
    )
    .await;
    assert_eq!(
        (code(&b), b["error"]["field"].clone()),
        ("invalid_value", json!("labels"))
    );
    let b = golden(
        &f,
        "user_items_bad_note",
        patch(&url(&f, 0, ""), c, json!({ "note": "   " })),
    )
    .await;
    assert_eq!(b["error"]["field"], "note");
    let b = golden(
        &f,
        "user_items_bad_cursor",
        get("/api/v1/user-items?cursor=nope", c),
    )
    .await;
    assert_eq!(
        (code(&b), b["error"]["field"].clone()),
        ("bad_cursor", json!("cursor"))
    );
    assert_eq!(
        json_of(&f.app, patch(&unknown, c, json!({ "favourite": true })))
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        json_of(&f.app, delete(&unknown, c)).await.0,
        StatusCode::NOT_FOUND
    );
}

// ---- collections -----------------------------------------------------------

#[tokio::test]
async fn collections_round_trip() {
    let (f, c) = ready().await;
    let c = Some(c.as_str());
    let b = golden(
        &f,
        "collections_create",
        send(
            "POST",
            "/api/v1/collections",
            c,
            Some(json!({ "name": "  Watchlist ", "note": "later" })),
        ),
    )
    .await;
    assert_eq!(
        (b["name"].clone(), b["items"].clone()),
        (json!("Watchlist"), json!(0))
    );
    let id = b["id"].as_i64().unwrap();
    let base = format!("/api/v1/collections/{id}");

    let b = golden(
        &f,
        "collections_add",
        send(
            "POST",
            &format!("{base}/items"),
            c,
            Some(hashes_of(&f, &[2, 0, 1, 2])),
        ),
    )
    .await;
    assert_eq!(b["items"], 3);
    // Again: the ones in it stay where they are.
    let b = json_of(
        &f.app,
        send(
            "POST",
            &format!("{base}/items"),
            c,
            Some(hashes_of(&f, &[0, 3])),
        ),
    )
    .await
    .1;
    assert_eq!(b["items"], 4);
    let b = golden(&f, "collections_get", get(&base, c)).await;
    let order = |b: &Value| -> Vec<String> {
        b["entries"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["torrent"]["hash"].as_str().unwrap().to_string())
            .collect()
    };
    let want = |ix: &[usize]| -> Vec<String> { ix.iter().map(|&i| f.hashes[i].clone()).collect() };
    assert_eq!(order(&b), want(&[2, 0, 1, 3]));
    assert_eq!(b["items"], 4);

    // Paged by position.
    let p1 = json_of(&f.app, get(&format!("{base}?limit=3"), c)).await.1;
    assert_eq!(p1["entries"].as_array().unwrap().len(), 3);
    let after = p1["next_after"].as_i64().unwrap();
    let p2 = json_of(&f.app, get(&format!("{base}?limit=3&after={after}"), c))
        .await
        .1;
    assert_eq!(order(&p2), want(&[3]));
    assert_eq!(p2["next_after"], Value::Null);

    let b = golden(
        &f,
        "collections_reorder",
        put(&format!("{base}/order"), c, hashes_of(&f, &[3, 1, 0, 2])),
    )
    .await;
    assert_eq!(b["items"], 4);
    assert_eq!(
        order(&json_of(&f.app, get(&base, c)).await.1),
        want(&[3, 1, 0, 2])
    );
    let b = golden(
        &f,
        "collections_reorder_stale",
        put(&format!("{base}/order"), c, hashes_of(&f, &[3, 1, 0])),
    )
    .await;
    assert_eq!(code(&b), "stale_order");
    let dup = put(&format!("{base}/order"), c, hashes_of(&f, &[3, 1, 0, 0]));
    assert_eq!(code(&json_of(&f.app, dup).await.1), "stale_order");

    let b = golden(
        &f,
        "collections_remove_items",
        send(
            "POST",
            &format!("{base}/items/remove"),
            c,
            Some(hashes_of(&f, &[1, 4])),
        ),
    )
    .await;
    assert_eq!(b["items"], 3);
    let b = golden(
        &f,
        "collections_patch",
        patch(&base, c, json!({ "name": "Renamed" })),
    )
    .await;
    assert_eq!(
        (b["name"].clone(), b["note"].clone()),
        (json!("Renamed"), json!("later"))
    );
    let b = json_of(&f.app, patch(&base, c, json!({ "note": null })))
        .await
        .1;
    assert_eq!(b["note"], Value::Null);
    let b = json_of(&f.app, get("/api/v1/collections", c)).await.1;
    assert_eq!(b[0]["id"], id);
    golden(&f, "collections_list", get("/api/v1/collections", c)).await;
    assert_eq!(
        json_of(&f.app, delete(&base, c)).await.1,
        json!({ "ok": true })
    );
    assert_eq!(
        json_of(&f.app, get(&base, c)).await.0,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn collection_errors() {
    let (f, c) = ready().await;
    let c = Some(c.as_str());
    let b = golden(
        &f,
        "collections_not_found",
        get("/api/v1/collections/99", c),
    )
    .await;
    assert_eq!(code(&b), "collection_not_found");
    for (m, p) in [
        ("PATCH", ""),
        ("DELETE", ""),
        ("POST", "/items"),
        ("POST", "/items/remove"),
        ("PUT", "/order"),
    ] {
        let body = (m != "DELETE").then(|| {
            if m == "PATCH" {
                json!({ "name": "x" })
            } else {
                hashes_of(&f, &[0])
            }
        });
        let (st, b) = json_of(
            &f.app,
            send(m, &format!("/api/v1/collections/99{p}"), c, body),
        )
        .await;
        assert_eq!(
            (st, code(&b)),
            (StatusCode::NOT_FOUND, "collection_not_found"),
            "{m} {p}"
        );
    }
    let b = golden(
        &f,
        "collections_bad_name",
        send(
            "POST",
            "/api/v1/collections",
            c,
            Some(json!({ "name": "  " })),
        ),
    )
    .await;
    assert_eq!(
        (code(&b), b["error"]["field"].clone()),
        ("invalid_value", json!("name"))
    );
    let id = json_of(
        &f.app,
        send(
            "POST",
            "/api/v1/collections",
            c,
            Some(json!({ "name": "A" })),
        ),
    )
    .await
    .1["id"]
        .as_i64()
        .unwrap();
    let items = format!("/api/v1/collections/{id}/items");
    let b = golden(
        &f,
        "collections_add_unknown_hash",
        send(
            "POST",
            &items,
            c,
            Some(json!({ "hashes": [f.hashes[0], "ab".repeat(20)] })),
        ),
    )
    .await;
    assert_eq!(
        (code(&b), b["error"]["field"].clone()),
        ("torrent_not_found", json!("hashes"))
    );
    // All or nothing.
    assert_eq!(
        json_of(&f.app, get(&format!("/api/v1/collections/{id}"), c))
            .await
            .1["items"],
        0
    );
    let b = golden(
        &f,
        "collections_add_none",
        send("POST", &items, c, Some(json!({ "hashes": [] }))),
    )
    .await;
    assert_eq!(code(&b), "invalid_value");
    let many = json!({ "hashes": vec![f.hashes[0].clone(); 501] });
    assert_eq!(
        code(&json_of(&f.app, send("POST", &items, c, Some(many))).await.1),
        "invalid_value"
    );
    let bad = json_of(
        &f.app,
        send("POST", &items, c, Some(json!({ "hashes": ["zz"] }))),
    )
    .await
    .1;
    assert_eq!(
        (code(&bad), bad["error"]["field"].clone()),
        ("bad_hash", json!("hashes"))
    );
}

// ---- saved searches --------------------------------------------------------

#[tokio::test]
async fn saved_searches_round_trip() {
    let (f, c) = ready().await;
    let c = Some(c.as_str());
    let body = json!({ "name": " Knight ", "q": "knight 1080p", "sort": "-size", "alert": true, "rss": false });
    let b = golden(
        &f,
        "saved_create",
        send("POST", "/api/v1/saved-searches", c, Some(body)),
    )
    .await;
    assert_eq!(
        (b["name"].clone(), b["q"].clone(), b["paused"].clone()),
        (json!("Knight"), json!("knight 1080p"), json!(false))
    );
    assert_eq!(
        (
            b["alerts"].clone(),
            b["unread"].clone(),
            b["last_alert_at"].clone()
        ),
        (json!(0), json!(0), Value::Null)
    );
    assert!(b.get("channels").is_none() && b.get("watermark").is_none());
    let id = b["id"].as_i64().unwrap();
    let one = format!("/api/v1/saved-searches/{id}");
    assert_eq!(json_of(&f.app, get(&one, c)).await.1, b);
    // An AST works too, its text stored canonically.
    let (_, parsed) = json_of(&f.app, get("/api/v1/parse?q=batman", c)).await;
    let by_ast =
        json!({ "name": "AST", "ast": parsed["ast"], "alert": false, "rss": true, "paused": true });
    let b2 = json_of(
        &f.app,
        send("POST", "/api/v1/saved-searches", c, Some(by_ast)),
    )
    .await
    .1;
    assert_eq!(
        (b2["q"].clone(), b2["paused"].clone()),
        (json!("batman"), json!(true))
    );
    let list = golden(&f, "saved_list", get("/api/v1/saved-searches", c)).await;
    assert_eq!(list.as_array().unwrap().len(), 2);

    let b = golden(
        &f,
        "saved_patch",
        patch(
            &one,
            c,
            json!({ "name": "Renamed", "sort": null, "paused": true }),
        ),
    )
    .await;
    assert_eq!(
        (b["q"].clone(), b["sort"].clone(), b["alert"].clone()),
        (json!("knight 1080p"), Value::Null, json!(true))
    );
    let b = json_of(&f.app, patch(&one, c, json!({ "q": "batman" })))
        .await
        .1;
    assert_eq!(
        (b["q"].clone(), b["name"].clone(), b["paused"].clone()),
        (json!("batman"), json!("Renamed"), json!(true))
    );
    assert_eq!(
        json_of(&f.app, delete(&one, c)).await.1,
        json!({ "ok": true })
    );
    assert_eq!(json_of(&f.app, get(&one, c)).await.0, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn saved_search_errors() {
    let (f, c) = ready().await;
    let c = Some(c.as_str());
    let b = golden(&f, "saved_not_found", get("/api/v1/saved-searches/99", c)).await;
    assert_eq!(code(&b), "saved_search_not_found");
    assert_eq!(
        json_of(&f.app, delete("/api/v1/saved-searches/99", c))
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        json_of(
            &f.app,
            patch("/api/v1/saved-searches/99", c, json!({ "paused": true }))
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    let bad = json!({ "name": "x", "q": "year:", "alert": true, "rss": false });
    let b = golden(
        &f,
        "saved_query_syntax",
        send("POST", "/api/v1/saved-searches", c, Some(bad)),
    )
    .await;
    assert_eq!(code(&b), "query_syntax");
    assert!(b["error"]["start"].is_number() && b["error"]["end"].is_number());
    let b = golden(
        &f,
        "saved_bad_name",
        send(
            "POST",
            "/api/v1/saved-searches",
            c,
            Some(json!({ "name": " ", "alert": true, "rss": false })),
        ),
    )
    .await;
    assert_eq!(
        (code(&b), b["error"]["field"].clone()),
        ("invalid_value", json!("name"))
    );
    let b = golden(
        &f,
        "saved_unknown_sort",
        send(
            "POST",
            "/api/v1/saved-searches",
            c,
            Some(json!({ "name": "x", "sort": "bogus", "alert": true, "rss": false })),
        ),
    )
    .await;
    assert_eq!(code(&b), "unknown_sort");
    let both = json!({ "name": "x", "q": "a", "ast": {"op": "all"}, "alert": true, "rss": false });
    assert_eq!(
        code(
            &json_of(
                &f.app,
                send("POST", "/api/v1/saved-searches", c, Some(both))
            )
            .await
            .1
        ),
        "q_and_ast"
    );
    // `alert` and `rss` are required.
    let (st, b) = json_of(
        &f.app,
        send(
            "POST",
            "/api/v1/saved-searches",
            c,
            Some(json!({ "name": "x" })),
        ),
    )
    .await;
    assert_eq!(st, StatusCode::UNPROCESSABLE_ENTITY, "{b}");
    let bad_q = json!({ "q": "year:" });
    let (_, b) = json_of(
        &f.app,
        send(
            "POST",
            "/api/v1/saved-searches",
            c,
            Some(json!({ "name": "ok", "alert": true, "rss": false })),
        ),
    )
    .await;
    let id = b["id"].as_i64().unwrap();
    let b = json_of(
        &f.app,
        patch(&format!("/api/v1/saved-searches/{id}"), c, bad_q),
    )
    .await
    .1;
    assert_eq!(code(&b), "query_syntax");
}

#[tokio::test]
async fn patching_keeps_channels_and_resuming_moves_the_watermark() {
    // Search off: the watermark is the store's highest torrent id.
    let f = fixture(&catalogue()[..2], false);
    let c = session(&f).await;
    let c = Some(c.as_str());
    let b = json_of(
        &f.app,
        send(
            "POST",
            "/api/v1/saved-searches",
            c,
            Some(json!({ "name": "S", "q": "show", "alert": true, "rss": true, "paused": true })),
        ),
    )
    .await
    .1;
    let id = b["id"].as_i64().unwrap();
    let mut store = Store::open(&f.db, StoreConfig::default()).unwrap();
    let s = store.saved_search(id).unwrap().unwrap();
    assert_eq!((s.watermark, s.channels), (2, 0));
    // The notifier (B9) sets channels; the API doesn't know them.
    let input = lodestar_store::SavedSearchInput {
        name: &s.name,
        query: &s.query,
        ast: &s.ast,
        sort: None,
        alert: true,
        rss: true,
        channels: 0b101,
        paused: true,
    };
    assert!(store.update_saved_search(id, &input, 2, 5).unwrap());

    // More arrives while it is paused.
    let raw = metainfo(
        "Later.Show.S02E01.1080p.WEB.x264-ZZZ",
        &[(String::new(), GB)],
    );
    let info = Info::parse(&raw).unwrap();
    store
        .insert(&[NewTorrent {
            info: &info,
            raw_info: Some(&raw),
            discovered_at: 1_700_000_100,
            metadata_at: 1_700_000_101,
            extra_flags: 0,
            source: SourceId::new(0).unwrap(),
        }])
        .unwrap();
    assert_eq!(store.max_id().unwrap(), 3);

    let one = format!("/api/v1/saved-searches/{id}");
    json_of(&f.app, patch(&one, c, json!({ "name": "Renamed" }))).await;
    let s = store.saved_search(id).unwrap().unwrap();
    assert_eq!(
        (s.channels, s.watermark, s.name.as_str(), s.paused),
        (0b101, 2, "Renamed", true)
    );
    let b = json_of(&f.app, patch(&one, c, json!({ "paused": false })))
        .await
        .1;
    assert_eq!(b["paused"], false);
    let s = store.saved_search(id).unwrap().unwrap();
    assert_eq!((s.channels, s.watermark), (0b101, 3));
}

// ---- content safety --------------------------------------------------------

/// Marks, collects and saves the normal, filter-hidden and Adult torrents
/// (Adult shown while doing so), then hides Adult again.
async fn marked(f: &Fixture, c: &str) -> i64 {
    let c = Some(c);
    f.adult.store(true, Ordering::Relaxed);
    for (n, i) in [0, ADULT, 1, HIDDEN, 2, 3, 4].into_iter().enumerate() {
        patch_ok(f, c, i, json!({ "favourite": true, "labels": [if i == ADULT { "secret" } else { "keep" }], "note": format!("n{n}") })).await;
    }
    let (_, col) = json_of(
        &f.app,
        send(
            "POST",
            "/api/v1/collections",
            c,
            Some(json!({ "name": "All" })),
        ),
    )
    .await;
    let id = col["id"].as_i64().unwrap();
    let add = send(
        "POST",
        &format!("/api/v1/collections/{id}/items"),
        c,
        Some(hashes_of(f, &[0, ADULT, 1, HIDDEN, 2, 3, 4])),
    );
    let (st, b) = json_of(&f.app, add).await;
    assert_eq!((st, b["items"].clone()), (StatusCode::OK, json!(6)));
    f.adult.store(false, Ordering::Relaxed);
    id
}

#[tokio::test]
async fn adult_and_filter_hidden_torrents_stay_out_of_every_list_and_count() {
    let (f, c) = ready().await;
    let col = marked(&f, &c).await;
    let c = Some(c.as_str());
    let base = format!("/api/v1/collections/{col}");
    let body = |b: &Value| b.to_string();
    let adult_name = catalogue()[ADULT].name.to_string();
    let hidden_name = catalogue()[HIDDEN].name.to_string();

    // Lists.
    let (_, items) = json_of(&f.app, get("/api/v1/user-items", c)).await;
    assert_eq!(items["items"].as_array().unwrap().len(), 5);
    let (_, labels) = json_of(&f.app, get("/api/v1/user-items/labels", c)).await;
    assert_eq!(labels, json!([{ "count": 5, "label": "keep" }]));
    let (_, page) = json_of(&f.app, get(&base, c)).await;
    assert_eq!(
        (
            page["items"].clone(),
            page["entries"].as_array().unwrap().len()
        ),
        (json!(5), 5)
    );
    let (_, cols) = json_of(&f.app, get("/api/v1/collections", c)).await;
    assert_eq!(cols[0]["items"], 5);
    // No trace of either anywhere.
    for b in [&items, &labels, &page, &cols] {
        let t = body(b);
        assert!(
            !t.contains(&adult_name) && !t.contains("secret") && !t.contains(&f.hashes[ADULT]),
            "{t}"
        );
        assert!(!t.contains(&hidden_name), "{t}");
    }

    // By hash: Adult is exactly an unknown hash.
    let unknown = format!("/api/v1/user-items/{}", "ab".repeat(20));
    let (st0, want) = call(&f.app, get(&unknown, c)).await;
    assert_eq!(st0, StatusCode::NOT_FOUND);
    for req in [
        get(&url(&f, ADULT, ""), c),
        patch(&url(&f, ADULT, ""), c, json!({ "favourite": false })),
        delete(&url(&f, ADULT, ""), c),
    ] {
        let (st, got) = call(&f.app, req).await;
        assert_eq!((st, got.as_str()), (st0, want.as_str()));
    }
    let new = json_of(
        &f.app,
        send(
            "POST",
            "/api/v1/collections",
            c,
            Some(json!({ "name": "B" })),
        ),
    )
    .await
    .1["id"]
        .as_i64()
        .unwrap();
    let add = send(
        "POST",
        &format!("/api/v1/collections/{new}/items"),
        c,
        Some(hashes_of(&f, &[0, ADULT])),
    );
    let (_, got) = json_of(&f.app, add).await;
    let unknown_add = send(
        "POST",
        &format!("/api/v1/collections/{new}/items"),
        c,
        Some(json!({ "hashes": [f.hashes[0], "ab".repeat(20)] })),
    );
    assert_eq!(got, json_of(&f.app, unknown_add).await.1);
    // The marks were not touched, nothing was added.
    assert_eq!(
        json_of(&f.app, get(&format!("/api/v1/collections/{new}"), c))
            .await
            .1["items"],
        0
    );

    // The filter-hidden one: in lists only with hidden=true, markable by hash.
    let (_, with) = json_of(&f.app, get("/api/v1/user-items?hidden=true", c)).await;
    assert_eq!(with["items"].as_array().unwrap().len(), 6);
    assert!(body(&with).contains(&hidden_name) && !body(&with).contains(&adult_name));
    let (_, with) = json_of(&f.app, get(&format!("{base}?hidden=true"), c)).await;
    assert_eq!(
        (
            with["items"].clone(),
            with["entries"].as_array().unwrap().len()
        ),
        (json!(6), 6)
    );
    assert_eq!(
        json_of(&f.app, get("/api/v1/user-items/labels?hidden=true", c))
            .await
            .1,
        json!([{ "count": 6, "label": "keep" }])
    );
    assert_eq!(
        json_of(&f.app, get("/api/v1/collections?hidden=true", c))
            .await
            .1[1]["items"],
        6
    );
    assert_eq!(
        json_of(&f.app, get(&url(&f, HIDDEN, ""), c)).await.0,
        StatusCode::OK
    );
    let (st, _) = json_of(
        &f.app,
        patch(&url(&f, HIDDEN, ""), c, json!({ "labels": ["x"] })),
    )
    .await;
    assert_eq!(st, StatusCode::OK);

    // Adult shown: it appears.
    f.adult.store(true, Ordering::Relaxed);
    let (_, items) = json_of(&f.app, get("/api/v1/user-items", c)).await;
    assert_eq!(items["items"].as_array().unwrap().len(), 6);
    assert!(body(&items).contains(&adult_name));
    assert_eq!(json_of(&f.app, get(&base, c)).await.1["items"], 6);
    assert_eq!(
        json_of(&f.app, get(&url(&f, ADULT, ""), c)).await.0,
        StatusCode::OK
    );
}

#[tokio::test]
async fn reorder_and_remove_leave_withheld_entries_alone() {
    let (f, c) = ready().await;
    let col = marked(&f, &c).await;
    let c = Some(c.as_str());
    let base = format!("/api/v1/collections/{col}");
    // Entries in order: 0, ADULT, 1, HIDDEN, 2, 3, 4. The client sees
    // 0, 1, 2, 3, 4 and reverses them.
    let (st, b) = json_of(
        &f.app,
        put(&format!("{base}/order"), c, hashes_of(&f, &[4, 3, 2, 1, 0])),
    )
    .await;
    assert_eq!((st, b["items"].clone()), (StatusCode::OK, json!(5)), "{b}");
    f.adult.store(true, Ordering::Relaxed);
    let (_, page) = json_of(&f.app, get(&format!("{base}?hidden=true"), c)).await;
    let got: Vec<&str> = page["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["torrent"]["hash"].as_str().unwrap())
        .collect();
    let want: Vec<&str> = [4, ADULT, 3, HIDDEN, 2, 1, 0]
        .iter()
        .map(|&i| f.hashes[i].as_str())
        .collect();
    assert_eq!(got, want);
    f.adult.store(false, Ordering::Relaxed);
    // Naming Adult while hidden is as stale as naming nothing.
    let b = json_of(
        &f.app,
        put(
            &format!("{base}/order"),
            c,
            hashes_of(&f, &[4, 3, 2, 1, 0, ADULT]),
        ),
    )
    .await
    .1;
    assert_eq!(code(&b), "stale_order");
    // Removing Adult by hash is a no-op while hidden.
    let rm = send(
        "POST",
        &format!("{base}/items/remove"),
        c,
        Some(hashes_of(&f, &[ADULT, 0])),
    );
    assert_eq!(json_of(&f.app, rm).await.1["items"], 4);
    f.adult.store(true, Ordering::Relaxed);
    assert_eq!(json_of(&f.app, get(&base, c)).await.1["items"], 5);
}

#[tokio::test]
async fn paging_past_withheld_rows_gives_full_pages_without_repeats() {
    let (f, c) = ready().await;
    let col = marked(&f, &c).await;
    let c = Some(c.as_str());
    for (path, key, cursor) in [
        ("/api/v1/user-items?limit=2".to_string(), "items", "cursor="),
        (
            format!("/api/v1/collections/{col}?limit=2"),
            "entries",
            "after=",
        ),
    ] {
        let mut seen = Vec::new();
        let mut sizes = Vec::new();
        let mut next = path.clone();
        loop {
            let (st, b) = json_of(&f.app, get(&next, c)).await;
            assert_eq!(st, StatusCode::OK, "{b}");
            let page = b[key].as_array().unwrap();
            sizes.push(page.len());
            seen.extend(
                page.iter()
                    .map(|r| r["torrent"]["hash"].as_str().unwrap().to_string()),
            );
            let n = b.get("next_cursor").or(b.get("next_after")).unwrap();
            if n.is_null() {
                break;
            }
            let n = n.as_str().map_or(n.to_string(), str::to_string);
            next = format!("{path}&{cursor}{n}");
            assert!(sizes.len() < 10);
        }
        assert_eq!(sizes, [2, 2, 1], "{path}");
        let mut uniq = seen.clone();
        uniq.sort();
        uniq.dedup();
        assert_eq!(uniq.len(), 5, "{seen:?}");
        assert!(!seen.contains(&f.hashes[ADULT]) && !seen.contains(&f.hashes[HIDDEN]));
    }
}

// ---- guards ----------------------------------------------------------------

#[tokio::test]
async fn guards_session_origin_and_content_type() {
    let (f, c) = ready().await;
    let hash = &f.hashes[0];
    let routes: Vec<(&str, String, Option<Value>)> = vec![
        ("GET", "/api/v1/user-items".into(), None),
        ("GET", "/api/v1/user-items/labels".into(), None),
        ("GET", format!("/api/v1/user-items/{hash}"), None),
        (
            "PATCH",
            format!("/api/v1/user-items/{hash}"),
            Some(json!({})),
        ),
        ("DELETE", format!("/api/v1/user-items/{hash}"), None),
        ("GET", "/api/v1/collections".into(), None),
        (
            "POST",
            "/api/v1/collections".into(),
            Some(json!({ "name": "x" })),
        ),
        ("GET", "/api/v1/collections/1".into(), None),
        ("PATCH", "/api/v1/collections/1".into(), Some(json!({}))),
        ("DELETE", "/api/v1/collections/1".into(), None),
        (
            "POST",
            "/api/v1/collections/1/items".into(),
            Some(json!({ "hashes": [hash] })),
        ),
        (
            "POST",
            "/api/v1/collections/1/items/remove".into(),
            Some(json!({ "hashes": [hash] })),
        ),
        (
            "PUT",
            "/api/v1/collections/1/order".into(),
            Some(json!({ "hashes": [hash] })),
        ),
        ("GET", "/api/v1/saved-searches".into(), None),
        (
            "POST",
            "/api/v1/saved-searches".into(),
            Some(json!({ "name": "x", "alert": true, "rss": false })),
        ),
        ("GET", "/api/v1/saved-searches/1".into(), None),
        ("PATCH", "/api/v1/saved-searches/1".into(), Some(json!({}))),
        ("DELETE", "/api/v1/saved-searches/1".into(), None),
    ];
    for (m, uri, body) in routes {
        let (st, b) = json_of(&f.app, send(m, &uri, None, body.clone())).await;
        assert_eq!(
            (st, code(&b)),
            (StatusCode::UNAUTHORIZED, "login_required"),
            "{m} {uri}"
        );
        if m == "GET" {
            continue;
        }
        // A cross-origin write.
        let mut req = send(m, &uri, Some(&c), body.clone());
        req.headers_mut()
            .insert(header::HOST, "lodestar.local".parse().unwrap());
        req.headers_mut()
            .insert(header::ORIGIN, "https://evil.example".parse().unwrap());
        let (st, b) = json_of(&f.app, req).await;
        assert_eq!(
            (st, code(&b)),
            (StatusCode::FORBIDDEN, "cross_origin"),
            "{m} {uri}"
        );
        // Not JSON.
        let mut req = send(m, &uri, Some(&c), body);
        req.headers_mut()
            .insert(header::CONTENT_TYPE, "text/plain".parse().unwrap());
        let (st, _) = json_of(&f.app, req).await;
        assert_eq!(st, StatusCode::UNSUPPORTED_MEDIA_TYPE, "{m} {uri}");
    }
}

#[tokio::test]
async fn without_a_write_connection_writes_are_503() {
    let f = fixture_with(&catalogue(), true, false);
    let c = session(&f).await;
    let c = Some(c.as_str());
    let h = &f.hashes[0];
    for (m, uri, body) in [
        (
            "PATCH",
            format!("/api/v1/user-items/{h}"),
            Some(json!({ "favourite": true })),
        ),
        ("DELETE", format!("/api/v1/user-items/{h}"), None),
        (
            "POST",
            "/api/v1/collections".to_string(),
            Some(json!({ "name": "x" })),
        ),
        (
            "POST",
            "/api/v1/saved-searches".to_string(),
            Some(json!({ "name": "x", "alert": true, "rss": false })),
        ),
    ] {
        let (st, b) = json_of(&f.app, send(m, &uri, c, body)).await;
        assert_eq!(
            (st, code(&b)),
            (StatusCode::SERVICE_UNAVAILABLE, "user_data_unavailable"),
            "{m} {uri}"
        );
    }
    // Reads still work.
    assert_eq!(
        json_of(&f.app, get("/api/v1/user-items", c)).await.0,
        StatusCode::OK
    );
}

#[tokio::test]
async fn positions_are_the_places_among_shown_entries() {
    let (f, c) = ready().await;
    let col = marked(&f, &c).await;
    let c = Some(c.as_str());
    let base = format!("/api/v1/collections/{col}");
    // Entries: 0, ADULT, 1, HIDDEN, 2, 3, 4; shown by default: 5.
    let (_, page) = json_of(&f.app, get(&base, c)).await;
    let positions: Vec<i64> = page["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["position"].as_i64().unwrap())
        .collect();
    assert_eq!(positions, [1, 2, 3, 4, 5]);
    let (_, p1) = json_of(&f.app, get(&format!("{base}?limit=2"), c)).await;
    assert_eq!(p1["next_after"], 2);
    let (_, p3) = json_of(&f.app, get(&format!("{base}?limit=2&after=4"), c)).await;
    assert_eq!(
        (
            p3["entries"][0]["position"].clone(),
            p3["next_after"].clone()
        ),
        (json!(5), Value::Null)
    );
    // Reordering keeps that true.
    json_of(
        &f.app,
        put(&format!("{base}/order"), c, hashes_of(&f, &[4, 3, 2, 1, 0])),
    )
    .await;
    let (_, page) = json_of(&f.app, get(&base, c)).await;
    let positions: Vec<i64> = page["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["position"].as_i64().unwrap())
        .collect();
    assert_eq!(positions, [1, 2, 3, 4, 5]);
}

#[tokio::test]
async fn saved_search_alert_counts_leave_out_withheld_torrents() {
    let (f, c) = ready().await;
    let c = Some(c.as_str());
    // The notifier (B9) records hits: one ordinary, one Adult. The search
    // starts at watermark 0 so both count as new.
    let mut store = Store::open(&f.db, StoreConfig::default()).unwrap();
    let input = lodestar_store::SavedSearchInput {
        name: "S",
        query: "",
        ast: "{\"op\":\"all\"}",
        sort: None,
        alert: true,
        rss: false,
        channels: 0,
        paused: false,
    };
    let id = store.create_saved_search(&input, 0, 5).unwrap();
    store
        .record_percolation(
            7,
            &[lodestar_store::SearchHits {
                saved_search_id: id,
                torrent_ids: &[1, 7],
            }],
            100,
        )
        .unwrap();
    let one = format!("/api/v1/saved-searches/{id}");
    // The store counts both; Adult is hidden, so the API counts one.
    let b = json_of(&f.app, get(&one, c)).await.1;
    assert_eq!(
        (
            b["alerts"].clone(),
            b["unread"].clone(),
            b["last_alert_at"].clone()
        ),
        (json!(1), json!(1), json!(100))
    );
    f.adult.store(true, Ordering::Relaxed);
    let b = json_of(&f.app, get(&one, c)).await.1;
    assert_eq!(
        (b["alerts"].clone(), b["unread"].clone()),
        (json!(2), json!(2))
    );
}

#[tokio::test]
async fn lists_and_counts_cross_the_store_batch_boundary() {
    // 230 ordinary torrents and 30 Adult ones, the Adult ones first
    // discovered so a store page of 200 is mostly withheld.
    let mut all: Vec<T<'static>> = Vec::new();
    let names: Vec<String> = (0..260)
        .map(|i| format!("Item.{i:03}.2020.1080p.WEB.x264-GRP"))
        .collect();
    let names: &'static [String] = Box::leak(names.into_boxed_slice());
    for (i, n) in names.iter().enumerate() {
        let mut x = t(n, &[("a.mkv", GB)]);
        x.adult = i >= 230;
        all.push(x);
    }
    let f = fixture(&all, true);
    let c = session(&f).await;
    let c = Some(c.as_str());
    f.adult.store(true, Ordering::Relaxed);
    for i in (0..260).rev() {
        patch_ok(&f, c, i, json!({ "favourite": true, "labels": ["x"] })).await;
    }
    let col = json_of(
        &f.app,
        send(
            "POST",
            "/api/v1/collections",
            c,
            Some(json!({ "name": "Big" })),
        ),
    )
    .await
    .1["id"]
        .as_i64()
        .unwrap();
    for chunk in (0..260).collect::<Vec<usize>>().chunks(100) {
        let (st, _) = json_of(
            &f.app,
            send(
                "POST",
                &format!("/api/v1/collections/{col}/items"),
                c,
                Some(hashes_of(&f, chunk)),
            ),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
    }
    f.adult.store(false, Ordering::Relaxed);
    // Most-recent-first marks put the 30 Adult ones last in the list.
    let mut seen = Vec::new();
    let mut next = "/api/v1/user-items?limit=100".to_string();
    loop {
        let b = json_of(&f.app, get(&next, c)).await.1;
        seen.extend(
            b["items"]
                .as_array()
                .unwrap()
                .iter()
                .map(|r| r["hash"].as_str().unwrap().to_string()),
        );
        let Some(cur) = b["next_cursor"].as_str() else {
            break;
        };
        next = format!("/api/v1/user-items?limit=100&cursor={cur}");
    }
    seen.sort();
    seen.dedup();
    assert_eq!(seen.len(), 230);
    assert_eq!(
        json_of(&f.app, get("/api/v1/user-items/labels", c)).await.1,
        json!([{ "count": 230, "label": "x" }])
    );
    assert_eq!(
        json_of(&f.app, get(&format!("/api/v1/collections/{col}"), c))
            .await
            .1["items"],
        230
    );
    let b = json_of(
        &f.app,
        get(&format!("/api/v1/collections/{col}?limit=200&after=100"), c),
    )
    .await
    .1;
    assert_eq!(
        (
            b["entries"].as_array().unwrap().len(),
            b["next_after"].clone()
        ),
        (130, Value::Null)
    );
    // Reordering more than 500 shown entries is not capped; here, 230.
    let order = (0..230).rev().collect::<Vec<usize>>();
    let (st, b) = json_of(
        &f.app,
        put(
            &format!("/api/v1/collections/{col}/order"),
            c,
            hashes_of(&f, &order),
        ),
    )
    .await;
    assert_eq!(
        (st, b["items"].clone()),
        (StatusCode::OK, json!(230)),
        "{b}"
    );
}
