//! `/search` paging through cursors (§8 "Stable pagination", B5a): one
//! snapshot per search while torrents keep arriving, authenticated and
//! session-bound cursors, and exact counts on demand.

use std::collections::HashSet;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Request, StatusCode, header};
use lodestar_api::{ApiState, Auth, router};
use lodestar_bencode::{OwnedValue, to_vec};
use lodestar_metainfo::Info;
use lodestar_search::SearchIndex;
use lodestar_store::{NewTorrent, SourceId, Store, StoreConfig};
use serde_json::{Value, json};
use tower::ServiceExt;

const PASSWORD: &str = "correct horse battery";

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
    added: usize,
    _dir: tempfile::TempDir,
}

impl Fixture {
    /// Stores and indexes `n` more torrents named `Show.Episode.{k}.mkv`.
    fn add(&mut self, n: usize) {
        let raws: Vec<Vec<u8>> = (self.added..self.added + n)
            .map(|k| raw(&format!("Show.Episode.{k}.1080p.mkv")))
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
        self.store.insert(&batch).unwrap();
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
    let app = Router::new().nest(
        "/api/v1",
        router(ApiState {
            auth: auth.clone(),
            index: Some(index.clone().into()),
            reader: Arc::new(Mutex::new(reader)),
            filters: None,
            settings: None,
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
    let mut b = Request::post(uri).header(header::CONTENT_TYPE, "application/json");
    if let Some(c) = cookie {
        b = b.header(header::COOKIE, c);
    }
    b.body(Body::from(body.to_string())).unwrap()
}

/// Sets the instance up and returns a session; later calls log in again
/// for another session.
async fn session(f: &Fixture) -> String {
    let (status, cookie, _) = match f.auth.setup_token() {
        Some(token) => {
            let body = json!({ "token": token, "password": PASSWORD });
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

fn ids(body: &Value) -> Vec<i64> {
    body["hits"]
        .as_array()
        .unwrap()
        .iter()
        .map(|h| h["id"].as_i64().unwrap())
        .collect()
}

fn cursor_of(body: &Value) -> Option<String> {
    body["next_cursor"].as_str().map(str::to_string)
}

fn encode(s: &str) -> String {
    form_urlencoded::byte_serialize(s.as_bytes()).collect()
}

#[tokio::test]
async fn cursors_page_one_snapshot_while_torrents_arrive() {
    let mut f = fixture(120);
    let c = session(&f).await;
    for sort in ["relevance", "-discovered", "%2Bsize"] {
        let base = format!("/api/v1/search?q=show&sort={sort}&limit=7");
        let (status, _, first) = call(&f.app, get(&base, &c)).await;
        assert_eq!(status, StatusCode::OK, "{first}");
        assert_eq!(first["total"], f.added);
        assert_eq!(first["total_approx"], false);
        let total = first["total"].clone();
        let mut seen = ids(&first);
        let mut next = cursor_of(&first);
        let mut offset = 7;
        while let Some(cursor) = next {
            assert!(cursor.len() <= 256);
            assert!(cursor.bytes().all(|b| b.is_ascii_hexdigit()));
            // New torrents land between pages; the snapshot doesn't move.
            f.add(3);
            let uri = format!("{base}&cursor={}", encode(&cursor));
            let (status, _, page) = call(&f.app, get(&uri, &c)).await;
            assert_eq!(status, StatusCode::OK, "{page}");
            assert_eq!(page["total"], total);
            assert_eq!(page["offset"], offset);
            offset += ids(&page).len();
            seen.extend(ids(&page));
            next = cursor_of(&page);
        }
        let unique: HashSet<i64> = seen.iter().copied().collect();
        assert_eq!(unique.len(), seen.len(), "{sort}: a duplicate");
        assert_eq!(
            seen.len(),
            total.as_u64().unwrap() as usize,
            "{sort}: a gap"
        );
        // A new search sees what arrived meanwhile.
        let (_, _, fresh) = call(&f.app, get(&base, &c)).await;
        assert_eq!(fresh["total"], f.added);
    }
}

#[tokio::test]
async fn post_takes_a_cursor_in_the_body() {
    let f = fixture(30);
    let c = session(&f).await;
    let body = json!({ "q": "show", "limit": 20 });
    let (_, _, first) = call(&f.app, post("/api/v1/search", Some(&c), body)).await;
    let cursor = cursor_of(&first).unwrap();
    let body = json!({ "q": "show", "limit": 20, "cursor": cursor, "exact": true });
    let (status, _, second) = call(&f.app, post("/api/v1/search", Some(&c), body)).await;
    assert_eq!(status, StatusCode::OK, "{second}");
    assert_eq!(ids(&second).len(), 10);
    assert_eq!(second["next_cursor"], Value::Null);
    assert_eq!(second["offset"], 20);
}

async fn error(f: &Fixture, uri: &str, cookie: &str) -> (StatusCode, String, Value) {
    let (status, _, body) = call(&f.app, get(uri, cookie)).await;
    let code = body["error"]["code"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    (status, code, body)
}

#[tokio::test]
async fn bad_cursors_are_refused_with_clear_codes() {
    let f = fixture(30);
    let c = session(&f).await;
    let base = "/api/v1/search?q=show&limit=5";
    let (_, _, first) = call(&f.app, get(base, &c)).await;
    let cursor = cursor_of(&first).unwrap();
    let with = |q: &str, cursor: &str| format!("/api/v1/search?{q}&limit=5&cursor={cursor}");

    // With an offset.
    let (status, code, body) =
        error(&f, &format!("{}&offset=5", with("q=show", &cursor)), &c).await;
    assert_eq!(
        (status, code.as_str()),
        (StatusCode::BAD_REQUEST, "cursor_and_offset")
    );
    assert_eq!(body["error"]["field"], "offset");

    // Tampered: every byte flipped in turn is refused (the epoch's bytes
    // read as another process's cursor, the rest fail the tag).
    for i in 0..cursor.len() {
        let mut t = cursor.clone().into_bytes();
        t[i] = if t[i] == b'0' { b'1' } else { b'0' };
        let t = String::from_utf8(t).unwrap();
        let (status, code, _) = error(&f, &with("q=show", &t), &c).await;
        if i < 8 {
            assert_eq!(
                (status, code.as_str()),
                (StatusCode::GONE, "cursor_expired"),
                "byte {i}"
            );
        } else {
            assert_eq!(
                (status, code.as_str()),
                (StatusCode::BAD_REQUEST, "bad_cursor"),
                "byte {i}"
            );
        }
    }
    for junk in ["", "zz", "abc", &"a".repeat(300), &cursor.to_uppercase()] {
        let (status, code, _) = error(&f, &with("q=show", &encode(junk)), &c).await;
        assert_eq!(
            (status, code.as_str()),
            (StatusCode::BAD_REQUEST, "bad_cursor"),
            "{junk:?}"
        );
    }

    // From another search: another query, sort or hidden setting.
    for q in ["q=episode", "q=show&sort=size", "q=show&hidden=true"] {
        let (status, code, body) = error(&f, &with(q, &cursor), &c).await;
        assert_eq!(
            (status, code.as_str()),
            (StatusCode::BAD_REQUEST, "bad_cursor"),
            "{q}"
        );
        assert!(
            body["error"]["message"]
                .as_str()
                .unwrap()
                .contains("another search")
        );
    }

    // Another session's cursor reads as expired: its lease isn't theirs.
    let other = session(&f).await;
    assert_ne!(other, c);
    let (status, code, body) = error(&f, &with("q=show", &cursor), &other).await;
    assert_eq!(
        (status, code.as_str()),
        (StatusCode::GONE, "cursor_expired")
    );
    assert_eq!(body["error"]["field"], "cursor");
    // And still works for its own.
    let (status, _, _) = call(&f.app, get(&with("q=show", &cursor), &c)).await;
    assert_eq!(status, StatusCode::OK);

    // `exact` is a flag.
    let (status, code, _) = error(&f, "/api/v1/search?q=show&exact=maybe", &c).await;
    assert_eq!(
        (status, code.as_str()),
        (StatusCode::BAD_REQUEST, "bad_parameter")
    );
    for yes in ["1", "true"] {
        let (status, _, body) = call(
            &f.app,
            get(&format!("/api/v1/search?q=show&exact={yes}"), &c),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["total_approx"], false);
        assert_eq!(body["total"], 30);
    }
}

#[tokio::test]
async fn deep_pages_go_past_the_offset_window_through_cursors() {
    let f = fixture(260);
    let c = session(&f).await;
    // Offsets stop at 10,000; cursors don't (here: past the 200-candidate
    // re-rank window, which is where relevance order changes windows).
    let base = "/api/v1/search?q=show+episode&limit=200";
    let (_, _, first) = call(&f.app, get(base, &c)).await;
    let cursor = cursor_of(&first).unwrap();
    let (_, _, second) = call(&f.app, get(&format!("{base}&cursor={cursor}"), &c)).await;
    let mut all = ids(&first);
    all.extend(ids(&second));
    let unique: HashSet<i64> = all.iter().copied().collect();
    assert_eq!(unique.len(), 260);
    assert_eq!(second["next_cursor"], Value::Null);
    // The same pages by offset.
    let (_, _, by_offset) = call(&f.app, get(&format!("{base}&offset=200"), &c)).await;
    assert_eq!(ids(&by_offset), ids(&second));
}
