//! The API end to end through the router: auth, CSRF checks, search,
//! parse and torrent detail.

use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use lodestar_api::{ApiEvents, ApiState, Auth, router};
use lodestar_bencode::{OwnedValue, to_vec};
use lodestar_metainfo::Info;
use lodestar_search::SearchIndex;
use lodestar_store::{
    HotColumns, NewTorrent, SourceId, Store, StoreConfig, SwarmUpdate, TrackerCounts, WriteBatch,
};
use serde_json::{Value, json};
use tower::ServiceExt;

fn torrent(name: &str, files: &[&str]) -> Vec<u8> {
    let d = |e: Vec<(&str, OwnedValue)>| {
        OwnedValue::Dict(
            e.into_iter()
                .map(|(k, v)| (k.as_bytes().to_vec(), v))
                .collect(),
        )
    };
    let b = |s: &str| OwnedValue::Bytes(s.as_bytes().to_vec());
    let files = files
        .iter()
        .map(|p| {
            d(vec![
                ("length", OwnedValue::Int(1 << 20)),
                ("path", OwnedValue::List(p.split('/').map(b).collect())),
            ])
        })
        .collect();
    to_vec(&d(vec![
        ("files", OwnedValue::List(files)),
        ("name", b(name)),
        ("piece length", OwnedValue::Int(1 << 16)),
        ("pieces", OwnedValue::Bytes(vec![0; 20])),
    ]))
}

struct Fixture {
    app: Router,
    auth: Arc<Auth>,
    hashes: Vec<String>,
    events: Arc<Recorder>,
    _dir: tempfile::TempDir,
}

/// Records what the API reports.
#[derive(Debug, Default)]
struct Recorder {
    interest: Mutex<Vec<i64>>,
    searches: Mutex<Vec<Option<usize>>>,
}

impl ApiEvents for Recorder {
    fn interest(&self, id: i64) {
        self.interest.lock().unwrap().push(id);
    }

    fn searched(&self, _took: std::time::Duration, total: Option<usize>) {
        self.searches.lock().unwrap().push(total);
    }
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("t.db");
    let mut store = Store::open(&db, StoreConfig::default()).unwrap();
    let raws = [
        torrent(
            "The.Dark.Knight.2008.1080p",
            &["The.Dark.Knight.mkv", "Subs/en.srt"],
        ),
        torrent("Batman Begins 720p", &["batman.mkv"]),
    ];
    let infos: Vec<Info> = raws.iter().map(|r| Info::parse(r).unwrap()).collect();
    let batch: Vec<NewTorrent<'_>> = infos
        .iter()
        .zip(&raws)
        .map(|(info, raw)| NewTorrent {
            info,
            raw_info: Some(raw),
            discovered_at: 1_700_000_000,
            metadata_at: 1_700_000_001,
            extra_flags: 0,
            source: SourceId::new(0).unwrap(),
        })
        .collect();
    let ids: Vec<i64> = store
        .insert(&batch)
        .unwrap()
        .iter()
        .map(|i| i.id())
        .collect();
    // The first torrent has been scraped; the second never has.
    let tracker = store.upsert_tracker("udp://t.example:1337", 1).unwrap();
    store
        .write(&WriteBatch {
            swarm: &[SwarmUpdate {
                torrent_id: ids[0],
                at: 1_700_000_100,
                seeders: 12,
                leechers: 3,
                completed: 90,
                trackers_reporting: 1,
                trackers_checked: 2,
                dht: Some((10, 4)),
                per_tracker: vec![TrackerCounts {
                    tracker,
                    seeders: 12,
                    leechers: 3,
                    completed: 90,
                }],
                next_scrape_at: 1_700_021_700,
                flags: 0,
                interest: false,
            }],
            ..WriteBatch::default()
        })
        .unwrap();
    let hot = Arc::new(HotColumns::new());
    store.load_hot(&hot).unwrap();
    let index = SearchIndex::in_memory().unwrap().with_hot(hot.clone());
    let mut w = index.writer(15 << 20).unwrap();
    w.catch_up(&mut store, 10).unwrap();
    w.commit().unwrap();
    let auth = Arc::new(Auth::load(&dir.path().join("auth.json"), false).unwrap());
    let reader = Store::open_reader(&db, StoreConfig::default()).unwrap();
    let events = Arc::new(Recorder::default());
    let app = Router::new().nest(
        "/api/v1",
        router(ApiState {
            auth: auth.clone(),
            index: Some(index),
            reader: Arc::new(Mutex::new(reader)),
            hot: Some(hot),
            events: Some(events.clone()),
        }),
    );
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
    Fixture {
        app,
        auth,
        hashes,
        events,
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

async fn logged_in(f: &Fixture) -> String {
    let token = f.auth.setup_token().unwrap();
    let (status, cookie, _) = call(
        &f.app,
        post(
            "/api/v1/setup",
            None,
            json!({ "token": token, "password": "correct horse battery" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    cookie.unwrap()
}

#[tokio::test]
async fn setup_login_and_protection() {
    let f = fixture();
    let (_, _, body) = call(&f.app, get("/api/v1/auth", None)).await;
    assert_eq!(body, json!({ "setup_required": true, "logged_in": false }));
    let (status, _, body) = call(&f.app, get("/api/v1/search?q=knight", None)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["setup_required"], json!(true));
    let (status, _, _) = call(
        &f.app,
        post(
            "/api/v1/setup",
            None,
            json!({ "token": "nope", "password": "correct horse battery" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    let cookie = logged_in(&f).await;
    let (_, _, body) = call(&f.app, get("/api/v1/auth", Some(&cookie))).await;
    assert_eq!(body, json!({ "setup_required": false, "logged_in": true }));
    let (status, _, _) = call(&f.app, get("/api/v1/search?q=knight", Some(&cookie))).await;
    assert_eq!(status, StatusCode::OK);

    let (status, again, _) = call(
        &f.app,
        post(
            "/api/v1/login",
            None,
            json!({ "password": "correct horse battery" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(again.unwrap().starts_with("lodestar_session="));
    let (status, _, _) = call(
        &f.app,
        post(
            "/api/v1/login",
            None,
            json!({ "password": "wrong password!" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    let (_, cleared, _) = call(&f.app, post("/api/v1/logout", Some(&cookie), json!({}))).await;
    assert_eq!(cleared.as_deref(), Some("lodestar_session="));
}

#[tokio::test]
async fn cross_site_writes_are_refused() {
    let f = fixture();
    let form = Request::post("/api/v1/login")
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .body(Body::from("password=x"))
        .unwrap();
    assert_eq!(
        call(&f.app, form).await.0,
        StatusCode::UNSUPPORTED_MEDIA_TYPE
    );
    let foreign = Request::post("/api/v1/login")
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::HOST, "lodestar.lan:8090")
        .header(header::ORIGIN, "https://evil.example")
        .body(Body::from(r#"{"password":"x"}"#))
        .unwrap();
    assert_eq!(call(&f.app, foreign).await.0, StatusCode::FORBIDDEN);
    let same = Request::post("/api/v1/login")
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::HOST, "lodestar.lan:8090")
        .header(header::ORIGIN, "http://lodestar.lan:8090")
        .body(Body::from(r#"{"password":"x"}"#))
        .unwrap();
    assert_eq!(
        call(&f.app, same).await.0,
        StatusCode::CONFLICT,
        "not set up yet, but allowed through"
    );
}

#[tokio::test]
async fn search_parse_and_detail() {
    let f = fixture();
    let c = logged_in(&f).await;
    let c = Some(c.as_str());

    let (status, _, body) = call(&f.app, get("/api/v1/search?q=dark%20knight", c)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["total"], json!(1));
    assert_eq!(body["hits"][0]["name"], json!("The.Dark.Knight.2008.1080p"));
    assert_eq!(body["hits"][0]["hash"], json!(f.hashes[0]));
    assert_eq!(body["hits"][0]["files"], json!(2));
    assert_eq!(body["stemmed"], json!(false));

    // A file clause lists the matching files of each hit.
    let (_, _, body) = call(&f.app, get("/api/v1/search?q=file:(ext:srt)", c)).await;
    assert_eq!(body["total"], json!(1));
    assert_eq!(body["hits"][0]["matched_files"], json!([1]));
    let (_, _, body) = call(&f.app, get("/api/v1/search?q=dark", c)).await;
    assert!(body["hits"][0].get("matched_files").is_none());

    let (_, _, body) = call(&f.app, get("/api/v1/search?q=mkv&sort=%2Bsize&limit=1", c)).await;
    assert_eq!(
        (
            body["total"].clone(),
            body["hits"].as_array().unwrap().len()
        ),
        (json!(2), 1)
    );
    assert_eq!(body["hits"][0]["name"], json!("Batman Begins 720p"));

    let (status, _, body) = call(&f.app, get("/api/v1/search?q=sise:1", c)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(
        (body["start"].clone(), body["end"].clone()),
        (json!(0), json!(5))
    );
    let (status, _, _) = call(&f.app, get("/api/v1/search?q=x&sort=popularity", c)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _, _) = call(&f.app, get("/api/v1/search?q=x&offset=9990&limit=50", c)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let (_, _, parsed) = call(&f.app, get("/api/v1/parse?q=batman%20-cam", c)).await;
    assert_eq!(parsed["canonical"], json!("batman -cam"));
    let (status, _, body) = call(
        &f.app,
        post("/api/v1/search", c, json!({ "ast": parsed["ast"] })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["hits"][0]["name"], json!("Batman Begins 720p"));
    let bad_ast = json!({ "ast": { "op": "word", "field": "any", "word": "two words" } });
    let (status, _, _) = call(&f.app, post("/api/v1/search", c, bad_ast)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let (status, _, body) =
        call(&f.app, get(&format!("/api/v1/torrents/{}", f.hashes[0]), c)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["file_list_total"], json!(2));
    assert_eq!(body["file_list"][1]["path"], json!("Subs/en.srt"));
    assert!(
        body["magnet"]
            .as_str()
            .unwrap()
            .starts_with(&format!("magnet:?xt=urn:btih:{}", f.hashes[0]))
    );
    let (_, _, body) = call(
        &f.app,
        get(
            &format!("/api/v1/torrents/{}/files?offset=1&limit=5", f.hashes[0]),
            c,
        ),
    )
    .await;
    assert_eq!(body["files"].as_array().unwrap().len(), 1);
    let (status, _, _) = call(
        &f.app,
        get(&format!("/api/v1/torrents/{}", "0".repeat(40)), c),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _, _) = call(&f.app, get("/api/v1/torrents/xyz", c)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn swarm_health_in_search_and_detail() {
    let f = fixture();
    let cookie = logged_in(&f).await;
    let c = Some(cookie.as_str());

    let (status, _, body) = call(&f.app, get("/api/v1/search?q=&sort=seeders", c)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let hits = body["hits"].as_array().unwrap();
    assert_eq!(hits.len(), 2);
    let h = &hits[0];
    assert_eq!(h["seeders"], 12, "scraped first: {body}");
    assert_eq!(
        (&h["trackers_reporting"], &h["trackers_checked"]),
        (&json!(1), &json!(2))
    );
    assert_eq!(h["last_scrape_at"], 1_700_000_100);
    assert_eq!(
        (&h["seeders_approx"], &h["swarm_suspect"]),
        (&json!(false), &json!(false))
    );
    assert!(h.get("dht_seeds").is_none(), "detail only");
    // Never scraped: unknown, not zero, all six together.
    for k in [
        "seeders",
        "leechers",
        "completed",
        "trackers_reporting",
        "trackers_checked",
        "last_scrape_at",
    ] {
        assert_eq!(hits[1][k], Value::Null, "{k}");
    }
    assert_eq!(hits[1]["seeders_approx"], false);
    assert!(hits[1]["last_seen_at"].is_i64());
    assert_eq!(*f.events.searches.lock().unwrap(), [Some(2)]);
    assert_eq!(f.events.interest.lock().unwrap().len(), 2, "top hits");

    let (status, _, body) = call(&f.app, get("/api/v1/search?q=seeders:%3E%3D10", c)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["total"], 1);
    let (_, _, body) = call(&f.app, get("/api/v1/search?q=knight&explain=1", c)).await;
    let parts = &body["hits"][0]["explain"];
    assert!(parts["seeders"].as_f64().unwrap() > 1.0, "{body}");
    for bad in ["sort=sideways", "tz=900"] {
        let (status, _, _) = call(&f.app, get(&format!("/api/v1/search?q=x&{bad}"), c)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{bad}");
    }
    let (status, _, _) = call(&f.app, get("/api/v1/search?q=time:22:00..02:00&tz=-300", c)).await;
    assert_eq!(status, StatusCode::OK);

    f.events.interest.lock().unwrap().clear();
    let (status, _, body) =
        call(&f.app, get(&format!("/api/v1/torrents/{}", f.hashes[0]), c)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["seeders"], 12);
    assert_eq!(body["trackers_reporting"], 1);
    assert_eq!(body["trackers_checked"], 2);
    assert_eq!(
        (&body["dht_seeds"], &body["dht_peers"]),
        (&json!(10), &json!(4))
    );
    assert_eq!(body["last_scrape_at"], 1_700_000_100);
    assert_eq!(body["swarm_trackers"][0]["url"], "udp://t.example:1337");
    let day = &body["swarm_history"]["daily"][0];
    assert_eq!(day["seeders"], 12);
    assert_eq!(
        day["at"],
        1_700_000_100 / 86_400 * 86_400,
        "start of the UTC day"
    );
    let week = &body["swarm_history"]["weekly"][0];
    assert_eq!(week["at"].as_i64().unwrap() % (7 * 86_400), 0);
    assert_eq!(body["swarm_suspect"], false);
    assert_eq!(
        f.events.interest.lock().unwrap().len(),
        1,
        "a detail view is interest"
    );

    let (_, _, body) = call(&f.app, get(&format!("/api/v1/torrents/{}", f.hashes[1]), c)).await;
    assert_eq!(body["seeders"], Value::Null, "never scraped");
    assert_eq!(body["last_scrape_at"], Value::Null);
    assert_eq!(body["dht_seeds"], Value::Null);
    assert_eq!(body["swarm_history"], Value::Null);
    assert_eq!(body["swarm_trend"], Value::Null);
    assert_eq!(body["swarm_trackers"], json!([]));
}
