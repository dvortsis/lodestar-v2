//! `GET /discover` (§10.7) through the router: Newest ordered, paged
//! and tabbed by type, the unavailable sections' shape, and what it
//! never shows (hidden torrents, Adult while hidden), with goldens in
//! `tests/golden/` (`LODESTAR_BLESS=1` rewrites them).

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use lodestar_api::{AdultVisibility, ApiState, Auth, router};
use lodestar_bencode::{OwnedValue, to_vec};
use lodestar_classify::Correction;
use lodestar_metainfo::Info;
use lodestar_search::SearchIndex;
use lodestar_store::{NewTorrent, SourceId, Store, StoreConfig, flags};
use serde_json::{Value, json};
use tower::ServiceExt;

const PASSWORD: &str = "correct horse battery";

/// One fixture torrent: a name, its files (`""` alone is single-file),
/// whether it's corrected to Adult, and its extra flags.
struct T<'a> {
    name: &'a str,
    files: Vec<(String, i64)>,
    adult: bool,
    flags: i64,
}

fn t<'a>(name: &'a str, files: &[(&str, i64)]) -> T<'a> {
    T {
        name,
        files: files.iter().map(|(p, l)| (p.to_string(), *l)).collect(),
        adult: false,
        flags: 0,
    }
}

fn metainfo(name: &str, files: &[(String, i64)]) -> Vec<u8> {
    let d = |e: Vec<(&str, OwnedValue)>| {
        OwnedValue::Dict(
            e.into_iter()
                .map(|(k, v)| (k.as_bytes().to_vec(), v))
                .collect(),
        )
    };
    let b = |s: &str| OwnedValue::Bytes(s.as_bytes().to_vec());
    let mut entries = vec![
        ("name", b(name)),
        ("piece length", OwnedValue::Int(1 << 16)),
        ("pieces", OwnedValue::Bytes(vec![0; 20])),
    ];
    if let [(single, len)] = files
        && single.is_empty()
    {
        entries.push(("length", OwnedValue::Int(*len)));
    } else {
        let list = files
            .iter()
            .map(|(p, len)| {
                d(vec![
                    ("length", OwnedValue::Int(*len)),
                    ("path", OwnedValue::List(p.split('/').map(b).collect())),
                ])
            })
            .collect();
        entries.push(("files", OwnedValue::List(list)));
    }
    to_vec(&d(entries))
}

struct Fixture {
    app: Router,
    auth: Arc<Auth>,
    _dir: tempfile::TempDir,
}

fn fixture(torrents: &[T<'_>], search: bool) -> Fixture {
    fixture_with(torrents, search, false)
}

/// `adult`: "Show adult content" on.
fn fixture_with(torrents: &[T<'_>], search: bool, adult: bool) -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("t.db");
    let mut store = Store::open(&db, StoreConfig::default()).unwrap();
    let raws: Vec<Vec<u8>> = torrents
        .iter()
        .map(|t| metainfo(t.name, &t.files))
        .collect();
    let infos: Vec<Info> = raws.iter().map(|r| Info::parse(r).unwrap()).collect();
    let batch: Vec<NewTorrent<'_>> = infos
        .iter()
        .zip(&raws)
        .zip(torrents)
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
    // Classified, as the pipeline would have; `adult` as an Is adult
    // correction would.
    let mut enriched = Vec::new();
    for (id, t) in ids.iter().zip(torrents) {
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
    let index = SearchIndex::in_memory().unwrap();
    let mut w = index.writer(15 << 20).unwrap();
    w.catch_up(&mut store, 1000).unwrap();
    w.commit().unwrap();
    let auth = Arc::new(Auth::load(&dir.path().join("auth.json"), false).unwrap());
    let reader = Store::open_reader(&db, StoreConfig::default()).unwrap();
    let app = Router::new().nest(
        "/api/v1",
        router(ApiState {
            auth: auth.clone(),
            index: search.then(|| index.into()),
            reader: Arc::new(Mutex::new(reader)),
            filters: None,
            settings: None,
            ops: None,
            adult: AdultVisibility::follow(move || adult),
            insights: None,
        }),
    );
    Fixture {
        app,
        auth,
        _dir: dir,
    }
}

async fn call(app: &Router, req: Request<Body>) -> (StatusCode, String) {
    let resp = app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), 1 << 24)
        .await
        .unwrap();
    (status, String::from_utf8(bytes.to_vec()).unwrap())
}

async fn json_of(app: &Router, req: Request<Body>) -> (StatusCode, Value) {
    let (status, body) = call(app, req).await;
    (status, serde_json::from_str(&body).unwrap_or(Value::Null))
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

async fn session(f: &Fixture) -> String {
    let token = f.auth.setup_token().unwrap();
    let resp = f
        .app
        .clone()
        .oneshot(post(
            "/api/v1/setup",
            None,
            json!({ "token": token, "password": PASSWORD }),
        ))
        .await
        .unwrap();
    resp.headers()[header::SET_COOKIE]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_string()
}

/// Checks the status and body against `tests/golden/{name}.txt`.
async fn golden(f: &Fixture, name: &str, req: Request<Body>) -> Value {
    let (status, body) = call(&f.app, req).await;
    let got = format!("{}\n{}\n", status.as_u16(), blank_cursors(&body));
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

/// A `next_cursor` holds a random lease id: it becomes `"…"`.
fn blank_cursors(body: &str) -> String {
    let mut out = body.to_string();
    let pat = "\"next_cursor\":\"";
    let mut from = 0;
    while let Some(i) = out[from..].find(pat) {
        let start = from + i + pat.len();
        let end = out[start..].find('"').map_or(out.len(), |n| start + n);
        out.replace_range(start..end, "…");
        from = start;
    }
    out
}

const GB: i64 = 1 << 30;

/// Discovered one second apart, in this order.
fn catalogue() -> Vec<T<'static>> {
    let mut hidden = t(
        "Hidden.Movie.2020.1080p.BluRay.x264-HID",
        &[("Hidden.Movie.2020.1080p.mkv", 8 * GB)],
    );
    hidden.flags = flags::HIDDEN;
    let mut adult = t(
        "Some.Feature.2021.1080p.WEB.x264-ADL",
        &[("Some.Feature.2021.1080p.mp4", 2 * GB)],
    );
    adult.adult = true;
    vec![
        t(
            "The.Dark.Knight.2008.1080p.BluRay.x264-GRP",
            &[("The.Dark.Knight.2008.1080p.mkv", 8 * GB)],
        ),
        t(
            "Show.Name.S01E02.1080p.WEB.x264-AAA",
            &[("Show.Name.S01E02.1080p.mkv", GB)],
        ),
        hidden,
        t(
            "Batman.Begins.2005.720p.BluRay.x264-GRP",
            &[("Batman.Begins.2005.720p.mkv", 4 * GB)],
        ),
        adult,
        t(
            "Show.Name.S01E03.1080p.WEB.x264-AAA",
            &[("Show.Name.S01E03.1080p.mkv", GB)],
        ),
        t(
            "Gotham.Soundtrack.2008.FLAC",
            &[("01 Intro.flac", 30 << 20), ("02 Theme.flac", 30 << 20)],
        ),
    ]
}

fn names(newest: &Value) -> Vec<String> {
    newest["hits"]
        .as_array()
        .unwrap()
        .iter()
        .map(|h| h["name"].as_str().unwrap().to_string())
        .collect()
}

/// Every Newest page from `uri`, following `next_cursor`.
async fn all_pages(f: &Fixture, uri: &str, c: Option<&str>) -> Vec<String> {
    let mut out = Vec::new();
    let mut next = uri.to_string();
    for _ in 0..20 {
        let (status, body) = json_of(&f.app, get(&next, c)).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        out.extend(names(&body["newest"]));
        let Some(cursor) = body["newest"]["next_cursor"].as_str() else {
            return out;
        };
        let sep = if uri.contains('?') { '&' } else { '?' };
        next = format!("{uri}{sep}cursor={cursor}");
    }
    panic!("paging never ended");
}

#[tokio::test]
async fn newest_is_ordered_and_paged() {
    let all = catalogue();
    let f = fixture(&all, true);
    let c = session(&f).await;
    let c = Some(c.as_str());

    let body = golden(&f, "discover_newest", get("/api/v1/discover?limit=2", c)).await;
    let newest = &body["newest"];
    assert_eq!(newest["limit"], 2);
    assert_eq!(newest["offset"], 0);
    assert_eq!(newest["type"], Value::Null);
    // Hidden and Adult are left out of the total too.
    assert_eq!(newest["total"], 5);
    assert_eq!(newest["total_approx"], false);
    assert!(newest["next_cursor"].is_string());

    let got = all_pages(&f, "/api/v1/discover?limit=2", c).await;
    // Newest first, each once.
    let want: Vec<String> = [6, 5, 3, 1, 0]
        .iter()
        .map(|&i| all[i].name.to_string())
        .collect();
    assert_eq!(got, want);
    // The rows are search rows, in discovery order.
    let (_, body) = json_of(&f.app, get("/api/v1/discover", c)).await;
    let at: Vec<i64> = body["newest"]["hits"]
        .as_array()
        .unwrap()
        .iter()
        .map(|h| h["discovered_at"].as_i64().unwrap())
        .collect();
    assert!(at.windows(2).all(|w| w[0] > w[1]), "{at:?}");
    assert_eq!(body["newest"]["hits"][0]["content_type"], "music");
}

#[tokio::test]
async fn newest_by_type() {
    let all = catalogue();
    let f = fixture(&all, true);
    let c = session(&f).await;
    let c = Some(c.as_str());

    let body = golden(&f, "discover_tab_tv", get("/api/v1/discover?type=tv", c)).await;
    assert_eq!(body["newest"]["type"], "tv");
    assert_eq!(
        names(&body["newest"]),
        vec![all[5].name.to_string(), all[1].name.to_string()]
    );
    let got = all_pages(&f, "/api/v1/discover?type=movie&limit=1", c).await;
    assert_eq!(got, vec![all[3].name.to_string(), all[0].name.to_string()]);

    // A cursor belongs to its tab.
    let (_, body) = json_of(&f.app, get("/api/v1/discover?type=movie&limit=1", c)).await;
    let cursor = body["newest"]["next_cursor"].as_str().unwrap();
    let (status, body) = json_of(
        &f.app,
        get(
            &format!("/api/v1/discover?type=tv&limit=1&cursor={cursor}"),
            c,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["code"], "bad_cursor");

    golden(
        &f,
        "discover_unknown_type",
        get("/api/v1/discover?type=type:movie", c),
    )
    .await;
}

#[tokio::test]
async fn adult_and_hidden_are_excluded() {
    let all = catalogue();
    let f = fixture(&all, true);
    let c = session(&f).await;
    let c = Some(c.as_str());
    let everything = all_pages(&f, "/api/v1/discover?limit=1", c).await;
    for i in [2, 4] {
        assert!(!everything.contains(&all[i].name.to_string()), "{i}");
    }
    // Asking for the Adult tab while Adult is hidden shows nothing.
    let (status, body) = json_of(&f.app, get("/api/v1/discover?type=adult", c)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["newest"]["hits"], json!([]));
    assert_eq!(body["newest"]["total"], 0);
    let (_, body) = json_of(&f.app, get("/api/v1/discover?type=movie", c)).await;
    assert!(!body.to_string().contains("Some.Feature"), "{body}");
    assert!(!body.to_string().contains("Hidden.Movie"), "{body}");
}

#[tokio::test]
async fn unavailable_sections_and_errors() {
    let all = catalogue();
    let f = fixture(&all, true);
    let c = session(&f).await;
    let c = Some(c.as_str());
    let (_, body) = json_of(&f.app, get("/api/v1/discover", c)).await;
    for (section, reason) in [
        ("rising", "needs_swarm_history"),
        ("recently_resolved", "needs_resolution_history"),
    ] {
        assert_eq!(
            body[section],
            json!({ "hits": [], "reason": reason, "status": "unavailable", "truncated": false }),
            "{section}"
        );
    }
    golden(
        &f,
        "discover_bad_cursor",
        get("/api/v1/discover?cursor=00", c),
    )
    .await;
    let (status, _) = call(&f.app, get("/api/v1/discover", None)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    let f = fixture(&all, false);
    let c = session(&f).await;
    let (status, body) = json_of(&f.app, get("/api/v1/discover", Some(&c))).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(body["error"]["code"], "search_disabled");
}

#[tokio::test]
async fn adult_follows_the_setting() {
    let all = catalogue();
    // Hidden (the default): the Adult tab is empty and says why.
    let f = fixture(&all, true);
    let c = session(&f).await;
    let (_, body) = json_of(&f.app, get("/api/v1/discover?type=adult", Some(&c))).await;
    assert_eq!(body["newest"]["hits"], json!([]));
    assert_eq!(body["newest"]["notices"], json!(["adult_hidden"]));
    let (_, body) = json_of(&f.app, get("/api/v1/discover", Some(&c))).await;
    assert_eq!(body["newest"]["notices"], json!([]));

    // Shown: Adult appears in Newest and its tab, hidden torrents still don't.
    let f = fixture_with(&all, true, true);
    let c = session(&f).await;
    let c = Some(c.as_str());
    let everything = all_pages(&f, "/api/v1/discover?limit=2", c).await;
    assert!(
        everything.contains(&all[4].name.to_string()),
        "{everything:?}"
    );
    assert!(
        !everything.contains(&all[2].name.to_string()),
        "{everything:?}"
    );
    let (_, body) = json_of(&f.app, get("/api/v1/discover?type=adult", c)).await;
    assert_eq!(names(&body["newest"]), vec![all[4].name.to_string()]);
    assert_eq!(body["newest"]["notices"], json!([]));
    assert_eq!(body["newest"]["total"], 1);
}
