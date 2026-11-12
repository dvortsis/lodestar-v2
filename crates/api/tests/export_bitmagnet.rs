//! `/export?format=bitmagnet` (OUT-0) through the router: the bytes
//! against a golden file (`tests/golden/export_bitmagnet.ndjson`;
//! `LODESTAR_BLESS=1` rewrites it), every line checked the way bitmagnet's
//! `POST /import` decodes it, each exclusion by every source (query,
//! hashes, collection), `export_adult` on and off, hostile names, a large
//! export streamed with bounded memory, auth; and the existing CSV,
//! NDJSON and magnet formats shown to leave out `private_source` too.
//!
//! No in-tree program reads bitmagnet's import lines (the bitmagnet
//! importer in `crates/sources-importers` reads its PostgreSQL), so the
//! round trip is the golden file plus [`decode`], a model of bitmagnet's
//! own decoding (Go's `encoding/json` into `importer.Item`).

use std::path::PathBuf;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::body::{Body, BodyDataStream, Bytes};
use axum::http::{Request, StatusCode, header};
use axum::{Extension, Router};
use futures_core::Stream;
use lodestar_api::export::{ExportAdult, ExportConfig};
use lodestar_api::{AdultVisibility, ApiState, Auth, router};
use lodestar_bencode::{OwnedValue, to_vec};
use lodestar_classify::Correction;
use lodestar_metainfo::Info;
use lodestar_search::SearchIndex;
use lodestar_store::{NewTorrent, SourceId, Store, StoreConfig, flags};
use serde_json::{Map, Value, json};
use tower::ServiceExt;

const PASSWORD: &str = "correct horse battery";

/// One fixture torrent: its name, whether it's corrected to Adult, its
/// info dict's BEP 27 `private` bit, and extra flags.
#[derive(Clone)]
struct T {
    name: Vec<u8>,
    adult: bool,
    private: bool,
    flags: i64,
}

fn t(name: &str) -> T {
    T {
        name: name.as_bytes().to_vec(),
        adult: false,
        private: false,
        flags: 0,
    }
}

/// A single-file, 1 MiB info dict.
fn metainfo(name: &[u8], private: bool) -> Vec<u8> {
    let mut entries: Vec<(Vec<u8>, OwnedValue)> = vec![
        (b"length".to_vec(), OwnedValue::Int(1 << 20)),
        (b"name".to_vec(), OwnedValue::Bytes(name.to_vec())),
        (b"piece length".to_vec(), OwnedValue::Int(1 << 16)),
        (b"pieces".to_vec(), OwnedValue::Bytes(vec![0; 20])),
    ];
    if private {
        entries.push((b"private".to_vec(), OwnedValue::Int(1)));
    }
    to_vec(&OwnedValue::Dict(entries.into_iter().collect()))
}

struct Fixture {
    app: Router,
    auth: Arc<Auth>,
    store: Arc<Mutex<Store>>,
    hashes: Vec<String>,
    ids: Vec<i64>,
    /// Export's own adult setting (`export_adult`), live.
    export_adult: Arc<AtomicBool>,
    _dir: tempfile::TempDir,
}

/// Stores and classifies `ts` (ids 1, 2, … discovered one second apart
/// from 2023-11-14T22:13:20Z) and serves them. "Show adult content"
/// (search's) is on throughout, so only export's own setting decides.
fn fixture(ts: &[T], config: ExportConfig, export_adult: bool) -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("t.db");
    let mut store = Store::open(&db, StoreConfig::default()).unwrap();
    let (ids, hashes) = insert(&mut store, ts);
    let index = SearchIndex::in_memory().unwrap();
    {
        let mut w = index.writer(15 << 20).unwrap();
        w.catch_up(&mut store, 100_000).unwrap();
        w.commit().unwrap();
    }
    let auth = Arc::new(Auth::load(&dir.path().join("auth.json"), false).unwrap());
    let reader = Store::open_reader(&db, StoreConfig::default()).unwrap();
    let export_adult = Arc::new(AtomicBool::new(export_adult));
    let exported = export_adult.clone();
    let app = Router::new()
        .nest(
            "/api/v1",
            router(ApiState {
                auth: auth.clone(),
                index: Some(index.into()),
                reader: Arc::new(Mutex::new(reader)),
                filters: None,
                settings: None,
                ops: None,
                adult: AdultVisibility::follow(|| true),
                user_data: None,
                insights: None,
                hot: None,
                events: None,
            }),
        )
        .layer(Extension(config))
        .layer(Extension(ExportAdult(AdultVisibility::follow(move || {
            exported.load(Ordering::SeqCst)
        }))));
    Fixture {
        app,
        auth,
        store: Arc::new(Mutex::new(store)),
        hashes,
        ids,
        export_adult,
        _dir: dir,
    }
}

fn insert(store: &mut Store, ts: &[T]) -> (Vec<i64>, Vec<String>) {
    let raws: Vec<Vec<u8>> = ts.iter().map(|t| metainfo(&t.name, t.private)).collect();
    let infos: Vec<Info> = raws.iter().map(|r| Info::parse(r).unwrap()).collect();
    let batch: Vec<NewTorrent<'_>> = infos
        .iter()
        .zip(&raws)
        .zip(ts)
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
            json!({ "token": token, "password": PASSWORD, "notice_version": lodestar_api::legal::NOTICE_VERSION }),
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

struct Download {
    job: u64,
    headers: axum::http::HeaderMap,
    body: BodyDataStream,
}

impl Download {
    async fn next(&mut self) -> Option<Result<Bytes, axum::Error>> {
        std::future::poll_fn(|cx| Pin::new(&mut self.body).poll_next(cx)).await
    }

    async fn rest(mut self) -> Vec<u8> {
        let mut out = Vec::new();
        while let Some(piece) = self.next().await {
            out.extend_from_slice(&piece.expect("a complete download"));
        }
        out
    }
}

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

async fn job(f: &Fixture, cookie: &str, id: u64) -> Value {
    let (status, body) = json_of(&f.app, get(&format!("/api/v1/jobs/{id}"), Some(cookie))).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body
}

/// Waits (up to 15 s) for a job to finish.
async fn finished(f: &Fixture, cookie: &str, id: u64) -> Value {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let j = job(f, cookie, id).await;
        if !j["finished_at"].is_null() {
            return j;
        }
        assert!(Instant::now() < deadline, "job {id} didn't finish: {j}");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// An export's bytes and its finished job.
async fn export(f: &Fixture, c: &str, uri: &str, body: Option<Value>) -> (Vec<u8>, Value) {
    let d = open(f, c, uri, body).await;
    let id = d.job;
    let bytes = d.rest().await;
    let j = finished(f, c, id).await;
    assert_eq!(j["state"], "done", "{j}");
    (bytes, j)
}

/// bitmagnet's `importer.Item` fields (internal/importer/importer.go),
/// lowercased: Go's `encoding/json` matches keys without case.
const ITEM_FIELDS: [&str; 19] = [
    "source",
    "infohash",
    "name",
    "size",
    "private",
    "contenttype",
    "contentsource",
    "contentid",
    "title",
    "releasedate",
    "releaseyear",
    "episodes",
    "videoresolution",
    "videosource",
    "videocodec",
    "video3d",
    "videomodifier",
    "releasegroup",
    "publishedat",
];

/// bitmagnet's `ContentType` ENUM (internal/model/content_type.go).
const CONTENT_TYPES: [&str; 9] = [
    "movie",
    "tv_show",
    "music",
    "ebook",
    "comic",
    "audiobook",
    "game",
    "software",
    "xxx",
];

/// Splits a body the way bitmagnet's `/import` handler does (on `\n`,
/// skipping empty lines) and decodes each line as its `Item` would,
/// failing on anything it would refuse or misread: an unknown key (it
/// would be dropped), a hash that isn't 20 bytes of hex, a size that
/// isn't a `uint`, a content type outside its enum, a time Go's RFC 3339
/// parser refuses, a NUL (PostgreSQL text refuses it).
fn decode(body: &[u8]) -> Vec<Map<String, Value>> {
    let text = std::str::from_utf8(body).expect("UTF-8");
    assert!(
        text.is_empty() || text.ends_with('\n'),
        "the last line is cut"
    );
    text.split('\n')
        .filter(|l| !l.is_empty())
        .map(|l| {
            let v: Value = serde_json::from_str(l).unwrap_or_else(|e| panic!("{e}: {l}"));
            let m = v.as_object().expect("an object").clone();
            for k in m.keys() {
                assert!(
                    ITEM_FIELDS.contains(&k.to_ascii_lowercase().as_str()),
                    "bitmagnet ignores {k:?}"
                );
            }
            let hash = m["infoHash"].as_str().expect("infoHash");
            assert!(
                hash.len() == 40 && hash.bytes().all(|b| b.is_ascii_hexdigit()),
                "{hash}"
            );
            assert!(m["size"].is_u64(), "{l}");
            assert_eq!(m["source"], "lodestar");
            let name = m["name"].as_str().expect("name");
            assert!(!name.contains('\0'), "{l}");
            if let Some(ct) = m.get("contentType") {
                assert!(CONTENT_TYPES.contains(&ct.as_str().unwrap()), "{l}");
            }
            if let Some(at) = m.get("publishedAt") {
                let at = at.as_str().unwrap().as_bytes();
                // RFC 3339, UTC: YYYY-MM-DDTHH:MM:SSZ.
                assert_eq!(at.len(), 20, "{l}");
                for (i, b) in at.iter().enumerate() {
                    match i {
                        4 | 7 => assert_eq!(*b, b'-'),
                        10 => assert_eq!(*b, b'T'),
                        13 | 16 => assert_eq!(*b, b':'),
                        19 => assert_eq!(*b, b'Z'),
                        _ => assert!(b.is_ascii_digit(), "{l}"),
                    }
                }
            }
            m
        })
        .collect()
}

fn names(items: &[Map<String, Value>]) -> Vec<String> {
    items
        .iter()
        .map(|m| m["name"].as_str().unwrap().to_string())
        .collect()
}

fn golden(name: &str, got: &[u8]) {
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
    let want = String::from_utf8(want).unwrap().replace("\r\n", "\n");
    assert_eq!(
        String::from_utf8_lossy(got),
        want,
        "{name}: the export bytes changed"
    );
}

/// A movie, a show, music, an Adult scene, then hostile names: a line
/// break and a forged second object, a NUL, control and bidi
/// characters, a Shift_JIS name (not UTF-8).
fn golden_torrents() -> Vec<T> {
    vec![
        t("The.Dark.Knight.2008.1080p.BluRay.x264-GRP.mkv"),
        t("Breaking.Bad.S01E01.720p.WEB-DL.x264-GRP.mkv"),
        t("Gotham.Soundtrack.2014.FLAC"),
        T {
            adult: true,
            ..t("Studio.Scene.XXX.1080p.mp4")
        },
        t("Two\nLines\"},{\"infoHash\":\"0000000000000000000000000000000000000000\""),
        t("Nul\0Byte\u{7}Bell\u{1b}[31m\u{202e}4pm.exe\u{2028}"),
        T {
            // テスト アルバム in Shift_JIS.
            name: b"\x83e\x83X\x83g \x83A\x83\x8b\x83o\x83\x80 FLAC".to_vec(),
            ..t("")
        },
    ]
}

#[tokio::test]
async fn the_format_is_byte_exact_and_reads_as_bitmagnet_reads_it() {
    let f = fixture(&golden_torrents(), ExportConfig::default(), true);
    let c = session(&f).await;
    let d = open(
        &f,
        &c,
        "/api/v1/export?format=bitmagnet&sort=%2Bdiscovered",
        None,
    )
    .await;
    assert_eq!(d.headers[header::CONTENT_TYPE], "application/x-ndjson");
    assert_eq!(d.headers[header::CACHE_CONTROL], "no-store");
    assert_eq!(d.headers[header::X_CONTENT_TYPE_OPTIONS], "nosniff");
    let cd = d.headers[header::CONTENT_DISPOSITION].to_str().unwrap();
    assert!(
        cd.starts_with("attachment; filename=\"lodestar-export-")
            && cd.ends_with(".bitmagnet.ndjson\""),
        "{cd}"
    );
    let id = d.job;
    let body = d.rest().await;
    let j = finished(&f, &c, id).await;
    assert_eq!(
        (j["done"].clone(), j["skipped"].clone()),
        (json!(7), json!(0))
    );
    assert_eq!(j["export"]["format"], "bitmagnet");
    golden("export_bitmagnet.ndjson", &body);

    let items = decode(&body);
    assert_eq!(items.len(), 7, "one line per torrent, no trailer");
    let hashes: Vec<&str> = items
        .iter()
        .map(|m| m["infoHash"].as_str().unwrap())
        .collect();
    assert_eq!(hashes, f.hashes);
    assert_eq!(items[0]["publishedAt"], "2023-11-14T22:13:20Z");
    assert_eq!(items[0]["size"], 1 << 20);
    let types: Vec<Option<&str>> = items
        .iter()
        .map(|m| m.get("contentType").and_then(Value::as_str))
        .collect();
    assert_eq!(
        types[..4],
        [Some("movie"), Some("tv_show"), Some("music"), Some("xxx")]
    );
    // Bidi and separators are escaped, never raw.
    let raw = String::from_utf8(body.clone()).unwrap();
    assert!(!raw.contains(['\u{202e}', '\u{2028}', '\u{7}']), "{raw}");
    // The forged object stayed inside its name.
    assert!(items[4]["name"].as_str().unwrap().contains("\"},{"));
    assert_eq!(
        items[5]["name"],
        "Nul\u{fffd}Byte\u{7}Bell\u{1b}[31m\u{202e}4pm.exe\u{2028}"
    );
    // The six keys bitmagnet's guide names, and nothing else: no
    // setting, key, path, id or Lodestar-only field.
    for m in &items {
        for k in m.keys() {
            assert!(
                [
                    "infoHash",
                    "name",
                    "size",
                    "source",
                    "publishedAt",
                    "contentType"
                ]
                .contains(&k.as_str()),
                "{k}"
            );
        }
    }
}

/// An ordinary torrent, then one each that a bitmagnet export leaves
/// out whatever is asked: hidden by a rule, `private_source`, BEP 27
/// private, a remote worker's unverified row; and an Adult one, which
/// follows `export_adult`.
fn exclusion_torrents() -> Vec<T> {
    vec![
        t("Gotham.Soundtrack.FLAC"),
        T {
            flags: flags::HIDDEN,
            ..t("Hidden.By.A.Rule.mkv")
        },
        T {
            flags: flags::PRIVATE_SOURCE,
            ..t("Private.Tracker.Only.mkv")
        },
        T {
            private: true,
            ..t("Bep27.Private.Torrent.mkv")
        },
        T {
            flags: flags::REMOTE_UNVERIFIED,
            ..t("Remote.Unverified.mkv")
        },
        T {
            adult: true,
            ..t("Studio.Scene.XXX.1080p.mp4")
        },
        t("Abuse.Report.Deleted.mkv"),
    ]
}

/// Every source exported in the bitmagnet format, with `adult` the
/// export's own setting.
async fn every_source(f: &Fixture, c: &str, collection: i64, adult: bool) {
    f.export_adult.store(adult, Ordering::SeqCst);
    let unknown = "ab".repeat(20);
    let mut all = f.hashes.clone();
    all.push(unknown.clone());
    let want: &[&str] = if adult {
        &["Gotham.Soundtrack.FLAC", "Studio.Scene.XXX.1080p.mp4"]
    } else {
        &["Gotham.Soundtrack.FLAC"]
    };
    for (uri, body) in [
        (
            "/api/v1/export?format=bitmagnet&sort=%2Bdiscovered".to_string(),
            None,
        ),
        (
            "/api/v1/export".to_string(),
            Some(json!({ "hashes": all, "format": "bitmagnet" })),
        ),
        (
            format!("/api/v1/export?format=bitmagnet&collection={collection}"),
            None,
        ),
    ] {
        let (bytes, j) = export(f, c, &uri, body.clone()).await;
        let items = decode(&bytes);
        assert_eq!(names(&items), want, "{uri} {body:?}");
        let text = format!("{}{j}", String::from_utf8(bytes).unwrap());
        // Nothing left out shows, by hash or by name.
        for (i, h) in f.hashes.iter().enumerate() {
            let shown = i == 0 || (adult && i == 5);
            assert_eq!(text.contains(h.as_str()), shown, "{uri}: hash {i}");
        }
        assert!(!text.contains(&unknown));
        let mut gone = vec!["Hidden", "Private", "Bep27", "Remote", "Abuse"];
        if !adult {
            gone.push("Studio");
        }
        for name in gone {
            assert!(!text.contains(name), "{uri}: {name}");
        }
    }
}

#[tokio::test]
async fn hidden_private_unverified_and_deleted_never_export_and_adult_follows_its_setting() {
    let f = fixture(&exclusion_torrents(), ExportConfig::default(), false);
    let c = session(&f).await;
    let collection = {
        let mut store = f.store.lock().unwrap();
        let id = store.create_collection("Picks", None, 1).unwrap();
        store.add_to_collection(id, &f.ids, 1).unwrap();
        // Abuse material confirmed after it was stored (§20.5.2): the row
        // goes, and the index still has it until it catches up.
        let mut h = [0u8; 20];
        for (i, b) in h.iter_mut().enumerate() {
            *b = u8::from_str_radix(&f.hashes[6][2 * i..2 * i + 2], 16).unwrap();
        }
        assert_eq!(store.delete_torrents(&[h]).unwrap().len(), 1);
        id
    };
    every_source(&f, &c, collection, false).await;
    every_source(&f, &c, collection, true).await;
    every_source(&f, &c, collection, false).await;

    // Asking for what it leaves out is refused, not quietly ignored.
    for (uri, body, code) in [
        (
            "/api/v1/export?format=bitmagnet&hidden=true",
            None,
            "hidden_unused",
        ),
        (
            "/api/v1/export",
            Some(json!({ "hashes": f.hashes, "format": "bitmagnet", "hidden": true })),
            "hidden_unused",
        ),
        (
            "/api/v1/export?format=bitmagnet&fields=name",
            None,
            "fields_unused",
        ),
    ] {
        let req = match body {
            Some(b) => post(uri, Some(&c), b),
            None => get(uri, Some(&c)),
        };
        let (status, e) = json_of(&f.app, req).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{uri}");
        assert_eq!(e["error"]["code"], code, "{e}");
    }
    // `hidden=false` is what it does anyway.
    let (bytes, _) = export(&f, &c, "/api/v1/export?format=bitmagnet&hidden=false", None).await;
    assert_eq!(names(&decode(&bytes)), ["Gotham.Soundtrack.FLAC"]);
}

#[tokio::test]
async fn every_existing_format_leaves_out_private_source() {
    let f = fixture(&exclusion_torrents()[..3], ExportConfig::default(), true);
    let c = session(&f).await;
    let collection = {
        let mut store = f.store.lock().unwrap();
        let id = store.create_collection("Picks", None, 1).unwrap();
        store.add_to_collection(id, &f.ids, 1).unwrap();
        id
    };
    let private = &f.hashes[2];
    for format in ["csv", "json", "magnets"] {
        for (uri, body) in [
            (format!("/api/v1/export?format={format}&hidden=true"), None),
            (
                "/api/v1/export".to_string(),
                Some(json!({ "hashes": f.hashes, "format": format, "hidden": true })),
            ),
            (
                format!("/api/v1/export?format={format}&hidden=true&collection={collection}"),
                None,
            ),
        ] {
            let with_hash = if format == "magnets" {
                uri.clone()
            } else {
                // The hash column, so its absence means something.
                let sep = if uri.contains('?') { '&' } else { '?' };
                match &body {
                    None => format!("{uri}{sep}fields=hash,name"),
                    Some(_) => uri.clone(),
                }
            };
            let body = body.map(|mut b| {
                if format != "magnets" {
                    b["fields"] = json!("hash,name");
                }
                b
            });
            let (bytes, j) = export(&f, &c, &with_hash, body).await;
            let text = String::from_utf8(bytes).unwrap();
            assert!(!text.contains(private.as_str()), "{format} {with_hash}");
            assert!(!text.contains("Private.Tracker"), "{format} {with_hash}");
            // The other two are there: the format did write rows.
            assert!(text.contains(&f.hashes[0]), "{format} {with_hash}");
            assert!(text.contains(&f.hashes[1]), "{format} {with_hash}");
            assert_eq!(j["done"], 2, "{format} {with_hash}: {j}");
        }
    }
}

#[tokio::test]
async fn a_large_export_streams_with_bounded_memory() {
    let ts: Vec<T> = (0..300).map(|i| t(&format!("Filler.{i:04}.mkv"))).collect();
    let f = fixture(
        &ts,
        ExportConfig {
            chunk: 10,
            queue: 1,
            ..ExportConfig::default()
        },
        false,
    );
    let c = session(&f).await;
    let mut d = open(
        &f,
        &c,
        "/api/v1/export?format=bitmagnet&sort=%2Bdiscovered",
        None,
    )
    .await;
    let first = d.next().await.unwrap().unwrap();
    assert_eq!(decode(&first).len(), 10, "a chunk at a time");
    // Not reading: the export fills its small queue and waits there, so
    // what it holds is a few chunks whatever the export's size. Wait
    // until its progress stops moving.
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut before = job(&f, &c, d.job).await;
    let mut unchanged = 0;
    while unchanged < 3 {
        assert!(
            Instant::now() < deadline,
            "progress kept moving; last saw {before}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
        let now = job(&f, &c, d.job).await;
        unchanged = if now["done"] == before["done"] {
            unchanged + 1
        } else {
            0
        };
        before = now;
    }
    assert_eq!(before["state"], "running");
    let held = before["done"].as_u64().unwrap();
    // The chunk read, the one queued, the one in the stream, the one
    // waiting to be queued: never the whole export.
    assert!(held <= 50, "it read {held} of 300 rows ahead of the client");
    let mut body = first.to_vec();
    body.extend(d.rest().await);
    let items = decode(&body);
    assert_eq!(items.len(), 300);
    let hashes: Vec<&str> = items
        .iter()
        .map(|m| m["infoHash"].as_str().unwrap())
        .collect();
    assert_eq!(hashes, f.hashes);
}

#[tokio::test]
async fn it_needs_a_session_like_every_export() {
    let f = fixture(&exclusion_torrents()[..1], ExportConfig::default(), false);
    let (status, e) = json_of(&f.app, get("/api/v1/export?format=bitmagnet", None)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{e}");
    // A POST from another site is refused before anything runs.
    let c = session(&f).await;
    let req = Request::post("/api/v1/export")
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::COOKIE, &c)
        .header(header::HOST, "lodestar.local")
        .header(header::ORIGIN, "https://evil.example")
        .body(Body::from(json!({ "format": "bitmagnet" }).to_string()))
        .unwrap();
    let (status, e) = json_of(&f.app, req).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{e}");
    assert_eq!(e["error"]["code"], "cross_origin");
}
