//! `/search?explain=1` (§8, B5c): each hit's score in parts, adding up to
//! the score it was ranked by, without changing the ranking, capped to
//! the first page, behind the session and never about a torrent the
//! page left out. Also the result cache as `/search` and `/facets` use
//! it: a cached first page hands out a cursor on the snapshot it came
//! from.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::Router;
use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Request, StatusCode, header};
use lodestar_api::{AdultVisibility, ApiState, Auth, router};
use lodestar_bencode::{OwnedValue, to_vec};
use lodestar_metainfo::Info;
use lodestar_search::{SearchHandle, SearchIndex};
use lodestar_store::{NewTorrent, SourceId, Store, StoreConfig, flags};
use serde_json::{Value, json};
use tower::ServiceExt;

const PASSWORD: &str = "correct horse battery";
const T0: i64 = 1_700_000_000;

/// Torrent `i`: movies and episodes whose names share words, so phase 2
/// (name and title boosts) has something to order. Every fifth is
/// hidden by a filter rule.
fn raw(i: usize) -> Vec<u8> {
    let b = |s: &str| OwnedValue::Bytes(s.as_bytes().to_vec());
    let d = |e: Vec<(&str, OwnedValue)>| {
        OwnedValue::Dict(
            e.into_iter()
                .map(|(k, v)| (k.as_bytes().to_vec(), v))
                .collect(),
        )
    };
    let name = match i % 3 {
        // Detected Adult: hidden until "Show adult content" is on.
        _ if i % 10 == 7 => format!("Dark.Scene.{i}.XXX.1080p.mp4"),
        0 => format!("Dark.Knight.{}.1080p.BluRay.x264-GRP", 2000 + i),
        1 => format!("The.Dark.Side.S01E0{}.720p.WEB-DL", 1 + i % 9),
        _ => format!("Knight.Rider.{i}.Collection"),
    };
    let files = vec![
        d(vec![
            ("length", OwnedValue::Int(1 << 30)),
            ("path", OwnedValue::List(vec![b(&format!("{name}.mkv"))])),
        ]),
        d(vec![
            ("length", OwnedValue::Int(2_000)),
            (
                "path",
                OwnedValue::List(vec![b("extras"), b(&format!("dark night {i}.txt"))]),
            ),
        ]),
    ];
    to_vec(&d(vec![
        ("files", OwnedValue::List(files)),
        ("name", b(&name)),
        ("piece length", OwnedValue::Int(1 << 16)),
        ("pieces", OwnedValue::Bytes(vec![0; 20])),
    ]))
}

struct Fixture {
    app: Router,
    auth: Arc<Auth>,
    store: Store,
    index: SearchIndex,
    /// "Show adult content", read live.
    adult: Arc<AtomicBool>,
    added: usize,
    _dir: tempfile::TempDir,
}

impl Fixture {
    fn add(&mut self, n: usize) {
        let raws: Vec<Vec<u8>> = (self.added..self.added + n).map(raw).collect();
        let infos: Vec<Info> = raws.iter().map(|r| Info::parse(r).unwrap()).collect();
        let batch: Vec<NewTorrent<'_>> = infos
            .iter()
            .zip(&raws)
            .enumerate()
            .map(|(k, (info, raw))| {
                let i = self.added + k;
                NewTorrent {
                    info,
                    raw_info: Some(raw),
                    discovered_at: T0 + i as i64,
                    metadata_at: T0 + i as i64 + 1,
                    extra_flags: if i % 5 == 4 { flags::HIDDEN } else { 0 },
                    source: SourceId::new(0).unwrap(),
                }
            })
            .collect();
        self.store.insert(&batch).unwrap();
        lodestar_pipeline::enrich::enrich_stored(&mut self.store, 0, 10_000)
            .unwrap()
            .unwrap();
        let mut w = self.index.writer(15 << 20).unwrap();
        w.catch_up(&mut self.store, 10_000).unwrap();
        w.commit().unwrap();
        self.added += n;
    }
}

fn fixture(n: usize) -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("t.db");
    let store = Store::open(&db, StoreConfig::default()).unwrap();
    let index = SearchIndex::in_memory().unwrap();
    let auth = Arc::new(Auth::load(&dir.path().join("auth.json"), false).unwrap());
    let reader = Store::open_reader(&db, StoreConfig::default()).unwrap();
    let handle: SearchHandle = index.clone().into();
    let adult = Arc::new(AtomicBool::new(false));
    let shown = adult.clone();
    let app = Router::new().nest(
        "/api/v1",
        router(ApiState {
            auth: auth.clone(),
            index: Some(handle),
            reader: Arc::new(Mutex::new(reader)),
            filters: None,
            settings: None,
            ops: None,
            adult: AdultVisibility::follow(move || shown.load(Ordering::Relaxed)),
            insights: None,
        }),
    );
    let mut f = Fixture {
        app,
        auth,
        store,
        index,
        adult,
        added: 0,
        _dir: dir,
    };
    f.add(n);
    f
}

async fn call(app: &Router, mut req: Request<Body>) -> (StatusCode, Option<String>, Value) {
    req.extensions_mut()
        .insert(ConnectInfo(SocketAddr::from(([10, 0, 0, 1], 1))));
    let resp = app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let cookie = resp
        .headers()
        .get(header::SET_COOKIE)
        .map(|v| v.to_str().unwrap().split(';').next().unwrap().to_string());
    let bytes = axum::body::to_bytes(resp.into_body(), 1 << 22)
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

async fn session(f: &Fixture) -> String {
    let token = f.auth.setup_token().unwrap();
    let req = Request::post("/api/v1/setup")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            json!({ "token": token, "password": PASSWORD }).to_string(),
        ))
        .unwrap();
    let (status, cookie, _) = call(&f.app, req).await;
    assert_eq!(status, StatusCode::OK);
    cookie.unwrap()
}

async fn search(f: &Fixture, cookie: &str, query: &str) -> (StatusCode, Value) {
    let (s, _, v) = call(
        &f.app,
        get(&format!("/api/v1/search?{query}"), Some(cookie)),
    )
    .await;
    (s, v)
}

fn hit_ids(v: &Value) -> Vec<i64> {
    v["hits"]
        .as_array()
        .unwrap()
        .iter()
        .map(|h| h["id"].as_i64().unwrap())
        .collect()
}

fn hit_scores(v: &Value) -> Vec<f64> {
    v["hits"]
        .as_array()
        .unwrap()
        .iter()
        .map(|h| h["score"].as_f64().unwrap())
        .collect()
}

#[tokio::test]
async fn the_parts_add_up_and_the_ranking_is_unchanged() {
    let f = fixture(60);
    let c = session(&f).await;
    for q in [
        "q=dark+knight",
        "q=knight",
        "q=dark+-side",
        "q=name:knight",
        "q=",
    ] {
        let (s, plain) = search(&f, &c, &format!("{q}&limit=30")).await;
        assert_eq!(s, StatusCode::OK, "{plain}");
        assert!(plain.get("explain").is_none(), "only when asked for");
        let (s, explained) = search(&f, &c, &format!("{q}&limit=30&explain=1")).await;
        assert_eq!(s, StatusCode::OK, "{explained}");
        assert_eq!(hit_ids(&plain), hit_ids(&explained), "{q}");
        assert_eq!(hit_scores(&plain), hit_scores(&explained), "{q}");
        let e = &explained["explain"];
        assert_eq!(e["ranked_by"], "relevance");
        assert_eq!(e["weights"]["name"], 3.0);
        assert_eq!(e["weights"]["title"], 2.0);
        assert_eq!(e["weights"]["rerank_window"], 200);
        let hits = e["hits"].as_array().unwrap();
        let ids: Vec<i64> = hits.iter().map(|h| h["id"].as_i64().unwrap()).collect();
        assert_eq!(ids, hit_ids(&explained), "one per hit, in order");
        let mut title_seen = false;
        for (h, score) in hits.iter().zip(hit_scores(&explained)) {
            let p2 = &h["phase2"];
            let sum = h["phase1"].as_f64().unwrap()
                + p2["name"].as_f64().unwrap()
                + p2["title"].as_f64().unwrap();
            assert!(
                (sum - score).abs() <= 1e-4 * score.abs().max(1.0),
                "{q}: {h} sums to {sum}, ranked at {score}"
            );
            assert_eq!(h["score"].as_f64().unwrap(), score);
            title_seen |= p2["title"].as_f64().unwrap() > 0.0;
            for k in ["phrase", "exact_name", "file_cap"] {
                assert!(p2[k].is_null(), "{k}");
            }
            for k in [
                "seeders",
                "freshness",
                "junk",
                "trackers",
                "swarm_consistency",
            ] {
                assert!(h["priors"][k].is_null(), "{k}");
            }
        }
        if q == "q=dark+knight" {
            assert_eq!(e["two_phase"], true);
            assert!(title_seen, "the parsed titles boost some hits");
        }
        if q == "q=name:knight" || q == "q=" {
            assert_eq!(e["two_phase"], false);
        }
    }
    // A field sort ranks by the field: no relevance parts.
    let (_, v) = search(&f, &c, "q=knight&sort=size&explain=1").await;
    assert_eq!(v["explain"]["ranked_by"], "size");
    assert!(v["explain"]["hits"][0]["phase1"].is_null());
    assert!(v["explain"]["hits"][0]["phase2"].is_null());
}

#[tokio::test]
async fn explain_is_capped_to_the_first_page() {
    let f = fixture(150);
    let c = session(&f).await;
    let (s, v) = search(&f, &c, "q=&limit=200&explain=1").await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["limit"], 100);
    assert_eq!(v["hits"].as_array().unwrap().len(), 100);
    assert_eq!(v["explain"]["hits"].as_array().unwrap().len(), 100);
    let (s, v) = search(&f, &c, "q=&offset=10&explain=1").await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    assert_eq!(v["error"]["code"], "explain_first_page");
    let (_, first) = search(&f, &c, "q=&limit=10").await;
    let cursor = first["next_cursor"].as_str().unwrap();
    let (s, v) = search(&f, &c, &format!("q=&limit=10&cursor={cursor}&explain=1")).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    assert_eq!(v["error"]["code"], "explain_first_page");
    // Offset 0 is the first page.
    let (s, _) = search(&f, &c, "q=&offset=0&explain=true").await;
    assert_eq!(s, StatusCode::OK);
}

#[tokio::test]
async fn explain_needs_a_session_and_never_shows_a_hidden_torrent() {
    let f = fixture(60);
    let (s, _, _) = call(&f.app, get("/api/v1/search?q=knight&explain=1", None)).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    let c = session(&f).await;
    let hidden: Vec<i64> = (0..60)
        .filter(|i| i % 5 == 4)
        .map(|i| i as i64 + 1)
        .collect();
    let (_, v) = search(&f, &c, "q=knight&limit=100&explain=1").await;
    let explained: Vec<i64> = v["explain"]["hits"]
        .as_array()
        .unwrap()
        .iter()
        .map(|h| h["id"].as_i64().unwrap())
        .collect();
    assert!(!explained.is_empty());
    assert!(
        explained.iter().all(|id| !hidden.contains(id)),
        "{explained:?}"
    );
    // Asked for, they're explained like any other.
    let (_, v) = search(&f, &c, "q=knight&limit=100&hidden=true&explain=1").await;
    let with: Vec<i64> = v["explain"]["hits"]
        .as_array()
        .unwrap()
        .iter()
        .map(|h| h["id"].as_i64().unwrap())
        .collect();
    assert!(with.iter().any(|id| hidden.contains(id)));
}

#[tokio::test]
async fn a_cached_first_page_pages_on_through_its_snapshot() {
    let mut f = fixture(60);
    let c = session(&f).await;
    let (_, cold) = search(&f, &c, "q=dark&limit=10").await;
    let (_, warm) = search(&f, &c, "q=dark&limit=10").await;
    assert_eq!(hit_ids(&cold), hit_ids(&warm));
    assert_eq!(cold["total"], warm["total"]);
    // Both cursors read the same snapshot, so new torrents committed in
    // between appear in neither.
    f.add(60);
    let scroll = async |first: &Value| {
        let mut ids = hit_ids(first);
        let mut next = first["next_cursor"].as_str().map(str::to_owned);
        while let Some(cur) = next {
            let (s, v) = search(&f, &c, &format!("q=dark&limit=10&cursor={cur}")).await;
            assert_eq!(s, StatusCode::OK, "{v}");
            ids.extend(hit_ids(&v));
            next = v["next_cursor"].as_str().map(str::to_owned);
        }
        ids
    };
    let a = scroll(&cold).await;
    let b = scroll(&warm).await;
    assert_eq!(a, b);
    assert_eq!(a.len() as u64, cold["total"].as_u64().unwrap());
    // The search itself now counts the new torrents.
    let (_, now) = search(&f, &c, "q=dark&limit=10").await;
    assert!(now["total"].as_u64() > cold["total"].as_u64());
    // And facets follow the same generation.
    let (_, _, facets) = call(&f.app, get("/api/v1/facets?q=dark", Some(&c))).await;
    assert_eq!(facets["total"], now["total"]);
}

/// The §3 budget for a cached browse (≤ 2 ms p95), through `/search`
/// with its rows read from SQLite. Timing: run it in a release build
/// (`cargo test --release -p lodestar-api --test explain -- --ignored`).
#[tokio::test]
#[ignore = "timing; run in release"]
async fn a_cached_browse_and_facets_answer_within_two_milliseconds() {
    let f = fixture(3_000);
    let c = session(&f).await;
    for uri in ["/api/v1/search?q=&sort=discovered", "/api/v1/facets?q="] {
        call(&f.app, get(uri, Some(&c))).await;
        let mut times = Vec::new();
        for _ in 0..300 {
            let t = Instant::now();
            let (s, _, _) = call(&f.app, get(uri, Some(&c))).await;
            assert_eq!(s, StatusCode::OK);
            times.push(t.elapsed());
        }
        times.sort();
        let p95 = times[times.len() * 95 / 100];
        println!("{uri}: p50 {:?}, p95 {p95:?}", times[times.len() / 2]);
        assert!(p95 <= Duration::from_millis(2), "{uri}: {p95:?}");
    }
}

#[tokio::test]
async fn show_adult_content_never_shares_a_cached_result() {
    let f = fixture(60);
    let c = session(&f).await;
    let facets = async || {
        let (s, _, v) = call(&f.app, get("/api/v1/facets?q=dark", Some(&c))).await;
        assert_eq!(s, StatusCode::OK, "{v}");
        v
    };
    let adult_ids: Vec<i64> = (0..60)
        .filter(|i| i % 10 == 7)
        .map(|i| i as i64 + 1)
        .collect();
    let (_, off) = search(&f, &c, "q=dark&limit=100").await;
    let off_facets = facets().await;
    assert!(hit_ids(&off).iter().all(|id| !adult_ids.contains(id)));
    // Turned on between two identical requests: the second sees Adult.
    f.adult.store(true, Ordering::Relaxed);
    let (_, on) = search(&f, &c, "q=dark&limit=100").await;
    let on_facets = facets().await;
    assert!(hit_ids(&on).iter().any(|id| adult_ids.contains(id)));
    assert!(on["total"].as_u64() > off["total"].as_u64());
    assert_eq!(on_facets["total"], on["total"]);
    assert!(on_facets["total"].as_u64() > off_facets["total"].as_u64());
    // And off again: nothing Adult, from either path.
    f.adult.store(false, Ordering::Relaxed);
    let (_, again) = search(&f, &c, "q=dark&limit=100").await;
    assert_eq!(hit_ids(&again), hit_ids(&off));
    assert_eq!(facets().await["total"], off_facets["total"]);
    // A cursor from the Adult-shown search is refused once it's hidden.
    let (_, page) = search(&f, &c, "q=dark&limit=5").await;
    f.adult.store(true, Ordering::Relaxed);
    let cur = page["next_cursor"].as_str().unwrap();
    let (s, _) = search(&f, &c, &format!("q=dark&limit=5&cursor={cur}")).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
}
