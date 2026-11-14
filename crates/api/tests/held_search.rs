//! FILTER-15 through the router: a torrent a held write deletes drops out
//! of `/search` and Torznab results as soon as the write returns; its
//! count and facet values drop once the index commits the re-index log.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Request, StatusCode, header};
use axum::{Extension, Router};
use lodestar_api::keys::{self, scope};
use lodestar_api::torznab::TorznabAdult;
use lodestar_api::{AdultVisibility, ApiState, Auth, router};
use lodestar_bencode::{OwnedValue, to_vec};
use lodestar_classify::Correction;
use lodestar_metainfo::Info;
use lodestar_search::SearchIndex;
use lodestar_store::{
    Filtered, Held, KeyKind, NewApiKey, NewTorrent, SourceId, Store, StoreConfig, WriteBatch,
};
use serde_json::{Value, json};
use tower::ServiceExt;

const PASSWORD: &str = "correct horse battery";
const HELD: &str = "Big.Buck.Bunny.2008.1080p.mkv";
const KEPT: &str = "Bunny.Tales.2010.720p.mkv";

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

struct Fixture {
    app: Router,
    auth: Arc<Auth>,
    users: Arc<Mutex<Store>>,
    store: Store,
    writer: lodestar_search::Indexer,
    held: [u8; 20],
    _dir: tempfile::TempDir,
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("t.db");
    let mut store = Store::open(&db, StoreConfig::default()).unwrap();
    let raws = [raw(HELD), raw(KEPT)];
    let infos: Vec<Info> = raws.iter().map(|r| Info::parse(r).unwrap()).collect();
    let batch: Vec<NewTorrent<'_>> = infos
        .iter()
        .zip(&raws)
        .map(|(info, raw)| NewTorrent {
            info,
            raw_info: Some(raw),
            discovered_at: 1_700_000_000,
            metadata_at: 1_700_000_100,
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
    for id in ids {
        let row = store.torrent(id).unwrap().unwrap();
        let tree = store.files(id).unwrap().unwrap();
        let files = lodestar_classify::file_infos(&tree.files, tree.encoding);
        let (e, _) =
            lodestar_filter::reclassify::enrich_corrected(&row.name, &files, Correction::default());
        store.set_enrichments(&[(id, e)]).unwrap();
    }
    let index = SearchIndex::in_memory().unwrap();
    let mut writer = index.writer(15 << 20).unwrap();
    writer.catch_up(&mut store, 100).unwrap();
    writer.commit().unwrap();
    let auth = Arc::new(Auth::load(&dir.path().join("auth.json"), false).unwrap());
    let users = Arc::new(Mutex::new(
        Store::open(&db, StoreConfig::default()).unwrap(),
    ));
    let app = Router::new()
        .nest(
            "/api/v1",
            router(ApiState {
                auth: auth.clone(),
                index: Some(index.into()),
                reader: Arc::new(Mutex::new(
                    Store::open_reader(&db, StoreConfig::default()).unwrap(),
                )),
                filters: None,
                settings: None,
                adult: AdultVisibility::follow(|| false),
                ops: None,
                insights: None,
                user_data: Some(users.clone()),
                hot: None,
                events: None,
            }),
        )
        .layer(Extension(TorznabAdult(AdultVisibility::follow(|| false))));
    Fixture {
        app,
        auth,
        users,
        store,
        writer,
        held: infos[0].primary_hash().0,
        _dir: dir,
    }
}

async fn call(app: &Router, mut req: Request<Body>) -> (StatusCode, Option<String>, String) {
    req.extensions_mut()
        .insert(ConnectInfo(SocketAddr::from(([10, 0, 9, 1], 1))));
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

async fn session(f: &Fixture) -> String {
    let token = f.auth.setup_token().unwrap();
    let body = json!({ "token": token, "password": PASSWORD, "notice_version": lodestar_api::legal::NOTICE_VERSION });
    let req = Request::post("/api/v1/setup")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    let (status, cookie, _) = call(&f.app, req).await;
    assert_eq!(status, StatusCode::OK);
    cookie.unwrap()
}

fn torznab_key(f: &Fixture) -> String {
    let m = keys::mint(KeyKind::ApiKey).unwrap();
    f.users
        .lock()
        .unwrap()
        .create_api_key(
            &NewApiKey {
                kind: KeyKind::ApiKey,
                name: "arr",
                prefix: &m.prefix,
                hash: m.hash,
                scopes: scope::TORZNAB,
                saved_search_id: None,
                expires_at: None,
            },
            0,
        )
        .unwrap();
    m.secret.expose().to_string()
}

/// `/search?q=bunny`: its total and the names on the page.
async fn search(f: &Fixture, cookie: &str) -> (u64, Vec<String>) {
    let req = Request::get("/api/v1/search?q=bunny")
        .header(header::COOKIE, cookie)
        .body(Body::empty())
        .unwrap();
    let (status, _, body) = call(&f.app, req).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let v: Value = serde_json::from_str(&body).unwrap();
    let names = v["hits"]
        .as_array()
        .unwrap()
        .iter()
        .map(|h| h["name"].as_str().unwrap().to_owned())
        .collect();
    (v["total"].as_u64().unwrap(), names)
}

/// `/facets?q=bunny`: every facet value's count, summed over the type
/// facet (each torrent has one type).
async fn facet_total(f: &Fixture, cookie: &str) -> u64 {
    let req = Request::get("/api/v1/facets?q=bunny")
        .header(header::COOKIE, cookie)
        .body(Body::empty())
        .unwrap();
    let (status, _, body) = call(&f.app, req).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let v: Value = serde_json::from_str(&body).unwrap();
    v["facets"]["type"]["values"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v["count"].as_u64().unwrap())
        .sum()
}

/// Torznab's search feed for `bunny`.
async fn torznab(f: &Fixture, key: &str) -> String {
    let req = Request::get(format!("/api/v1/torznab/api?t=search&q=bunny&apikey={key}"))
        .body(Body::empty())
        .unwrap();
    let (status, _, body) = call(&f.app, req).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body
}

#[tokio::test]
async fn a_held_write_drops_the_torrent_from_search_at_once() {
    let mut f = fixture();
    let cookie = session(&f).await;
    let key = torznab_key(&f);
    let (total, names) = search(&f, &cookie).await;
    assert_eq!(total, 2);
    assert!(names.iter().any(|n| n == HELD));
    assert!(torznab(&f, &key).await.contains(HELD));
    assert_eq!(facet_total(&f, &cookie).await, 2);

    // The hold's write returns...
    let written = f
        .store
        .write(&WriteBatch {
            filtered: &[Filtered {
                hashes: &[f.held],
                rule: "U-1",
                action: 2,
                held: Some(Held::Tombstone),
                sealed: None,
                evidence: &[],
                at: 1,
                expires_at: 0,
                adult_hold: false,
            }],
            ..Default::default()
        })
        .unwrap();
    assert_eq!(written.deleted.len(), 1);

    // ...and the results leave it out at once, before the index commits.
    let (_, names) = search(&f, &cookie).await;
    assert_eq!(names, [KEPT]);
    let feed = torznab(&f, &key).await;
    assert!(!feed.contains(HELD) && feed.contains(KEPT));

    // Counts and facets follow once the index commits the re-index log.
    f.writer.catch_up(&mut f.store, 100).unwrap();
    f.writer.commit().unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let (total, _) = search(&f, &cookie).await;
        let facets = facet_total(&f, &cookie).await;
        if total == 1 && facets == 1 {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "still counted: total {total}, facets {facets}"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}
