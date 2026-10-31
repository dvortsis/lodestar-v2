//! Zero-result recovery on `/search` (§10.4 "No results") and
//! `/search/state` (Cold start, Index rebuilding): each removable clause's
//! count runs under the search's own visibility, so torrents a filter
//! rule hid and Adult while it's hidden never show up in a suggestion.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Request, StatusCode, header};
use lodestar_api::{AdultVisibility, ApiState, Auth, router};
use lodestar_bencode::{OwnedValue, to_vec};
use lodestar_classify::Correction;
use lodestar_metainfo::Info;
use lodestar_search::SearchIndex;
use lodestar_store::{Counter, NewTorrent, SourceId, Store, StoreConfig, flags};
use serde_json::{Value, json};
use tokio::sync::watch;
use tower::ServiceExt;

const PASSWORD: &str = "correct horse battery";

/// In id order, with their flags: two films, one a filter rule hid, and
/// two detected Adult.
const TORRENTS: [(&str, i64); 5] = [
    ("Big.Movie.2019.1080p.mkv", 0),
    ("Other.Movie.2018.720p.mkv", 0),
    ("Secret.Movie.2019.1080p.mkv", flags::HIDDEN),
    ("Studio.Scene.XXX.1080p.mp4", 0),
    ("Another.Scene.XXX.720p.mp4", 0),
];

fn raw(name: &str) -> Vec<u8> {
    let b = |s: &str| OwnedValue::Bytes(s.as_bytes().to_vec());
    let d = |e: Vec<(&str, OwnedValue)>| {
        OwnedValue::Dict(
            e.into_iter()
                .map(|(k, v)| (k.as_bytes().to_vec(), v))
                .collect(),
        )
    };
    to_vec(&d(vec![
        ("length", OwnedValue::Int(1 << 30)),
        ("name", b(name)),
        ("piece length", OwnedValue::Int(1 << 16)),
        ("pieces", OwnedValue::Bytes(vec![0; 20])),
    ]))
}

struct Fixture {
    app: Router,
    auth: Arc<Auth>,
    store: Store,
    show: watch::Sender<bool>,
    _dir: tempfile::TempDir,
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("t.db");
    let mut store = Store::open(&db, StoreConfig::default()).unwrap();
    let raws: Vec<Vec<u8>> = TORRENTS.iter().map(|(n, _)| raw(n)).collect();
    let infos: Vec<Info> = raws.iter().map(|r| Info::parse(r).unwrap()).collect();
    let batch: Vec<NewTorrent<'_>> = infos
        .iter()
        .zip(&raws)
        .zip(TORRENTS)
        .enumerate()
        .map(|(i, ((info, raw), (_, extra_flags)))| NewTorrent {
            info,
            raw_info: Some(raw),
            discovered_at: 1_700_000_000 + i as i64,
            metadata_at: 1_700_000_100,
            extra_flags,
            source: SourceId::new(0).unwrap(),
        })
        .collect();
    store.insert(&batch).unwrap();
    for id in 1..=TORRENTS.len() as i64 {
        let row = store.torrent(id).unwrap().unwrap();
        let tree = store.files(id).unwrap().unwrap();
        let files = lodestar_classify::file_infos(&tree.files, tree.encoding);
        let (e, _) =
            lodestar_filter::reclassify::enrich_corrected(&row.name, &files, Correction::default());
        store.set_enrichments(&[(id, e)]).unwrap();
    }
    let index = SearchIndex::in_memory().unwrap();
    let mut w = index.writer(15 << 20).unwrap();
    w.catch_up(&mut store, 100).unwrap();
    w.commit().unwrap();
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
            adult: AdultVisibility::follow(move || *rx.borrow()),
            ops: None,
            insights: None,
            user_data: None,
            hot: None,
            events: None,
        }),
    );
    Fixture {
        app,
        auth,
        store,
        show,
        _dir: dir,
    }
}

async fn call(app: &Router, mut req: Request<Body>) -> (StatusCode, Option<String>, Value) {
    req.extensions_mut()
        .insert(ConnectInfo(SocketAddr::from(([10, 0, 7, 1], 1))));
    let resp = app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let cookie = resp
        .headers()
        .get(header::SET_COOKIE)
        .map(|v| v.to_str().unwrap().split(';').next().unwrap().to_string());
    let bytes = axum::body::to_bytes(resp.into_body(), 1 << 22)
        .await
        .unwrap();
    (
        status,
        cookie,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
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

async fn get(f: &Fixture, c: &str, uri: &str) -> Value {
    let req = Request::get(uri)
        .header(header::COOKIE, c)
        .body(Body::empty())
        .unwrap();
    let (status, _, body) = call(&f.app, req).await;
    assert_eq!(status, StatusCode::OK, "{uri}: {body}");
    body
}

/// The search, again until every clause is counted: counts share a
/// 20 ms deadline, which a loaded runner can miss (a missed count is
/// absent). Gives up after 10 s with what it saw.
async fn search(f: &Fixture, c: &str, q: &str, more: &str) -> Value {
    let q: String = form_urlencoded::byte_serialize(q.as_bytes()).collect();
    let uri = format!("/api/v1/search?q={q}{more}");
    let until = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        let body = get(f, c, &uri).await;
        let missing = body["recovery"]["clauses"]
            .as_array()
            .is_some_and(|cs| cs.iter().any(|c| c.get("total").is_none()));
        if !missing {
            return body;
        }
        assert!(
            std::time::Instant::now() < until,
            "{uri}: still uncounted: {body}"
        );
        tokio::task::yield_now().await;
    }
}

/// `(clause, total)` per counted clause, in order.
fn counts(body: &Value) -> Vec<(String, Option<u64>)> {
    body["recovery"]["clauses"]
        .as_array()
        .unwrap_or_else(|| panic!("no recovery: {body}"))
        .iter()
        .map(|c| (c["clause"].as_str().unwrap().into(), c["total"].as_u64()))
        .collect()
}

fn pairs(of: &[(&str, u64)]) -> Vec<(String, Option<u64>)> {
    of.iter().map(|(c, n)| ((*c).into(), Some(*n))).collect()
}

#[tokio::test]
async fn an_empty_result_counts_each_clause_removed() {
    let f = fixture();
    let c = session(&f).await;
    let body = search(&f, &c, "movie nosuchword 1080p", "").await;
    assert_eq!(body["total"], 0);
    assert_eq!(
        counts(&body),
        pairs(&[("movie", 0), ("nosuchword", 1), ("1080p", 0)])
    );
    assert_eq!(body["recovery"]["skipped"], 0);
    // Each clause's `without` is the query that count was of: searching
    // it gives the same total, and its index removes that clause.
    for (i, clause) in body["recovery"]["clauses"]
        .as_array()
        .unwrap()
        .iter()
        .enumerate()
    {
        assert_eq!(clause["index"], i);
        assert_eq!(clause["total_approx"], false);
        let req = Request::post("/api/v1/search")
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::COOKIE, &c)
            .body(Body::from(json!({ "ast": clause["without"] }).to_string()))
            .unwrap();
        let (status, _, again) = call(&f.app, req).await;
        assert_eq!(status, StatusCode::OK, "{again}");
        assert_eq!(again["total"], clause["total"], "{clause}");
        assert_eq!(again["canonical"], clause["canonical"]);
    }
    // Results, a single clause, or a later page: no recovery.
    for (q, more) in [("movie", ""), ("nosuchword", ""), ("movie 1080p", "")] {
        let body = search(&f, &c, q, more).await;
        assert!(body.get("recovery").is_none(), "{q}: {body}");
    }
    let body = search(&f, &c, "movie nosuchword", "&offset=1").await;
    assert!(body.get("recovery").is_none(), "{body}");
    // Past eight clauses, free text is left out first; the filter counts.
    let body = search(&f, &c, "xa xb xc xd xe xf xg xh movie -size:>1TB", "").await;
    assert_eq!(body["recovery"]["skipped"], 2);
    let shown: Vec<u64> = body["recovery"]["clauses"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["index"].as_u64().unwrap())
        .collect();
    assert_eq!(shown, [0, 1, 2, 3, 4, 5, 6, 9]);
}

#[tokio::test]
async fn hidden_torrents_never_show_in_a_suggestion() {
    let f = fixture();
    let c = session(&f).await;
    // `secret` matches only the torrent a filter rule hid.
    let body = search(&f, &c, "secret nosuchword", "").await;
    assert_eq!(counts(&body), pairs(&[("secret", 0), ("nosuchword", 0)]));
    // Removing the clause that kept hidden torrents out doesn't bring
    // them back: the reduced query leaves them out as search would.
    let body = search(&f, &c, "secret -hidden:true", "").await;
    assert_eq!(body["total"], 0);
    assert_eq!(counts(&body), pairs(&[("secret", 2), ("-hidden:yes", 0)]));
    // Asking for hidden torrents (`hidden=true`) counts them.
    let body = search(&f, &c, "secret nosuchword", "&hidden=true").await;
    assert_eq!(counts(&body), pairs(&[("secret", 0), ("nosuchword", 1)]));
}

#[tokio::test]
async fn adult_never_shows_in_a_suggestion_while_hidden() {
    let f = fixture();
    let c = session(&f).await;
    // Every match is Adult: no clause's removal counts any of it, the
    // explicit `type:adult` included.
    for q in ["scene nosuchword", "type:adult scene", "type:adult -movie"] {
        let body = search(&f, &c, q, "").await;
        assert_eq!(body["total"], 0, "{q}");
        assert!(
            counts(&body).iter().all(|(_, n)| *n == Some(0)),
            "{q}: {body}"
        );
    }
    // Neither does the filter-hidden one with `hidden=true`.
    let body = search(&f, &c, "type:adult scene", "&hidden=true").await;
    assert!(counts(&body).iter().all(|(_, n)| *n == Some(0)), "{body}");
    // Shown, the same queries count it.
    f.show.send(true).unwrap();
    let body = search(&f, &c, "scene nosuchword", "").await;
    assert_eq!(counts(&body), pairs(&[("scene", 0), ("nosuchword", 2)]));
}

#[tokio::test]
async fn index_state_reads_size_growth_and_cold_start() {
    let mut f = fixture();
    let c = session(&f).await;
    let body = get(&f, &c, "/api/v1/search/state").await;
    assert_eq!(body["searchable_torrents"], 5);
    assert_eq!(body["cold_start"], true);
    assert_eq!(body["growing"], true);
    assert_eq!(body["rebuild"], Value::Null);
    assert_eq!(body["unavailable"], json!([]));
    // Nothing measured: null, not 0.
    assert_eq!(body["growth_per_hour"], Value::Null);
    assert_eq!(body["growth_hours"], 0.0);
    // An hour and a half measured over two hours: 45 made searchable.
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    let hour = now.div_euclid(3600);
    let counter = |metric, hour, dim, value| Counter {
        hour,
        metric,
        dim,
        key: 0,
        value,
    };
    // An import (key 1) in a measured hour isn't growth.
    let import = Counter {
        key: 1,
        ..counter(10, hour - 1, 4, 100_000)
    };
    f.store
        .add_counters(&[
            counter(12, hour - 2, 0, 3600),
            counter(12, hour - 1, 0, 1800),
            // The crawl dim isn't the index measured.
            counter(12, hour - 3, 1, 3600),
            counter(10, hour - 1, 4, 30),
            counter(10, hour - 2, 2, 15),
            import,
            // Made searchable while the index wasn't measured.
            counter(10, hour - 5, 2, 700),
            // Older than 24 hours.
            counter(12, hour - 30, 0, 3600),
            counter(10, hour - 30, 2, 500),
        ])
        .unwrap();
    let body = get(&f, &c, "/api/v1/search/state").await;
    assert_eq!(body["growth_hours"], 1.5);
    assert_eq!(body["growth_per_hour"], 30.0);
}
