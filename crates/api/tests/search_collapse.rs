//! `/search` with `collapse=group` (§8 "show best version, +N others",
//! GS-3): one row per group of versions with `others`, groups and
//! torrents counted, collapsed cursors that never repeat or skip a group
//! while torrents arrive, the ungrouped fallback while group data isn't
//! ready, and cursors of either kind kept to their own requests. Also
//! the errors for its parameters: a repeated one is named, and an
//! unknown value is echoed short and printable.

use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Request, StatusCode, header};
use lodestar_api::{ApiState, Auth, router};
use lodestar_bencode::{OwnedValue, to_vec};
use lodestar_metainfo::Info;
use lodestar_search::{Generations, SearchIndex};
use lodestar_store::{Enriched, NewTorrent, SourceId, Store, StoreConfig};
use serde_json::{Value, json};
use tower::ServiceExt;

const PASSWORD: &str = "correct horse battery";
/// Distinct works; each torrent is one version of one.
const WORKS: usize = 9;

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

/// Torrent `k`: a version of work `k % WORKS`, in one of a few releases.
fn name(k: usize) -> String {
    let work = [
        "Alpha", "Bravo", "Charlie", "Delta", "Echo", "Foxtrot", "Golf", "Hotel", "India",
    ][k % WORKS];
    let res = ["720p", "1080p", "2160p"][(k / WORKS) % 3];
    format!("{work}.Movie.2010.{res}.BluRay.x264-GRP{k}")
}

struct Fixture {
    app: Router,
    auth: Arc<Auth>,
    store: Store,
    index: SearchIndex,
    added: usize,
    _dir: tempfile::TempDir,
}

impl Fixture {
    /// Stores, classifies and indexes `n` more torrents.
    fn add(&mut self, n: usize) {
        let raws: Vec<Vec<u8>> = (self.added..self.added + n)
            .map(|k| raw(&name(k)))
            .collect();
        let infos: Vec<Info> = raws.iter().map(|r| Info::parse(r).unwrap()).collect();
        let batch: Vec<NewTorrent<'_>> = infos
            .iter()
            .zip(&raws)
            .map(|(info, raw)| NewTorrent {
                info,
                raw_info: Some(raw),
                discovered_at: 1_700_000_000 + self.added as i64,
                metadata_at: 1_700_000_001,
                extra_flags: 0,
                source: SourceId::new(0).unwrap(),
            })
            .collect();
        let ids: Vec<i64> = self
            .store
            .insert(&batch)
            .unwrap()
            .into_iter()
            .map(|i| i.id())
            .collect();
        let enriched: Vec<(i64, Enriched)> = ids
            .iter()
            .map(|&id| {
                let row = self.store.torrent(id).unwrap().unwrap();
                let tree = self.store.files(id).unwrap().unwrap();
                let files = lodestar_classify::file_infos(&tree.files, tree.encoding);
                let c = lodestar_classify::classify(&row.name, &files);
                let e = Enriched {
                    version: lodestar_classify::ENRICHMENT_VERSION,
                    content_type: c.content_type.code(),
                    junk_score: c.junk.score,
                    comp: c.composition.encode(),
                    data: lodestar_classify::Record::new(&c).encode(),
                };
                (id, e)
            })
            .collect();
        self.store.set_enrichments(&enriched).unwrap();
        let mut w = self.index.writer(15 << 20).unwrap();
        w.catch_up(&mut self.store, 10_000).unwrap();
        w.commit().unwrap();
        self.added += n;
    }
}

fn fixture_with(n: usize, index: SearchIndex) -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("t.db");
    let mut store = Store::open(&db, StoreConfig::default()).unwrap();
    store.register_source("dht", "1", 1).unwrap();
    let auth = Arc::new(Auth::load(&dir.path().join("auth.json"), false).unwrap());
    let reader = Store::open_reader(&db, StoreConfig::default()).unwrap();
    let app = Router::new().nest(
        "/api/v1",
        router(ApiState {
            auth: auth.clone(),
            index: Some(index.clone().into()),
            reader: Arc::new(Mutex::new(reader)),
            filters: None,
            settings: None,
            ops: None,
            user_data: None,
            hot: None,
            events: None,
            adult: Default::default(),
            insights: None,
        }),
    );
    let mut f = Fixture {
        app,
        auth,
        store,
        index,
        added: 0,
        _dir: dir,
    };
    f.add(n);
    f
}

fn fixture(n: usize) -> Fixture {
    fixture_with(n, SearchIndex::in_memory().unwrap())
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
    (
        status,
        cookie,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

fn get(uri: &str, cookie: &str) -> Request<Body> {
    Request::get(uri)
        .header(header::COOKIE, cookie)
        .body(Body::empty())
        .unwrap()
}

fn post(uri: &str, cookie: Option<&str>, body: Value) -> Request<Body> {
    let mut b = Request::post(uri)
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::ORIGIN, "http://localhost")
        .header(header::HOST, "localhost");
    if let Some(c) = cookie {
        b = b.header(header::COOKIE, c);
    }
    b.body(Body::from(body.to_string())).unwrap()
}

async fn session(f: &Fixture) -> String {
    let (status, cookie, _) = match f.auth.setup_token() {
        Some(token) => {
            let body = json!({ "token": token, "password": PASSWORD, "notice_version": lodestar_api::legal::NOTICE_VERSION });
            call(&f.app, post("/api/v1/setup", None, body)).await
        }
        None => {
            let body = json!({ "password": PASSWORD });
            call(&f.app, post("/api/v1/login", None, body)).await
        }
    };
    assert_eq!(status, StatusCode::OK);
    cookie.unwrap()
}

fn hits(body: &Value) -> &Vec<Value> {
    body["hits"].as_array().unwrap()
}

fn encode(s: &str) -> String {
    form_urlencoded::byte_serialize(s.as_bytes()).collect()
}

/// The work a hit's name is a version of.
fn work(hit: &Value) -> String {
    hit["name"]
        .as_str()
        .unwrap()
        .split('.')
        .next()
        .unwrap()
        .to_owned()
}

#[tokio::test]
async fn collapsed_pages_serve_each_group_once_with_its_others_and_counts() {
    let mut f = fixture(90);
    let c = session(&f).await;
    for sort in ["relevance", "-size", "%2Bdiscovered"] {
        // The snapshot this sort's lease takes.
        let n = f.added;
        let base = format!("/api/v1/search?q=movie&sort={sort}&limit=2&collapse=group");
        let (status, _, first) = call(&f.app, get(&base, &c)).await;
        assert_eq!(status, StatusCode::OK, "{first}");
        assert_eq!(first["collapsed"], true);
        assert!(first.get("collapse_unavailable").is_none());
        // 128 groups · 412 torrents: both counted, exactly.
        assert_eq!(first["total"], n);
        assert_eq!(first["total_approx"], false);
        assert_eq!(first["groups"], WORKS);
        assert_eq!(first["groups_approx"], false);
        let mut rows: Vec<Value> = hits(&first).clone();
        let mut next = first["next_cursor"].as_str().map(str::to_owned);
        let mut offset = rows.len();
        while let Some(cursor) = next {
            // Torrents arrive between pages: the lease doesn't move.
            f.add(4);
            let uri = format!("{base}&cursor={}", encode(&cursor));
            let (status, _, page) = call(&f.app, get(&uri, &c)).await;
            assert_eq!(status, StatusCode::OK, "{page}");
            assert_eq!(page["collapsed"], true);
            assert_eq!(page["groups"], WORKS);
            assert_eq!(page["offset"], offset, "offset counts groups");
            offset += hits(&page).len();
            rows.extend(hits(&page).iter().cloned());
            next = page["next_cursor"].as_str().map(str::to_owned);
        }
        let works: Vec<String> = rows.iter().map(work).collect();
        let distinct: HashSet<&String> = works.iter().collect();
        assert_eq!(
            distinct.len(),
            works.len(),
            "{sort}: a group twice: {works:?}"
        );
        assert_eq!(works.len(), WORKS, "{sort}: a group missing");
        // `others` + 1 over the rows is the torrent count, and each row's
        // is its work's versions in the snapshot, less itself.
        let summed: usize = rows
            .iter()
            .map(|r| r["others"].as_u64().unwrap() as usize + 1)
            .sum();
        assert_eq!(summed, n);
        let works = [
            "Alpha", "Bravo", "Charlie", "Delta", "Echo", "Foxtrot", "Golf", "Hotel", "India",
        ];
        for r in &rows {
            let w = works.iter().position(|x| *x == work(r)).unwrap();
            let versions = (0..n).filter(|k| k % WORKS == w).count();
            assert_eq!(r["others"], versions - 1, "{sort}: {r}");
        }
    }
}

#[tokio::test]
async fn the_row_is_the_best_version_under_the_sort() {
    let f = fixture(45);
    let c = session(&f).await;
    let (_, _, all) = call(
        &f.app,
        get("/api/v1/search?q=movie&sort=%2Bdiscovered&limit=200", &c),
    )
    .await;
    let mut first: HashMap<String, i64> = HashMap::new();
    for h in hits(&all) {
        first.entry(work(h)).or_insert(h["id"].as_i64().unwrap());
    }
    let (_, _, grouped) = call(
        &f.app,
        get(
            "/api/v1/search?q=movie&sort=%2Bdiscovered&limit=200&collapse=group",
            &c,
        ),
    )
    .await;
    assert_eq!(hits(&grouped).len(), WORKS);
    for h in hits(&grouped) {
        assert_eq!(h["id"].as_i64().unwrap(), first[&work(h)], "{h}");
    }
    // Ungrouped hits carry no `others`, and the response no group fields.
    assert!(hits(&all).iter().all(|h| h.get("others").is_none()));
    for k in [
        "collapsed",
        "groups",
        "groups_approx",
        "collapse_unavailable",
    ] {
        assert!(all.get(k).is_none(), "{k}");
    }
}

#[tokio::test]
async fn post_takes_collapse_too_and_rejects_unknown_values() {
    let f = fixture(27);
    let c = session(&f).await;
    let (status, _, body) = call(
        &f.app,
        post(
            "/api/v1/search",
            Some(&c),
            json!({ "q": "movie", "collapse": "group" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["collapsed"], true);
    assert_eq!(hits(&body).len(), WORKS);
    let (status, _, body) = call(&f.app, get("/api/v1/search?q=movie&collapse=title", &c)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["code"], "unknown_collapse", "{body}");
    let (status, _, body) = call(
        &f.app,
        get("/api/v1/search?q=movie&collapse=group&explain=1", &c),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["code"], "explain_collapse", "{body}");
    // `none` is today's search.
    let (_, _, none) = call(&f.app, get("/api/v1/search?q=movie&collapse=none", &c)).await;
    let (_, _, plain) = call(&f.app, get("/api/v1/search?q=movie", &c)).await;
    assert_eq!(hits(&none), hits(&plain));
    assert!(none.get("collapsed").is_none());
}

#[tokio::test]
async fn collapse_takes_group_none_or_nothing_and_names_them_otherwise() {
    let f = fixture(27);
    let c = session(&f).await;
    let (status, _, group) = call(&f.app, get("/api/v1/search?q=movie&collapse=group", &c)).await;
    assert_eq!(status, StatusCode::OK, "{group}");
    assert_eq!(group["collapsed"], true, "{group}");
    assert_eq!(hits(&group).len(), WORKS);
    for uri in [
        "/api/v1/search?q=movie&collapse=none",
        "/api/v1/search?q=movie",
    ] {
        let (status, _, body) = call(&f.app, get(uri, &c)).await;
        assert_eq!(status, StatusCode::OK, "{uri}: {body}");
        assert!(body.get("collapsed").is_none(), "{uri}: {body}");
        assert!(hits(&body).len() > WORKS, "{uri}: {body}");
    }
    // An unknown value, by GET or POST: the same 400 as an unknown sort,
    // naming the parameter and the values it takes.
    let (status, _, get_body) =
        call(&f.app, get("/api/v1/search?q=movie&collapse=bogus", &c)).await;
    let (post_status, _, post_body) = call(
        &f.app,
        post(
            "/api/v1/search",
            Some(&c),
            json!({ "q": "movie", "collapse": "bogus" }),
        ),
    )
    .await;
    // Present but empty isn't the default: it's a value, and unknown.
    let (empty_status, _, empty_body) =
        call(&f.app, get("/api/v1/search?q=movie&collapse=", &c)).await;
    for (status, body, value) in [
        (status, &get_body, "bogus"),
        (post_status, &post_body, "bogus"),
        (empty_status, &empty_body, ""),
    ] {
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        let e = &body["error"];
        assert_eq!(e["code"], "unknown_collapse", "{body}");
        assert_eq!(e["field"], "collapse", "{body}");
        assert_eq!(
            e["message"],
            format!("Unknown collapse {value:?}: use group or none."),
            "{body}"
        );
    }
    let (_, _, sort) = call(&f.app, get("/api/v1/search?q=movie&sort=bogus", &c)).await;
    let keys = |v: &Value| {
        let mut k: Vec<String> = v["error"].as_object().unwrap().keys().cloned().collect();
        k.sort();
        k
    };
    assert_eq!(keys(&get_body), keys(&sort), "{get_body} vs {sort}");
}

#[tokio::test]
async fn a_repeated_parameter_is_named() {
    let f = fixture(9);
    let c = session(&f).await;
    for (name, value) in [
        ("q", "movie"),
        ("sort", "size"),
        ("limit", "5"),
        ("offset", "0"),
        ("hidden", "true"),
        ("tz", "0"),
        ("cursor", "x"),
        ("collapse", "group"),
        ("exact", "1"),
        ("explain", "1"),
    ] {
        let uri = format!("/api/v1/search?{name}={value}&q=movie&{name}={value}");
        let (status, _, body) = call(&f.app, get(&uri, &c)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{uri}: {body}");
        let e = &body["error"];
        assert_eq!(e["code"], "repeated_parameter", "{uri}: {body}");
        assert_eq!(e["field"], name, "{uri}: {body}");
        assert_eq!(
            e["message"],
            format!("{name} was sent more than once: send it once."),
            "{uri}"
        );
    }
    // O1: any `ast` on GET, repeated or not, is ast_needs_post, as on /facets.
    for uri in [
        "/api/v1/search?ast=x&ast=y",
        "/api/v1/search?ast=x",
        "/api/v1/search?q=a&ast",
    ] {
        let (status, _, body) = call(&f.app, get(uri, &c)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{uri}: {body}");
        assert_eq!(body["error"]["code"], "ast_needs_post", "{uri}: {body}");
        assert_eq!(body["error"]["field"], "ast", "{uri}: {body}");
    }
    // Signed out, it's still a 401 first: the parameters aren't read.
    let anon = Request::get("/api/v1/search?sort=a&sort=b")
        .body(Body::empty())
        .unwrap();
    let (status, _, body) = call(&f.app, anon).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
    // HEAD, which `get()` also serves, reads the query too: its answer is
    // GET's repeated_parameter refusal (no body, the same length), not the
    // extractor's shorter generic bad_parameter.
    let uri = "/api/v1/search?q=a&q=b";
    let (get_status, get_len) = status_and_length(&f.app, "GET", uri, &c).await;
    let (head_status, head_len) = status_and_length(&f.app, "HEAD", uri, &c).await;
    assert_eq!(get_status, StatusCode::BAD_REQUEST);
    assert_eq!(head_status, StatusCode::BAD_REQUEST);
    assert!(!get_len.is_empty());
    assert_eq!(head_len, get_len);
    // POST reads its body, not the URL's query.
    let (status, _, body) = call(
        &f.app,
        post("/api/v1/search?q=a&q=b", Some(&c), json!({ "q": "movie" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    // Unknown parameters stay ignored, repeated or not.
    let (status, _, body) = call(&f.app, get("/api/v1/search?q=movie&x=1&x=2", &c)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

#[tokio::test]
async fn an_unknown_value_is_echoed_short_and_printable() {
    let f = fixture(9);
    let c = session(&f).await;
    for name in ["sort", "collapse"] {
        // 10 kB: the echo stops at 64 characters and says so.
        let long = "z".repeat(10_240);
        let (status, _, body) = call(
            &f.app,
            get(&format!("/api/v1/search?q=movie&{name}={long}"), &c),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{name}");
        let message = body["error"]["message"].as_str().unwrap();
        let shown = format!("\"{}…\"", "z".repeat(64));
        assert!(message.contains(&shown), "{name}: {message}");
        assert!(message.len() < 300, "{name}: {} bytes", message.len());
        // Control characters (a newline, an escape sequence, a NUL) come
        // back escaped, never raw.
        let raw = "a\n\u{1b}[31mb\u{0}";
        let (status, _, body) = call(
            &f.app,
            get(
                &format!("/api/v1/search?q=movie&{name}={}", encode(raw)),
                &c,
            ),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{name}");
        let message = body["error"]["message"].as_str().unwrap();
        assert!(
            !message.chars().any(char::is_control),
            "{name}: {message:?}"
        );
        assert!(
            message.contains(r#""a\n\u{1b}[31mb\0""#),
            "{name}: {message}"
        );
    }
}

#[tokio::test]
async fn cursors_stay_with_their_own_kind_of_request() {
    let f = fixture(60);
    let c = session(&f).await;
    // An ungrouped cursor (version 1, as before GS-3) still pages an
    // ungrouped search.
    let base = "/api/v1/search?q=movie&limit=5";
    let (_, _, first) = call(&f.app, get(base, &c)).await;
    let plain = encode(first["next_cursor"].as_str().unwrap());
    let (status, _, page) = call(&f.app, get(&format!("{base}&cursor={plain}"), &c)).await;
    assert_eq!(status, StatusCode::OK, "{page}");
    assert_eq!(hits(&page).len(), 5);
    // But not a collapsed one while group data is ready, nor the reverse.
    let (status, _, body) = call(
        &f.app,
        get(&format!("{base}&collapse=group&cursor={plain}"), &c),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["error"]["code"], "bad_cursor");
    assert_eq!(body["error"]["field"], "cursor");
    assert_eq!(
        body["error"]["message"],
        "That cursor belongs to an ungrouped search: send it without collapse=group, or start the search again."
    );
    let grouped = "/api/v1/search?q=movie&limit=2&collapse=group";
    let (_, _, first) = call(&f.app, get(grouped, &c)).await;
    let cursor = encode(first["next_cursor"].as_str().unwrap());
    // A collapsed cursor without its collapse value, and with the other
    // one: the error says which search it belongs to.
    for uri in [
        format!("{base}&cursor={cursor}"),
        format!("{base}&collapse=none&cursor={cursor}"),
    ] {
        let (status, _, body) = call(&f.app, get(&uri, &c)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{uri}: {body}");
        assert_eq!(body["error"]["code"], "bad_cursor", "{uri}");
        assert_eq!(body["error"]["field"], "cursor", "{uri}");
        assert_eq!(
            body["error"]["message"],
            "That cursor belongs to a collapsed search: send collapse=group with it, or start the search again.",
            "{uri}"
        );
    }
    let (status, _, page) = call(&f.app, get(&format!("{grouped}&cursor={cursor}"), &c)).await;
    assert_eq!(status, StatusCode::OK, "{page}");
}

#[tokio::test]
async fn without_group_data_the_search_answers_ungrouped_and_says_why() {
    // `group_key` without a producer, as an index from before GS-2b (or
    // a v7 index serving during the rebuild) reports it.
    let index = SearchIndex::in_memory()
        .unwrap()
        .with_generations(Generations::current().with("group_key", 0))
        .unwrap();
    let f = fixture_with(30, index);
    let c = session(&f).await;
    let base = "/api/v1/search?q=movie&limit=4&collapse=group";
    let (status, _, body) = call(&f.app, get(base, &c)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.get("collapsed").is_none());
    assert_eq!(body["collapse_unavailable"], "group_key_not_ready");
    assert!(body.get("groups").is_none());
    assert_eq!(body["total"], 30);
    assert_eq!(hits(&body).len(), 4);
    assert!(hits(&body).iter().all(|h| h.get("others").is_none()));
    // Its cursor is an ungrouped one, and pages the same request on.
    let cursor = encode(body["next_cursor"].as_str().unwrap());
    let (status, _, page) = call(&f.app, get(&format!("{base}&cursor={cursor}"), &c)).await;
    assert_eq!(status, StatusCode::OK, "{page}");
    assert_eq!(page["collapse_unavailable"], "group_key_not_ready");
    assert_eq!(page["offset"], 4);
}

#[tokio::test]
async fn exact_counts_both_on_demand() {
    let f = fixture(36);
    let c = session(&f).await;
    let (status, _, body) = call(
        &f.app,
        get("/api/v1/search?q=movie&collapse=group&exact=1", &c),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["total"], 36);
    assert_eq!(body["total_approx"], false);
    assert_eq!(body["groups"], WORKS);
    assert_eq!(body["groups_approx"], false);
}

/// A request's status and `content-length`: what a HEAD answer has to
/// compare, since it carries no body.
async fn status_and_length(
    app: &Router,
    method: &str,
    uri: &str,
    cookie: &str,
) -> (StatusCode, String) {
    let mut req = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::COOKIE, cookie)
        .body(Body::empty())
        .unwrap();
    req.extensions_mut()
        .insert(ConnectInfo(SocketAddr::from(([10, 0, 0, 1], 1))));
    let resp = app.clone().oneshot(req).await.unwrap();
    let len = resp
        .headers()
        .get(header::CONTENT_LENGTH)
        .map(|v| v.to_str().unwrap().to_owned())
        .unwrap_or_default();
    (resp.status(), len)
}
