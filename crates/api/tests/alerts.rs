//! Saved-search alerts through the router: the list, the read marker,
//! the badge, the `alerts` SSE event and the RSS feed, with what each
//! leaves out (filter-hidden, Adult while hidden, private-source in the
//! feed), who reaches a feed, and that no token, query or torrent name
//! reaches a log. Goldens are in `tests/golden/` (`LODESTAR_BLESS=1`
//! rewrites them).

use std::net::SocketAddr;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Request, StatusCode, header};
use axum::{Extension, Router};
use lodestar_api::auth::ProxyConfig;
use lodestar_api::events::{EventHub, EventSources, EventsConfig};
use lodestar_api::keys::{mint, scope};
use lodestar_api::{AdultVisibility, ApiState, Auth, router};
use lodestar_bencode::{OwnedValue, to_vec};
use lodestar_classify::Correction;
use lodestar_metainfo::Info;
use lodestar_store::{
    KeyKind, NewApiKey, NewTorrent, SavedSearchInput, SearchHits, SourceId, Store, StoreConfig,
    flags,
};
use serde_json::{Value, json};
use tower::ServiceExt;

const PASSWORD: &str = "correct horse battery";
const PEER: &str = "192.0.2.10:5000";

struct T {
    name: &'static str,
    adult: bool,
    flags: i64,
}

const fn t(name: &'static str) -> T {
    T {
        name,
        adult: false,
        flags: 0,
    }
}

/// In id order (1-6): two ordinary, one a filter hid, one Adult, one
/// found only through a private source, and one with a hostile name.
const CATALOGUE: [T; 6] = [
    t("Planet.Earth.III.2160p.WEB"),
    t("Ocean.Docs.1080p.BluRay"),
    T {
        flags: flags::HIDDEN,
        ..t("Hidden.By.A.Rule.1080p")
    },
    T {
        adult: true,
        ..t("Some.Adult.Feature.1080p")
    },
    T {
        flags: flags::PRIVATE_SOURCE,
        ..t("Private.Tracker.Exclusive.1080p")
    },
    t("Tom & Jerry <script>alert('x')</script> \"q\" \u{202e}gnp.exe ]]>"),
];

fn metainfo(name: &str) -> Vec<u8> {
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
    store: Arc<Mutex<Store>>,
    hub: Arc<EventHub>,
    adult: Arc<AtomicBool>,
    hashes: Vec<String>,
    /// Saved searches: `nature` (RSS, alerts on every torrent), `quiet`
    /// (no RSS, alerts on 1 and 2) and `empty` (RSS, no alerts).
    nature: i64,
    quiet: i64,
    empty: i64,
    _dir: tempfile::TempDir,
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("t.db");
    let mut store = Store::open(&db, StoreConfig::default()).unwrap();
    let raws: Vec<Vec<u8>> = CATALOGUE.iter().map(|t| metainfo(t.name)).collect();
    let infos: Vec<Info> = raws.iter().map(|r| Info::parse(r).unwrap()).collect();
    let batch: Vec<NewTorrent<'_>> = infos
        .iter()
        .zip(&raws)
        .zip(&CATALOGUE)
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
    let mut enriched = Vec::new();
    for (id, t) in ids.iter().zip(&CATALOGUE) {
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
    let hashes = ids
        .iter()
        .map(|id| {
            let row = store.torrent(*id).unwrap().unwrap();
            row.info_hash.0.iter().map(|b| format!("{b:02x}")).collect()
        })
        .collect();
    let ast = serde_json::to_string(&lodestar_search::parse("SecretQueryText").unwrap()).unwrap();
    let search = |store: &mut Store, name: &str, rss: bool| {
        store
            .create_saved_search(
                &SavedSearchInput {
                    name,
                    query: "SecretQueryText",
                    ast: &ast,
                    sort: None,
                    alert: true,
                    rss,
                    channels: 0,
                    paused: false,
                },
                0,
                1_700_000_000,
            )
            .unwrap()
    };
    let nature = search(&mut store, "nature", true);
    let quiet = search(&mut store, "quiet", false);
    let empty = search(&mut store, "empty", true);
    store
        .record_percolation(
            6,
            &[
                SearchHits {
                    saved_search_id: nature,
                    torrent_ids: &ids,
                },
                SearchHits {
                    saved_search_id: quiet,
                    torrent_ids: &ids[..2],
                },
                SearchHits {
                    saved_search_id: empty,
                    torrent_ids: &[],
                },
            ],
            1_700_000_500,
        )
        .unwrap();
    let adult = Arc::new(AtomicBool::new(false));
    let shown = adult.clone();
    let visibility = AdultVisibility::follow(move || shown.load(Ordering::Relaxed));
    let auth = Arc::new(Auth::load(&dir.path().join("auth.json"), false).unwrap());
    let reader = Store::open_reader(&db, StoreConfig::default()).unwrap();
    let store = Arc::new(Mutex::new(store));
    let hub = EventHub::start(
        EventsConfig::default(),
        EventSources {
            adult: visibility.clone(),
            ..EventSources::default()
        },
    );
    let app = Router::new()
        .nest(
            "/api/v1",
            router(ApiState {
                auth: auth.clone(),
                index: None,
                reader: Arc::new(Mutex::new(reader)),
                filters: None,
                settings: None,
                ops: None,
                user_data: Some(store.clone()),
                adult: visibility,
                insights: None,
                hot: None,
                events: None,
            }),
        )
        .layer(Extension(hub.clone()));
    Fixture {
        app,
        auth,
        store,
        hub,
        adult,
        hashes,
        nature,
        quiet,
        empty,
        _dir: dir,
    }
}

struct Reply {
    status: StatusCode,
    headers: axum::http::HeaderMap,
    text: String,
}

impl Reply {
    fn json(&self) -> Value {
        serde_json::from_str(&self.text).unwrap_or(Value::Null)
    }
}

async fn send_from(app: &Router, mut req: Request<Body>, peer: &str) -> Reply {
    let addr: SocketAddr = peer.parse().unwrap();
    req.extensions_mut().insert(ConnectInfo(addr));
    let resp = app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let headers = resp.headers().clone();
    let bytes = axum::body::to_bytes(resp.into_body(), 1 << 24)
        .await
        .unwrap();
    Reply {
        status,
        headers,
        text: String::from_utf8(bytes.to_vec()).unwrap(),
    }
}

async fn send(app: &Router, req: Request<Body>) -> Reply {
    send_from(app, req, PEER).await
}

fn get(uri: &str, cookie: Option<&str>) -> Request<Body> {
    get_with(uri, cookie, &[])
}

fn get_with(uri: &str, cookie: Option<&str>, headers: &[(&str, &str)]) -> Request<Body> {
    let mut b = Request::get(uri).header(header::HOST, "lodestar.lan:8090");
    if let Some(c) = cookie {
        b = b.header(header::COOKIE, c);
    }
    for (k, v) in headers {
        b = b.header(*k, *v);
    }
    b.body(Body::empty()).unwrap()
}

fn post(uri: &str, cookie: Option<&str>, body: Value) -> Request<Body> {
    let mut b = Request::post(uri)
        .header(header::HOST, "lodestar.lan:8090")
        .header(header::CONTENT_TYPE, "application/json");
    if let Some(c) = cookie {
        b = b.header(header::COOKIE, c);
    }
    b.body(Body::from(body.to_string())).unwrap()
}

fn session(f: &Fixture) -> String {
    let set = match f.auth.setup_token() {
        Some(token) => f
            .auth
            .setup([127, 0, 0, 1].into(), &token, PASSWORD)
            .unwrap(),
        None => f.auth.login([127, 0, 0, 1].into(), PASSWORD).unwrap(),
    };
    set.split(';').next().unwrap().to_string()
}

/// A key or token with `scopes`, for `feed` (a token's one search).
fn key(f: &Fixture, kind: KeyKind, scopes: u32, feed: Option<i64>) -> (i64, String) {
    let m = mint(kind).unwrap();
    let k = f
        .store
        .lock()
        .unwrap()
        .create_api_key(
            &NewApiKey {
                kind,
                name: "reader",
                prefix: &m.prefix,
                hash: m.hash,
                scopes,
                saved_search_id: feed,
                expires_at: None,
            },
            0,
        )
        .unwrap();
    (k.id, m.secret.expose().to_string())
}

/// Checks the status and body against `tests/golden/{name}.txt`. Every
/// time in them comes from the fixture, so they're stable.
fn golden(name: &str, r: &Reply) {
    let got = format!("{}\n{}\n", r.status.as_u16(), r.text);
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
}

fn torrent_ids(page: &Value) -> Vec<i64> {
    page["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["torrent"]["id"].as_i64().unwrap())
        .collect()
}

// ---- the list, the badge, marking read --------------------------------------

#[tokio::test]
async fn the_list_shows_only_what_search_would() {
    let f = fixture();
    let c = session(&f);
    let uri = format!("/api/v1/saved-searches/{}/alerts", f.nature);
    let r = send(&f.app, get(&uri, Some(&c))).await;
    golden("alerts_list", &r);
    // Newest first; no filter-hidden (3) and no Adult (4) while hidden.
    // A private-source torrent is still yours to see in the app.
    assert_eq!(torrent_ids(&r.json()), [6, 5, 2, 1]);
    assert!(r.json()["items"][0]["read"] == json!(false));
    f.adult.store(true, Ordering::Relaxed);
    let r = send(&f.app, get(&uri, Some(&c))).await;
    assert_eq!(torrent_ids(&r.json()), [6, 5, 4, 2, 1]);
}

#[tokio::test]
async fn pages_skip_withheld_rows_and_their_cursor_names_a_shown_one() {
    let f = fixture();
    let c = session(&f);
    let base = format!("/api/v1/saved-searches/{}/alerts", f.nature);
    let mut seen = Vec::new();
    let mut cursors = Vec::new();
    let mut uri = format!("{base}?limit=1");
    loop {
        let page = send(&f.app, get(&uri, Some(&c))).await.json();
        let ids = torrent_ids(&page);
        assert!(ids.len() <= 1);
        seen.extend(ids);
        let Some(next) = page["next_cursor"].as_str() else {
            break;
        };
        cursors.push(next.to_string());
        uri = format!("{base}?limit=1&cursor={next}");
    }
    assert_eq!(seen, [6, 5, 2, 1]);
    // Never a withheld torrent's id (3 hidden, 4 Adult).
    assert_eq!(cursors, ["6", "5", "2"]);
    // A page of 2 that ends on withheld rows still knows it's last.
    let page = send(&f.app, get(&format!("{base}?limit=2&cursor=5"), Some(&c)))
        .await
        .json();
    assert_eq!(torrent_ids(&page), [2, 1]);
    assert_eq!(page["next_cursor"], Value::Null);
    // Limits are clamped to 1-200.
    for (limit, n) in [("0", 1), ("999", 4)] {
        let page = send(&f.app, get(&format!("{base}?limit={limit}"), Some(&c)))
            .await
            .json();
        assert_eq!(torrent_ids(&page).len(), n, "limit={limit}");
    }
}

#[tokio::test]
async fn list_errors() {
    let f = fixture();
    let c = session(&f);
    let r = send(&f.app, get("/api/v1/saved-searches/999/alerts", Some(&c))).await;
    golden("alerts_list_unknown", &r);
    for bad in ["x", "0", "-4", "1.5"] {
        let uri = format!("/api/v1/saved-searches/{}/alerts?cursor={bad}", f.nature);
        let r = send(&f.app, get(&uri, Some(&c))).await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{bad}");
        assert_eq!(r.json()["error"]["code"], "bad_cursor");
    }
}

#[tokio::test]
async fn the_badge_counts_what_the_list_shows_and_agrees_with_saved_searches() {
    let f = fixture();
    let c = session(&f);
    for adult in [false, true] {
        f.adult.store(adult, Ordering::Relaxed);
        let r = send(&f.app, get("/api/v1/alerts/unread", Some(&c))).await;
        let badge = r.json()["unread"].as_u64().unwrap();
        let list = send(&f.app, get("/api/v1/saved-searches", Some(&c)))
            .await
            .json();
        let sum: u64 = list
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s["unread"].as_u64().unwrap())
            .sum();
        assert_eq!(badge, sum, "adult shown: {adult}");
        // nature: 4 shown (+1 Adult when shown); quiet: 2.
        assert_eq!(badge, if adult { 7 } else { 6 });
        if !adult {
            golden("alerts_unread", &r);
        }
    }
}

#[tokio::test]
async fn marking_read_moves_the_divider_and_reveals_no_count() {
    let f = fixture();
    let c = session(&f);
    let uri = format!("/api/v1/saved-searches/{}/alerts", f.nature);
    let page = send(&f.app, get(&uri, Some(&c))).await.json();
    // The second-newest shown alert: the newest stays unread.
    let through = page["items"][1]["id"].as_i64().unwrap();
    let r = send(
        &f.app,
        post(
            "/api/v1/alerts/read",
            Some(&c),
            json!({ "saved_search": f.nature, "through": through }),
        ),
    )
    .await;
    golden("alerts_mark_read", &r);
    let page = send(&f.app, get(&uri, Some(&c))).await.json();
    let read: Vec<bool> = page["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["read"].as_bool().unwrap())
        .collect();
    assert_eq!(read, [false, true, true, true]);
    let r = send(&f.app, get("/api/v1/alerts/unread", Some(&c))).await;
    assert_eq!(r.json()["unread"], 3, "nature's newest and quiet's two");
    // Every search at once.
    let r = send(
        &f.app,
        post(
            "/api/v1/alerts/read",
            Some(&c),
            json!({ "through": i64::MAX }),
        ),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);
    let r = send(&f.app, get("/api/v1/alerts/unread", Some(&c))).await;
    assert_eq!(r.json()["unread"], 0);
    // An unknown search is a 404.
    let r = send(
        &f.app,
        post(
            "/api/v1/alerts/read",
            Some(&c),
            json!({ "saved_search": 999, "through": 1 }),
        ),
    )
    .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    assert_eq!(r.json()["error"]["code"], "saved_search_not_found");
}

#[tokio::test]
async fn writes_are_same_origin_json_and_everything_needs_a_session() {
    let f = fixture();
    let c = session(&f);
    let mut cross = post("/api/v1/alerts/read", Some(&c), json!({ "through": 1 }));
    cross
        .headers_mut()
        .insert(header::ORIGIN, "http://evil.example".parse().unwrap());
    let r = send(&f.app, cross).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    assert_eq!(r.json()["error"]["code"], "cross_origin");
    let form = Request::post("/api/v1/alerts/read")
        .header(header::COOKIE, &c)
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .body(Body::from("through=1"))
        .unwrap();
    assert_eq!(
        send(&f.app, form).await.status,
        StatusCode::UNSUPPORTED_MEDIA_TYPE
    );
    for req in [
        get(&format!("/api/v1/saved-searches/{}/alerts", f.nature), None),
        get("/api/v1/alerts/unread", None),
        post("/api/v1/alerts/read", None, json!({ "through": 1 })),
        get(&format!("/api/v1/feeds/{}", f.nature), None),
    ] {
        let r = send(&f.app, req).await;
        assert_eq!(r.status, StatusCode::UNAUTHORIZED);
        assert_eq!(r.json()["error"]["code"], "login_required");
    }
}

/// A search whose matches are mostly withheld: the list pages and the
/// feed fill from the few shown ones, however many are withheld between.
#[tokio::test]
async fn many_withheld_alerts_page_and_feed_correctly() {
    let f = fixture();
    let c = session(&f);
    let (search, shown) = {
        let mut store = f.store.lock().unwrap();
        let raws: Vec<Vec<u8>> = (0..450)
            .map(|i| metainfo(&format!("Withheld.{i}.1080p")))
            .collect();
        let infos: Vec<Info> = raws.iter().map(|r| Info::parse(r).unwrap()).collect();
        let batch: Vec<NewTorrent<'_>> = infos
            .iter()
            .zip(&raws)
            .enumerate()
            .map(|(i, (info, raw))| NewTorrent {
                info,
                raw_info: Some(raw),
                discovered_at: 1_700_001_000,
                metadata_at: 1_700_001_000,
                // Every 150th is shown; the rest a filter hid.
                extra_flags: if i % 150 == 0 { 0 } else { flags::HIDDEN },
                source: SourceId::new(0).unwrap(),
            })
            .collect();
        let ids: Vec<i64> = store
            .insert(&batch)
            .unwrap()
            .into_iter()
            .map(|i| i.id())
            .collect();
        let shown: Vec<i64> = ids.iter().step_by(150).rev().copied().collect();
        let search = store
            .create_saved_search(
                &SavedSearchInput {
                    name: "mostly hidden",
                    query: "x",
                    ast: "{}",
                    sort: None,
                    alert: true,
                    rss: true,
                    channels: 0,
                    paused: false,
                },
                0,
                0,
            )
            .unwrap();
        store
            .record_percolation(
                *ids.last().unwrap(),
                &[SearchHits {
                    saved_search_id: search,
                    torrent_ids: &ids,
                }],
                1_700_002_000,
            )
            .unwrap();
        (search, shown)
    };
    assert_eq!(shown.len(), 3);
    let base = format!("/api/v1/saved-searches/{search}/alerts");
    let mut seen = Vec::new();
    let mut uri = format!("{base}?limit=2");
    loop {
        let page = send(&f.app, get(&uri, Some(&c))).await.json();
        seen.extend(torrent_ids(&page));
        let Some(next) = page["next_cursor"].as_str() else {
            break;
        };
        assert!(shown.contains(&next.parse().unwrap()), "{next}");
        uri = format!("{base}?limit=2&cursor={next}");
    }
    assert_eq!(seen, shown);
    let r = send(&f.app, get(&feed_uri(search, None), Some(&c))).await;
    assert_eq!(guids(&r.text).len(), 3);
    assert!(!r.text.contains("Withheld.1."));
    let badge = send(&f.app, get("/api/v1/alerts/unread", Some(&c)))
        .await
        .json();
    assert_eq!(badge["unread"], 6 + 3);
}

/// Every saved search runs against every commit, so at most 1,000 are
/// kept.
#[tokio::test]
async fn saved_searches_are_capped() {
    let f = fixture();
    let c = session(&f);
    {
        let mut store = f.store.lock().unwrap();
        let have = store.saved_search_count().unwrap();
        for i in have..1_000 {
            store
                .create_saved_search(
                    &SavedSearchInput {
                        name: &format!("s{i}"),
                        query: "x",
                        ast: "{}",
                        sort: None,
                        alert: false,
                        rss: false,
                        channels: 0,
                        paused: true,
                    },
                    0,
                    0,
                )
                .unwrap();
        }
    }
    let body = json!({ "name": "one more", "q": "x", "alert": true, "rss": false });
    let r = send(
        &f.app,
        post("/api/v1/saved-searches", Some(&c), body.clone()),
    )
    .await;
    assert_eq!(r.status, StatusCode::CONFLICT);
    assert_eq!(r.json()["error"]["code"], "too_many_saved_searches");
    // Deleting one makes room.
    let del = Request::delete(format!("/api/v1/saved-searches/{}", f.empty))
        .header(header::HOST, "lodestar.lan:8090")
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::COOKIE, &c)
        .body(Body::empty())
        .unwrap();
    assert_eq!(send(&f.app, del).await.status, StatusCode::OK);
    let r = send(&f.app, post("/api/v1/saved-searches", Some(&c), body)).await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
}

// ---- SSE ---------------------------------------------------------------------

/// The `alerts` frames a client reconnecting after `last` is handed.
async fn replayed(f: &Fixture, cookie: &str, last: &str) -> String {
    use futures_core::Stream;
    let req = get_with(
        "/api/v1/events?topics=alerts",
        Some(cookie),
        &[("last-event-id", last)],
    );
    let resp = f.app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let mut stream = resp.into_body().into_data_stream();
    let mut text = String::new();
    loop {
        let next = std::future::poll_fn(|cx| Pin::new(&mut stream).poll_next(cx));
        match tokio::time::timeout(Duration::from_millis(300), next).await {
            Ok(Some(Ok(bytes))) => text.push_str(std::str::from_utf8(&bytes).unwrap()),
            _ => return text,
        }
    }
}

fn data_lines(text: &str) -> Vec<Value> {
    text.lines()
        .filter_map(|l| l.strip_prefix("data: "))
        .map(|d| serde_json::from_str(d).unwrap())
        .collect()
}

#[tokio::test]
async fn announce_counts_only_shown_alerts_and_screens_replays() {
    let f = fixture();
    let c = session(&f);
    let created = {
        let mut store = f.store.lock().unwrap();
        let more = store.create_saved_search(
            &SavedSearchInput {
                name: "more",
                query: "x",
                ast: "{}",
                sort: None,
                alert: false,
                rss: false,
                channels: 0,
                paused: false,
            },
            0,
            0,
        );
        let more = more.unwrap();
        // `more` matched only withheld torrents; `nature`'s run is
        // re-made for a fresh search with the same hits.
        let fresh = store
            .create_saved_search(
                &SavedSearchInput {
                    name: "fresh",
                    query: "x",
                    ast: "{}",
                    sort: None,
                    alert: true,
                    rss: false,
                    channels: 0,
                    paused: false,
                },
                0,
                0,
            )
            .unwrap();
        store
            .record_percolation(
                6,
                &[
                    SearchHits {
                        saved_search_id: more,
                        torrent_ids: &[3, 4],
                    },
                    SearchHits {
                        saved_search_id: fresh,
                        torrent_ids: &[1, 2, 3, 4, 5, 6],
                    },
                ],
                0,
            )
            .unwrap()
    };
    let marker = f.hub.settings_saved("crawler", 1).expect("a marker event");
    let adult = AdultVisibility::follow({
        let a = f.adult.clone();
        move || a.load(Ordering::Relaxed)
    });
    let sent = {
        let store = f.store.lock().unwrap();
        lodestar_api::alerts::announce(&f.hub, &store, &created, &adult).unwrap()
    };
    assert_eq!(sent, 1, "`more` had nothing shown: no event");
    let text = replayed(&f, &c, &marker).await;
    let events = data_lines(&text);
    assert_eq!(events.len(), 1, "{text}");
    let fresh_alerts: Vec<_> = created.iter().filter(|a| a.saved_search_id == 5).collect();
    let max_shown_alert = fresh_alerts
        .iter()
        .filter(|a| ![3, 4].contains(&a.torrent_id))
        .map(|a| a.id)
        .max()
        .unwrap();
    assert_eq!(
        events[0],
        json!({
            "alert": max_shown_alert,
            "matched": 4,
            "max_id": 6,
            "notify": true,
            "saved_search": 5
        })
    );
    // Shown: Adult counts. Then hidden again: that event is withheld from
    // a replay (one `resync` instead), as `torrents` events are.
    f.adult.store(true, Ordering::Relaxed);
    let marker = f.hub.settings_saved("crawler", 2).unwrap();
    {
        let store = f.store.lock().unwrap();
        lodestar_api::alerts::announce(&f.hub, &store, &created, &adult).unwrap();
    }
    let shown = data_lines(&replayed(&f, &c, &marker).await);
    assert_eq!(shown.len(), 2);
    assert_eq!(shown[1]["matched"], 5);
    assert_eq!(shown[0]["matched"], 1, "`more`'s Adult alert, now shown");
    f.adult.store(false, Ordering::Relaxed);
    let text = replayed(&f, &c, &marker).await;
    assert!(
        data_lines(&text).iter().all(|e| e.get("matched").is_none()),
        "{text}"
    );
    assert!(text.contains("event: resync"), "{text}");
}

// ---- feeds -------------------------------------------------------------------

fn feed_uri(id: i64, token: Option<&str>) -> String {
    match token {
        Some(t) => format!("/api/v1/feeds/{id}?token={t}"),
        None => format!("/api/v1/feeds/{id}"),
    }
}

/// The parts of a feed a check needs: each item's guid, title and
/// enclosure, read with a minimal scan (the XML itself is held to the
/// golden and to the renderer's own parser tests).
fn guids(xml: &str) -> Vec<String> {
    xml.split("<guid isPermaLink=\"false\">")
        .skip(1)
        .map(|s| s.split('<').next().unwrap().to_string())
        .collect()
}

#[tokio::test]
async fn the_feed_shows_shown_public_alerts_with_magnets() {
    let f = fixture();
    let c = session(&f);
    let r = send(&f.app, get(&feed_uri(f.nature, None), Some(&c))).await;
    assert_eq!(r.status, StatusCode::OK);
    golden("alerts_feed", &r);
    assert_eq!(
        r.headers[header::CONTENT_TYPE],
        "application/rss+xml; charset=utf-8"
    );
    assert_eq!(r.headers[header::CACHE_CONTROL], "private, max-age=60");
    assert_eq!(r.headers[header::REFERRER_POLICY], "no-referrer");
    assert_eq!(r.headers[header::X_CONTENT_TYPE_OPTIONS], "nosniff");
    assert_eq!(
        r.headers[header::CONTENT_SECURITY_POLICY],
        "default-src 'none'; sandbox"
    );
    // Filter-hidden (3), Adult while hidden (4) and private-source (5)
    // never appear.
    let h = &f.hashes;
    assert_eq!(guids(&r.text), [h[5].clone(), h[1].clone(), h[0].clone()]);
    for left_out in [&h[2], &h[3], &h[4]] {
        assert!(!r.text.contains(left_out.as_str()));
    }
    for name in ["Hidden.By", "Adult.Feature", "Private.Tracker"] {
        assert!(!r.text.contains(name), "{name}");
    }
    // The hostile name is text: no live markup, no bidi override.
    assert!(!r.text.contains("<script>") && !r.text.contains('\u{202e}'));
    assert!(r.text.contains("&lt;script&gt;"));
    assert!(
        r.text
            .contains(&format!("<link>http://lodestar.lan:8090/t/{}</link>", h[0]))
    );
    assert!(r.text.contains(&format!(
        "<enclosure url=\"magnet:?xt=urn:btih:{}&amp;dn=Planet.Earth.III.2160p.WEB\" type=\"application/x-bittorrent\" length=\"1073741824\"/>",
        h[0]
    )));
    assert!(r.text.contains(&format!(
        "<link>http://lodestar.lan:8090/alerts/{}</link>",
        f.nature
    )));
    // Showing Adult brings it back (not the hidden or the private one).
    f.adult.store(true, Ordering::Relaxed);
    let r = send(&f.app, get(&feed_uri(f.nature, None), Some(&c))).await;
    assert_eq!(guids(&r.text).len(), 4);
    assert!(guids(&r.text).contains(&h[3]));
    // An empty feed is a valid one; its build date is the search's.
    let r = send(&f.app, get(&feed_uri(f.empty, None), Some(&c))).await;
    assert_eq!(r.status, StatusCode::OK);
    assert!(!r.text.contains("<item>"));
    assert!(
        r.text
            .contains("<lastBuildDate>Tue, 14 Nov 2023 22:13:20 GMT</lastBuildDate>")
    );
}

#[tokio::test]
async fn etags_are_strong_and_304_when_unchanged() {
    let f = fixture();
    let c = session(&f);
    let r = send(&f.app, get(&feed_uri(f.nature, None), Some(&c))).await;
    let tag = r.headers[header::ETAG].to_str().unwrap().to_string();
    assert!(tag.starts_with('"') && !tag.starts_with("W/"), "{tag}");
    // The same body gives the same tag.
    let again = send(&f.app, get(&feed_uri(f.nature, None), Some(&c))).await;
    assert_eq!(again.headers[header::ETAG], tag.as_str());
    for inm in [tag.as_str(), "\"other\", ".to_owned().as_str(), "*"] {
        let inm = if inm.ends_with(", ") {
            format!("{inm}{tag}")
        } else {
            inm.to_string()
        };
        let r = send(
            &f.app,
            get_with(
                &feed_uri(f.nature, None),
                Some(&c),
                &[("if-none-match", &inm)],
            ),
        )
        .await;
        assert_eq!(r.status, StatusCode::NOT_MODIFIED, "{inm}");
        assert!(r.text.is_empty());
        assert_eq!(r.headers[header::ETAG], tag.as_str());
        assert_eq!(r.headers[header::CACHE_CONTROL], "private, max-age=60");
    }
    let r = send(
        &f.app,
        get_with(
            &feed_uri(f.nature, None),
            Some(&c),
            &[("if-none-match", "\"stale\"")],
        ),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);
    // A change in what's shown changes the tag.
    f.adult.store(true, Ordering::Relaxed);
    let r = send(&f.app, get(&feed_uri(f.nature, None), Some(&c))).await;
    assert_ne!(r.headers[header::ETAG], tag.as_str());
}

#[tokio::test]
async fn who_reaches_a_feed() {
    let f = fixture();
    session(&f);
    let (_, own) = key(&f, KeyKind::FeedToken, scope::FEEDS, Some(f.nature));
    let (_, other) = key(&f, KeyKind::FeedToken, scope::FEEDS, Some(f.empty));
    let (_, every) = key(&f, KeyKind::FeedToken, scope::FEEDS, None);
    let (revoked_id, revoked) = key(&f, KeyKind::FeedToken, scope::FEEDS, None);
    f.store
        .lock()
        .unwrap()
        .revoke_api_key(revoked_id, 1)
        .unwrap();
    let (_, feeds_key) = key(&f, KeyKind::ApiKey, scope::FEEDS, None);
    let (_, search_key) = key(&f, KeyKind::ApiKey, scope::SEARCH | scope::TORZNAB, None);
    let ok = |r: &Reply| r.status == StatusCode::OK && r.text.contains("<rss version=\"2.0\">");
    // A token for this search, an all-feeds token, and a key with FEEDS.
    let mut bodies = Vec::new();
    for uri in [
        feed_uri(f.nature, Some(&own)),
        feed_uri(f.nature, Some(&every)),
    ] {
        let r = send(&f.app, get(&uri, None)).await;
        assert!(ok(&r), "{} {}", r.status, r.text);
        bodies.push(r.text);
    }
    let bearer = format!("Bearer {feeds_key}");
    for req in [
        get_with(
            &feed_uri(f.nature, None),
            None,
            &[("authorization", &bearer)],
        ),
        get_with(
            &feed_uri(f.nature, None),
            None,
            &[("x-api-key", &feeds_key)],
        ),
        get(
            &format!("/api/v1/feeds/{}?apikey={feeds_key}", f.nature),
            None,
        ),
    ] {
        let r = send(&f.app, req).await;
        assert!(ok(&r), "{} {}", r.status, r.text);
        bodies.push(r.text);
    }
    // The all-feeds token reaches another search's feed too.
    assert!(ok(&send(
        &f.app,
        get(&feed_uri(f.empty, Some(&every)), None)
    )
    .await));
    // Whoever reads it, the feed is the same document.
    assert!(bodies.windows(2).all(|w| w[0] == w[1]));
    // One 404 for another search's token, a search without RSS, an
    // unknown id and an id that isn't a number.
    let mut not_found = Vec::new();
    for uri in [
        feed_uri(f.nature, Some(&other)),
        feed_uri(f.quiet, Some(&every)),
        feed_uri(999, Some(&every)),
        format!("/api/v1/feeds/abc?token={every}"),
        format!("/api/v1/feeds/-1?token={every}"),
    ] {
        let r = send(&f.app, get(&uri, None)).await;
        assert_eq!(r.status, StatusCode::NOT_FOUND, "{uri}");
        not_found.push(r.text);
    }
    assert!(not_found.windows(2).all(|w| w[0] == w[1]));
    golden(
        "alerts_feed_not_found",
        &send(&f.app, get(&feed_uri(f.quiet, Some(&every)), None)).await,
    );
    // A revoked or expired token is a bad key; a key without FEEDS
    // lacks the scope.
    let expired = {
        let m = mint(KeyKind::FeedToken).unwrap();
        f.store
            .lock()
            .unwrap()
            .create_api_key(
                &NewApiKey {
                    kind: KeyKind::FeedToken,
                    name: "old",
                    prefix: &m.prefix,
                    hash: m.hash,
                    scopes: scope::FEEDS,
                    saved_search_id: None,
                    expires_at: Some(1),
                },
                0,
            )
            .unwrap();
        m.secret.expose().to_string()
    };
    for t in [&revoked, &expired] {
        let r = send(&f.app, get(&feed_uri(f.nature, Some(t)), None)).await;
        assert_eq!(
            (r.status, r.json()["error"]["code"].as_str()),
            (StatusCode::UNAUTHORIZED, Some("bad_api_key"))
        );
    }
    let r = send(
        &f.app,
        get_with(
            &feed_uri(f.nature, None),
            None,
            &[("x-api-key", &search_key)],
        ),
    )
    .await;
    assert_eq!(
        (r.status, r.json()["error"]["code"].as_str()),
        (StatusCode::FORBIDDEN, Some("key_scope"))
    );
    // A token reaches no other route, even the alert list.
    for uri in [
        format!("/api/v1/saved-searches/{}/alerts?token={every}", f.nature),
        format!("/api/v1/alerts/unread?token={every}"),
        format!("/api/v1/saved-searches?token={every}"),
    ] {
        let r = send(&f.app, get(&uri, None)).await;
        assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{uri}");
    }
}

#[tokio::test]
async fn the_origin_is_the_host_and_https_only_through_a_trusted_proxy() {
    let f = fixture();
    let c = session(&f);
    let channel_link = |xml: &str| {
        xml.split("<link>")
            .nth(1)
            .unwrap()
            .split('<')
            .next()
            .unwrap()
            .to_string()
    };
    let link_for = async |host: &str, extra: &[(&str, &str)], peer: &str| {
        let mut req = get_with(&feed_uri(f.nature, None), Some(&c), extra);
        req.headers_mut()
            .insert(header::HOST, host.parse().unwrap());
        channel_link(&send_from(&f.app, req, peer).await.text)
    };
    let id = f.nature;
    assert_eq!(
        link_for("Example.LAN:8090", &[], PEER).await,
        format!("http://example.lan:8090/alerts/{id}")
    );
    for bad in ["evil.example/x?", "user@host", "a b", "host:99999", ""] {
        assert_eq!(
            link_for(bad, &[], PEER).await,
            format!("http://localhost/alerts/{id}"),
            "{bad:?}"
        );
    }
    // Without a `Host` (HTTP/2 sends `:authority`), the request target's
    // authority.
    let req = Request::get(format!("http://h2.lan:8443{}", feed_uri(f.nature, None)))
        .header(header::COOKIE, &c)
        .body(Body::empty())
        .unwrap();
    assert_eq!(
        channel_link(&send(&f.app, req).await.text),
        format!("http://h2.lan:8443/alerts/{id}")
    );
    // X-Forwarded-Proto is ignored from anyone but a trusted proxy...
    let https = [("x-forwarded-proto", "https")];
    assert!(
        link_for("lodestar.lan", &https, PEER)
            .await
            .starts_with("http://")
    );
    f.auth
        .set_proxy(ProxyConfig {
            enabled: true,
            trusted: vec!["10.0.0.2".into()],
            ..ProxyConfig::default()
        })
        .unwrap();
    assert!(
        link_for("lodestar.lan", &https, PEER)
            .await
            .starts_with("http://")
    );
    // ...and believed from one.
    assert_eq!(
        link_for("lodestar.lan", &https, "10.0.0.2:4000").await,
        format!("https://lodestar.lan/alerts/{id}")
    );
    assert!(
        link_for(
            "lodestar.lan",
            &[("x-forwarded-proto", "http")],
            "10.0.0.2:4000"
        )
        .await
        .starts_with("http://")
    );
}

/// No token, query or torrent name reaches a log, through every flow
/// here, failures included.
#[tokio::test]
async fn no_token_query_or_name_reaches_a_log() {
    capture::start();
    let f = fixture();
    let c = session(&f);
    let (_, token) = key(&f, KeyKind::FeedToken, scope::FEEDS, None);
    let (_, other) = key(&f, KeyKind::FeedToken, scope::FEEDS, Some(f.empty));
    for uri in [
        feed_uri(f.nature, Some(&token)),
        feed_uri(f.nature, Some(&other)),
        feed_uri(f.quiet, Some(&token)),
        format!("/api/v1/feeds/x?token={token}"),
        format!("/api/v1/feeds/{}?token={token}x", f.nature),
        format!("/api/v1/saved-searches/{}/alerts?token={token}", f.nature),
    ] {
        let r = send(&f.app, get(&uri, None)).await;
        assert!(!r.text.contains(&token) && !r.text.contains(&other));
        for v in r.headers.values() {
            let v = v.to_str().unwrap_or("");
            assert!(!v.contains(&token) && !v.contains(&other));
        }
    }
    send(
        &f.app,
        get(
            &format!("/api/v1/saved-searches/{}/alerts", f.nature),
            Some(&c),
        ),
    )
    .await;
    let created: Vec<_> = f.store.lock().unwrap().alerts(f.nature, None, 10).unwrap();
    {
        let store = f.store.lock().unwrap();
        lodestar_api::alerts::announce(&f.hub, &store, &created, &AdultVisibility::default())
            .unwrap();
    }
    let log = capture::lines();
    for secret in [
        token.as_str(),
        other.as_str(),
        "SecretQueryText",
        "secretquerytext",
        "Planet.Earth",
        "Tom & Jerry",
        "Ocean.Docs",
    ] {
        assert!(!log.contains(secret), "{secret} reached the log:\n{log}");
    }
}

/// Every log line written while the test binary runs, from any thread.
mod capture {
    use std::fmt::Write as _;
    use std::sync::{Mutex, OnceLock};

    use tracing::field::{Field, Visit};
    use tracing::span::{Attributes, Id, Record};
    use tracing::{Event, Metadata, Subscriber};

    static LINES: Mutex<String> = Mutex::new(String::new());

    struct Capture;

    struct Fields(String);

    impl Visit for Fields {
        fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
            let _ = write!(self.0, " {}={:?}", field.name(), value);
        }
    }

    impl Subscriber for Capture {
        fn enabled(&self, _: &Metadata<'_>) -> bool {
            true
        }

        fn new_span(&self, span: &Attributes<'_>) -> Id {
            let mut f = Fields(String::new());
            span.record(&mut f);
            let _ = writeln!(LINES.lock().unwrap(), "span{}", f.0);
            Id::from_u64(1)
        }

        fn record(&self, _: &Id, values: &Record<'_>) {
            let mut f = Fields(String::new());
            values.record(&mut f);
            let _ = writeln!(LINES.lock().unwrap(), "record{}", f.0);
        }

        fn record_follows_from(&self, _: &Id, _: &Id) {}

        fn event(&self, event: &Event<'_>) {
            let mut f = Fields(String::new());
            event.record(&mut f);
            let _ = writeln!(LINES.lock().unwrap(), "{}{}", event.metadata().level(), f.0);
        }

        fn enter(&self, _: &Id) {}

        fn exit(&self, _: &Id) {}
    }

    pub fn start() {
        static ONCE: OnceLock<()> = OnceLock::new();
        ONCE.get_or_init(|| {
            tracing::subscriber::set_global_default(Capture).expect("no other subscriber");
        });
    }

    pub fn lines() -> String {
        LINES.lock().unwrap().clone()
    }
}
