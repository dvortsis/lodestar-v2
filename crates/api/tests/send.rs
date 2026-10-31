//! `POST /api/v1/send` and the usage data routes end to end: torrents to
//! a stand-in qBittorrent through a saved client, never one a filter rule
//! hides or an Adult one while "Show adult content" is off; and Clear
//! usage data behind its confirmation.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::Router;
use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{HeaderMap, Request, StatusCode, header};
use axum::routing::{get, post};
use lodestar_api::settings::{
    DownloadClientKind, Imported, Saved, SavedClient, SectionValues, SendLimits, SettingsApi,
    SettingsBatch, SettingsFailure, SettingsImport, SettingsIndex, SettingsPreview,
};
use lodestar_api::{AdultVisibility, ApiState, Auth, router};
use lodestar_bencode::{OwnedValue, to_vec};
use lodestar_classify::Correction;
use lodestar_metainfo::Info;
use lodestar_store::{
    NewTorrent, QueryLogEntry, QueryLogging, SourceId, Store, StoreConfig, flags,
};
use serde_json::{Value, json};
use tokio::sync::{Semaphore, watch};
use tower::ServiceExt;

/// In id order: visible, hidden by a filter rule, Adult, visible, found
/// only through a private source, BEP 27 private, hybrid v1+v2, v2 only.
const NAMES: [&str; 8] = [
    "Big.Movie.2019.1080p.mkv",
    "Hidden.By.Rule.2019.mkv",
    "Studio.Scene.XXX.1080p.mp4",
    "Other.Movie.2018.720p.mkv",
    "Private.Tracker.Only.2018.mkv",
    "Bep27.Private.2018.mkv",
    "Hybrid.Release.2020.mkv",
    "V2.Only.Release.2021.mkv",
];

fn dict(entries: Vec<(&str, OwnedValue)>) -> OwnedValue {
    OwnedValue::Dict(
        entries
            .into_iter()
            .map(|(k, v)| (k.as_bytes().to_vec(), v))
            .collect(),
    )
}

/// Torrent `i`'s info dictionary (see [`NAMES`]).
fn raw(i: usize) -> Vec<u8> {
    let name = NAMES[i];
    let b = |s: &str| OwnedValue::Bytes(s.as_bytes().to_vec());
    let int = OwnedValue::Int;
    // One 10-byte file, as v2 describes it.
    let tree = || {
        dict(vec![(
            name,
            dict(vec![(
                "",
                dict(vec![
                    ("length", int(10)),
                    ("pieces root", OwnedValue::Bytes(vec![1; 32])),
                ]),
            )]),
        )])
    };
    let mut e = vec![("name", b(name)), ("piece length", int(1 << 14))];
    if i != 7 {
        let len = if i == 6 { 10 } else { 1 << 30 };
        e.push(("length", int(len)));
        e.push(("pieces", OwnedValue::Bytes(vec![0; 20])));
    }
    if i == 5 {
        e.push(("private", int(1)));
    }
    if i >= 6 {
        e.push(("file tree", tree()));
        e.push(("meta version", int(2)));
    }
    to_vec(&dict(e))
}

/// The saved client the registry would give, and this router's own send
/// limits, so tests running at once don't share them.
#[derive(Debug)]
struct OneClient(Option<SavedClient>, SendLimits);

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
    fn send_limits(&self) -> &SendLimits {
        &self.1
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
    /// The hybrid torrent's truncated v2 hash, and the v2-only torrent's
    /// full v2 hash.
    hybrid_v2: String,
    v2_full: String,
    show: watch::Sender<bool>,
    _dir: tempfile::TempDir,
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|b| format!("{b:02x}")).collect()
}

fn fixture(client: Option<SavedClient>) -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("t.db");
    let mut store = Store::open(&db, StoreConfig::default()).unwrap();
    let raws: Vec<Vec<u8>> = (0..NAMES.len()).map(raw).collect();
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
            extra_flags: match i {
                1 => flags::HIDDEN,
                4 => flags::PRIVATE_SOURCE,
                _ => 0,
            },
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
    let hashes = infos.iter().map(|i| hex(&i.primary_hash().0)).collect();
    let hybrid_v2 = hex(&infos[6].v2.unwrap().truncated().0);
    let v2_full = hex(&infos[7].v2.unwrap().0);
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
            settings: Some(Arc::new(OneClient(client, SendLimits::new()))),
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
        hybrid_v2,
        v2_full,
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
    answer(f.app.clone(), request(f, method, uri, body)).await
}

fn request(f: &Fixture, method: &str, uri: &str, body: Option<Value>) -> Request<Body> {
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
    req
}

async fn answer(app: Router, req: Request<Body>) -> (StatusCode, Value) {
    let resp = app.oneshot(req).await.unwrap();
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
    // Every torrent, an unknown hash, the hybrid's v2 alias and a repeat.
    let all: Vec<String> = f
        .hashes
        .iter()
        .cloned()
        .chain([unknown.clone(), f.hybrid_v2.clone(), f.hashes[0].clone()])
        .collect();
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
            // Withheld looks exactly like unknown.
            ("refused", "not_found"),
            ("refused", "not_found"),
            ("sent", "-"),
            ("refused", "private"),
            ("refused", "private"),
            ("sent", "-"),
            ("sent", "-"),
            ("refused", "not_found"),
            ("refused", "same_torrent"),
            ("refused", "same_torrent"),
        ]
    );
    let messages: Vec<&str> = v["results"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| r["code"] == json!("not_found"))
        .map(|r| r["message"].as_str().unwrap())
        .collect();
    assert!(messages.windows(2).all(|w| w[0] == w[1]), "{messages:?}");
    assert_eq!(
        (v["client"].clone(), v["sent"].clone()),
        (json!("home"), json!(4))
    );
    // One request, with only the visible torrents in it, each once: the
    // v2-only one by its full v2 hash.
    let sent = added.lock().unwrap().clone();
    assert_eq!(sent.len(), 1);
    for i in [0, 3, 6] {
        assert_eq!(sent[0].matches(&f.hashes[i]).count(), 1, "torrent {i}");
    }
    assert!(sent[0].contains(&format!("urn:btmh:1220{}", f.v2_full)));
    assert!(!sent[0].contains(&format!("btih:{}", f.hashes[7])));
    for i in [1, 2, 4, 5] {
        assert!(!sent[0].contains(&f.hashes[i]), "torrent {i} was sent");
    }
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

/// How the stand-in Transmission answers each `torrent-add`, in turn.
#[derive(Clone, Copy)]
enum Add {
    New,
    Duplicate,
    Refuse,
}

/// A Transmission daemon: a 409 with the session id first, then each
/// `torrent-add` answered as `script` says.
async fn transmission(script: Vec<Add>) -> SocketAddr {
    let next = Arc::new(Mutex::new(script.into_iter()));
    let app = Router::new().fallback(post(move |headers: HeaderMap, body: String| {
        let next = next.clone();
        async move {
            let mut h = HeaderMap::new();
            if headers.get("x-transmission-session-id").is_none() {
                h.insert("x-transmission-session-id", "s1".parse().unwrap());
                return (StatusCode::CONFLICT, h, String::new());
            }
            let call: Value = serde_json::from_str(&body).unwrap();
            let answer = match call["method"].as_str() {
                Some("torrent-add") => match next.lock().unwrap().next().unwrap_or(Add::Refuse) {
                    Add::New => json!({ "result": "success", "arguments": {
                        "torrent-added": { "hashString": "a".repeat(40), "id": 1, "name": "x" } } }),
                    Add::Duplicate => json!({ "result": "success", "arguments": {
                        "torrent-duplicate": { "hashString": "a".repeat(40), "id": 1, "name": "x" } } }),
                    Add::Refuse => json!({ "result": "invalid or corrupt torrent file" }),
                },
                _ => json!({ "result": "success", "arguments": { "rpc-version": 17, "version": "4.0.0" } }),
            };
            (StatusCode::OK, h, answer.to_string())
        }
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    addr
}

fn transmission_client(addr: SocketAddr) -> SavedClient {
    SavedClient {
        kind: DownloadClientKind::Transmission,
        username: String::new(),
        password: None,
        category: String::new(),
        ..saved(addr)
    }
}

/// Outcomes and codes of a send's results.
fn outcomes(v: &Value) -> Vec<(String, String)> {
    v["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| {
            (
                r["outcome"].as_str().unwrap().to_string(),
                r["code"].as_str().unwrap_or("-").to_string(),
            )
        })
        .collect()
}

#[tokio::test]
async fn a_send_that_fails_part_way_reports_what_went_in() {
    let addr = transmission(vec![Add::New, Add::Refuse]).await;
    let f = fixture(Some(transmission_client(addr)));
    // Visible, hidden, visible, visible: the client takes the first add
    // and refuses the second, so the third is never tried.
    let hashes = [&f.hashes[0], &f.hashes[1], &f.hashes[3], &f.hashes[6]];
    let (status, v) = call(
        &f,
        "POST",
        "/api/v1/send",
        Some(json!({ "hashes": hashes })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{v}");
    let want: Vec<(String, String)> = [
        ("sent", "-"),
        ("refused", "not_found"),
        ("failed", "rejected"),
        ("failed", "rejected"),
    ]
    .iter()
    .map(|(a, b)| (a.to_string(), b.to_string()))
    .collect();
    assert_eq!(outcomes(&v), want);
    assert_eq!(v["sent"], json!(1));
}

#[tokio::test]
async fn a_send_the_client_already_has_counts_as_done() {
    let addr = transmission(vec![Add::Duplicate, Add::Duplicate]).await;
    let f = fixture(Some(transmission_client(addr)));
    let hashes = [&f.hashes[0], &f.hashes[3]];
    let (_, v) = call(
        &f,
        "POST",
        "/api/v1/send",
        Some(json!({ "hashes": hashes })),
    )
    .await;
    let dup = ("duplicate".to_string(), "-".to_string());
    assert_eq!(outcomes(&v), [dup.clone(), dup]);
    assert_eq!(v["sent"], json!(2));
}

#[tokio::test]
async fn sends_past_the_rate_limit_are_refused() {
    // Unknown hashes: nothing reaches a client either way.
    let f = fixture(Some(saved("192.0.2.1:9".parse().unwrap())));
    let body = || Some(json!({ "hashes": ["f".repeat(40)] }));
    for _ in 0..lodestar_api::settings::MAX_SENDS {
        let (status, v) = call(&f, "POST", "/api/v1/send", body()).await;
        assert_eq!(status, StatusCode::OK, "{v}");
        assert_eq!(v["results"][0]["code"], json!("not_found"));
    }
    let (status, v) = call(&f, "POST", "/api/v1/send", body()).await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS, "{v}");
    assert_eq!(v["error"]["code"], json!("send_rate_limited"));
}

/// A Transmission whose adds wait at `gate` until the test releases them,
/// counting each in `seen`.
async fn held_transmission(seen: Arc<AtomicUsize>, gate: Arc<Semaphore>) -> SocketAddr {
    let app = Router::new().fallback(post(move |headers: HeaderMap, body: String| {
        let (seen, gate) = (seen.clone(), gate.clone());
        async move {
            let mut h = HeaderMap::new();
            if headers.get("x-transmission-session-id").is_none() {
                h.insert("x-transmission-session-id", "s1".parse().unwrap());
                return (StatusCode::CONFLICT, h, String::new());
            }
            let call: Value = serde_json::from_str(&body).unwrap();
            if call["method"] == json!("torrent-add") {
                seen.fetch_add(1, Ordering::SeqCst);
                gate.acquire().await.unwrap().forget();
            }
            let answer = json!({ "result": "success", "arguments": {
                "torrent-added": { "hashString": "a".repeat(40), "id": 1, "name": "x" } } });
            (StatusCode::OK, h, answer.to_string())
        }
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    addr
}

#[tokio::test]
async fn a_third_send_while_two_run_is_refused_and_sends_nothing() {
    let seen = Arc::new(AtomicUsize::new(0));
    let gate = Arc::new(Semaphore::new(0));
    let addr = held_transmission(seen.clone(), gate.clone()).await;
    let f = fixture(Some(transmission_client(addr)));
    let send = |i: usize| {
        let req = request(
            &f,
            "POST",
            "/api/v1/send",
            Some(json!({ "hashes": [f.hashes[i]] })),
        );
        tokio::spawn(answer(f.app.clone(), req))
    };
    let running = [send(0), send(3)];
    // Both adds are at the client, held there.
    let deadline = Instant::now() + Duration::from_secs(10);
    while seen.load(Ordering::SeqCst) < 2 {
        assert!(
            Instant::now() < deadline,
            "the stand-in saw {} adds",
            seen.load(Ordering::SeqCst)
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let (status, v) = call(
        &f,
        "POST",
        "/api/v1/send",
        Some(json!({ "hashes": [f.hashes[6]] })),
    )
    .await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS, "{v}");
    assert_eq!(v["error"]["code"], json!("sends_busy"));
    gate.add_permits(2);
    for r in running {
        let (status, v) = r.await.unwrap();
        assert_eq!(
            (status, v["sent"].clone()),
            (StatusCode::OK, json!(1)),
            "{v}"
        );
    }
    assert_eq!(
        seen.load(Ordering::SeqCst),
        2,
        "the refused send added nothing"
    );
}
