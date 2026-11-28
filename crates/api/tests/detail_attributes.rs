//! `GET /torrents/{hash}`'s `attributes`: the release attributes the
//! name parsed to, read from the stored enrichment record, the same map
//! `POST /compare` shows for the torrent (X-detail-attributes).

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use lodestar_api::{AdultVisibility, ApiState, Auth, router};
use lodestar_bencode::{OwnedValue, to_vec};
use lodestar_classify::Correction;
use lodestar_metainfo::{Info, Limits};
use lodestar_release_parser::MAX_INPUT_BYTES;
use lodestar_store::{NewTorrent, SourceId, Store, StoreConfig};
use serde_json::{Map, Value, json};
use tower::ServiceExt;

const PASSWORD: &str = "correct horse battery";

/// A single-file torrent's info dictionary.
fn metainfo(name: &str) -> Vec<u8> {
    let b = |s: &str| OwnedValue::Bytes(s.as_bytes().to_vec());
    let d = OwnedValue::Dict(
        [
            ("length", OwnedValue::Int(1 << 30)),
            ("name", b(name)),
            ("piece length", OwnedValue::Int(1 << 16)),
            ("pieces", OwnedValue::Bytes(vec![0; 20])),
        ]
        .into_iter()
        .map(|(k, v)| (k.as_bytes().to_vec(), v))
        .collect(),
    );
    to_vec(&d)
}

struct Fixture {
    app: Router,
    cookie: String,
    hashes: Vec<String>,
    _dir: tempfile::TempDir,
}

/// One torrent per name; `(name, classified, adult)`. A classified one
/// is enriched as the pipeline would; `adult` as an Is adult correction
/// would. `shown`: "Show adult content" on.
async fn fixture(torrents: &[(&str, bool, bool)], shown: bool) -> Fixture {
    fixture_with(torrents, shown, false).await
}

/// As [`fixture`]; `corrupt` stores every classified torrent's record as
/// bytes this build can't read (a rollback, a newer build's snapshot).
async fn fixture_with(torrents: &[(&str, bool, bool)], shown: bool, corrupt: bool) -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("t.db");
    let mut store = Store::open(&db, StoreConfig::default()).unwrap();
    let raws: Vec<Vec<u8>> = torrents.iter().map(|(n, ..)| metainfo(n)).collect();
    let infos: Vec<Info> = raws.iter().map(|r| Info::parse(r).unwrap()).collect();
    let batch: Vec<NewTorrent<'_>> = infos
        .iter()
        .zip(&raws)
        .enumerate()
        .map(|(i, (info, raw))| NewTorrent {
            info,
            raw_info: Some(raw),
            discovered_at: 1_700_000_000 + i as i64,
            metadata_at: 1_700_000_001 + i as i64,
            extra_flags: 0,
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
    for (id, (_, classified, adult)) in ids.iter().zip(torrents) {
        if !classified {
            continue;
        }
        let row = store.torrent(*id).unwrap().unwrap();
        let tree = store.files(*id).unwrap().unwrap();
        let files = lodestar_classify::file_infos(&tree.files, tree.encoding);
        let correction = Correction {
            adult: adult.then_some(true),
            not_junk: false,
        };
        // The parse runs here, at classify time: bounded however hostile
        // the name. Generous for the slowest CI runner in a debug build.
        let started = Instant::now();
        let (mut e, _) =
            lodestar_filter::reclassify::enrich_corrected(&row.name, &files, correction);
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "classifying {} bytes took {:?}",
            row.name.len(),
            started.elapsed()
        );
        if corrupt {
            e.data = b"e999\nnot\ta record".to_vec();
        }
        enriched.push((*id, e));
    }
    store.set_enrichments(&enriched).unwrap();
    let auth = Arc::new(Auth::load(&dir.path().join("auth.json"), false).unwrap());
    let reader = Store::open_reader(&db, StoreConfig::default()).unwrap();
    let app = Router::new().nest(
        "/api/v1",
        router(ApiState {
            auth: auth.clone(),
            index: None,
            reader: Arc::new(Mutex::new(reader)),
            filters: None,
            settings: None,
            ops: None,
            user_data: None,
            adult: AdultVisibility::follow(move || shown),
            insights: None,
            hot: None,
            events: None,
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
    let token = auth.setup_token().unwrap();
    let resp = app
        .clone()
        .oneshot(post(
            "/api/v1/setup",
            None,
            json!({ "token": token, "password": PASSWORD, "notice_version": lodestar_api::legal::NOTICE_VERSION }),
        ))
        .await
        .unwrap();
    let cookie = resp.headers()[header::SET_COOKIE]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_string();
    Fixture {
        app,
        cookie,
        hashes,
        _dir: dir,
    }
}

fn post(uri: &str, cookie: Option<&str>, body: Value) -> Request<Body> {
    let mut b = Request::post(uri).header(header::CONTENT_TYPE, "application/json");
    if let Some(c) = cookie {
        b = b.header(header::COOKIE, c);
    }
    b.body(Body::from(body.to_string())).unwrap()
}

async fn call(f: &Fixture, req: Request<Body>) -> (StatusCode, Value) {
    let resp = f.app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), 1 << 24)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

async fn detail(f: &Fixture, i: usize) -> (StatusCode, Value) {
    let req = Request::get(format!("/api/v1/torrents/{}", f.hashes[i]))
        .header(header::COOKIE, &f.cookie)
        .body(Body::empty())
        .unwrap();
    call(f, req).await
}

fn attributes(body: &Value) -> &Value {
    body.as_object()
        .expect("a detail object")
        .get("attributes")
        .expect("`attributes` is always present")
}

#[tokio::test]
async fn a_classified_torrent_carries_its_release_attributes() {
    let f = fixture(
        &[("The.Movie.2008.1080p.BluRay.x264-GROUP", true, false)],
        false,
    )
    .await;
    let (status, body) = detail(&f, 0).await;
    assert_eq!(status, StatusCode::OK);
    let a = attributes(&body).as_object().expect("an object");
    assert_eq!(a["title"], "The Movie");
    assert_eq!(a["year"], "2008");
    assert_eq!(a["res"], "1080p");
    assert_eq!(a["vcodec"], "h264");
    assert_eq!(a["group"], "GROUP");
    // Every value is a string: never null, never a list.
    assert!(a.values().all(Value::is_string), "{a:?}");
}

#[tokio::test]
async fn an_unclassified_torrent_has_null_attributes() {
    let f = fixture(
        &[("The.Movie.2008.1080p.BluRay.x264-GROUP", false, false)],
        false,
    )
    .await;
    let (status, body) = detail(&f, 0).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(attributes(&body), &Value::Null);
}

#[tokio::test]
async fn a_record_this_build_cant_read_has_null_attributes() {
    let f = fixture_with(
        &[("The.Movie.2008.1080p.BluRay.x264-GROUP", true, false)],
        false,
        true,
    )
    .await;
    let (status, body) = detail(&f, 0).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(attributes(&body), &Value::Null);
}

#[tokio::test]
async fn an_adult_torrent_stays_hidden_and_shows_attributes_when_shown() {
    let name = "Some.Scene.2021.1080p.WEB-GROUP";
    let hidden = fixture(&[(name, true, true)], false).await;
    let (status, body) = detail(&hidden, 0).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"]["code"], "adult_hidden");
    assert!(body.get("attributes").is_none());
    let shown = fixture(&[(name, true, true)], true).await;
    let (status, body) = detail(&shown, 0).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(attributes(&body)["res"], "1080p");
}

/// One torrent reads the same in Compare and in its detail: compare's
/// column, its nulls (attributes only another torrent has) left out, is
/// the detail's map.
#[tokio::test]
async fn compare_and_detail_return_the_same_map() {
    let f = fixture(
        &[
            ("The.Movie.2008.1080p.BluRay.x264.DTS-GROUP", true, false),
            ("The Movie (2008) [720p] [WEBRip] [x265] [YTS]", true, false),
            (
                "Show.S01E02.Title.2160p.WEB-DL.DDP5.1.HDR.HEVC-GRP",
                true,
                false,
            ),
            (
                "[SubGroup] Anime Name - 12 [1080p][Multiple Subtitle]",
                true,
                false,
            ),
        ],
        false,
    )
    .await;
    let (status, compare) = call(
        &f,
        post(
            "/api/v1/compare",
            Some(&f.cookie),
            json!({ "hashes": f.hashes }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{compare}");
    for (i, column) in compare["torrents"].as_array().unwrap().iter().enumerate() {
        let from_compare: Map<String, Value> = column["attributes"]
            .as_object()
            .unwrap()
            .iter()
            .filter(|(_, v)| !v.is_null())
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        let (status, body) = detail(&f, i).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            attributes(&body).as_object().unwrap(),
            &from_compare,
            "torrent {i}"
        );
        assert!(!from_compare.is_empty(), "torrent {i} parsed to nothing");
    }
}

/// `unit` repeated to fill `bytes` at most.
fn fill(unit: &str, bytes: usize) -> String {
    unit.repeat(bytes / unit.len())
}

/// Hostile and odd names, as long as metainfo lets a name be (bidi and
/// zero-width controls, combining marks, non-BMP letters, a flood of
/// markers, control characters) are classified within the fixture's
/// deadline and served: the route answers in time, the map stays bounded
/// by the parser's input cap, and nothing reads past it.
#[tokio::test]
async fn hostile_names_are_served_within_bounds() {
    let cap = Limits::default().max_name_bytes;
    let long = fill("A.", cap);
    let bidi = fill("\u{202e}\u{200b}\u{fdfa}\u{1d518}\u{1d52b}\u{0301}", cap);
    let flood = "[Grp] ".to_string() + &fill("S01E01 1080p 720p x265 HEVC AAC FLAC ", cap - 6);
    let nul = "Name\u{0}With\u{7f}Controls\u{feff}.2008.1080p";
    let names = [long.as_str(), bidi.as_str(), flood.as_str(), nul];
    assert!(
        names
            .iter()
            .all(|n| n.len() <= cap && n.len() > cap / 2 || n == &nul)
    );
    let torrents: Vec<(&str, bool, bool)> = names.iter().map(|n| (*n, true, false)).collect();
    let f = fixture(&torrents, false).await;
    for i in 0..names.len() {
        let started = Instant::now();
        let (status, body) = detail(&f, i).await;
        // Generous for the slowest CI runner in a debug build: the read is
        // a point lookup and a decode of a record of at most a few KB.
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "name {i}: {:?}",
            started.elapsed()
        );
        assert_eq!(status, StatusCode::OK, "name {i}: {body}");
        let a = attributes(&body).as_object().expect("an object");
        let bytes: usize = a
            .iter()
            .map(|(k, v)| k.len() + v.as_str().unwrap().len())
            .sum();
        // Each value is a slice of the parser's truncated input, or a
        // short canonical form: a few times the input cap at most.
        assert!(bytes <= 4 * MAX_INPUT_BYTES, "name {i}: {bytes} bytes");
        for v in a.values() {
            assert!(
                v.as_str().unwrap().len() <= MAX_INPUT_BYTES,
                "name {i}: {v}"
            );
        }
    }
}

/// The document says what the route sends: `attributes` is required, an
/// object of strings or null, so the web client's type is
/// `{[key: string]: string} | null`. The committed `openapi.json` is held
/// to this document by CI's drift check.
#[test]
fn the_document_describes_attributes_as_a_nullable_string_map() {
    let doc: Value = serde_json::from_str(&lodestar_api::openapi::json()).unwrap();
    let schema = &doc["components"]["schemas"]["TorrentDetail"];
    assert!(
        schema["required"]
            .as_array()
            .unwrap()
            .contains(&json!("attributes"))
    );
    let field = &schema["properties"]["attributes"];
    assert_eq!(field["type"], json!(["object", "null"]));
    assert_eq!(field["additionalProperties"], json!({"type": "string"}));
    let description = field["description"].as_str().unwrap();
    assert!(description.contains("Null when"), "{description}");
    assert!(description.contains("`{}`"), "{description}");
    // The example is a real parse's map.
    assert!(field["example"]["res"].is_string());
}
