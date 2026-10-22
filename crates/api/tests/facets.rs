//! `/facets` (§8, §10.4, B6): the facet rail's counts over exactly what
//! `/search` returns, reconciled against independent counts over the
//! store (the `analytics-validation` matrix, §10.17), read from a
//! search's lease, and its response bytes (goldens in B1's style, in
//! `tests/golden/facets_*.txt`; `LODESTAR_BLESS=1` rewrites them).

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Request, StatusCode, header};
use lodestar_api::{ApiState, Auth, router};
use lodestar_bencode::{OwnedValue, to_vec};
use lodestar_classify::{ContentType, Record};
use lodestar_metainfo::Info;
use lodestar_search::SearchIndex;
use lodestar_store::{NewTorrent, SourceId, Store, StoreConfig, flags};
use serde_json::{Value, json};
use tower::ServiceExt;

const PASSWORD: &str = "correct horse battery";
/// Discovery times: one a day from here.
const T0: i64 = 1_700_000_000;
const DAY: i64 = 86_400;
/// The crawler's first run, as the store records it: ten days in.
const FIRST_RUN: i64 = T0 + 10 * DAY;

/// Torrent `i`: a movie, an episode, an album or a program, with a
/// resolution, year, language and files that vary with `i`. Every
/// fourth is private.
fn raw(i: usize) -> Vec<u8> {
    let b = |s: &str| OwnedValue::Bytes(s.as_bytes().to_vec());
    let d = |e: Vec<(&str, OwnedValue)>| {
        OwnedValue::Dict(
            e.into_iter()
                .map(|(k, v)| (k.as_bytes().to_vec(), v))
                .collect(),
        )
    };
    let res = ["1080p", "2160p", "720p"][i % 3];
    let year = 1995 + (i % 7) * 4;
    let lang = ["", ".FRENCH", ".GERMAN"][i % 3];
    let (name, files): (String, Vec<(String, i64)>) = match i % 4 {
        0 => {
            let n = format!("Movie.Title{i}.{year}{lang}.{res}.BluRay.x264-GRP{}", i % 5);
            let mut files = vec![(format!("{n}.mkv"), (1 << 30) + i as i64 * 7_000_000)];
            if i % 8 == 0 {
                files.push((format!("{n}.nfo"), 2_000));
            }
            (n, files)
        }
        1 => {
            let n = format!("Show.Name{i}.S01E0{}.{res}.WEB-DL.x265-TEAM", 1 + i % 9);
            let files = vec![
                (format!("{n}.mkv"), 300_000_000 + i as i64 * 1_000_000),
                (format!("{n}.srt"), 40_000),
            ];
            (n, files)
        }
        2 => {
            let n = format!("Artist{i} - Album{i} ({year}) [FLAC]");
            let files = (1..=4)
                .map(|t| (format!("0{t} Track.flac"), 30_000_000 + t * 1_000))
                .chain([("cover.jpg".to_owned(), 90_000)])
                .collect();
            (n, files)
        }
        _ => {
            let n = format!("Tool{i}.v1.{}.Setup", i % 10);
            (n.clone(), vec![(format!("{n}.exe"), 5_000_000 + i as i64)])
        }
    };
    let list = files
        .iter()
        .map(|(p, len)| {
            d(vec![
                ("length", OwnedValue::Int(*len)),
                ("path", OwnedValue::List(vec![b(p)])),
            ])
        })
        .collect();
    let mut entries = vec![
        ("files", OwnedValue::List(list)),
        ("name", b(&name)),
        ("piece length", OwnedValue::Int(1 << 16)),
        ("pieces", OwnedValue::Bytes(vec![0; 20])),
    ];
    if i % 4 == 1 {
        entries.push(("private", OwnedValue::Int(1)));
    }
    to_vec(&d(entries))
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
    /// Stores, classifies and indexes `n` more torrents; every seventh
    /// is hidden by a filter rule.
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
                    discovered_at: T0 + i as i64 * DAY,
                    metadata_at: T0 + i as i64 * DAY + 1,
                    extra_flags: if i % 7 == 6 { flags::HIDDEN } else { 0 },
                    source: SourceId::new(0).unwrap(),
                }
            })
            .collect();
        self.store.insert(&batch).unwrap();
        let ids = self
            .store
            .unenriched(0, lodestar_classify::ENRICHMENT_VERSION, 10_000)
            .unwrap();
        let mut enriched = Vec::new();
        for id in ids {
            let row = self.store.torrent(id).unwrap().unwrap();
            let tree = self.store.files(id).unwrap().unwrap();
            let files = lodestar_classify::file_infos(&tree.files, tree.encoding);
            let (e, _) = lodestar_filter::reclassify::enrich_corrected(
                &row.name,
                &files,
                lodestar_classify::Correction::default(),
            );
            enriched.push((id, e));
        }
        self.store.set_enrichments(&enriched).unwrap();
        let mut w = self.index.writer(15 << 20).unwrap();
        w.catch_up(&mut self.store, 10_000).unwrap();
        w.commit().unwrap();
        self.added += n;
    }
}

fn fixture(n: usize) -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("t.db");
    let mut store = Store::open(&db, StoreConfig::default()).unwrap();
    store.set_setting_i64("first_run_at", FIRST_RUN).unwrap();
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
            adult: Default::default(),
            hot: None,
            events: None,
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

async fn call(app: &Router, mut req: Request<Body>) -> (StatusCode, Option<String>, String) {
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
    (status, cookie, String::from_utf8(bytes.to_vec()).unwrap())
}

async fn json_of(app: &Router, req: Request<Body>) -> (StatusCode, Value) {
    let (status, _, body) = call(app, req).await;
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
    let (status, cookie, _) = call(
        &f.app,
        post(
            "/api/v1/setup",
            None,
            json!({ "token": token, "password": PASSWORD }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    cookie.unwrap()
}

/// `took_ms` zeroed, the rest as sent.
fn normalize(body: &str) -> String {
    let pat = "\"took_ms\":";
    let Some(i) = body.find(pat) else {
        return body.to_string();
    };
    let start = i + pat.len();
    let end = body[start..]
        .find(|c: char| !(c.is_ascii_digit() || matches!(c, '.' | 'e' | 'E' | '-' | '+')))
        .map_or(body.len(), |n| start + n);
    format!("{}0{}", &body[..start], &body[end..])
}

/// Checks status and body against `tests/golden/{name}.txt`.
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
            "{name}: the response bytes changed"
        );
    }
    serde_json::from_str(&body).unwrap_or(Value::Null)
}

/// An independent count over the store: `SELECT COUNT(*)` with `cond`
/// over the torrents a search shows (not hidden unless `hidden`).
fn sql_count(store: &Store, cond: &str, hidden: bool) -> u64 {
    let visible = if hidden {
        "1".to_owned()
    } else {
        format!("flags & {} = 0", flags::HIDDEN)
    };
    let sql = format!("SELECT COUNT(*) FROM torrents WHERE ({cond}) AND {visible}");
    store
        .connection()
        .query_row(&sql, [], |r| r.get::<_, i64>(0))
        .unwrap() as u64
}

/// `SELECT … GROUP BY content_type`, by type name.
fn sql_types(store: &Store, cond: &str, hidden: bool) -> BTreeMap<String, u64> {
    let visible = if hidden {
        "1".to_owned()
    } else {
        format!("flags & {} = 0", flags::HIDDEN)
    };
    let sql = format!(
        "SELECT content_type, COUNT(*) FROM torrents WHERE ({cond}) AND {visible} GROUP BY content_type"
    );
    let conn = store.connection();
    let mut stmt = conn.prepare(&sql).unwrap();
    stmt.query_map([], |r| Ok((r.get::<_, u8>(0)?, r.get::<_, i64>(1)?)))
        .unwrap()
        .map(|r| {
            let (code, n) = r.unwrap();
            (
                ContentType::from_code(code).unwrap().as_str().to_owned(),
                n as u64,
            )
        })
        .collect()
}

/// The values of a release field over the visible torrents matching
/// `cond`, from the stored records (not the index).
fn stored_values(store: &Store, cond: &str, field: &str) -> BTreeMap<String, u64> {
    let sql = format!(
        "SELECT p.data FROM torrent_parsed p JOIN torrents t ON t.id = p.torrent_id \
         WHERE ({cond}) AND t.flags & {} = 0",
        flags::HIDDEN
    );
    let conn = store.connection();
    let mut stmt = conn.prepare(&sql).unwrap();
    let mut out = BTreeMap::new();
    for data in stmt.query_map([], |r| r.get::<_, Vec<u8>>(0)).unwrap() {
        let record = Record::decode(&data.unwrap()).unwrap();
        let mut seen = Vec::new();
        for v in record
            .release_all(field)
            .flat_map(|v| v.split(", "))
            .map(str::to_lowercase)
        {
            if !seen.contains(&v) {
                seen.push(v);
            }
        }
        for v in seen {
            *out.entry(v).or_default() += 1;
        }
    }
    out
}

fn counts(facet: &Value) -> BTreeMap<String, u64> {
    facet["values"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|v| v["count"].as_u64().unwrap() > 0)
        .map(|v| {
            (
                v["value"].as_str().unwrap().to_owned(),
                v["count"].as_u64().unwrap(),
            )
        })
        .collect()
}

fn count_of(facet: &Value, value: &str) -> u64 {
    facet["values"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["value"] == value)
        .map_or(0, |v| v["count"].as_u64().unwrap())
}

#[tokio::test]
async fn facets_count_what_search_returns() {
    let f = fixture(84);
    let cookie = session(&f).await;
    let c = Some(cookie.as_str());
    for (q, hidden) in [
        ("", false),
        ("", true),
        ("movie", false),
        ("type:movie,tv", false),
        ("type:music private:no", true),
        ("res:1080p | res:720p", false),
        ("-res:2160p ext:mkv", false),
        ("nothing matches this", false),
    ] {
        let enc = |s: &str| {
            s.replace(' ', "%20")
                .replace('|', "%7C")
                .replace(':', "%3A")
        };
        let (status, search) = json_of(
            &f.app,
            get(&format!("/api/v1/search?q={}&hidden={hidden}", enc(q)), c),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{q}");
        let (status, by_get) = json_of(
            &f.app,
            get(&format!("/api/v1/facets?q={}&hidden={hidden}", enc(q)), c),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{q}: {by_get}");
        let (_, by_post) = json_of(
            &f.app,
            post("/api/v1/facets", c, json!({ "q": q, "hidden": hidden })),
        )
        .await;
        for got in [&by_get, &by_post] {
            assert_eq!(got["total"], search["total"], "{q} hidden={hidden}");
            assert_eq!(got["total_approx"], json!(false));
            assert_eq!(got["canonical"], search["canonical"]);
            // Histograms reconcile to the result count.
            let total = search["total"].as_u64().unwrap();
            for (name, h) in got["histograms"].as_object().unwrap() {
                let sum: u64 = h["bins"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|b| b["count"].as_u64().unwrap())
                    .sum::<u64>()
                    + h["missing"].as_u64().unwrap();
                assert_eq!(sum, total, "{q}: {name}");
            }
        }
        assert_eq!(by_get, {
            let mut p = by_post.clone();
            p["took_ms"] = by_get["took_ms"].clone();
            p
        });
    }
}

#[tokio::test]
async fn counts_reconcile_with_independent_store_counts() {
    let f = fixture(84);
    let cookie = session(&f).await;
    let c = Some(cookie.as_str());
    let code = |t: ContentType| t.code();
    let (movie, tv, music) = (
        code(ContentType::Movie),
        code(ContentType::Tv),
        code(ContentType::Music),
    );

    // Browse: every facet over every visible torrent.
    let (_, got) = json_of(&f.app, post("/api/v1/facets", c, json!({}))).await;
    let all = sql_count(&f.store, "1", false);
    assert_eq!(got["total"].as_u64().unwrap(), all);
    assert_eq!(
        counts(&got["facets"]["type"]),
        sql_types(&f.store, "1", false)
    );
    let private = sql_count(&f.store, &format!("flags & {} != 0", flags::PRIVATE), false);
    assert_eq!(count_of(&got["facets"]["private"], "yes"), private);
    assert_eq!(count_of(&got["facets"]["private"], "no"), all - private);
    for (facet, field) in [("res", "res"), ("lang", "lang"), ("group", "group")] {
        assert_eq!(
            counts(&got["facets"][facet]),
            stored_values(&f.store, "1", field),
            "{facet}"
        );
    }
    // Every histogram bin is a `COUNT(*)` over its edges.
    for (name, column) in [("size", "size"), ("discovered", "discovered_at")] {
        for b in got["histograms"][name]["bins"].as_array().unwrap() {
            let lo = b["lo"].as_i64().unwrap();
            let cond = match b["hi"].as_i64() {
                Some(hi) => format!("{column} >= {lo} AND {column} < {hi}"),
                None => format!("{column} >= {lo}"),
            };
            assert_eq!(
                b["count"].as_u64().unwrap(),
                sql_count(&f.store, &cond, false),
                "{name} {b}"
            );
        }
    }
    let discovered = &got["histograms"]["discovered"];
    assert_eq!(discovered["unit"], json!("day"));
    assert_eq!(got["backfill_before"], json!(FIRST_RUN));
    for b in discovered["bins"].as_array().unwrap() {
        let ends_before = b["hi"].as_i64().unwrap() <= FIRST_RUN;
        assert_eq!(b["backfill"], json!(ends_before), "{b}");
    }

    // A type selection: `type` ignores it, every other facet counts the
    // results, and the selected types' counts sum to the result count.
    let (_, got) = json_of(
        &f.app,
        post("/api/v1/facets", c, json!({ "q": "type:movie,tv" })),
    )
    .await;
    let picked = format!("content_type IN ({movie}, {tv})");
    let results = sql_count(&f.store, &picked, false);
    assert_eq!(got["total"].as_u64().unwrap(), results);
    let types = &got["facets"]["type"];
    assert_eq!(counts(types), sql_types(&f.store, "1", false));
    let selected: u64 = types["values"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|v| v["selected"] == json!(true))
        .map(|v| v["count"].as_u64().unwrap())
        .sum();
    assert_eq!(selected, results);
    assert_eq!(
        count_of(&got["facets"]["private"], "yes"),
        sql_count(
            &f.store,
            &format!("{picked} AND flags & {} != 0", flags::PRIVATE),
            false
        )
    );
    assert_eq!(
        counts(&got["facets"]["res"]),
        stored_values(&f.store, &format!("t.{picked}"), "res")
    );

    // Two selections, hidden torrents included: each facet drops only
    // its own clause.
    let (_, got) = json_of(
        &f.app,
        post(
            "/api/v1/facets",
            c,
            json!({ "q": "type:music private:no", "hidden": true }),
        ),
    )
    .await;
    let not_private = format!("flags & {} = 0", flags::PRIVATE);
    assert_eq!(
        counts(&got["facets"]["type"]),
        sql_types(&f.store, &not_private, true)
    );
    let music_only = format!("content_type = {music}");
    assert_eq!(
        count_of(&got["facets"]["private"], "no"),
        sql_count(&f.store, &format!("{music_only} AND {not_private}"), true)
    );
    assert_eq!(
        count_of(&got["facets"]["private"], "yes"),
        sql_count(
            &f.store,
            &format!("{music_only} AND flags & {} != 0", flags::PRIVATE),
            true
        )
    );
}

#[tokio::test]
async fn a_lease_counts_its_search_snapshot() {
    let mut f = fixture(40);
    let cookie = session(&f).await;
    let c = Some(cookie.as_str());
    let (_, search) = json_of(&f.app, get("/api/v1/search?q=&limit=5", c)).await;
    let cursor = search["next_cursor"].as_str().unwrap().to_owned();
    let before = search["total"].as_u64().unwrap();
    f.add(20);
    let (status, leased) = json_of(&f.app, get(&format!("/api/v1/facets?lease={cursor}"), c)).await;
    assert_eq!(status, StatusCode::OK, "{leased}");
    assert_eq!(leased["total"].as_u64().unwrap(), before);
    let types: u64 = counts(&leased["facets"]["type"]).values().sum();
    assert_eq!(types, before, "facets from the lease's snapshot");
    let (_, now) = json_of(&f.app, get("/api/v1/facets", c)).await;
    assert!(now["total"].as_u64().unwrap() > before);
    // A lease for another search, or a forged one, is refused.
    let (status, body) = json_of(
        &f.app,
        get(&format!("/api/v1/facets?q=movie&lease={cursor}"), c),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["code"], json!("bad_cursor"));
    assert_eq!(body["error"]["field"], json!("lease"));
    let (status, body) = json_of(&f.app, get("/api/v1/facets?lease=00", c)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["field"], json!("lease"));
}

#[tokio::test]
async fn golden_responses() {
    let f = fixture(28);
    let cookie = session(&f).await;
    let c = Some(cookie.as_str());
    golden(&f, "facets_browse", get("/api/v1/facets", c)).await;
    golden(
        &f,
        "facets_selection",
        post(
            "/api/v1/facets",
            c,
            json!({ "q": "type:movie,tv -res:720p", "limits": { "group": 1, "ext": 2 } }),
        ),
    )
    .await;
    golden(
        &f,
        "facets_get_limits",
        get("/api/v1/facets?q=movie&limit.lang=1&hidden=1", c),
    )
    .await;
    golden(
        &f,
        "facets_unknown_facet",
        get("/api/v1/facets?limit.seeders=3", c),
    )
    .await;
    golden(
        &f,
        "facets_bad_limit",
        post("/api/v1/facets", c, json!({ "limits": { "ext": 101 } })),
    )
    .await;
    golden(
        &f,
        "facets_bad_parameter",
        get("/api/v1/facets?limit.ext=many", c),
    )
    .await;
    golden(
        &f,
        "facets_ast_needs_post",
        get("/api/v1/facets?ast=%7B%7D", c),
    )
    .await;
    golden(
        &f,
        "facets_query_syntax",
        get("/api/v1/facets?q=sise%3A1", c),
    )
    .await;
    golden(
        &f,
        "facets_unknown_field",
        post("/api/v1/facets", c, json!({ "q": "x", "limit": 3 })),
    )
    .await;
    golden(&f, "facets_login_required", get("/api/v1/facets", None)).await;
}

#[tokio::test]
async fn swarm_buckets_are_null_until_m4() {
    let f = fixture(8);
    let cookie = session(&f).await;
    let (_, got) = json_of(&f.app, get("/api/v1/facets", Some(&cookie))).await;
    assert_eq!(got["buckets"], json!({ "seeders": null, "trackers": null }));
    assert_eq!(got["unavailable"], json!(["seeders", "trackers"]));
    assert_eq!(got["sampled"], json!(false));
    assert_eq!(got["sample_rate"], Value::Null);
}

#[tokio::test]
async fn search_disabled() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("t.db");
    Store::open(&db, StoreConfig::default()).unwrap();
    let auth = Arc::new(Auth::load(&dir.path().join("auth.json"), false).unwrap());
    let app = Router::new().nest(
        "/api/v1",
        router(ApiState {
            auth: auth.clone(),
            index: None,
            reader: Arc::new(Mutex::new(
                Store::open_reader(&db, StoreConfig::default()).unwrap(),
            )),
            filters: None,
            settings: None,
            adult: Default::default(),
            hot: None,
            events: None,
        }),
    );
    let token = auth.setup_token().unwrap();
    let (_, cookie, _) = call(
        &app,
        post(
            "/api/v1/setup",
            None,
            json!({ "token": token, "password": PASSWORD }),
        ),
    )
    .await;
    let (status, body) = json_of(&app, get("/api/v1/facets", cookie.as_deref())).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(body["error"]["code"], json!("search_disabled"));
}
