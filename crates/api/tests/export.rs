//! `/export` and `/jobs` through the router: each format byte for byte
//! (goldens in `tests/golden/export_*`; `LODESTAR_BLESS=1` rewrites
//! them), what's never exported, chunking beside a writer with the WAL
//! bounded, the reader lock and backpressure, throttle, cap, pause,
//! resume, cancel, the lease, and the jobs' limits and errors.

use std::future::Future;
use std::future::poll_fn;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::task::Poll;
use std::time::{Duration, Instant};

use axum::body::{Body, BodyDataStream, Bytes};
use axum::http::{Request, StatusCode, header};
use axum::{Extension, Router};
use futures_core::Stream;
use lodestar_api::export::{ExportAdult, ExportConfig};
use lodestar_api::jobs::Jobs;
use lodestar_api::{AdultVisibility, ApiState, Auth, router};
use lodestar_bencode::{OwnedValue, to_vec};
use lodestar_classify::Correction;
use lodestar_metainfo::Info;
use lodestar_search::{LeaseConfig, Leases, SearchHandle, SearchIndex};
use lodestar_store::{NewTorrent, SourceId, Store, StoreConfig, flags};
use serde_json::{Value, json};
use tower::ServiceExt;

const PASSWORD: &str = "correct horse battery";

/// One fixture torrent: raw name bytes, whether it's corrected to Adult,
/// and its extra flags. Single-file, 1 MiB.
#[derive(Clone)]
struct T {
    name: Vec<u8>,
    adult: bool,
    flags: i64,
}

fn t(name: &str) -> T {
    T {
        name: name.as_bytes().to_vec(),
        adult: false,
        flags: 0,
    }
}

fn metainfo(name: &[u8], files: &[(&str, i64)]) -> Vec<u8> {
    let d = |e: Vec<(&str, OwnedValue)>| {
        OwnedValue::Dict(
            e.into_iter()
                .map(|(k, v)| (k.as_bytes().to_vec(), v))
                .collect(),
        )
    };
    let mut entries = vec![
        ("name", OwnedValue::Bytes(name.to_vec())),
        ("piece length", OwnedValue::Int(1 << 16)),
        ("pieces", OwnedValue::Bytes(vec![0; 20])),
    ];
    if files.is_empty() {
        entries.push(("length", OwnedValue::Int(1 << 20)));
    } else {
        let list = files
            .iter()
            .map(|(p, len)| {
                d(vec![
                    ("length", OwnedValue::Int(*len)),
                    (
                        "path",
                        OwnedValue::List(
                            p.split('/')
                                .map(|s| OwnedValue::Bytes(s.as_bytes().to_vec()))
                                .collect(),
                        ),
                    ),
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
    /// The writer, for collections and inserts mid-export.
    store: Arc<Mutex<Store>>,
    index: SearchIndex,
    db: PathBuf,
    hashes: Vec<String>,
    ids: Vec<i64>,
    /// "Show adult content" (search's), live.
    adult: Arc<AtomicBool>,
    /// Export's own adult setting (`export_adult`), live.
    export_adult: Arc<AtomicBool>,
    _dir: tempfile::TempDir,
}

#[derive(Clone, Default)]
struct Opts {
    config: ExportConfig,
    adult: bool,
    no_search: bool,
    leases: Option<LeaseConfig>,
    /// The server's own registry, as `lodestar serve` would layer it.
    jobs: Option<Arc<Jobs>>,
}

/// Stores and classifies `ts` (in order: ids 1, 2, …, discovered one
/// second apart) and serves them.
fn fixture(ts: &[T], o: Opts) -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("t.db");
    let mut store = Store::open(&db, StoreConfig::default()).unwrap();
    let (ids, hashes) = insert(&mut store, ts, 0);
    let index = SearchIndex::in_memory().unwrap();
    {
        let mut w = index.writer(15 << 20).unwrap();
        w.catch_up(&mut store, 100_000).unwrap();
        w.commit().unwrap();
    }
    let auth = Arc::new(Auth::load(&dir.path().join("auth.json"), false).unwrap());
    let reader = Store::open_reader(&db, StoreConfig::default()).unwrap();
    let adult = Arc::new(AtomicBool::new(o.adult));
    let shown = adult.clone();
    let export_adult = Arc::new(AtomicBool::new(o.adult));
    let exported = export_adult.clone();
    let handle: SearchHandle = match o.leases {
        Some(c) => SearchHandle::fixed(index.clone(), Leases::new(c)),
        None => index.clone().into(),
    };
    let app = Router::new()
        .nest(
            "/api/v1",
            router(ApiState {
                auth: auth.clone(),
                index: (!o.no_search).then_some(handle),
                reader: Arc::new(Mutex::new(reader)),
                filters: None,
                settings: None,
                ops: None,
                adult: AdultVisibility::follow(move || shown.load(Ordering::SeqCst)),
                user_data: None,
            }),
        )
        .layer(Extension(o.config))
        .layer(Extension(ExportAdult(AdultVisibility::follow(move || {
            exported.load(Ordering::SeqCst)
        }))));
    let app = match o.jobs {
        Some(jobs) => app.layer(Extension(jobs)),
        None => app,
    };
    Fixture {
        app,
        auth,
        store: Arc::new(Mutex::new(store)),
        index,
        db,
        hashes,
        ids,
        adult,
        export_adult,
        _dir: dir,
    }
}

/// Inserts and classifies `ts`; `n` makes names unique across calls.
fn insert(store: &mut Store, ts: &[T], n: i64) -> (Vec<i64>, Vec<String>) {
    let raws: Vec<Vec<u8>> = ts.iter().map(|t| metainfo(&t.name, &[])).collect();
    let infos: Vec<Info> = raws.iter().map(|r| Info::parse(r).unwrap()).collect();
    let batch: Vec<NewTorrent<'_>> = infos
        .iter()
        .zip(&raws)
        .zip(ts)
        .enumerate()
        .map(|(i, ((info, raw), t))| NewTorrent {
            info,
            raw_info: Some(raw),
            discovered_at: 1_700_000_000 + n + i as i64,
            metadata_at: 1_700_000_001 + n + i as i64,
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
    for (id, t) in ids.iter().zip(ts) {
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
    (ids, hashes)
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

async fn json_of(app: &Router, req: Request<Body>) -> (StatusCode, Value) {
    let resp = app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), 1 << 24)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

/// A download being read a piece at a time.
struct Download {
    job: u64,
    headers: axum::http::HeaderMap,
    body: BodyDataStream,
}

impl Download {
    async fn next(&mut self) -> Option<Result<Bytes, axum::Error>> {
        poll_fn(|cx| Pin::new(&mut self.body).poll_next(cx)).await
    }

    /// The rest of it, or `Err` when it ended with an error.
    async fn rest(mut self) -> Result<Vec<u8>, Vec<u8>> {
        let mut out = Vec::new();
        while let Some(piece) = self.next().await {
            match piece {
                Ok(b) => out.extend_from_slice(&b),
                Err(_) => return Err(out),
            }
        }
        Ok(out)
    }
}

/// Starts an export (`body`: POSTed when `Some`), expecting a 200.
async fn open(f: &Fixture, cookie: &str, uri: &str, body: Option<Value>) -> Download {
    let req = match body {
        Some(b) => post(uri, Some(cookie), b),
        None => get(uri, Some(cookie)),
    };
    let resp = f.app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "{uri}");
    let headers = resp.headers().clone();
    let job = headers["x-export-job"].to_str().unwrap().parse().unwrap();
    Download {
        job,
        headers,
        body: resp.into_body().into_data_stream(),
    }
}

async fn export(f: &Fixture, cookie: &str, uri: &str, body: Option<Value>) -> (u64, Vec<u8>) {
    let d = open(f, cookie, uri, body).await;
    let job = d.job;
    (job, d.rest().await.expect("a complete download"))
}

async fn job(f: &Fixture, cookie: &str, id: u64) -> Value {
    let (status, body) = json_of(&f.app, get(&format!("/api/v1/jobs/{id}"), Some(cookie))).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body
}

/// Waits (up to 5 s) for a job to finish.
async fn finished(f: &Fixture, cookie: &str, id: u64) -> Value {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let j = job(f, cookie, id).await;
        if !j["finished_at"].is_null() {
            return j;
        }
        assert!(Instant::now() < deadline, "job {id} didn't finish: {j}");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

async fn control(f: &Fixture, cookie: &str, id: u64, what: &str) -> (StatusCode, Value) {
    json_of(
        &f.app,
        post(
            &format!("/api/v1/jobs/{id}/{what}"),
            Some(cookie),
            json!({}),
        ),
    )
    .await
}

fn lines(body: &[u8]) -> Vec<String> {
    String::from_utf8(body.to_vec())
        .unwrap()
        .lines()
        .map(str::to_string)
        .collect()
}

/// The NDJSON rows (the trailer left out) by a key.
fn column(body: &[u8], key: &str) -> Vec<Value> {
    lines(body)
        .iter()
        .map(|l| serde_json::from_str::<Value>(l).unwrap())
        .filter(|v| v.get("trailer").is_none())
        .map(|v| v[key].clone())
        .collect()
}

fn trailer(body: &[u8]) -> Value {
    let last = lines(body).pop().unwrap();
    serde_json::from_str::<Value>(&last).unwrap()["trailer"].clone()
}

fn golden(name: &str, got: &[u8], crlf: bool) {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden")
        .join(name);
    if std::env::var_os("LODESTAR_BLESS").is_some() {
        std::fs::write(&path, got).unwrap();
        return;
    }
    let want = std::fs::read(&path)
        .unwrap_or_else(|e| panic!("{}: {e} (bless with LODESTAR_BLESS=1)", path.display()));
    // A Windows checkout may have converted the line ends.
    let mut want = String::from_utf8(want).unwrap().replace("\r\n", "\n");
    if crlf {
        want = want.replace('\n', "\r\n");
    }
    assert_eq!(
        String::from_utf8_lossy(got),
        want,
        "{name}: the export bytes changed"
    );
}

/// The golden fixture: an ordinary torrent, two formula-injection names,
/// a Shift_JIS name (not UTF-8), and one with control and bidi
/// characters. Nothing is scraped, so every swarm field is null.
fn golden_torrents() -> Vec<T> {
    vec![
        t("The.Dark.Knight.2008.1080p.BluRay.x264-GRP"),
        t("=HYPERLINK(\"http://example.invalid\",\"click\"), 2024"),
        t("@SUM(1+1)-cmd"),
        T {
            // テスト アルバム in Shift_JIS.
            name: b"\x83e\x83X\x83g \x83A\x83\x8b\x83o\x83\x80 FLAC".to_vec(),
            adult: false,
            flags: 0,
        },
        t("Holiday\u{7}Clips\u{202e}4pm.exe"),
    ]
}

#[tokio::test]
async fn each_format_is_byte_exact() {
    let f = fixture(&golden_torrents(), Opts::default());
    let c = session(&f).await;
    let (_, csv) = export(&f, &c, "/api/v1/export?sort=%2Bdiscovered", None).await;
    golden("export_all.csv", &csv, true);
    let (_, ndjson) = export(
        &f,
        &c,
        "/api/v1/export?sort=%2Bdiscovered&format=json",
        None,
    )
    .await;
    golden("export_all.ndjson", &ndjson, false);
    let (_, magnets) = export(
        &f,
        &c,
        "/api/v1/export?sort=%2Bdiscovered&format=magnets",
        None,
    )
    .await;
    golden("export_all_magnets.txt", &magnets, false);
    let (_, picked) = export(
        &f,
        &c,
        "/api/v1/export?sort=%2Bdiscovered&fields=seeders,name,hash,magnet",
        None,
    )
    .await;
    golden("export_fields.csv", &picked, true);

    // Every row: one per torrent, with the never-scraped swarm fields
    // null in JSON and empty in CSV (never 0).
    assert_eq!(
        column(&ndjson, "id"),
        (1..=5).map(Value::from).collect::<Vec<_>>()
    );
    for key in [
        "seeders",
        "leechers",
        "completed",
        "trackers_reporting",
        "trackers_checked",
        "last_scrape_at",
    ] {
        assert!(column(&ndjson, key).iter().all(Value::is_null), "{key}");
    }
    assert_eq!(
        trailer(&ndjson),
        json!({ "cap": 1_000_000, "rows": 5, "skipped": 0, "truncated": false })
    );
    let header = lines(&picked)[0].clone();
    assert_eq!(header, "hash,name,seeders,magnet");
    assert!(
        lines(&picked)[1..]
            .iter()
            .all(|l| l.split(',').nth(2) != Some("0"))
    );

    // Names and magnets are the detail view's, byte for byte.
    let names = column(&ndjson, "name");
    let mags: Vec<String> = lines(&magnets);
    for (i, hash) in f.hashes.iter().enumerate() {
        let (status, detail) =
            json_of(&f.app, get(&format!("/api/v1/torrents/{hash}"), Some(&c))).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(names[i], detail["name"]);
        assert_eq!(mags[i], detail["magnet"].as_str().unwrap());
    }

    // Formula injection is defused, quotes doubled, controls escaped.
    let text = String::from_utf8(csv.clone()).unwrap();
    assert!(text.contains("\"'=HYPERLINK(\"\"http://example.invalid\"\",\"\"click\"\"), 2024\""));
    assert!(text.contains(",'@SUM(1+1)-cmd,"));
    assert!(text.contains("Holiday\\u{7}Clips\\u{202e}4pm.exe"));
    assert!(!text.contains('\u{202e}') && !text.contains('\u{7}'));
    // Every line ends CRLF, and the header comes first.
    assert!(text.starts_with("id,hash,hash_v2,name,title,"));
    assert_eq!(text.matches("\r\n").count(), 6);
    assert_eq!(text.matches('\n').count(), 6);
}

#[tokio::test]
async fn an_empty_export_is_a_header_a_trailer_or_nothing() {
    let f = fixture(&golden_torrents(), Opts::default());
    let c = session(&f).await;
    let (_, csv) = export(
        &f,
        &c,
        "/api/v1/export?q=nothingmatches&fields=id,name",
        None,
    )
    .await;
    assert_eq!(csv, b"id,name\r\n");
    let (_, ndjson) = export(&f, &c, "/api/v1/export?q=nothingmatches&format=json", None).await;
    assert_eq!(
        String::from_utf8(ndjson).unwrap(),
        "{\"trailer\":{\"cap\":1000000,\"rows\":0,\"skipped\":0,\"truncated\":false}}\n"
    );
    let (id, magnets) = export(
        &f,
        &c,
        "/api/v1/export?q=nothingmatches&format=magnets",
        None,
    )
    .await;
    assert!(magnets.is_empty());
    let j = finished(&f, &c, id).await;
    assert_eq!(j["state"], "done");
    assert_eq!(
        (j["done"].clone(), j["total"].clone()),
        (json!(0), json!(0))
    );
}

#[tokio::test]
async fn downloads_are_attachments_with_generated_names() {
    let f = fixture(&golden_torrents(), Opts::default());
    let c = session(&f).await;
    for (format, ct, ext) in [
        ("csv", "text/csv; charset=utf-8; header=present", "csv"),
        ("json", "application/x-ndjson", "ndjson"),
        ("magnets", "text/plain; charset=utf-8", "txt"),
    ] {
        let d = open(&f, &c, &format!("/api/v1/export?format={format}"), None).await;
        assert_eq!(d.headers[header::CONTENT_TYPE], ct);
        assert_eq!(d.headers[header::CACHE_CONTROL], "no-store");
        assert_eq!(d.headers[header::X_CONTENT_TYPE_OPTIONS], "nosniff");
        let cd = d.headers[header::CONTENT_DISPOSITION]
            .to_str()
            .unwrap()
            .to_string();
        let name = cd
            .strip_prefix("attachment; filename=\"lodestar-export-")
            .and_then(|r| r.strip_suffix(&format!(".{ext}\"")))
            .unwrap_or_else(|| panic!("{cd}"));
        assert_eq!(name.len(), 15, "{cd}");
        assert!(
            name.bytes().all(|b| b.is_ascii_digit() || b == b'-'),
            "{cd}"
        );
        d.rest().await.unwrap();
    }
}

/// An ordinary torrent, one a filter rule hid, an Adult one and a
/// private-source one.
fn visibility_torrents() -> Vec<T> {
    vec![
        t("Gotham.Soundtrack.FLAC"),
        T {
            flags: flags::HIDDEN,
            ..t("Hidden.By.A.Rule.mkv")
        },
        T {
            adult: true,
            ..t("Studio.Scene.XXX.1080p.mp4")
        },
        T {
            flags: flags::PRIVATE_SOURCE,
            ..t("Private.Tracker.Only.mkv")
        },
    ]
}

async fn names_and_skipped(
    f: &Fixture,
    c: &str,
    uri: &str,
    body: Option<Value>,
) -> (Vec<String>, u64, Vec<u8>, Value) {
    let (id, bytes) = export(f, c, uri, body).await;
    let j = finished(f, c, id).await;
    assert_eq!(j["state"], "done", "{j}");
    let names = column(&bytes, "name")
        .into_iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    (names, j["skipped"].as_u64().unwrap(), bytes, j)
}

#[tokio::test]
async fn what_search_hides_and_private_sources_are_never_exported() {
    let f = fixture(&visibility_torrents(), Opts::default());
    let c = session(&f).await;
    let collection = {
        let mut store = f.store.lock().unwrap();
        let id = store.create_collection("Picks", None, 1).unwrap();
        store.add_to_collection(id, &f.ids, 1).unwrap();
        id
    };
    let unknown = "ab".repeat(20);
    let mut all = f.hashes.clone();
    all.push(unknown.clone());

    // By query.
    let (names, skipped, ..) = names_and_skipped(
        &f,
        &c,
        "/api/v1/export?format=json&sort=%2Bdiscovered",
        None,
    )
    .await;
    assert_eq!(names, ["Gotham.Soundtrack.FLAC"]);
    // The index leaves out the hidden and Adult ones; the private one is
    // dropped on reading.
    assert_eq!(skipped, 1);
    let (names, ..) = names_and_skipped(
        &f,
        &c,
        "/api/v1/export?format=json&sort=%2Bdiscovered&hidden=true",
        None,
    )
    .await;
    assert_eq!(names, ["Gotham.Soundtrack.FLAC", "Hidden.By.A.Rule.mkv"]);
    // Asking for Adult while it's hidden finds nothing.
    let (names, ..) =
        names_and_skipped(&f, &c, "/api/v1/export?format=json&q=type:adult", None).await;
    assert!(names.is_empty());

    // By hashes: the withheld and the unknown are counted, never named.
    let (names, skipped, bytes, j) = names_and_skipped(
        &f,
        &c,
        "/api/v1/export",
        Some(json!({ "hashes": all, "format": "json" })),
    )
    .await;
    assert_eq!(names, ["Gotham.Soundtrack.FLAC"]);
    assert_eq!(skipped, 4);
    assert_eq!(j["total"], 5);
    let text = format!("{}{j}", String::from_utf8(bytes).unwrap());
    for h in f.hashes[1..].iter().chain([&unknown]) {
        assert!(!text.contains(h.as_str()), "{h} leaked");
    }
    let (names, skipped, ..) = names_and_skipped(
        &f,
        &c,
        "/api/v1/export",
        Some(json!({ "hashes": all, "format": "json", "hidden": true })),
    )
    .await;
    assert_eq!(names, ["Gotham.Soundtrack.FLAC", "Hidden.By.A.Rule.mkv"]);
    assert_eq!(skipped, 3);

    // By collection.
    let uri = format!("/api/v1/export?format=json&collection={collection}");
    let (names, skipped, ..) = names_and_skipped(&f, &c, &uri, None).await;
    assert_eq!(names, ["Gotham.Soundtrack.FLAC"]);
    assert_eq!(skipped, 3);
    let (names, skipped, ..) = names_and_skipped(&f, &c, &format!("{uri}&hidden=true"), None).await;
    assert_eq!(names, ["Gotham.Soundtrack.FLAC", "Hidden.By.A.Rule.mkv"]);
    assert_eq!(skipped, 2);

    // "Show adult content" is search's: exports keep Adult out until
    // their own setting includes it (§20.9).
    f.adult.store(true, Ordering::SeqCst);
    let (names, ..) = names_and_skipped(
        &f,
        &c,
        "/api/v1/export?format=json&hidden=true&sort=%2Bdiscovered",
        None,
    )
    .await;
    assert_eq!(names, ["Gotham.Soundtrack.FLAC", "Hidden.By.A.Rule.mkv"]);
    f.adult.store(false, Ordering::SeqCst);

    // With export's on, it's there; the private one never is.
    f.export_adult.store(true, Ordering::SeqCst);
    for (uri, body) in [
        (
            "/api/v1/export?format=json&hidden=true&sort=%2Bdiscovered".to_string(),
            None,
        ),
        (
            "/api/v1/export".to_string(),
            Some(json!({ "hashes": all, "format": "json", "hidden": true })),
        ),
        (format!("{uri}&hidden=true"), None),
    ] {
        let (names, ..) = names_and_skipped(&f, &c, &uri, body).await;
        assert_eq!(
            names,
            [
                "Gotham.Soundtrack.FLAC",
                "Hidden.By.A.Rule.mkv",
                "Studio.Scene.XXX.1080p.mp4"
            ],
            "{uri}"
        );
    }
    // Magnets and CSV leave out the same.
    let (_, magnets) = export(
        &f,
        &c,
        "/api/v1/export",
        Some(json!({ "hashes": all, "format": "magnets", "hidden": true })),
    )
    .await;
    assert_eq!(lines(&magnets).len(), 3);
    assert!(!String::from_utf8(magnets).unwrap().contains(&f.hashes[3]));
}

#[tokio::test]
async fn adult_turned_off_mid_export_is_withheld_from_then_on() {
    let mut ts = Vec::new();
    for i in 0..40 {
        ts.push(T {
            adult: i % 2 == 1,
            ..t(&format!("Clip.{i:02}.mkv"))
        });
    }
    let f = fixture(
        &ts,
        Opts {
            adult: true,
            config: ExportConfig {
                chunk: 10,
                queue: 1,
                ..ExportConfig::default()
            },
            ..Opts::default()
        },
    );
    let c = session(&f).await;
    let mut d = open(
        &f,
        &c,
        "/api/v1/export?format=json&sort=%2Bdiscovered",
        None,
    )
    .await;
    let first = d.next().await.unwrap().unwrap();
    assert_eq!(
        column(&first, "id").len(),
        10,
        "both kinds in the first chunk"
    );
    f.export_adult.store(false, Ordering::SeqCst);
    let id = d.job;
    let rest = d.rest().await.unwrap();
    let j = finished(&f, &c, id).await;
    // At most two more chunks were read before it noticed (one queued,
    // one being sent); after that, no Adult.
    let ids = column(&rest, "id");
    let adult_after: Vec<&Value> = ids
        .iter()
        .skip(20)
        .filter(|v| v.as_i64().unwrap() % 2 == 0)
        .collect();
    assert!(adult_after.is_empty(), "{adult_after:?}");
    assert!(j["skipped"].as_u64().unwrap() >= 5, "{j}");
    assert_eq!(
        j["skipped"].as_u64().unwrap() + j["done"].as_u64().unwrap(),
        40
    );
}

#[tokio::test]
async fn a_large_export_reads_many_chunks_in_order() {
    let ts: Vec<T> = (0..600).map(|i| t(&format!("Filler.{i:04}.mkv"))).collect();
    let f = fixture(
        &ts,
        Opts {
            config: ExportConfig {
                chunk: 37,
                ..ExportConfig::default()
            },
            ..Opts::default()
        },
    );
    let c = session(&f).await;
    let want: Vec<Value> = f.ids.iter().map(|&i| Value::from(i)).collect();
    let (id, body) = export(
        &f,
        &c,
        "/api/v1/export?format=json&sort=%2Bdiscovered",
        None,
    )
    .await;
    assert_eq!(column(&body, "id"), want);
    let j = finished(&f, &c, id).await;
    assert_eq!(
        (j["done"].clone(), j["total"].clone()),
        (json!(600), json!(600))
    );
    // Descending, and by relevance, page the same lease.
    let (_, body) = export(&f, &c, "/api/v1/export?format=json&sort=-discovered", None).await;
    let mut rev = want.clone();
    rev.reverse();
    assert_eq!(column(&body, "id"), rev);
    let (_, body) = export(&f, &c, "/api/v1/export?format=json&q=filler", None).await;
    let mut got = column(&body, "id");
    got.sort_by_key(|v| v.as_i64());
    assert_eq!(got, want);
    // Hashes, in the order given.
    let mut hashes = f.hashes.clone();
    hashes.reverse();
    let (_, body) = export(
        &f,
        &c,
        "/api/v1/export",
        Some(json!({ "hashes": hashes, "format": "json", "fields": "id" })),
    )
    .await;
    assert_eq!(column(&body, "id"), rev);
}

#[tokio::test]
async fn the_wal_stays_bounded_beside_a_writer() {
    let ts: Vec<T> = (0..400).map(|i| t(&format!("Filler.{i:04}.mkv"))).collect();
    let f = fixture(
        &ts,
        Opts {
            config: ExportConfig {
                chunk: 16,
                rows_per_sec: 400,
                ..ExportConfig::default()
            },
            ..Opts::default()
        },
    );
    let c = session(&f).await;
    let stop = Arc::new(AtomicBool::new(false));
    let most = Arc::new(AtomicU64::new(0));
    let written = Arc::new(AtomicU64::new(0));
    let writer = {
        let (store, stop, most, written) =
            (f.store.clone(), stop.clone(), most.clone(), written.clone());
        let wal = f.db.with_extension("db-wal");
        std::thread::spawn(move || {
            let mut n = 0i64;
            while !stop.load(Ordering::SeqCst) {
                let batch: Vec<T> = (0..50)
                    .map(|i| t(&format!("Writer.{n}.{i}.{}", "x".repeat(200))))
                    .collect();
                let mut s = store.lock().unwrap();
                insert(&mut s, &batch, 10_000 + n);
                assert!(std::fs::metadata(&wal).unwrap().len() > 0);
                written.fetch_add(1, Ordering::SeqCst);
                // Folding the WAL back needs every reader past it.
                s.checkpoint().unwrap();
                let after = std::fs::metadata(&wal).map_or(0, |m| m.len());
                most.fetch_max(after, Ordering::SeqCst);
                n += 50;
            }
        })
    };
    let (id, body) = export(
        &f,
        &c,
        "/api/v1/export?format=json&sort=%2Bdiscovered",
        None,
    )
    .await;
    stop.store(true, Ordering::SeqCst);
    writer.join().unwrap();
    assert_eq!(column(&body, "id").len(), 400);
    assert_eq!(finished(&f, &c, id).await["state"], "done");
    let (most, written) = (most.load(Ordering::SeqCst), written.load(Ordering::SeqCst));
    // The writer checkpointed again and again while the export ran (a
    // second at 400 rows a second), and every checkpoint emptied the
    // WAL: no read transaction outlived a chunk.
    assert!(written >= 3, "only {written} checkpoints");
    assert_eq!(most, 0, "the WAL kept {most} bytes after a checkpoint");
}

#[tokio::test]
async fn a_client_that_stops_reading_holds_no_lock() {
    let ts: Vec<T> = (0..200).map(|i| t(&format!("Filler.{i:04}.mkv"))).collect();
    let f = fixture(
        &ts,
        Opts {
            config: ExportConfig {
                chunk: 10,
                queue: 1,
                ..ExportConfig::default()
            },
            ..Opts::default()
        },
    );
    let c = session(&f).await;
    let mut d = open(
        &f,
        &c,
        "/api/v1/export?format=json&sort=%2Bdiscovered",
        None,
    )
    .await;
    assert!(d.next().await.unwrap().is_ok());
    // Not reading: the export waits on the socket, and everything else
    // (which needs the reader lock) still answers at once.
    tokio::time::sleep(Duration::from_millis(100)).await;
    let before = job(&f, &c, d.job).await;
    assert_eq!(before["state"], "running");
    for _ in 0..3 {
        let started = Instant::now();
        let (status, _) = json_of(
            &f.app,
            get(&format!("/api/v1/torrents/{}", f.hashes[5]), Some(&c)),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let (status, _) = json_of(&f.app, get("/api/v1/search?q=filler", Some(&c))).await;
        assert_eq!(status, StatusCode::OK);
        assert!(started.elapsed() < Duration::from_secs(1));
    }
    tokio::time::sleep(Duration::from_millis(100)).await;
    let after = job(&f, &c, d.job).await;
    assert_eq!(before["done"], after["done"], "it stopped reading");
    let rest = d.rest().await.unwrap();
    assert_eq!(column(&rest, "id").len() + 10, 200);
}

#[tokio::test]
async fn reads_are_throttled() {
    let ts: Vec<T> = (0..300).map(|i| t(&format!("Filler.{i:04}.mkv"))).collect();
    let f = fixture(
        &ts,
        Opts {
            config: ExportConfig {
                chunk: 25,
                rows_per_sec: 500,
                ..ExportConfig::default()
            },
            ..Opts::default()
        },
    );
    let c = session(&f).await;
    let started = Instant::now();
    let (id, body) = export(&f, &c, "/api/v1/export?format=magnets", None).await;
    let took = started.elapsed();
    assert_eq!(lines(&body).len(), 300);
    // 300 rows at 500 a second: the last chunk's wait is after its rows.
    assert!(took >= Duration::from_millis(550), "{took:?}");
    let rate = finished(&f, &c, id).await["rate"].as_f64().unwrap();
    assert!(rate < 600.0, "{rate}");
}

#[tokio::test]
async fn concurrent_exports_share_one_rate() {
    let ts: Vec<T> = (0..150).map(|i| t(&format!("Filler.{i:04}.mkv"))).collect();
    let f = fixture(
        &ts,
        Opts {
            config: ExportConfig {
                chunk: 25,
                rows_per_sec: 500,
                ..ExportConfig::default()
            },
            ..Opts::default()
        },
    );
    let c = session(&f).await;
    let started = Instant::now();
    let (a, b) = tokio::join!(
        export(&f, &c, "/api/v1/export?format=magnets", None),
        export(&f, &c, "/api/v1/export?format=magnets", None),
    );
    let took = started.elapsed();
    assert_eq!(lines(&a.1).len() + lines(&b.1).len(), 300);
    // 300 rows between them at 500 a second, not 500 each.
    assert!(took >= Duration::from_millis(550), "{took:?}");
}

#[tokio::test]
async fn a_request_dropped_during_setup_frees_its_slot() {
    let ts: Vec<T> = (0..50).map(|i| t(&format!("Filler.{i:04}.mkv"))).collect();
    let f = fixture(&ts, Opts::default());
    let c = session(&f).await;
    let collection = {
        let mut store = f.store.lock().unwrap();
        let id = store.create_collection("Picks", None, 1).unwrap();
        store.add_to_collection(id, &f.ids, 1).unwrap();
        id
    };
    // More than the running limit, each dropped while its setup (the
    // query's search, or counting the collection) is still under way.
    for i in 0..6 {
        let uri = if i % 2 == 0 {
            "/api/v1/export?q=filler".to_string()
        } else {
            format!("/api/v1/export?collection={collection}")
        };
        let mut fut = Box::pin(f.app.clone().oneshot(get(&uri, Some(&c))));
        let status = poll_fn(|cx| {
            Poll::Ready(match fut.as_mut().poll(cx) {
                Poll::Ready(r) => Some(r.unwrap().status()),
                Poll::Pending => None,
            })
        })
        .await;
        // Pending: dropped mid-setup. A leaked slot shows up as a 429.
        assert_ne!(status, Some(StatusCode::TOO_MANY_REQUESTS), "request {i}");
        drop(fut);
    }
    // No job is left running.
    let (_, list) = json_of(&f.app, get("/api/v1/jobs", Some(&c))).await;
    for j in list["jobs"].as_array().unwrap() {
        assert!(!j["finished_at"].is_null(), "a dropped setup left {j}");
    }
    // And a new export still gets a slot.
    let (_, body) = export(&f, &c, "/api/v1/export?format=magnets", None).await;
    assert_eq!(lines(&body).len(), 50);
}

#[tokio::test]
async fn the_cap_truncates_and_says_so() {
    let ts: Vec<T> = (0..20).map(|i| t(&format!("Filler.{i:04}.mkv"))).collect();
    let config = ExportConfig {
        chunk: 3,
        cap: 7,
        ..ExportConfig::default()
    };
    let f = fixture(
        &ts,
        Opts {
            config,
            ..Opts::default()
        },
    );
    let c = session(&f).await;
    let (id, body) = export(
        &f,
        &c,
        "/api/v1/export?format=json&sort=%2Bdiscovered",
        None,
    )
    .await;
    assert_eq!(column(&body, "id").len(), 7);
    assert_eq!(
        trailer(&body),
        json!({ "cap": 7, "rows": 7, "skipped": 0, "truncated": true })
    );
    let j = finished(&f, &c, id).await;
    assert_eq!(
        j["export"],
        json!({ "cap": 7, "format": "json", "truncated": true })
    );
    assert_eq!(
        (j["done"].clone(), j["total"].clone()),
        (json!(7), json!(7))
    );
    // CSV ends cleanly at the cap too, and its job says so.
    let (id, body) = export(&f, &c, "/api/v1/export?fields=id", None).await;
    assert_eq!(lines(&body).len(), 8);
    assert_eq!(finished(&f, &c, id).await["export"]["truncated"], true);
    // Exactly the cap isn't truncated.
    let (_, body) = export(
        &f,
        &c,
        "/api/v1/export",
        Some(json!({ "hashes": &f.hashes[..7], "format": "json" })),
    )
    .await;
    assert_eq!(trailer(&body)["truncated"], false);
}

#[tokio::test]
async fn pause_resume_and_cancel() {
    let ts: Vec<T> = (0..100).map(|i| t(&format!("Filler.{i:04}.mkv"))).collect();
    let f = fixture(
        &ts,
        Opts {
            config: ExportConfig {
                chunk: 10,
                queue: 1,
                ..ExportConfig::default()
            },
            ..Opts::default()
        },
    );
    let c = session(&f).await;
    let mut d = open(
        &f,
        &c,
        "/api/v1/export?format=json&sort=%2Bdiscovered",
        None,
    )
    .await;
    let mut got = d.next().await.unwrap().unwrap().to_vec();
    let (status, j) = control(&f, &c, d.job, "pause").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(j["state"], "paused");
    assert_eq!(j["eta_s"], Value::Null);
    // Pausing again does nothing.
    assert_eq!(control(&f, &c, d.job, "pause").await.1["state"], "paused");
    // What was queued before the pause drains; then nothing comes.
    let mut drained = 0;
    while let Ok(Some(piece)) = tokio::time::timeout(Duration::from_millis(500), d.next()).await {
        got.extend_from_slice(&piece.unwrap());
        drained += 1;
        assert!(drained <= 3, "a paused export kept reading");
    }
    let done = job(&f, &c, d.job).await["done"].clone();
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(job(&f, &c, d.job).await["done"], done);
    // Other requests run while it's paused.
    let (status, _) = json_of(&f.app, get("/api/v1/search?q=filler", Some(&c))).await;
    assert_eq!(status, StatusCode::OK);
    let (status, j) = control(&f, &c, d.job, "resume").await;
    assert_eq!(
        (status, j["state"].clone()),
        (StatusCode::OK, json!("running"))
    );
    let id = d.job;
    got.extend(d.rest().await.unwrap());
    let want: Vec<Value> = f.ids.iter().map(|&i| Value::from(i)).collect();
    assert_eq!(column(&got, "id"), want, "no row lost or repeated");
    assert_eq!(finished(&f, &c, id).await["state"], "done");
    // Finished: no more controls.
    for what in ["pause", "resume", "cancel"] {
        let (status, body) = control(&f, &c, id, what).await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(body["error"]["code"], "job_finished");
    }

    // Cancel mid-stream: the download ends with an error, not cleanly.
    let mut d = open(&f, &c, "/api/v1/export?format=json", None).await;
    d.next().await.unwrap().unwrap();
    let (status, j) = control(&f, &c, d.job, "cancel").await;
    assert_eq!(
        (status, j["state"].clone()),
        (StatusCode::OK, json!("cancelled"))
    );
    let id = d.job;
    let partial = d.rest().await.unwrap_err();
    assert!(!String::from_utf8(partial).unwrap().contains("trailer"));
    let j = finished(&f, &c, id).await;
    assert_eq!(j["state"], "cancelled");
    assert!(j.get("error").is_none());

    // Cancelling a paused job stops it at once.
    let mut d = open(&f, &c, "/api/v1/export?format=json", None).await;
    d.next().await.unwrap().unwrap();
    control(&f, &c, d.job, "pause").await;
    control(&f, &c, d.job, "cancel").await;
    let id = d.job;
    assert!(d.rest().await.is_err());
    assert_eq!(finished(&f, &c, id).await["state"], "cancelled");

    // A client that goes away cancels its export.
    let mut d = open(&f, &c, "/api/v1/export?format=json", None).await;
    d.next().await.unwrap().unwrap();
    let id = d.job;
    drop(d);
    assert_eq!(finished(&f, &c, id).await["state"], "cancelled");

    // So does one that goes away while its export is paused: at once,
    // not when the pause runs out.
    let mut d = open(&f, &c, "/api/v1/export?format=json", None).await;
    d.next().await.unwrap().unwrap();
    control(&f, &c, d.job, "pause").await;
    let id = d.job;
    drop(d);
    assert_eq!(finished(&f, &c, id).await["state"], "cancelled");
    // Its slot is free.
    let (_, list) = json_of(&f.app, get("/api/v1/jobs", Some(&c))).await;
    assert!(
        list["jobs"]
            .as_array()
            .unwrap()
            .iter()
            .all(|j| !j["finished_at"].is_null())
    );
}

#[tokio::test]
async fn a_pause_and_a_stall_run_out() {
    let ts: Vec<T> = (0..100).map(|i| t(&format!("Filler.{i:04}.mkv"))).collect();
    let f = fixture(
        &ts,
        Opts {
            config: ExportConfig {
                chunk: 10,
                queue: 1,
                pause_limit: Duration::from_millis(150),
                stall_limit: Duration::from_millis(150),
                ..ExportConfig::default()
            },
            ..Opts::default()
        },
    );
    let c = session(&f).await;
    // Paused, and read: it waits at its next step until the pause runs
    // out.
    let mut d = open(&f, &c, "/api/v1/export?format=json", None).await;
    d.next().await.unwrap().unwrap();
    control(&f, &c, d.job, "pause").await;
    let id = d.job;
    assert!(d.rest().await.is_err());
    let j = finished(&f, &c, id).await;
    assert_eq!(j["state"], "failed");
    assert_eq!(j["error"]["code"], "pause_expired");

    // Running, and not read.

    let mut d = open(&f, &c, "/api/v1/export?format=json", None).await;
    d.next().await.unwrap().unwrap();
    let id = d.job;
    let j = finished(&f, &c, id).await;
    assert_eq!(j["state"], "failed");
    assert_eq!(j["error"]["code"], "export_stalled");
    assert!(
        d.rest().await.is_err(),
        "a stalled download ends with an error"
    );
}

#[tokio::test]
async fn the_exports_lease_outlives_the_registrys_expiry_and_new_commits() {
    let ts: Vec<T> = (0..50).map(|i| t(&format!("Filler.{i:04}.mkv"))).collect();
    let f = fixture(
        &ts,
        Opts {
            config: ExportConfig {
                chunk: 10,
                queue: 1,
                ..ExportConfig::default()
            },
            // Search's leases expire almost at once.
            leases: Some(LeaseConfig {
                ttl: Duration::from_millis(1),
                max_age: Duration::from_millis(1),
                ..LeaseConfig::default()
            }),
            ..Opts::default()
        },
    );
    let c = session(&f).await;
    let mut d = open(
        &f,
        &c,
        "/api/v1/export?format=json&q=filler&sort=%2Bdiscovered",
        None,
    )
    .await;
    let mut got = d.next().await.unwrap().unwrap().to_vec();
    control(&f, &c, d.job, "pause").await;
    // More matches are committed, and the pause outlasts any lease.
    {
        let more: Vec<T> = (0..30)
            .map(|i| t(&format!("Filler.late.{i}.mkv")))
            .collect();
        let mut s = f.store.lock().unwrap();
        insert(&mut s, &more, 5_000);
        let mut w = f.index.writer(15 << 20).unwrap();
        w.catch_up(&mut s, 1000).unwrap();
        w.commit().unwrap();
    }
    tokio::time::sleep(Duration::from_millis(50)).await;
    control(&f, &c, d.job, "resume").await;
    got.extend(d.rest().await.unwrap());
    let want: Vec<Value> = f.ids.iter().map(|&i| Value::from(i)).collect();
    assert_eq!(column(&got, "id"), want, "the snapshot it started with");
}

#[tokio::test]
async fn closing_the_servers_registry_ends_open_downloads() {
    let ts: Vec<T> = (0..100).map(|i| t(&format!("Filler.{i:04}.mkv"))).collect();
    let jobs = Arc::new(Jobs::default());
    let f = fixture(
        &ts,
        Opts {
            config: ExportConfig {
                chunk: 10,
                queue: 1,
                ..ExportConfig::default()
            },
            jobs: Some(jobs.clone()),
            ..Opts::default()
        },
    );
    let c = session(&f).await;
    let mut reading = open(&f, &c, "/api/v1/export?format=json", None).await;
    reading.next().await.unwrap().unwrap();
    let paused = open(&f, &c, "/api/v1/export?format=json", None).await;
    control(&f, &c, paused.job, "pause").await;
    // The routes use the server's registry, not one of their own.
    let (_, list) = json_of(&f.app, get("/api/v1/jobs", Some(&c))).await;
    assert_eq!(list["jobs"].as_array().unwrap().len(), 2);
    jobs.close();
    let (a, b) = (reading.job, paused.job);
    assert!(reading.rest().await.is_err());
    assert!(paused.rest().await.is_err());
    for id in [a, b] {
        assert_eq!(finished(&f, &c, id).await["state"], "cancelled");
    }
    let (status, body) = json_of(&f.app, get("/api/v1/export", Some(&c))).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(body["error"]["code"], "shutting_down");
}

#[tokio::test]
async fn jobs_are_listed_limited_and_checked() {
    let ts: Vec<T> = (0..100).map(|i| t(&format!("Filler.{i:04}.mkv"))).collect();
    let f = fixture(
        &ts,
        Opts {
            config: ExportConfig {
                chunk: 5,
                queue: 1,
                ..ExportConfig::default()
            },
            ..Opts::default()
        },
    );
    let c = session(&f).await;
    // Four running (their clients not reading); a fifth is refused.
    let mut open_ones = Vec::new();
    for _ in 0..4 {
        open_ones.push(open(&f, &c, "/api/v1/export?format=json", None).await);
    }
    let (status, body) = json_of(&f.app, get("/api/v1/export", Some(&c))).await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(body["error"]["code"], "too_many_jobs");
    let (status, list) = json_of(&f.app, get("/api/v1/jobs", Some(&c))).await;
    assert_eq!(status, StatusCode::OK);
    let jobs = list["jobs"].as_array().unwrap();
    assert_eq!(jobs.len(), 4);
    let ids: Vec<u64> = jobs.iter().map(|j| j["id"].as_u64().unwrap()).collect();
    assert!(ids.windows(2).all(|w| w[0] > w[1]), "newest first");
    for j in jobs {
        assert_eq!(j["kind"], "export");
        assert_eq!(j["state"], "running");
        assert_eq!(j["total"], 100);
        assert_eq!(j["total_approx"], false);
        assert!(j["started_at"].as_i64().unwrap() > 1_700_000_000);
        assert!(j["finished_at"].is_null());
    }
    // One cancelled frees a slot.
    let first = open_ones.remove(0);
    control(&f, &c, first.job, "cancel").await;
    finished(&f, &c, first.job).await;
    open_ones.push(open(&f, &c, "/api/v1/export?format=json", None).await);
    for d in open_ones {
        let id = d.job;
        drop(d);
        finished(&f, &c, id).await;
    }

    // Unknown ids.
    for (req, code) in [
        (get("/api/v1/jobs/999", Some(&c)), "job_not_found"),
        (
            post("/api/v1/jobs/999/pause", Some(&c), json!({})),
            "job_not_found",
        ),
        (get("/api/v1/jobs/x", Some(&c)), "bad_path"),
    ] {
        let (status, body) = json_of(&f.app, req).await;
        assert_eq!(body["error"]["code"], code, "{body}");
        assert!(status.is_client_error());
    }

    // Writes are same-origin JSON.
    let d = open(&f, &c, "/api/v1/export?format=json", None).await;
    let cross = Request::post(format!("/api/v1/jobs/{}/cancel", d.job))
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::COOKIE, &c)
        .header(header::HOST, "lodestar.local")
        .header(header::ORIGIN, "https://evil.example")
        .body(Body::from("{}"))
        .unwrap();
    let (status, body) = json_of(&f.app, cross).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["error"]["code"], "cross_origin");
    let form = Request::post(format!("/api/v1/jobs/{}/pause", d.job))
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .header(header::COOKIE, &c)
        .body(Body::from(""))
        .unwrap();
    let (status, _) = json_of(&f.app, form).await;
    assert_eq!(status, StatusCode::UNSUPPORTED_MEDIA_TYPE);
    let cross = Request::post("/api/v1/export")
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::COOKIE, &c)
        .header(header::HOST, "lodestar.local")
        .header(header::ORIGIN, "https://evil.example")
        .body(Body::from("{}"))
        .unwrap();
    assert_eq!(json_of(&f.app, cross).await.0, StatusCode::FORBIDDEN);
    assert_eq!(job(&f, &c, d.job).await["state"], "running");
    drop(d);

    // No session, no jobs or exports.
    for req in [
        get("/api/v1/jobs", None),
        get("/api/v1/jobs/1", None),
        get("/api/v1/export", None),
        post("/api/v1/export", None, json!({})),
        post("/api/v1/jobs/1/cancel", None, json!({})),
    ] {
        let (status, body) = json_of(&f.app, req).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(body["error"]["code"], "login_required");
    }
}

#[tokio::test]
async fn bad_requests_get_the_envelope() {
    let f = fixture(&golden_torrents(), Opts::default());
    let c = session(&f).await;
    let many: Vec<String> = (0..10_001).map(|i| format!("{i:040x}")).collect();
    for (req, status, code) in [
        (
            post(
                "/api/v1/export",
                Some(&c),
                json!({ "q": "a", "collection": 1 }),
            ),
            400,
            "one_source",
        ),
        (
            post(
                "/api/v1/export",
                Some(&c),
                json!({ "hashes": [], "collection": 1 }),
            ),
            400,
            "one_source",
        ),
        (
            post("/api/v1/export", Some(&c), json!({ "hashes": many })),
            400,
            "too_many_hashes",
        ),
        (
            post("/api/v1/export", Some(&c), json!({ "hashes": ["xyz"] })),
            400,
            "bad_hash",
        ),
        (
            post(
                "/api/v1/export",
                Some(&c),
                json!({ "hashes": [], "sort": "size" }),
            ),
            400,
            "sort_unused",
        ),
        (
            get("/api/v1/export?collection=42", Some(&c)),
            404,
            "collection_not_found",
        ),
        (
            get("/api/v1/export?fields=name,score", Some(&c)),
            400,
            "unknown_field",
        ),
        (
            get("/api/v1/export?format=magnets&fields=name", Some(&c)),
            400,
            "fields_unused",
        ),
        (
            get("/api/v1/export?q=sise:1", Some(&c)),
            400,
            "query_syntax",
        ),
        (
            get("/api/v1/export?sort=seeders", Some(&c)),
            400,
            "unknown_sort",
        ),
        (
            get("/api/v1/export?format=xml", Some(&c)),
            400,
            "bad_parameter",
        ),
    ] {
        let (got, body) = json_of(&f.app, req).await;
        assert_eq!(got.as_u16(), status, "{body}");
        assert_eq!(body["error"]["code"], code, "{body}");
    }
    // An empty selection exports nothing.
    let (_, body) = export(
        &f,
        &c,
        "/api/v1/export",
        Some(json!({ "hashes": [], "format": "json" })),
    )
    .await;
    assert_eq!(trailer(&body)["rows"], 0);
    // Repeats are exported once.
    let twice = vec![
        f.hashes[0].clone(),
        f.hashes[0].clone().to_uppercase(),
        f.hashes[1].clone(),
    ];
    let (_, body) = export(
        &f,
        &c,
        "/api/v1/export",
        Some(json!({ "hashes": twice, "format": "magnets" })),
    )
    .await;
    assert_eq!(lines(&body).len(), 2);

    // A query needs search; selections don't.
    let f = fixture(
        &golden_torrents(),
        Opts {
            no_search: true,
            ..Opts::default()
        },
    );
    let c = session(&f).await;
    let (status, body) = json_of(&f.app, get("/api/v1/export", Some(&c))).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(body["error"]["code"], "search_disabled");
    let (_, body) = export(
        &f,
        &c,
        "/api/v1/export",
        Some(json!({ "hashes": f.hashes, "format": "magnets" })),
    )
    .await;
    assert_eq!(lines(&body).len(), 5);
}

/// The §1.6.4 overhead, measured rather than gated (B10b): search p95
/// and ingest throughput with and without an export streaming at the
/// default throttle beside them. Run in release:
/// `cargo test --release -p lodestar-api --test export -- --ignored --nocapture overhead`.
#[tokio::test]
#[ignore = "a measurement; run in release"]
async fn overhead() {
    // `LODESTAR_EXPORT_TORRENTS` sets the corpus (50,000 by default).
    let n: usize = std::env::var("LODESTAR_EXPORT_TORRENTS")
        .ok()
        .and_then(|r| r.parse().ok())
        .unwrap_or(50_000);
    let ts: Vec<T> = (0..n)
        .map(|i| {
            t(&format!(
                "Filler.Movie.{i:05}.2019.1080p.WEB-DL.x264-GRP{}",
                i % 97
            ))
        })
        .collect();
    // `LODESTAR_EXPORT_RATE` and `LODESTAR_EXPORT_CHUNK` try others.
    let rows_per_sec = std::env::var("LODESTAR_EXPORT_RATE")
        .ok()
        .and_then(|r| r.parse().ok())
        .unwrap_or(lodestar_api::export::ROWS_PER_SEC);
    let f = fixture(
        &ts,
        Opts {
            config: ExportConfig {
                rows_per_sec,
                chunk: std::env::var("LODESTAR_EXPORT_CHUNK")
                    .ok()
                    .and_then(|r| r.parse().ok())
                    .unwrap_or(lodestar_api::export::CHUNK),
                ..ExportConfig::default()
            },
            ..Opts::default()
        },
    );
    let c = session(&f).await;
    let queries = [
        "filler",
        "movie 2019",
        "x264 grp7",
        "web-dl 1080p",
        "filler.movie.0042*",
    ];

    /// Search latencies, ms.
    async fn searches(f: &Fixture, c: &str, queries: &[&str], rounds: usize) -> Vec<f64> {
        let mut took = Vec::new();
        for i in 0..rounds {
            // A word nothing has, negated, keeps the results but makes each
            // query new, so the result cache (B5c) doesn't answer it.
            static NONCE: AtomicU64 = AtomicU64::new(0);
            let nonce = NONCE.fetch_add(1, Ordering::Relaxed);
            let q = format!("{} -zq{nonce}", queries[i % queries.len()]);
            let started = Instant::now();
            let (status, _) = json_of(
                &f.app,
                get(
                    &format!("/api/v1/search?q={}", q.replace(' ', "%20")),
                    Some(c),
                ),
            )
            .await;
            assert_eq!(status, StatusCode::OK);
            took.push(started.elapsed().as_secs_f64() * 1000.0);
        }
        took
    }

    /// Torrents ingested in `secs`, as `(count, seconds)`.
    async fn ingest(store: Arc<Mutex<Store>>, secs: f64, base: i64) -> (f64, f64) {
        tokio::task::spawn_blocking(move || {
            let started = Instant::now();
            let mut done = 0;
            let mut k = 0i64;
            while started.elapsed().as_secs_f64() < secs {
                let batch: Vec<T> = (0..100)
                    .map(|i| t(&format!("Ingest.{base}.{k}.{i}.2020.720p")))
                    .collect();
                insert(&mut store.lock().unwrap(), &batch, base + k);
                done += 100;
                k += 100;
            }
            (done as f64, started.elapsed().as_secs_f64())
        })
        .await
        .unwrap()
    }

    fn quantile(v: &mut [f64], q: f64) -> f64 {
        v.sort_by(f64::total_cmp);
        v[((v.len() as f64 * q) as usize).min(v.len() - 1)]
    }

    // An export streaming as fast as the throttle lets it while
    // `active`, read as it comes, again and again, from its own thread
    // (as another connection would be served). While not active, its
    // client stops reading, so the export waits holding nothing: the
    // quiet windows.
    let stop = Arc::new(AtomicBool::new(false));
    let active = Arc::new(AtomicBool::new(false));
    let rows = Arc::new(AtomicU64::new(0));
    // `LODESTAR_EXPORTS` runs several at once (they share one rate).
    let exports: usize = std::env::var("LODESTAR_EXPORTS")
        .ok()
        .and_then(|r| r.parse().ok())
        .unwrap_or(1);
    let exporters: Vec<_> = (0..exports)
        .map(|_| {
            let (app, c, stop, active, rows) = (
                f.app.clone(),
                c.clone(),
                stop.clone(),
                active.clone(),
                rows.clone(),
            );
            std::thread::spawn(move || {
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .unwrap();
                rt.block_on(async move {
                    while !stop.load(Ordering::SeqCst) {
                        let resp = app
                            .clone()
                            .oneshot(get("/api/v1/export?format=csv", Some(&c)))
                            .await
                            .unwrap();
                        let mut body = resp.into_body().into_data_stream();
                        loop {
                            while !active.load(Ordering::SeqCst) && !stop.load(Ordering::SeqCst) {
                                tokio::time::sleep(Duration::from_millis(5)).await;
                            }
                            if stop.load(Ordering::SeqCst) {
                                break;
                            }
                            match poll_fn(|cx| Pin::new(&mut body).poll_next(cx)).await {
                                Some(piece) => {
                                    let n = piece.unwrap().iter().filter(|&&b| b == b'\n').count();
                                    rows.fetch_add(n as u64, Ordering::Relaxed);
                                }
                                None => break,
                            }
                        }
                    }
                })
            })
        })
        .collect();

    // Warm up, then alternate quiet and busy windows so drift (the store
    // grows as ingest runs) falls on both alike.
    searches(&f, &c, &queries, 300).await;
    let (mut quiet_ms, mut busy_ms) = (Vec::new(), Vec::new());
    let (mut quiet_in, mut busy_in) = ((0.0, 0.0), (0.0, 0.0));
    let (mut busy_rows, mut busy_secs) = (0u64, 0.0);
    let windows = 16;
    for w in 0..windows {
        let on = w % 2 == 1;
        active.store(on, Ordering::SeqCst);
        tokio::time::sleep(Duration::from_millis(200)).await;
        let (r0, t0) = (rows.load(Ordering::Relaxed), Instant::now());
        let took = searches(&f, &c, &queries, 300).await;
        let (n, secs) = ingest(f.store.clone(), 1.0, 1_000_000 * (w + 1)).await;
        if on {
            busy_ms.extend(took);
            busy_in = (busy_in.0 + n, busy_in.1 + secs);
            busy_rows += rows.load(Ordering::Relaxed) - r0;
            busy_secs += t0.elapsed().as_secs_f64();
        } else {
            quiet_ms.extend(took);
            quiet_in = (quiet_in.0 + n, quiet_in.1 + secs);
        }
    }
    stop.store(true, Ordering::SeqCst);
    for e in exporters {
        e.join().unwrap();
    }
    let pct = |a: f64, b: f64| (b - a) / a * 100.0;
    let (q50, b50) = (quantile(&mut quiet_ms, 0.5), quantile(&mut busy_ms, 0.5));
    let (q95, b95) = (quantile(&mut quiet_ms, 0.95), quantile(&mut busy_ms, 0.95));
    let (qi, bi) = (quiet_in.0 / quiet_in.1, busy_in.0 / busy_in.1);
    println!(
        "torrents: {n}, throttle {rows_per_sec} rows/s, {exports} export(s), {windows} alternating windows",
        n = ts.len()
    );
    println!(
        "search p50 {q50:.3} ms -> {b50:.3} ms ({:+.1}%); p95 {q95:.3} ms -> {b95:.3} ms ({:+.1}%)",
        pct(q50, b50),
        pct(q95, b95)
    );
    println!(
        "ingest {qi:.0} -> {bi:.0} torrents/s ({:+.1}%)",
        pct(qi, bi)
    );
    println!(
        "export rows while busy: {busy_rows} in {busy_secs:.1} s ({:.0}/s)",
        busy_rows as f64 / busy_secs
    );
}
