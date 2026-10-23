//! `/suggest` and `/parse?natural=true` (B7) over HTTP: each kind of
//! item, the visibility rules ("Show adult content", `hidden`), errors,
//! and the session guard.
//!
//! Responses in `golden/suggest_*.txt` and `golden/parse_natural*.txt`
//! are compared byte for byte.
//! `LODESTAR_BLESS=1 cargo test -p lodestar-api --test suggest` rewrites
//! them; review the diff before committing it.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Request, StatusCode, header};
use lodestar_api::{AdultVisibility, ApiState, Auth, router};
use lodestar_bencode::{OwnedValue, to_vec};
use lodestar_metainfo::Info;
use lodestar_search::SearchIndex;
use lodestar_store::{NewTorrent, SourceId, Store, StoreConfig, flags};
use serde_json::{Value, json};
use tokio::sync::watch;
use tower::ServiceExt;

const PASSWORD: &str = "correct horse battery";

/// Names, and whether a filter rule hid them.
const TORRENTS: &[(&str, bool)] = &[
    ("The.Dark.Knight.2008.1080p.BluRay.x264-GRP.mkv", false),
    (
        "The.Dark.Knight.2008.2160p.UHD.BluRay.x265-OTHER.mkv",
        false,
    ),
    ("The.Dark.Tower.2017.720p.WEB-DL.x264-GRP.mkv", false),
    ("Darkest.Secret.XXX.1080p.mp4", false),
    (
        "Darkly.Hidden.Thing.2020.1080p.WEB-DL.x264-HIDGRP.mkv",
        true,
    ),
    ("Amélie.2001.1080p.BluRay.x264-GRP.mkv", false),
];

fn raw(name: &str) -> Vec<u8> {
    let b = |s: &str| OwnedValue::Bytes(s.as_bytes().to_vec());
    to_vec(&OwnedValue::Dict(
        [
            ("length", OwnedValue::Int(1 << 30)),
            ("name", b(name)),
            ("piece length", OwnedValue::Int(1 << 16)),
            ("pieces", OwnedValue::Bytes(vec![0; 20])),
        ]
        .into_iter()
        .map(|(k, v)| (k.as_bytes().to_vec(), v))
        .collect(),
    ))
}

struct Fixture {
    app: Router,
    auth: Arc<Auth>,
    show: watch::Sender<bool>,
    _dir: tempfile::TempDir,
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("t.db");
    let mut store = Store::open(&db, StoreConfig::default()).unwrap();
    for (name, hidden) in TORRENTS {
        let raw = raw(name);
        let info = Info::parse(&raw).unwrap();
        store
            .insert(&[NewTorrent {
                info: &info,
                raw_info: Some(&raw),
                discovered_at: 1_700_000_000,
                metadata_at: 1_700_000_100,
                extra_flags: if *hidden { flags::HIDDEN } else { 0 },
                source: SourceId::new(0).unwrap(),
            }])
            .unwrap();
    }
    while lodestar_pipeline::enrich::enrich_stored(&mut store, 0, 100)
        .unwrap()
        .is_some()
    {}
    let index = SearchIndex::in_memory().unwrap();
    let mut w = index.writer(15 << 20).unwrap();
    w.catch_up(&mut store, 100).unwrap();
    w.commit().unwrap();
    index.titles().rebuild_from(&store).unwrap();
    let (show, rx) = watch::channel(false);
    let reader = Store::open_reader(&db, StoreConfig::default()).unwrap();
    let auth = Arc::new(Auth::load(&dir.path().join("auth.json"), false).unwrap());
    let app = Router::new().nest(
        "/api/v1",
        router(ApiState {
            auth: auth.clone(),
            index: Some(index.into()),
            reader: Arc::new(Mutex::new(reader)),
            filters: None,
            settings: None,
            ops: None,
            user_data: None,
            insights: None,
            adult: AdultVisibility::follow(move || *rx.borrow()),
        }),
    );
    Fixture {
        app,
        auth,
        show,
        _dir: dir,
    }
}

async fn call(app: &Router, mut req: Request<Body>) -> (StatusCode, Option<String>, String) {
    req.extensions_mut()
        .insert(ConnectInfo(SocketAddr::from(([10, 0, 9, 1], 1))));
    let resp = app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let cookie = resp
        .headers()
        .get(header::SET_COOKIE)
        .map(|v| v.to_str().unwrap().split(';').next().unwrap().to_string());
    let bytes = axum::body::to_bytes(resp.into_body(), 1 << 22)
        .await
        .unwrap();
    (status, cookie, String::from_utf8(bytes.to_vec()).unwrap())
}

fn get(uri: &str, cookie: Option<&str>) -> Request<Body> {
    let mut b = Request::get(uri);
    if let Some(c) = cookie {
        b = b.header(header::COOKIE, c);
    }
    b.body(Body::empty()).unwrap()
}

async fn session(f: &Fixture) -> String {
    let token = f.auth.setup_token().unwrap();
    let body = json!({ "token": token, "password": PASSWORD });
    let req = Request::post("/api/v1/setup")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    let (status, cookie, _) = call(&f.app, req).await;
    assert_eq!(status, StatusCode::OK);
    cookie.unwrap()
}

fn encode(s: &str) -> String {
    form_urlencoded::byte_serialize(s.as_bytes()).collect()
}

/// Sends `uri`, checks status and body against `golden/{name}.txt`.
async fn golden(f: &Fixture, cookie: &str, name: &str, uri: &str) -> Value {
    let (status, _, body) = call(&f.app, get(uri, Some(cookie))).await;
    let got = format!("{}\n{}\n", status.as_u16(), body);
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

fn texts(body: &Value, kind: &str) -> Vec<String> {
    body["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|i| i["kind"] == kind)
        .map(|i| i["text"].as_str().unwrap().to_string())
        .collect()
}

#[tokio::test]
async fn needs_a_session() {
    let f = fixture();
    let (status, _, body) = call(&f.app, get("/api/v1/suggest?q=dark", None)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
    let (status, _, _) = call(&f.app, get("/api/v1/parse?q=4k&natural=true", None)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn each_kind_of_item() {
    let f = fixture();
    let c = session(&f).await;
    let titles = golden(
        &f,
        &c,
        "suggest_titles",
        &format!("/api/v1/suggest?q={}", encode("res:1080p the dark")),
    )
    .await;
    assert_eq!(
        texts(&titles, "title"),
        ["The Dark Knight", "The Dark Tower"]
    );
    golden(&f, &c, "suggest_values", "/api/v1/suggest?q=res%3A").await;
    golden(&f, &c, "suggest_fields", "/api/v1/suggest?q=re").await;
    golden(&f, &c, "suggest_chip", "/api/v1/suggest?q=-x265").await;
    golden(
        &f,
        &c,
        "suggest_did_you_mean",
        &format!("/api/v1/suggest?q={}", encode("dark knigth ")),
    )
    .await;
    // The caret in the middle; ranges in characters.
    let body = golden(
        &f,
        &c,
        "suggest_unicode_caret",
        &format!("/api/v1/suggest?q={}&pos=4", encode("日本 amé 1080p")),
    )
    .await;
    assert_eq!(body["items"][0]["replace"], json!({ "end": 6, "start": 3 }));
    // Limits are clamped; empty input has nothing.
    let (_, _, body) = call(&f.app, get("/api/v1/suggest?q=the+dark&limit=1", Some(&c))).await;
    let body: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(body["items"].as_array().unwrap().len(), 1);
    golden(&f, &c, "suggest_empty", "/api/v1/suggest?q=").await;
}

#[tokio::test]
async fn adult_and_hidden_follow_the_search() {
    let f = fixture();
    let c = session(&f).await;
    let hidden = golden(&f, &c, "suggest_adult_hidden", "/api/v1/suggest?q=dark").await;
    assert!(texts(&hidden, "title").is_empty(), "{hidden}");
    let (_, _, body) = call(&f.app, get("/api/v1/suggest?q=ext%3Amp", Some(&c))).await;
    assert_eq!(
        serde_json::from_str::<Value>(&body).unwrap()["items"],
        json!([])
    );
    // "Show adult content" turned on applies to the next request.
    f.show.send(true).unwrap();
    let shown = golden(&f, &c, "suggest_adult_shown", "/api/v1/suggest?q=dark").await;
    assert_eq!(texts(&shown, "title"), ["Darkest Secret XXX"]);
    f.show.send(false).unwrap();
    // `hidden=true` shows what a filter rule hid, as `/search` does.
    let body = golden(
        &f,
        &c,
        "suggest_hidden_included",
        "/api/v1/suggest?q=group%3A&hidden=true",
    )
    .await;
    assert!(
        texts(&body, "value").contains(&"hidgrp".to_string()),
        "{body}"
    );
    let (_, _, body) = call(&f.app, get("/api/v1/suggest?q=group%3A", Some(&c))).await;
    let body: Value = serde_json::from_str(&body).unwrap();
    assert!(!texts(&body, "value").contains(&"hidgrp".to_string()));
}

#[tokio::test]
async fn bad_requests() {
    let f = fixture();
    let c = session(&f).await;
    golden(&f, &c, "suggest_bad_pos", "/api/v1/suggest?q=abc&pos=4").await;
    let long = "a".repeat(2049);
    golden(
        &f,
        &c,
        "suggest_too_long",
        &format!("/api/v1/suggest?q={long}"),
    )
    .await;
    golden(&f, &c, "suggest_bad_limit", "/api/v1/suggest?q=a&limit=x").await;
}

#[tokio::test]
async fn parse_proposes_natural_phrasing() {
    let f = fixture();
    let c = session(&f).await;
    let q = "4k movies from 2020 over 10gb";
    let body = golden(
        &f,
        &c,
        "parse_natural",
        &format!("/api/v1/parse?q={}&natural=true", encode(q)),
    )
    .await;
    assert_eq!(
        body["natural"]["canonical"],
        "res:2160p type:movie year:2020 size:>=10GB"
    );
    // The query itself is still parsed as written.
    assert_eq!(body["canonical"], q);
    // Without `natural`, the response is as before.
    let (_, _, plain) = call(&f.app, get("/api/v1/parse?q=4k", Some(&c))).await;
    assert!(!plain.contains("natural"), "{plain}");
    let words = vec!["word"; 33].join(" ");
    golden(
        &f,
        &c,
        "parse_natural_too_long",
        &format!("/api/v1/parse?q={}&natural=true", encode(&words)),
    )
    .await;
}
