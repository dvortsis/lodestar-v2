//! Golden compatibility tests: every `/api/v1` endpoint's response bytes,
//! status included, against the files in `tests/golden/`.
//!
//! The files were recorded from the `json!`-built handlers, before the
//! typed DTOs (`src/dto/`) replaced them, so these tests prove the DTOs
//! serialize byte for byte as the old code did: same keys, same order,
//! same null and absent fields. The one intended difference is the swarm
//! fields of `docs/metrics.md` §7.1/§7.2, added as `null` (never scraped):
//! each test checks they're all there and null on every hit and detail,
//! then strips them, and the rest must match the old bytes exactly.
//!
//! Since then, fields were added through bless (B5a: `next_cursor`,
//! `total_approx` and the classification fields of hits).
//!
//! Values that move between runs (times, `took_ms`) are zeroed before the
//! comparison, and a `next_cursor` (random lease ids) becomes `"…"`. `LODESTAR_BLESS=1 cargo test -p lodestar-api --test golden`
//! rewrites the files; review the diff before committing it.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Request, header};
use lodestar_api::filters::FiltersApi;
use lodestar_api::{ApiState, Auth, router};
use lodestar_bencode::{OwnedValue, to_vec};
use lodestar_filter::lists::SharedIps;
use lodestar_filter::{Action, Core, CsList, Engine, Handle, HeldSet, Lists, Sealer, Switches};
use lodestar_metainfo::Info;
use lodestar_search::SearchIndex;
use lodestar_store::{Filtered, Held, NewTorrent, SourceId, Store, StoreConfig, WriteBatch};
use serde_json::{Value, json};
use tower::ServiceExt;

static NEXT_IP: AtomicU32 = AtomicU32::new(1);

const PASSWORD: &str = "correct horse battery";

/// Keys whose numbers depend on the clock.
const VOLATILE: [&str; 6] = [
    "took_ms",
    "at",
    "expires_at",
    "shadow_until",
    "checked_at",
    "updated_at",
];

/// Swarm fields added to every search hit and detail (§7.1), null until
/// swarm health is served.
const SUMMARY_SWARM: [&str; 6] = [
    "completed",
    "last_scrape_at",
    "leechers",
    "seeders",
    "trackers_checked",
    "trackers_reporting",
];
/// And to the detail view only (§7.2).
const DETAIL_SWARM: [&str; 4] = ["dht_peers", "dht_seeds", "swarm_history", "swarm_trend"];

/// Checks the added swarm fields are all present and null, and removes
/// them from `body`'s bytes.
fn strip_swarm(body: &str) -> String {
    let Ok(v) = serde_json::from_str::<Value>(body) else {
        return body.to_string();
    };
    let mut keys: Vec<&str> = Vec::new();
    let check = |t: &Value, names: &[&str]| {
        for k in names {
            assert_eq!(t.get(*k), Some(&Value::Null), "{k} should be null in {t}");
        }
    };
    if let Some(hits) = v.get("hits").and_then(Value::as_array) {
        hits.iter().for_each(|h| check(h, &SUMMARY_SWARM));
        if !hits.is_empty() {
            keys.extend(SUMMARY_SWARM);
        }
    }
    if v.get("magnet").is_some() {
        check(&v, &SUMMARY_SWARM);
        check(&v, &DETAIL_SWARM);
        keys.extend(SUMMARY_SWARM);
        keys.extend(DETAIL_SWARM);
    }
    let mut out = body.to_string();
    for k in keys {
        // Never alone in an object: drop it with the comma before it, or
        // after it when it's first.
        out = out
            .replace(&format!(",\"{k}\":null"), "")
            .replace(&format!("\"{k}\":null,"), "");
    }
    out
}

struct Fixture {
    app: Router,
    auth: Arc<Auth>,
    api: Arc<FiltersApi>,
    hashes: Vec<String>,
    _dir: tempfile::TempDir,
}

fn torrent(name: &str, files: &[(&str, i64)]) -> Vec<u8> {
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

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("t.db");
    let mut store = Store::open(&db, StoreConfig::default()).unwrap();
    let raws = [
        torrent(
            "The.Dark.Knight.2008.1080p.BluRay.x264-GRP",
            &[
                ("The.Dark.Knight.2008.1080p.mkv", 8 << 30),
                ("Subs/English.srt", 90_000),
                ("Sample/sample.mkv", 40 << 20),
            ],
        ),
        torrent("Batman Begins 720p", &[("batman.mkv", 4 << 30)]),
        torrent("Holiday.Clips.2023.Lexb.mp4", &[("", 900 << 20)]),
        torrent("Gotham.Soundtrack.FLAC", &[("01 Intro.flac", 30 << 20)]),
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
    store.insert(&batch).unwrap();
    // Classified, as the pipeline would have.
    let ids = store
        .unenriched(0, lodestar_classify::ENRICHMENT_VERSION, 100)
        .unwrap();
    let mut enriched = Vec::new();
    for id in ids {
        let row = store.torrent(id).unwrap().unwrap();
        let tree = store.files(id).unwrap().unwrap();
        let files = lodestar_classify::file_infos(&tree.files, tree.encoding);
        let (e, _) = lodestar_filter::reclassify::enrich_corrected(
            &row.name,
            &files,
            lodestar_classify::Correction::default(),
        );
        enriched.push((id, e));
    }
    store.set_enrichments(&enriched).unwrap();
    let index = SearchIndex::in_memory().unwrap();
    let mut w = index.writer(15 << 20).unwrap();
    w.catch_up(&mut store, 10).unwrap();
    w.commit().unwrap();

    let reader = Store::open_reader(&db, StoreConfig::default()).unwrap();
    let engine = Handle::new(Engine::new(Core::new(
        Lists::shipped().unwrap(),
        CsList::default(),
        Switches::default(),
    )));
    let api = Arc::new(FiltersApi {
        store: Arc::new(Mutex::new(store)),
        engine: Arc::new(engine),
        held: Arc::new(HeldSet::default()),
        sealer: Some(Arc::new(Sealer::new(&[7; 32]))),
        ips: Arc::new(SharedIps::default()),
        index: Some(index.clone().into()),
        reenrich: Arc::new(AtomicBool::new(false)),
        jobs: Default::default(),
        quarantine_secs: 30 * 86_400,
        shutdown: Default::default(),
    });
    let auth = Arc::new(Auth::load(&dir.path().join("auth.json"), false).unwrap());
    let app = Router::new().nest(
        "/api/v1",
        router(ApiState {
            auth: auth.clone(),
            index: Some(index.into()),
            reader: Arc::new(Mutex::new(reader)),
            filters: Some(api.clone()),
            hot: None,
            events: None,
            settings: None,
            ops: None,
            user_data: Some(Arc::new(Mutex::new(
                Store::open(&db, StoreConfig::default()).unwrap(),
            ))),
            adult: Default::default(),
            insights: None,
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
        api,
        hashes,
        _dir: dir,
    }
}

/// Zeroes the number after each `"key":` in [`VOLATILE`], and blanks
/// cursors.
fn normalize(body: &str) -> String {
    let mut out = body.to_string();
    let pat = "\"next_cursor\":\"";
    let mut from = 0;
    while let Some(i) = out[from..].find(pat) {
        let start = from + i + pat.len();
        let end = out[start..].find('"').map_or(out.len(), |n| start + n);
        out.replace_range(start..end, "…");
        from = start;
    }
    for key in VOLATILE {
        let pat = format!("\"{key}\":");
        let mut from = 0;
        while let Some(i) = out[from..].find(&pat) {
            let start = from + i + pat.len();
            let end = out[start..]
                .find(|c: char| !(c.is_ascii_digit() || matches!(c, '.' | '-' | 'e' | 'E' | '+')))
                .map_or(out.len(), |n| start + n);
            if end > start {
                out.replace_range(start..end, "0");
            }
            from = start;
        }
    }
    out
}

/// Sends `req` and checks the status and body against `tests/golden/{name}.txt`.
/// Each request comes from its own address unless it says otherwise, so
/// the per-IP login limit only bites where a case means it to.
async fn golden(f: &Fixture, name: &str, req: Request<Body>) -> Value {
    golden_blanking(f, name, req, &[]).await
}

/// [`golden`], with the string values of `blank` (random secrets and
/// prefixes) as `"…"` and these numbers zeroed: `created_at`,
/// `last_used_at`, `revoked_at`.
async fn golden_blanking(f: &Fixture, name: &str, mut req: Request<Body>, blank: &[&str]) -> Value {
    if req.extensions().get::<ConnectInfo<SocketAddr>>().is_none() {
        let n = NEXT_IP.fetch_add(1, Ordering::Relaxed);
        req.extensions_mut().insert(ConnectInfo(SocketAddr::from((
            [10, 0, (n >> 8) as u8, n as u8],
            1,
        ))));
    }
    let resp = f.app.clone().oneshot(req).await.unwrap();
    let status = resp.status().as_u16();
    let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20)
        .await
        .unwrap();
    let body = String::from_utf8(bytes.to_vec()).unwrap();
    let mut shown = normalize(&strip_swarm(&body));
    if !blank.is_empty() {
        shown = blank_keys(&shown, blank);
    }
    let got = format!("{status}\n{shown}\n");
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden")
        .join(format!("{name}.txt"));
    if std::env::var_os("LODESTAR_BLESS").is_some() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, &got).unwrap();
    } else {
        let want = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("{}: {e} (bless with LODESTAR_BLESS=1)", path.display()));
        // A Windows checkout may have converted the line ends.
        let want = want.replace("\r\n", "\n");
        assert_eq!(got, want, "{name}: the response bytes changed");
    }
    serde_json::from_str(&body).unwrap_or(Value::Null)
}

/// The keys/tokens cases' volatile values: `blank`'s strings and the
/// creation and use times.
fn blank_keys(body: &str, blank: &[&str]) -> String {
    let Ok(mut v) = serde_json::from_str::<Value>(body) else {
        return body.to_string();
    };
    fn walk(v: &mut Value, blank: &[&str]) {
        match v {
            Value::Object(m) => {
                for (k, x) in m.iter_mut() {
                    if blank.contains(&k.as_str()) && x.is_string() {
                        *x = json!("…");
                    } else if matches!(k.as_str(), "created_at" | "last_used_at" | "revoked_at")
                        && x.is_number()
                    {
                        *x = json!(0);
                    } else {
                        walk(x, blank);
                    }
                }
            }
            Value::Array(a) => a.iter_mut().for_each(|x| walk(x, blank)),
            _ => {}
        }
    }
    walk(&mut v, blank);
    v.to_string()
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
    cookie(
        f,
        post(
            "/api/v1/setup",
            None,
            json!({ "token": token, "password": PASSWORD }),
        ),
    )
    .await
}

/// A session for a fixture that is already set up.
async fn login(f: &Fixture) -> String {
    cookie(
        f,
        post("/api/v1/login", None, json!({ "password": PASSWORD })),
    )
    .await
}

async fn cookie(f: &Fixture, req: Request<Body>) -> String {
    let resp = f.app.clone().oneshot(req).await.unwrap();
    resp.headers()[header::SET_COOKIE]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_string()
}

#[tokio::test]
async fn auth_endpoints() {
    let f = fixture();
    golden(&f, "auth_status_fresh", get("/api/v1/auth", None)).await;
    golden(
        &f,
        "error_login_required_setup",
        get("/api/v1/search?q=x", None),
    )
    .await;
    golden(
        &f,
        "setup_bad_token",
        post(
            "/api/v1/setup",
            None,
            json!({ "token": "nope", "password": PASSWORD }),
        ),
    )
    .await;
    let token = f.auth.setup_token().unwrap();
    golden(
        &f,
        "setup_weak_password",
        post(
            "/api/v1/setup",
            None,
            json!({ "token": token, "password": "short" }),
        ),
    )
    .await;
    golden(
        &f,
        "error_bad_body_missing_field",
        post("/api/v1/setup", None, json!({ "token": token })),
    )
    .await;
    golden(
        &f,
        "error_not_json",
        Request::post("/api/v1/login")
            .header(header::CONTENT_TYPE, "text/plain")
            .body(Body::from("password=x"))
            .unwrap(),
    )
    .await;
    golden(
        &f,
        "error_bad_json",
        Request::post("/api/v1/login")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from("{"))
            .unwrap(),
    )
    .await;
    golden(
        &f,
        "setup_ok",
        post(
            "/api/v1/setup",
            None,
            json!({ "token": token, "password": PASSWORD }),
        ),
    )
    .await;
    golden(
        &f,
        "setup_done",
        post(
            "/api/v1/setup",
            None,
            json!({ "token": token, "password": PASSWORD }),
        ),
    )
    .await;
    golden(
        &f,
        "login_bad_password",
        post(
            "/api/v1/login",
            None,
            json!({ "password": "wrong wrong wrong" }),
        ),
    )
    .await;
    golden(
        &f,
        "login_ok",
        post("/api/v1/login", None, json!({ "password": PASSWORD })),
    )
    .await;
    let c = login(&f).await;
    golden(&f, "auth_status_logged_in", get("/api/v1/auth", Some(&c))).await;
    golden(
        &f,
        "error_login_required",
        get("/api/v1/search?q=x", Some("lodestar_session=forged")),
    )
    .await;
    golden(
        &f,
        "error_cross_origin",
        Request::post("/api/v1/logout")
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::ORIGIN, "https://evil.example")
            .header(header::HOST, "lodestar.local")
            .body(Body::from("{}"))
            .unwrap(),
    )
    .await;
    golden(
        &f,
        "password_bad_old",
        post(
            "/api/v1/password",
            Some(&c),
            json!({ "old": "not the password", "new": "a brand new password" }),
        ),
    )
    .await;
    golden(
        &f,
        "password_ok",
        post(
            "/api/v1/password",
            Some(&c),
            json!({ "old": PASSWORD, "new": "a brand new password" }),
        ),
    )
    .await;
    golden(&f, "logout_ok", post("/api/v1/logout", None, json!({}))).await;
    // Five attempts per address, then a wait.
    for i in 0..6 {
        let mut req = post(
            "/api/v1/login",
            None,
            json!({ "password": "wrong wrong wrong" }),
        );
        req.extensions_mut()
            .insert(ConnectInfo(SocketAddr::from(([192, 0, 2, 1], 1))));
        golden(&f, &format!("login_attempt_{i}"), req).await;
    }
    golden(&f, "error_no_route", get("/api/v1/nope", None)).await;
    golden(
        &f,
        "error_method_not_allowed",
        Request::delete("/api/v1/login")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
}

#[tokio::test]
async fn search_and_parse_endpoints() {
    let f = fixture();
    let c = session(&f).await;
    let c = Some(c.as_str());
    let parse = |q: &str| {
        format!(
            "/api/v1/parse?q={}",
            form_urlencoded::byte_serialize(q.as_bytes()).collect::<String>()
        )
    };
    golden(
        &f,
        "parse_every_node",
        get(
            &parse(
                "(dark | knight) -cam \"dark knight\" dark NEAR/3 knight comput* ~knigth \
                 size:700MB..1.5GB discovered:<7d ext:mkv private:no res:1080 \
                 file:(ext:flac size:>50MB) cat:video>=80% bytes:video=10..50 year:2019..2021",
            ),
            c,
        ),
    )
    .await;
    golden(&f, "parse_empty", get("/api/v1/parse", c)).await;
    golden(&f, "parse_error_span", get(&parse("x sise:1"), c)).await;
    golden(&f, "search_words", get("/api/v1/search?q=batman", c)).await;
    golden(
        &f,
        "search_browse_sorted_paged",
        get("/api/v1/search?sort=%2Bsize&limit=2&offset=1", c),
    )
    .await;
    for (i, q) in [
        "file%3Aext%3Amkv",
        "file%3Asample",
        "file%3A%28knight%29",
        "dark+file%3Amkv",
    ]
    .iter()
    .enumerate()
    {
        golden(
            &f,
            &format!("search_file_clause_{i}"),
            get(&format!("/api/v1/search?q={q}"), c),
        )
        .await;
    }
    golden(
        &f,
        "search_hidden_included",
        get("/api/v1/search?q=gotham&hidden=true", c),
    )
    .await;
    let ast = json!({ "op": "word", "field": "any", "word": "knight" });
    golden(
        &f,
        "search_post_ast",
        post("/api/v1/search", c, json!({ "ast": ast, "limit": 5 })),
    )
    .await;
    golden(
        &f,
        "search_post_q",
        post(
            "/api/v1/search",
            c,
            json!({ "q": "dark", "sort": "-files" }),
        ),
    )
    .await;
    golden(
        &f,
        "search_unknown_sort",
        get("/api/v1/search?q=x&sort=popularity", c),
    )
    .await;
    golden(
        &f,
        "search_bad_parameter",
        get("/api/v1/search?limit=lots", c),
    )
    .await;
    golden(
        &f,
        "search_window_too_deep",
        get("/api/v1/search?offset=9990&limit=20", c),
    )
    .await;
    golden(&f, "search_ast_needs_post", get("/api/v1/search?ast=x", c)).await;
    golden(
        &f,
        "search_q_and_ast",
        post("/api/v1/search", c, json!({ "q": "x", "ast": ast })),
    )
    .await;
    golden(
        &f,
        "search_invalid_ast",
        post(
            "/api/v1/search",
            c,
            json!({ "ast": { "op": "and", "of": [] } }),
        ),
    )
    .await;
    golden(
        &f,
        "search_bad_ast_field",
        post(
            "/api/v1/search",
            c,
            json!({ "ast": { "op": "word", "field": "nowhere", "word": "x" } }),
        ),
    )
    .await;
}

#[tokio::test]
async fn torrent_endpoints() {
    let f = fixture();
    let c = session(&f).await;
    let c = Some(c.as_str());
    for (i, h) in f.hashes.iter().enumerate() {
        golden(
            &f,
            &format!("torrent_detail_{i}"),
            get(&format!("/api/v1/torrents/{h}"), c),
        )
        .await;
    }
    let h = &f.hashes[0];
    golden(
        &f,
        "torrent_files_page",
        get(&format!("/api/v1/torrents/{h}/files?offset=1&limit=1"), c),
    )
    .await;
    golden(
        &f,
        "torrent_files_all",
        get(&format!("/api/v1/torrents/{h}/files"), c),
    )
    .await;
    golden(
        &f,
        "torrent_files_bad_param",
        get(&format!("/api/v1/torrents/{h}/files?offset=-1"), c),
    )
    .await;
    golden(&f, "torrent_bad_hash", get("/api/v1/torrents/xyz", c)).await;
    golden(
        &f,
        "torrent_not_found",
        get(&format!("/api/v1/torrents/{}", "ab".repeat(20)), c),
    )
    .await;
}

fn quarantine(f: &Fixture, hashes: &[[u8; 20]], rule: &str) {
    let sealer = f.api.sealer.clone().unwrap();
    let sealed: Vec<Vec<u8>> = hashes
        .iter()
        .enumerate()
        .map(|(i, _)| sealer.seal(&format!("Held.Name.{i}")).unwrap())
        .collect();
    let filtered: Vec<Filtered<'_>> = hashes
        .iter()
        .enumerate()
        .map(|(i, h)| Filtered {
            hashes: std::slice::from_ref(h),
            rule,
            action: Action::Quarantine.code(),
            held: Some(Held::Quarantine),
            sealed: Some(&sealed[i]),
            evidence: &[],
            at: now(),
            expires_at: now() + 86_400,
            adult_hold: false,
        })
        .collect();
    f.api
        .store
        .lock()
        .unwrap()
        .write(&WriteBatch {
            filtered: &filtered,
            ..WriteBatch::default()
        })
        .unwrap();
    for h in hashes {
        f.api.held.insert(*h);
    }
}

async fn job_until_finished(f: &Fixture, c: Option<&str>, job: &Value) -> Request<Body> {
    let uri = format!("/api/v1/filters/preview/jobs/{job}");
    let mut last = Value::Null;
    for _ in 0..1500 {
        let resp = f.app.clone().oneshot(get(&uri, c)).await.unwrap();
        let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20)
            .await
            .unwrap();
        let v: Value = serde_json::from_slice(&bytes).unwrap();
        if v["state"] != "running" {
            return get(&uri, c);
        }
        last = v;
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    panic!("the preview job never finished; last saw {last}");
}

#[tokio::test]
async fn filters_and_classify_endpoints() {
    let f = fixture();
    let c = session(&f).await;
    let c = Some(c.as_str());
    let apply = |change: Value, password: Option<&str>| {
        post(
            "/api/v1/filters/apply",
            c,
            json!({ "change": change, "password": password }),
        )
    };
    golden(&f, "filters_overview_fresh", get("/api/v1/filters", c)).await;
    golden(&f, "filters_queue_empty", get("/api/v1/filters/queue", c)).await;
    golden(&f, "filters_fixtures", get("/api/v1/filters/fixtures", c)).await;

    // Strengthening changes, one of each layer.
    golden(
        &f,
        "apply_add_rule",
        apply(
            json!({ "op": "add_rule", "query": "ext:exe", "action": "hide", "unless": "type:software", "shadow_days": 7 }),
            None,
        ),
    )
    .await;
    golden(
        &f,
        "apply_add_rule_bad_query",
        apply(
            json!({ "op": "add_rule", "query": "ext:", "action": "hide" }),
            None,
        ),
    )
    .await;
    golden(
        &f,
        "apply_add_rule_bad_action",
        apply(
            json!({ "op": "add_rule", "query": "ext:exe", "action": "explode" }),
            None,
        ),
    )
    .await;
    golden(
        &f,
        "apply_add_allow",
        apply(json!({ "op": "add_allow", "query": "linux iso" }), None),
    )
    .await;
    golden(
        &f,
        "apply_add_allow_confirmed",
        apply(
            json!({ "op": "add_allow", "query": "linux iso" }),
            Some(PASSWORD),
        ),
    )
    .await;
    golden(
        &f,
        "apply_pack_switch",
        apply(
            json!({ "op": "pack_switch", "id": "malware", "on": true }),
            None,
        ),
    )
    .await;
    golden(
        &f,
        "apply_subscribe",
        apply(
            json!({ "op": "subscribe", "url": "https://example.org/ips.p2p", "kind": "ip", "name": "Ranges" }),
            None,
        ),
    )
    .await;
    golden(
        &f,
        "apply_subscribe_bad_kind",
        apply(
            json!({ "op": "subscribe", "url": "https://example.org/x", "kind": "weird" }),
            None,
        ),
    )
    .await;
    // Weakening: refused with the assessment, then confirmed.
    for (name, what) in [
        ("hashes", json!({ "hashes": [f.hashes[1]] })),
        ("keyword", json!({ "keyword": "lexb" })),
        ("query", json!({ "query": "size:>1GB" })),
        ("payload", json!({ "payload": "00112233445566ff" })),
    ] {
        let mut change = json!({ "op": "add_override", "scope": "type:video", "expires_days": 30 });
        change
            .as_object_mut()
            .unwrap()
            .extend(what.as_object().unwrap().clone());
        golden(
            &f,
            &format!("apply_add_override_{name}_confirm_required"),
            apply(change.clone(), None),
        )
        .await;
        golden(
            &f,
            &format!("apply_add_override_{name}"),
            apply(change, Some(PASSWORD)),
        )
        .await;
    }
    golden(
        &f,
        "apply_add_override_wrong_password",
        apply(
            json!({ "op": "add_override", "keyword": "x" }),
            Some("wrong wrong wrong"),
        ),
    )
    .await;
    golden(
        &f,
        "apply_bad_match",
        apply(
            json!({ "op": "add_override", "keyword": "x", "query": "y" }),
            Some(PASSWORD),
        ),
    )
    .await;
    golden(
        &f,
        "apply_add_exception",
        apply(
            json!({ "op": "add_exception", "rule": "U-1", "hashes": [f.hashes[2]] }),
            Some(PASSWORD),
        ),
    )
    .await;
    golden(
        &f,
        "apply_floor_locked",
        apply(
            json!({ "op": "core_switch", "rule": "CS-1a", "on": false }),
            Some(PASSWORD),
        ),
    )
    .await;
    golden(
        &f,
        "apply_remove_missing",
        apply(json!({ "op": "remove_rule", "id": 999 }), Some(PASSWORD)),
    )
    .await;
    golden(
        &f,
        "apply_unknown_op",
        apply(json!({ "op": "explode" }), None),
    )
    .await;

    // The review queue, and what releasing from it does.
    quarantine(&f, &[[0x42; 20], [0x43; 20], [0x44; 20]], "CS-3a");
    golden(&f, "filters_queue", get("/api/v1/filters/queue", c)).await;
    golden(
        &f,
        "preview_core_switch_off",
        post(
            "/api/v1/filters/preview",
            c,
            json!({ "change": { "op": "core_switch", "rule": "CS-3a", "on": false } }),
        ),
    )
    .await;
    golden(
        &f,
        "preview_keyword_override",
        post(
            "/api/v1/filters/preview",
            c,
            json!({ "change": { "op": "add_override", "keyword": "held" } }),
        ),
    )
    .await;
    golden(
        &f,
        "apply_reveal_name",
        apply(
            json!({ "op": "reveal_name", "hash": "42".repeat(20) }),
            Some(PASSWORD),
        ),
    )
    .await;
    golden(
        &f,
        "apply_false_positive",
        apply(
            json!({ "op": "false_positive", "hash": "42".repeat(20) }),
            Some(PASSWORD),
        ),
    )
    .await;
    golden(
        &f,
        "apply_confirm",
        apply(json!({ "op": "confirm", "hash": "43".repeat(20) }), None),
    )
    .await;
    golden(
        &f,
        "apply_remove_cs1",
        apply(
            json!({ "op": "remove_cs1", "hashes": ["43".repeat(20)] }),
            Some(PASSWORD),
        ),
    )
    .await;
    golden(
        &f,
        "apply_core_switch_off",
        apply(
            json!({ "op": "core_switch", "rule": "CS-3a", "on": false }),
            Some(PASSWORD),
        ),
    )
    .await;
    golden(
        &f,
        "apply_report_abuse",
        apply(
            json!({ "op": "report_abuse", "hashes": [f.hashes[3]] }),
            None,
        ),
    )
    .await;
    golden(
        &f,
        "apply_undo",
        apply(json!({ "op": "undo" }), Some(PASSWORD)),
    )
    .await;
    golden(&f, "filters_overview_full", get("/api/v1/filters", c)).await;

    // Preview jobs.
    let job = golden(
        &f,
        "preview_job_start",
        post(
            "/api/v1/filters/preview/jobs",
            c,
            json!({ "change": { "op": "add_rule", "query": "ext:scr", "action": "flag" } }),
        ),
    )
    .await["job"]
        .clone();
    let req = job_until_finished(&f, c, &job).await;
    golden(&f, "preview_job_done", req).await;
    let failing = golden(
        &f,
        "preview_job_start_failing",
        post(
            "/api/v1/filters/preview/jobs",
            c,
            json!({ "change": { "op": "core_switch", "rule": "CS-1a", "on": false } }),
        ),
    )
    .await["job"]
        .clone();
    let req = job_until_finished(&f, c, &failing).await;
    golden(&f, "preview_job_failed", req).await;
    golden(
        &f,
        "preview_job_cancel",
        post(
            &format!("/api/v1/filters/preview/jobs/{job}/cancel"),
            c,
            json!({}),
        ),
    )
    .await;
    golden(
        &f,
        "preview_job_not_found",
        get("/api/v1/filters/preview/jobs/999", c),
    )
    .await;
    golden(
        &f,
        "preview_job_bad_path",
        get("/api/v1/filters/preview/jobs/abc", c),
    )
    .await;

    // The editor's live test.
    golden(
        &f,
        "filters_test",
        post(
            "/api/v1/filters/test",
            c,
            json!({
                "query": "ext:exe", "unless": "type:movie",
                "samples": [
                    { "name": "Free.Software.Pack", "files": [
                        { "path": "setup.exe", "size": 50_000_000 },
                        { "path": "readme.txt", "size": 2_000 },
                    ] },
                    { "name": "Some.Movie.2020", "files": ["movie.mkv", "trailer.exe"], "type": "movie" },
                    { "name": "Screens", "files": ["saver.scr"] },
                ],
            }),
        ),
    )
    .await;
    golden(
        &f,
        "filters_test_no_draft",
        post(
            "/api/v1/filters/test",
            c,
            json!({ "samples": [{ "name": "x", "files": ["a.scr"] }] }),
        ),
    )
    .await;
    golden(
        &f,
        "filters_test_unknown_type",
        post(
            "/api/v1/filters/test",
            c,
            json!({ "samples": [{ "name": "x", "type": "nonsense" }] }),
        ),
    )
    .await;

    // Classification.
    golden(&f, "classify_empty", get("/api/v1/classify", c)).await;
    golden(
        &f,
        "classify_correct",
        post(
            "/api/v1/classify/correct",
            c,
            json!({ "hash": f.hashes[1], "adult": true, "not_junk": true }),
        ),
    )
    .await;
    golden(
        &f,
        "classify_correct_clear",
        post(
            "/api/v1/classify/correct",
            c,
            json!({ "hash": f.hashes[0], "adult": null }),
        ),
    )
    .await;
    golden(
        &f,
        "classify_correct_not_found",
        post(
            "/api/v1/classify/correct",
            c,
            json!({ "hash": "ab".repeat(20) }),
        ),
    )
    .await;
    let rule = golden(
        &f,
        "classify_add_rule",
        post(
            "/api/v1/classify/rules",
            c,
            json!({ "query": "lexb", "adult": true }),
        ),
    )
    .await["id"]
        .clone();
    golden(
        &f,
        "classify_add_rule_tests_type",
        post(
            "/api/v1/classify/rules",
            c,
            json!({ "query": "type:movie", "adult": true }),
        ),
    )
    .await;
    golden(&f, "classify_with_rules", get("/api/v1/classify", c)).await;
    golden(
        &f,
        "classify_remove_rule",
        post("/api/v1/classify/rules/remove", c, json!({ "id": rule })),
    )
    .await;
    golden(
        &f,
        "classify_remove_rule_missing",
        post("/api/v1/classify/rules/remove", c, json!({ "id": rule })),
    )
    .await;
}

#[tokio::test]
async fn filters_without_an_index_or_filters() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("t.db");
    let store = Store::open(&db, StoreConfig::default()).unwrap();
    let reader = Store::open_reader(&db, StoreConfig::default()).unwrap();
    let auth = Arc::new(Auth::load(&dir.path().join("auth.json"), false).unwrap());
    let engine = Handle::new(Engine::new(Core::new(
        Lists::shipped().unwrap(),
        CsList::default(),
        Switches::default(),
    )));
    let api = Arc::new(FiltersApi {
        store: Arc::new(Mutex::new(store)),
        engine: Arc::new(engine),
        held: Arc::new(HeldSet::default()),
        sealer: None,
        ips: Arc::new(SharedIps::default()),
        index: None,
        reenrich: Arc::new(AtomicBool::new(false)),
        jobs: Default::default(),
        quarantine_secs: 30 * 86_400,
        shutdown: Default::default(),
    });
    let app = |filters: Option<Arc<FiltersApi>>| {
        Router::new().nest(
            "/api/v1",
            router(ApiState {
                auth: auth.clone(),
                index: None,
                reader: Arc::new(Mutex::new(
                    Store::open_reader(&db, StoreConfig::default()).unwrap(),
                )),
                filters,
                hot: None,
                events: None,
                settings: None,
                ops: None,
                user_data: None,
                adult: Default::default(),
                insights: None,
            }),
        )
    };
    drop(reader);
    let f = Fixture {
        app: app(Some(api.clone())),
        auth: auth.clone(),
        api: api.clone(),
        hashes: Vec::new(),
        _dir: tempfile::tempdir().unwrap(),
    };
    let c = session(&f).await;
    let c = Some(c.as_str());
    // Without an index, a rule change queues nothing (null).
    let rule = golden(
        &f,
        "classify_add_rule_no_index",
        post(
            "/api/v1/classify/rules",
            c,
            json!({ "query": "lexb", "adult": false }),
        ),
    )
    .await["id"]
        .clone();
    golden(
        &f,
        "classify_remove_rule_no_index",
        post("/api/v1/classify/rules/remove", c, json!({ "id": rule })),
    )
    .await;
    golden(&f, "search_disabled", get("/api/v1/search?q=x", c)).await;
    let f = Fixture {
        app: app(None),
        auth,
        api,
        hashes: Vec::new(),
        _dir: dir,
    };
    golden(&f, "filters_unavailable", get("/api/v1/filters", c)).await;
}

/// B12: the keys and tokens, proxy and sessions endpoints.
#[tokio::test]
async fn keys_proxy_and_sessions_endpoints() {
    let f = fixture();
    let c = session(&f).await;
    let c = Some(c.as_str());
    let blank = ["secret", "prefix"];
    golden(&f, "keys_empty", get("/api/v1/keys", c)).await;
    golden(
        &f,
        "keys_create_confirm_required",
        post(
            "/api/v1/keys",
            c,
            json!({ "name": "Prowlarr", "kind": "api_key", "scopes": ["torznab"] }),
        ),
    )
    .await;
    golden(
        &f,
        "keys_create_bad_scopes",
        post(
            "/api/v1/keys",
            c,
            json!({ "name": "Prowlarr", "kind": "api_key", "scopes": [] }),
        ),
    )
    .await;
    golden(
        &f,
        "keys_create_wrong_password",
        post(
            "/api/v1/keys",
            c,
            json!({ "name": "Prowlarr", "kind": "api_key", "scopes": ["torznab"], "password": "nope nope nope" }),
        ),
    )
    .await;
    let made = golden_blanking(
        &f,
        "keys_create",
        post(
            "/api/v1/keys",
            c,
            json!({ "name": "Prowlarr", "kind": "api_key", "scopes": ["torznab", "search"], "password": PASSWORD }),
        ),
        &blank,
    )
    .await;
    let id = made["key"]["id"].as_i64().unwrap();
    golden_blanking(
        &f,
        "keys_create_feed_token",
        post(
            "/api/v1/keys",
            c,
            json!({ "name": "All feeds", "kind": "feed_token", "expires_at": 4_102_444_800_i64, "password": PASSWORD }),
        ),
        &blank,
    )
    .await;
    golden_blanking(&f, "keys_list", get("/api/v1/keys", c), &blank).await;
    golden_blanking(
        &f,
        "keys_regenerate",
        post(
            &format!("/api/v1/keys/{id}/regenerate"),
            c,
            json!({ "password": PASSWORD }),
        ),
        &blank,
    )
    .await;
    golden(
        &f,
        "keys_regenerate_missing",
        post(
            &format!("/api/v1/keys/{id}/regenerate"),
            c,
            json!({ "password": PASSWORD }),
        ),
    )
    .await;
    golden_blanking(
        &f,
        "keys_revoke",
        post(&format!("/api/v1/keys/{}/revoke", id + 1), c, json!({})),
        &blank,
    )
    .await;
    let mut del = post(&format!("/api/v1/keys/{}", id + 1), c, json!({}));
    *del.method_mut() = axum::http::Method::DELETE;
    golden(&f, "keys_delete", del).await;

    golden(&f, "proxy_get", get("/api/v1/auth/proxy", c)).await;
    let mut put = post(
        "/api/v1/auth/proxy",
        c,
        json!({ "enabled": true, "trusted": ["172.17.0.0/16"] }),
    );
    *put.method_mut() = axum::http::Method::PUT;
    golden(&f, "proxy_put_confirm_required", put).await;
    let mut put = post(
        "/api/v1/auth/proxy",
        c,
        json!({ "enabled": true, "trusted": ["0.0.0.0/0"] }),
    );
    *put.method_mut() = axum::http::Method::PUT;
    golden(&f, "proxy_put_bad", put).await;
    let mut put = post(
        "/api/v1/auth/proxy",
        c,
        json!({ "enabled": true, "trusted": ["172.17.0.9/16", "10.0.0.2"], "allowed_users": ["ann"], "password": PASSWORD }),
    );
    *put.method_mut() = axum::http::Method::PUT;
    golden(&f, "proxy_put", put).await;

    golden(
        &f,
        "sessions_revoke_others_confirm_required",
        post("/api/v1/sessions/revoke-others", c, json!({})),
    )
    .await;
    golden(
        &f,
        "sessions_revoke_others",
        post(
            "/api/v1/sessions/revoke-others",
            c,
            json!({ "password": PASSWORD }),
        ),
    )
    .await;
}
