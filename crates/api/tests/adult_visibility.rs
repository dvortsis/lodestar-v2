//! "Show adult content" (§20.9, B17): off by default, search, counts,
//! cursors and detail by hash leave Adult out unless it's on, explicit
//! adult queries get the `adult_hidden` notice instead of hits, and
//! visibility follows the effective class (your Is adult / Not adult
//! corrections), live.
//!
//! Responses in `golden/adult_*.txt` are compared byte for byte, with
//! `took_ms` zeroed and `next_cursor` as `"…"`.
//! `LODESTAR_BLESS=1 cargo test -p lodestar-api --test adult_visibility`
//! rewrites them; review the diff before committing it.

use std::net::SocketAddr;
use std::path::PathBuf;
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
use lodestar_store::{NewTorrent, SourceId, Store, StoreConfig};
use serde_json::{Value, json};
use tokio::sync::watch;
use tower::ServiceExt;

const PASSWORD: &str = "correct horse battery";

/// The torrents, in id order: two detected adult, three not.
const NAMES: [&str; 5] = [
    "Studio.Scene.XXX.1080p.mp4",
    "Another.Clip.XXX.720p.mp4",
    "Big.Movie.2019.1080p.mkv",
    "Other.Movie.2018.720p.mkv",
    "Third.Movie.2017.2160p.mkv",
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
    index: SearchIndex,
    /// "Show adult content", as the settings registry publishes it.
    show: watch::Sender<bool>,
    hashes: Vec<String>,
    _dir: tempfile::TempDir,
}

impl Fixture {
    /// Classifies torrent `i` again with `fix`, as `/classify/correct`
    /// does, without indexing it.
    fn correct(&mut self, i: usize, adult: bool) {
        let id = i as i64 + 1;
        let row = self.store.torrent(id).unwrap().unwrap();
        let tree = self.store.files(id).unwrap().unwrap();
        let files = lodestar_classify::file_infos(&tree.files, tree.encoding);
        let fix = Correction {
            adult: Some(adult),
            not_junk: false,
        };
        let (e, _) = lodestar_filter::reclassify::enrich_corrected(&row.name, &files, fix);
        self.store.set_enrichments(&[(id, e)]).unwrap();
    }

    /// What the live indexer does at its next commit.
    fn reindex(&mut self) {
        let mut w = self.index.writer(15 << 20).unwrap();
        w.catch_up(&mut self.store, 100).unwrap();
        w.commit().unwrap();
    }
}

fn fixture() -> Fixture {
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
            extra_flags: 0,
            source: SourceId::new(0).unwrap(),
        })
        .collect();
    store.insert(&batch).unwrap();
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
    let index = SearchIndex::in_memory().unwrap();
    let (show, rx) = watch::channel(false);
    let reader = Store::open_reader(&db, StoreConfig::default()).unwrap();
    let auth = Arc::new(Auth::load(&dir.path().join("auth.json"), false).unwrap());
    let app = Router::new().nest(
        "/api/v1",
        router(ApiState {
            auth: auth.clone(),
            index: Some(index.clone().into()),
            reader: Arc::new(Mutex::new(reader)),
            filters: None,
            settings: None,
            adult: AdultVisibility::follow(move || *rx.borrow()),
            ops: None,
            insights: None,
            user_data: None,
        }),
    );
    let mut f = Fixture {
        app,
        auth,
        store,
        index,
        show,
        hashes,
        _dir: dir,
    };
    // Classified, as the pipeline would have.
    for i in 0..NAMES.len() {
        let id = i as i64 + 1;
        let row = f.store.torrent(id).unwrap().unwrap();
        let tree = f.store.files(id).unwrap().unwrap();
        let files = lodestar_classify::file_infos(&tree.files, tree.encoding);
        let (e, _) =
            lodestar_filter::reclassify::enrich_corrected(&row.name, &files, Correction::default());
        f.store.set_enrichments(&[(id, e)]).unwrap();
    }
    f.reindex();
    f
}

async fn call(app: &Router, mut req: Request<Body>) -> (StatusCode, Option<String>, String) {
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
    (status, cookie, String::from_utf8(bytes.to_vec()).unwrap())
}

async fn json_call(app: &Router, req: Request<Body>) -> (StatusCode, Value) {
    let (status, _, body) = call(app, req).await;
    (status, serde_json::from_str(&body).unwrap_or(Value::Null))
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

async fn session(f: &Fixture) -> String {
    let token = f.auth.setup_token().unwrap();
    let body = json!({ "token": token, "password": PASSWORD });
    let (status, cookie, _) = call(&f.app, post("/api/v1/setup", None, body)).await;
    assert_eq!(status, StatusCode::OK);
    cookie.unwrap()
}

/// `took_ms` zeroed, `next_cursor` as `"…"`.
fn normalize(body: &str) -> String {
    let mut v: Value = match serde_json::from_str(body) {
        Ok(v) => v,
        Err(_) => return body.to_string(),
    };
    if let Some(o) = v.as_object_mut() {
        if o.contains_key("took_ms") {
            o.insert("took_ms".into(), json!(0));
        }
        if o.get("next_cursor").is_some_and(Value::is_string) {
            o.insert("next_cursor".into(), json!("…"));
        }
    }
    v.to_string()
}

/// Sends `req`, checks status and body against `golden/{name}.txt`.
async fn golden(f: &Fixture, name: &str, req: Request<Body>) -> Value {
    let (status, _, body) = call(&f.app, req).await;
    let got = format!("{}\n{}\n", status.as_u16(), normalize(&body));
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

fn names(body: &Value) -> Vec<String> {
    let mut out: Vec<String> = body["hits"]
        .as_array()
        .unwrap()
        .iter()
        .map(|h| h["name"].as_str().unwrap().to_string())
        .collect();
    out.sort();
    out
}

fn sorted(of: &[usize]) -> Vec<String> {
    let mut out: Vec<String> = of.iter().map(|&i| NAMES[i].to_string()).collect();
    out.sort();
    out
}

fn encode(s: &str) -> String {
    form_urlencoded::byte_serialize(s.as_bytes()).collect()
}

#[tokio::test]
async fn adult_is_hidden_by_default_in_results_and_counts() {
    let f = fixture();
    let c = session(&f).await;
    let body = golden(&f, "adult_hidden_browse", get("/api/v1/search", &c)).await;
    assert_eq!(names(&body), sorted(&[2, 3, 4]));
    assert_eq!(body["total"], 3);
    assert_eq!(body["notices"], json!([]));
    // Exact counts run the same query.
    let (status, body) = json_call(&f.app, get("/api/v1/search?exact=1", &c)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        (body["total"].clone(), body["total_approx"].clone()),
        (json!(3), json!(false))
    );
    // A word only adult torrents match.
    let (_, body) = json_call(&f.app, get("/api/v1/search?q=xxx", &c)).await;
    assert_eq!(body["total"], 0);
    assert_eq!(body["notices"], json!([]));
    // `hidden=true` brings back filter-hidden torrents, never Adult.
    let (_, body) = json_call(&f.app, get("/api/v1/search?hidden=true", &c)).await;
    assert_eq!(names(&body), sorted(&[2, 3, 4]));
}

#[tokio::test]
async fn explicit_adult_queries_get_no_hits_and_a_notice() {
    let f = fixture();
    let c = session(&f).await;
    let body = golden(
        &f,
        "adult_hidden_type_query",
        get("/api/v1/search?q=type:adult", &c),
    )
    .await;
    assert_eq!(
        (body["total"].clone(), body["hits"].clone()),
        (json!(0), json!([]))
    );
    assert_eq!(body["notices"], json!(["adult_hidden"]));
    for q in [
        "adult:scene",
        "adult:scene,movie",
        "type:adult | type:movie",
        "xxx type:adult",
    ] {
        let (status, body) =
            json_call(&f.app, get(&format!("/api/v1/search?q={}", encode(q)), &c)).await;
        assert_eq!(status, StatusCode::OK, "{q}");
        assert_eq!(body["notices"], json!(["adult_hidden"]), "{q}");
        assert!(
            body["hits"]
                .as_array()
                .unwrap()
                .iter()
                .all(|h| h["content_type"] != "adult"),
            "{q}: {body}"
        );
    }
    let (_, body) = json_call(
        &f.app,
        get("/api/v1/search?q=type:adult%20%7C%20type:movie", &c),
    )
    .await;
    assert_eq!(names(&body), sorted(&[2, 3, 4]));
    // Leaving Adult out yourself isn't asking for it.
    let (_, body) = json_call(&f.app, get("/api/v1/search?q=-type:adult", &c)).await;
    assert_eq!(body["notices"], json!([]));
    assert_eq!(body["total"], 3);
    // An AST asks the same way.
    let (_, parsed) = json_call(&f.app, get("/api/v1/parse?q=adult:vr", &c)).await;
    let ast = json!({ "ast": parsed["ast"] });
    let (status, body) = json_call(&f.app, post("/api/v1/search", Some(&c), ast)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["notices"], json!(["adult_hidden"]));
    assert_eq!(body["total"], 0);
}

#[tokio::test]
async fn turning_it_on_shows_adult_and_off_hides_it_again_live() {
    let f = fixture();
    let c = session(&f).await;
    f.show.send(true).unwrap();
    let body = golden(
        &f,
        "adult_shown_type_query",
        get("/api/v1/search?q=type:adult", &c),
    )
    .await;
    assert_eq!(names(&body), sorted(&[0, 1]));
    assert_eq!(body["notices"], json!([]));
    let (_, body) = json_call(&f.app, get("/api/v1/search", &c)).await;
    assert_eq!(body["total"], 5);
    let (_, body) = json_call(&f.app, get("/api/v1/search?q=adult:movie", &c)).await;
    assert_eq!(body["total"], 2);
    f.show.send(false).unwrap();
    let (_, body) = json_call(&f.app, get("/api/v1/search", &c)).await;
    assert_eq!(names(&body), sorted(&[2, 3, 4]));
    let (_, body) = json_call(&f.app, get("/api/v1/search?q=type:adult", &c)).await;
    assert_eq!(body["total"], 0);
    assert_eq!(body["notices"], json!(["adult_hidden"]));
}

#[tokio::test]
async fn a_cursor_from_before_a_toggle_is_refused() {
    let f = fixture();
    let c = session(&f).await;
    for (before, after) in [(false, true), (true, false)] {
        f.show.send(before).unwrap();
        let (status, first) =
            json_call(&f.app, get("/api/v1/search?sort=-discovered&limit=1", &c)).await;
        assert_eq!(status, StatusCode::OK);
        let cursor = first["next_cursor"].as_str().unwrap().to_string();
        let next = format!(
            "/api/v1/search?sort=-discovered&limit=1&cursor={}",
            encode(&cursor)
        );
        // Unchanged, it reads on.
        let (status, _) = json_call(&f.app, get(&next, &c)).await;
        assert_eq!(status, StatusCode::OK);
        f.show.send(after).unwrap();
        let (status, body) = json_call(&f.app, get(&next, &c)).await;
        assert_eq!(
            status,
            StatusCode::BAD_REQUEST,
            "{before} → {after}: {body}"
        );
        assert_eq!(body["error"]["code"], "bad_cursor");
        assert_eq!(body["error"]["field"], "cursor");
    }
    f.show.send(false).unwrap();
    let (_, first) = json_call(&f.app, get("/api/v1/search?sort=-discovered&limit=1", &c)).await;
    let cursor = first["next_cursor"].as_str().unwrap().to_string();
    f.show.send(true).unwrap();
    golden(
        &f,
        "adult_cursor_after_toggle",
        get(
            &format!(
                "/api/v1/search?sort=-discovered&limit=1&cursor={}",
                encode(&cursor)
            ),
            &c,
        ),
    )
    .await;
}

#[tokio::test]
async fn detail_by_hash_refuses_adult_while_hidden() {
    let f = fixture();
    let c = session(&f).await;
    let adult = format!("/api/v1/torrents/{}", f.hashes[0]);
    let body = golden(&f, "adult_detail_hidden", get(&adult, &c)).await;
    assert_eq!(body["error"]["code"], "adult_hidden");
    let (status, body) = json_call(&f.app, get(&format!("{adult}/files"), &c)).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"]["code"], "adult_hidden");
    let (status, _) = json_call(
        &f.app,
        get(&format!("/api/v1/torrents/{}", f.hashes[2]), &c),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let missing = format!("/api/v1/torrents/{}", "ab".repeat(20));
    let (status, body) = json_call(&f.app, get(&missing, &c)).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"]["code"], "torrent_not_found");
    f.show.send(true).unwrap();
    let (status, body) = json_call(&f.app, get(&adult, &c)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["name"], NAMES[0]);
    let (status, _) = json_call(&f.app, get(&format!("{adult}/files"), &c)).await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn visibility_follows_your_corrections() {
    let mut f = fixture();
    let c = session(&f).await;
    // Is adult on a movie; Not adult on a detected-adult torrent.
    f.correct(3, true);
    f.correct(1, false);
    // Before the index commit, the stored class already keeps the
    // corrected-adult torrent out of results and detail.
    let (_, body) = json_call(&f.app, get("/api/v1/search", &c)).await;
    assert!(!names(&body).contains(&NAMES[3].to_string()), "{body}");
    let (status, _) = json_call(
        &f.app,
        get(&format!("/api/v1/torrents/{}", f.hashes[3]), &c),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = json_call(
        &f.app,
        get(&format!("/api/v1/torrents/{}", f.hashes[1]), &c),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    // At the next commit (the live indexer's, every couple of seconds),
    // the index follows: no rebuild needed.
    f.reindex();
    let (_, body) = json_call(&f.app, get("/api/v1/search", &c)).await;
    assert_eq!(names(&body), sorted(&[1, 2, 4]));
    assert_eq!(body["total"], 3);
    f.show.send(true).unwrap();
    let (_, body) = json_call(&f.app, get("/api/v1/search?q=type:adult", &c)).await;
    assert_eq!(names(&body), sorted(&[0, 3]));
}

/// One facet's values, `value → count`.
fn facet(body: &Value, name: &str) -> Vec<(String, u64)> {
    body["facets"][name]["values"]
        .as_array()
        .map(|vs| {
            vs.iter()
                .map(|v| {
                    (
                        v["value"].as_str().unwrap().to_owned(),
                        v["count"].as_u64().unwrap(),
                    )
                })
                .collect()
        })
        .unwrap_or_default()
}

#[tokio::test]
async fn facets_count_what_search_shows_in_both_states() {
    let f = fixture();
    let c = session(&f).await;
    for shown in [false, true] {
        f.show.send(shown).unwrap();
        for q in ["", "movie", "type:movie", "type:movie,tv", "adult:movie"] {
            let (status, search) =
                json_call(&f.app, get(&format!("/api/v1/search?q={}", encode(q)), &c)).await;
            assert_eq!(status, StatusCode::OK, "{q}");
            let (status, body) =
                json_call(&f.app, get(&format!("/api/v1/facets?q={}", encode(q)), &c)).await;
            assert_eq!(status, StatusCode::OK, "{q}: {body}");
            assert_eq!(body["total"], search["total"], "shown={shown} {q}");
            let types = facet(&body, "type");
            if shown {
                // Disjunctive: a `type:` selection still counts the
                // Adult torrents the rest of the query matches.
                if q.is_empty() || q.starts_with("type:") {
                    assert!(types.contains(&("adult".into(), 2)), "{q}: {types:?}");
                }
            } else {
                // Hidden: no `adult` under `type`, listed or counted, and
                // no adult group, whatever the query selects.
                assert!(types.iter().all(|(v, _)| v != "adult"), "{q}: {types:?}");
                assert!(body["facets"].get("adult").is_none(), "{q}: {body}");
                assert!(
                    !body["unavailable"]
                        .as_array()
                        .unwrap()
                        .contains(&json!("adult"))
                );
            }
        }
        // The adult group: there exactly when Adult is shown.
        let (_, body) = json_call(&f.app, get("/api/v1/facets", &c)).await;
        let kinds = facet(&body, "adult");
        if shown {
            let (_, search) = json_call(&f.app, get("/api/v1/search?q=adult:movie", &c)).await;
            let movie = search["total"].as_u64().unwrap();
            assert!(movie > 0);
            assert!(kinds.contains(&("movie".into(), movie)), "{kinds:?}");
        } else {
            assert!(kinds.is_empty());
        }
    }
}

#[tokio::test]
async fn the_adult_facet_follows_your_corrections() {
    let mut f = fixture();
    let c = session(&f).await;
    // Not adult on a detected-adult torrent: its class is a movie's now,
    // whatever kind the classifier gave it.
    f.correct(1, false);
    f.reindex();
    f.show.send(true).unwrap();
    let (_, body) = json_call(&f.app, get("/api/v1/facets", &c)).await;
    let types = facet(&body, "type");
    assert!(types.contains(&("adult".into(), 1)), "{types:?}");
    let kinds = facet(&body, "adult");
    let in_class: u64 = kinds.iter().map(|(_, n)| n).sum();
    assert_eq!(in_class, 1, "only the Adult-class torrent: {kinds:?}");
    // Selecting a kind counts within the class too.
    let (_, body) = json_call(&f.app, get("/api/v1/facets?q=adult:movie", &c)).await;
    let kinds = facet(&body, "adult");
    assert!(kinds.iter().all(|(_, n)| *n <= 1), "{kinds:?}");
}

#[tokio::test]
async fn a_facets_lease_from_before_a_toggle_is_refused() {
    let f = fixture();
    let c = session(&f).await;
    for (before, after) in [(false, true), (true, false)] {
        f.show.send(before).unwrap();
        let (_, first) = json_call(&f.app, get("/api/v1/search?limit=1", &c)).await;
        let lease = first["next_cursor"].as_str().unwrap().to_string();
        let uri = format!("/api/v1/facets?lease={}", encode(&lease));
        let (status, body) = json_call(&f.app, get(&uri, &c)).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["total"], first["total"]);
        f.show.send(after).unwrap();
        let (status, body) = json_call(&f.app, get(&uri, &c)).await;
        assert_eq!(
            status,
            StatusCode::BAD_REQUEST,
            "{before} → {after}: {body}"
        );
        assert_eq!(body["error"]["code"], "bad_cursor");
        assert_eq!(body["error"]["field"], "lease");
    }
}

#[tokio::test]
async fn unauthenticated_requests_get_401() {
    let f = fixture();
    let _ = session(&f).await;
    for uri in [
        "/api/v1/search?q=type:adult".to_string(),
        format!("/api/v1/torrents/{}", f.hashes[0]),
    ] {
        let req = Request::get(&uri).body(Body::empty()).unwrap();
        let (status, body) = json_call(&f.app, req).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{uri}");
        assert!(!body.to_string().contains("adult_hidden"));
    }
}
