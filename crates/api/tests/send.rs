//! `POST /api/v1/send` and the usage data routes end to end: torrents to
//! a stand-in qBittorrent through a saved client, never one a filter rule
//! hides or an Adult one while "Show adult content" is off; and Clear
//! usage data behind its confirmation.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{HeaderMap, Request, StatusCode, header};
use axum::routing::{get, post};
use lodestar_api::settings::{
    DownloadClientKind, Imported, Saved, SavedClient, SectionValues, SettingsApi, SettingsBatch,
    SettingsFailure, SettingsImport, SettingsIndex, SettingsPreview,
};
use lodestar_api::{AdultVisibility, ApiState, Auth, router};
use lodestar_bencode::{OwnedValue, to_vec};
use lodestar_classify::Correction;
use lodestar_metainfo::Info;
use lodestar_store::{
    NewTorrent, QueryLogEntry, QueryLogging, SourceId, Store, StoreConfig, flags,
};
use serde_json::{Value, json};
use tokio::sync::watch;
use tower::ServiceExt;

/// In id order: visible, hidden by a filter rule, Adult, visible.
const NAMES: [&str; 4] = [
    "Big.Movie.2019.1080p.mkv",
    "Hidden.By.Rule.2019.mkv",
    "Studio.Scene.XXX.1080p.mp4",
    "Other.Movie.2018.720p.mkv",
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

/// The saved client the registry would give.
#[derive(Debug)]
struct OneClient(Option<SavedClient>);

impl SettingsApi for OneClient {
    fn index(&self) -> SettingsIndex {
        unimplemented!()
    }
    fn section(&self, _: &str) -> Option<SectionValues> {
        unimplemented!()
    }
    fn save(&self, _: SettingsBatch) -> Result<Saved, SettingsFailure> {
        unimplemented!()
    }
    fn preview(&self, _: SettingsBatch) -> Result<SettingsPreview, SettingsFailure> {
        unimplemented!()
    }
    fn export(&self, _: bool) -> String {
        unimplemented!()
    }
    fn import(&self, _: SettingsImport, _: bool) -> Result<Imported, SettingsFailure> {
        unimplemented!()
    }
    fn download_client(&self, id: Option<&str>) -> Option<SavedClient> {
        self.0.clone().filter(|c| id.is_none_or(|id| id == c.id))
    }
}

/// A qBittorrent that takes `admin` / `adminadmin` and keeps each add's
/// multipart body.
async fn qbittorrent(added: Arc<Mutex<Vec<String>>>) -> SocketAddr {
    let app = Router::new()
        .route(
            "/api/v2/auth/login",
            post(|| async {
                let mut h = HeaderMap::new();
                h.insert(header::SET_COOKIE, "SID=abc; path=/".parse().unwrap());
                (h, "Ok.")
            }),
        )
        .route("/api/v2/app/version", get(|| async { "v4.6.3" }))
        .route("/api/v2/app/webapiVersion", get(|| async { "2.9.3" }))
        .route(
            "/api/v2/torrents/add",
            post(move |body: String| {
                let added = added.clone();
                async move {
                    added.lock().unwrap().push(body);
                    "Ok."
                }
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    addr
}

struct Fixture {
    app: Router,
    cookie: String,
    hashes: Vec<String>,
    show: watch::Sender<bool>,
    _dir: tempfile::TempDir,
}

fn fixture(client: Option<SavedClient>) -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("t.db");
    let mut store = Store::open(&db, StoreConfig::default()).unwrap();
    let raws: Vec<Vec<u8>> = NAMES.iter().map(|n| raw(n)).collect();
    let infos: Vec<Info> = raws.iter().map(|r| Info::parse(r).unwrap()).collect();
    let batch: Vec<NewTorrent<'_>> = infos
        .iter()
        .zip(&raws)
        .enumerate()
        .map(|(i, (info, raw))| NewTorrent {
            info,
            raw_info: Some(raw),
            discovered_at: 1_700_000_000 + i as i64,
            metadata_at: 1_700_000_100,
            extra_flags: if i == 1 { flags::HIDDEN } else { 0 },
            source: SourceId::new(0).unwrap(),
        })
        .collect();
    store.insert(&batch).unwrap();
    // Torrent 3 is classified Adult, as `/classify/correct` would.
    let row = store.torrent(3).unwrap().unwrap();
    let tree = store.files(3).unwrap().unwrap();
    let files = lodestar_classify::file_infos(&tree.files, tree.encoding);
    let fix = Correction {
        adult: Some(true),
        not_junk: false,
    };
    let (e, _) = lodestar_filter::reclassify::enrich_corrected(&row.name, &files, fix);
    store.set_enrichments(&[(3, e)]).unwrap();
    let entry = |q| QueryLogEntry {
        at: 1_700_000_000,
        query: q,
        results: Some(1),
        latency_us: 10,
    };
    store
        .log_queries(QueryLogging::On, &[entry("ubuntu"), entry("debian")])
        .unwrap();
    let hashes = infos
        .iter()
        .map(|i| {
            i.primary_hash()
                .0
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect()
        })
        .collect();
    let (show, rx) = watch::channel(false);
    let reader = Store::open_reader(&db, StoreConfig::default()).unwrap();
    let auth = Arc::new(Auth::load(&dir.path().join("auth.json"), false).unwrap());
    let token = auth.setup_token().unwrap();
    let set = auth
        .setup([127, 0, 0, 1].into(), &token, "correct horse battery")
        .unwrap();
    let app = Router::new().nest(
        "/api/v1",
        router(ApiState {
            auth,
            index: None,
            reader: Arc::new(Mutex::new(reader)),
            filters: None,
            settings: Some(Arc::new(OneClient(client))),
            ops: None,
            user_data: Some(Arc::new(Mutex::new(store))),
            adult: AdultVisibility::follow(move || *rx.borrow()),
            insights: None,
            hot: None,
            events: None,
        }),
    );
    Fixture {
        app,
        cookie: set.split(';').next().unwrap().to_string(),
        hashes,
        show,
        _dir: dir,
    }
}

fn saved(addr: SocketAddr) -> SavedClient {
    SavedClient {
        id: "home".into(),
        kind: DownloadClientKind::Qbittorrent,
        url: format!("http://{addr}/"),
        username: "admin".into(),
        password: Some("adminadmin".into()),
        category: "tv".into(),
        save_path: String::new(),
        paused: false,
    }
}

async fn call(f: &Fixture, method: &str, uri: &str, body: Option<Value>) -> (StatusCode, Value) {
    let mut req = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::COOKIE, &f.cookie);
    if body.is_some() {
        req = req.header(header::CONTENT_TYPE, "application/json");
    }
    let mut req = req
        .body(Body::from(body.map(|b| b.to_string()).unwrap_or_default()))
        .unwrap();
    req.extensions_mut()
        .insert(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 1))));
    let resp = f.app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20)
        .await
        .unwrap();
    (status, serde_json::from_slice(&bytes).unwrap())
}

#[tokio::test]
async fn only_visible_torrents_are_sent_each_with_its_result() {
    let added = Arc::new(Mutex::new(Vec::new()));
    let addr = qbittorrent(added.clone()).await;
    let f = fixture(Some(saved(addr)));
    let unknown = "f".repeat(40);
    let all: Vec<String> = f.hashes.iter().cloned().chain([unknown.clone()]).collect();
    let (status, v) = call(&f, "POST", "/api/v1/send", Some(json!({ "hashes": all }))).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    let outcomes: Vec<(&str, &str)> = v["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| {
            (
                r["outcome"].as_str().unwrap(),
                r["code"].as_str().unwrap_or("-"),
            )
        })
        .collect();
    assert_eq!(
        outcomes,
        [
            ("sent", "-"),
            ("refused", "hidden"),
            ("refused", "adult_hidden"),
            ("sent", "-"),
            ("refused", "not_found"),
        ]
    );
    assert_eq!(
        (v["client"].clone(), v["sent"].clone()),
        (json!("home"), json!(2))
    );
    // One request, with only the two visible torrents in it.
    let sent = added.lock().unwrap().clone();
    assert_eq!(sent.len(), 1);
    assert!(sent[0].contains(&f.hashes[0]) && sent[0].contains(&f.hashes[3]));
    assert!(!sent[0].contains(&f.hashes[1]) && !sent[0].contains(&f.hashes[2]));
    assert!(
        !v.to_string().contains(&addr.to_string()),
        "the address isn't repeated"
    );
    // With Show adult content on, the Adult one goes too.
    f.show.send(true).unwrap();
    let (_, v) = call(
        &f,
        "POST",
        "/api/v1/send",
        Some(json!({ "hashes": [f.hashes[2]] })),
    )
    .await;
    assert_eq!(v["results"][0]["outcome"], json!("sent"));
}

#[tokio::test]
async fn a_send_needs_a_saved_client_and_real_hashes() {
    let f = fixture(None);
    let (status, v) = call(
        &f,
        "POST",
        "/api/v1/send",
        Some(json!({ "hashes": [f.hashes[0]] })),
    )
    .await;
    assert_eq!(
        (status, v["error"]["code"].clone()),
        (StatusCode::NOT_FOUND, json!("no_client"))
    );
    let added = Arc::new(Mutex::new(Vec::new()));
    let f = fixture(Some(saved(qbittorrent(added.clone()).await)));
    let (status, v) = call(
        &f,
        "POST",
        "/api/v1/send",
        Some(json!({ "client": "nas", "hashes": [f.hashes[0]] })),
    )
    .await;
    assert_eq!(
        (status, v["error"]["code"].clone()),
        (StatusCode::NOT_FOUND, json!("no_client"))
    );
    for hashes in [json!([]), json!(["not-a-hash"])] {
        let (status, v) = call(
            &f,
            "POST",
            "/api/v1/send",
            Some(json!({ "hashes": hashes })),
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{v}");
        assert_eq!(v["error"]["code"], json!("invalid_hashes"));
    }
    assert!(added.lock().unwrap().is_empty());
}

#[tokio::test]
async fn clearing_usage_data_needs_its_confirmation() {
    let f = fixture(None);
    let (status, v) = call(&f, "GET", "/api/v1/usage", None).await;
    assert_eq!((status, v), (StatusCode::OK, json!({ "queries": 2 })));
    for body in [json!({}), json!({ "confirm": false })] {
        let (status, v) = call(&f, "POST", "/api/v1/usage/clear", Some(body)).await;
        assert_eq!(status, StatusCode::PRECONDITION_REQUIRED, "{v}");
        assert_eq!(v["error"]["code"], json!("confirmation_required"));
    }
    let (_, v) = call(&f, "GET", "/api/v1/usage", None).await;
    assert_eq!(v["queries"], json!(2), "nothing deleted without it");
    let (status, v) = call(
        &f,
        "POST",
        "/api/v1/usage/clear",
        Some(json!({ "confirm": true })),
    )
    .await;
    assert_eq!((status, v), (StatusCode::OK, json!({ "queries": 2 })));
    let (_, v) = call(&f, "GET", "/api/v1/usage", None).await;
    assert_eq!(v["queries"], json!(0));
}
