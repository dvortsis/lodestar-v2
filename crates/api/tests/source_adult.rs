//! An always-adult source's torrents through search (X-source-adult,
//! owner Q7): a clean-named torrent that such a source found is classified
//! Adult by the pipeline's own path (`lodestar_pipeline::enrich`), so
//! search, counts and detail by hash leave it out while "Show adult
//! content" is off and show it once it's on. Exports and feeds read the
//! same effective class (`export.rs`, `digest.rs`).
//!
//! `/classify/correct` keeps it adult (X-source-adult-2): clearing a
//! correction or setting only Not junk never un-adults it; only your own
//! Not adult does.

use std::net::SocketAddr;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Request, StatusCode, header};
use lodestar_api::dto::filters::CorrectBody;
use lodestar_api::filters::FiltersApi;
use lodestar_api::{AdultVisibility, ApiState, Auth, router};
use lodestar_bencode::{OwnedValue, to_vec};
use lodestar_metainfo::Info;
use lodestar_pipeline::enrich::{Backfill, enrich_stored_with};
use lodestar_pipeline::source_adult::AdultSources;
use lodestar_search::SearchIndex;
use lodestar_store::{NewTorrent, Store, StoreConfig};
use serde_json::{Value, json};
use tokio::sync::watch;
use tower::ServiceExt;

const PASSWORD: &str = "correct horse battery";

/// Clean names: detection calls neither adult.
const NAMES: [&str; 2] = ["Garden.Tour.2022.1080p.mkv", "Lake.Walk.2021.1080p.mkv"];

fn raw(name: &str) -> Vec<u8> {
    let b = |s: &str| OwnedValue::Bytes(s.as_bytes().to_vec());
    to_vec(&OwnedValue::Dict(
        [
            (b"length".to_vec(), OwnedValue::Int(1 << 30)),
            (b"name".to_vec(), b(name)),
            (b"piece length".to_vec(), OwnedValue::Int(1 << 16)),
            (b"pieces".to_vec(), OwnedValue::Bytes(vec![0; 20])),
        ]
        .into_iter()
        .collect(),
    ))
}

async fn call(app: &Router, mut req: Request<Body>) -> (StatusCode, Option<String>, Value) {
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

#[tokio::test]
async fn an_always_adult_sources_clean_find_is_hidden_until_adult_is_shown() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("t.db");
    let mut store = Store::open(&db, StoreConfig::default()).unwrap();
    let adult = store.register_source("adult", "1", 1).unwrap();
    let plain = store.register_source("plain", "1", 1).unwrap();
    let raws: Vec<Vec<u8>> = NAMES.iter().map(|n| raw(n)).collect();
    let infos: Vec<Info> = raws.iter().map(|r| Info::parse(r).unwrap()).collect();
    let batch: Vec<NewTorrent<'_>> = infos
        .iter()
        .zip(&raws)
        .zip([adult, plain])
        .map(|((info, raw), source)| NewTorrent {
            info,
            raw_info: Some(raw),
            discovered_at: 1_700_000_000,
            metadata_at: 1_700_000_100,
            extra_flags: 0,
            source,
        })
        .collect();
    store.insert(&batch).unwrap();
    let hashes: Vec<String> = infos
        .iter()
        .map(|i| {
            i.primary_hash()
                .0
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect()
        })
        .collect();
    // Classified as the pipeline classifies them, with the first source
    // registered as always adult.
    let filter = lodestar_pipeline::default_filter();
    let ctx = Backfill {
        filter: Some(&filter),
        adult: AdultSources(1 << adult.get()),
        ..Backfill::default()
    };
    assert!(
        enrich_stored_with(&mut store, 0, 10, &ctx)
            .unwrap()
            .is_some()
    );
    let index = SearchIndex::in_memory().unwrap();
    let mut w = index.writer(15 << 20).unwrap();
    w.catch_up(&mut store, 100).unwrap();
    w.commit().unwrap();

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
            hot: None,
            events: None,
        }),
    );
    let token = auth.setup_token().unwrap();
    let setup = Request::post("/api/v1/setup")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            json!({ "token": token, "password": PASSWORD }).to_string(),
        ))
        .unwrap();
    let (status, cookie, _) = call(&app, setup).await;
    assert_eq!(status, StatusCode::OK);
    let c = cookie.unwrap();

    // Off (the default): only the plain source's torrent, in hits and
    // counts, and the adult one isn't served by hash.
    let (_, body) = {
        let (s, _, b) = call(&app, get("/api/v1/search", &c)).await;
        (s, b)
    };
    assert_eq!(names(&body), [NAMES[1]]);
    assert_eq!(body["total"], 1);
    let (_, _, body) = call(&app, get("/api/v1/search?q=garden", &c)).await;
    assert_eq!(body["total"], 0);
    let (status, _, _) = call(&app, get(&format!("/api/v1/torrents/{}", hashes[0]), &c)).await;
    assert_ne!(status, StatusCode::OK);
    let (status, _, _) = call(&app, get(&format!("/api/v1/torrents/{}", hashes[1]), &c)).await;
    assert_eq!(status, StatusCode::OK);

    // On: both.
    show.send(true).unwrap();
    let (_, _, body) = call(&app, get("/api/v1/search", &c)).await;
    assert_eq!(names(&body), NAMES);
    let (status, _, _) = call(&app, get(&format!("/api/v1/torrents/{}", hashes[0]), &c)).await;
    assert_eq!(status, StatusCode::OK);
}

/// A correction on a torrent an always-adult source found, through the
/// API's own handler: clear, Not junk only and Is adult leave it Adult;
/// your Not adult makes it detection's type; clearing again restores it.
#[test]
fn a_correction_never_un_adults_a_source_adult_torrent_but_yours() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("t.db");
    let mut store = Store::open(&db, StoreConfig::default()).unwrap();
    let adult = store.register_source("adult", "1", 1).unwrap();
    let plain = store.register_source("plain", "1", 1).unwrap();
    let raws: Vec<Vec<u8>> = NAMES.iter().map(|n| raw(n)).collect();
    let infos: Vec<Info> = raws.iter().map(|r| Info::parse(r).unwrap()).collect();
    let batch: Vec<NewTorrent<'_>> = infos
        .iter()
        .zip(&raws)
        .zip([adult, plain])
        .map(|((info, raw), source)| NewTorrent {
            info,
            raw_info: Some(raw),
            discovered_at: 1_700_000_000,
            metadata_at: 1_700_000_100,
            extra_flags: 0,
            source,
        })
        .collect();
    store.insert(&batch).unwrap();
    let mask = AdultSources(1 << adult.get());
    let filter = lodestar_pipeline::default_filter();
    let ctx = Backfill {
        filter: Some(&filter),
        adult: mask,
        ..Backfill::default()
    };
    assert!(
        enrich_stored_with(&mut store, 0, 10, &ctx)
            .unwrap()
            .is_some()
    );
    let hex = |i: &Info| -> String {
        i.primary_hash()
            .0
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    };
    let ids: Vec<i64> = infos
        .iter()
        .map(|i| store.find(&i.primary_hash()).unwrap().unwrap())
        .collect();
    let adult_code = Some(lodestar_classify::ContentType::Adult.code());
    assert_eq!(store.content_type(ids[0]).unwrap(), adult_code);
    assert_ne!(store.content_type(ids[1]).unwrap(), adult_code);

    let api = FiltersApi {
        store: Arc::new(Mutex::new(store)),
        engine: Arc::new(filter),
        held: Default::default(),
        sealer: None,
        ips: Default::default(),
        index: None,
        reenrich: Arc::new(AtomicBool::new(false)),
        jobs: Default::default(),
        quarantine_secs: 30 * 86_400,
        shutdown: Default::default(),
        adult_sources: mask.0,
    };
    let correct = |i: usize, adult: Option<bool>, not_junk: bool| {
        let r = api
            .correct(&CorrectBody {
                hash: hex(&infos[i]),
                adult,
                not_junk,
            })
            .unwrap();
        let stored = api.store.lock().unwrap().content_type(ids[i]).unwrap();
        (r.content_type, stored)
    };
    let is_adult = |(ty, stored): (String, Option<u8>)| ty == "adult" && stored == adult_code;
    let is_not = |(ty, stored): (String, Option<u8>)| ty != "adult" && stored != adult_code;

    // Clear (no correction at all), Not junk only, Is adult: still adult.
    assert!(is_adult(correct(0, None, false)), "clear");
    assert!(is_adult(correct(0, None, true)), "Not junk only");
    assert!(is_adult(correct(0, Some(true), false)), "Is adult");
    // Your own Not adult wins: detection's type.
    assert!(is_not(correct(0, Some(false), false)), "Not adult");
    assert!(is_not(correct(0, Some(false), true)), "Not adult, Not junk");
    // Clearing it again: the source makes it adult once more.
    assert!(is_adult(correct(0, None, false)), "cleared again");
    // The correction stored is yours alone: never a pretend Is adult.
    let stored = api.store.lock().unwrap().correction(ids[0]).unwrap();
    assert!(stored.is_none_or(|c| c.adult.is_none()), "{stored:?}");

    // A plain source's torrent: clearing leaves it detection's type.
    assert!(is_not(correct(1, None, true)), "plain, Not junk only");
}
