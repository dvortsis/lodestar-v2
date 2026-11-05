//! Provenance through the router (SRC-17a, architecture §18.5–§18.8):
//! the detail view's sources and claims, a hit's source and the
//! `source`/`verified`/`network` facets, and the privacy rule of §18.7
//! on every surface: a private source's name and its claims reach the
//! signed-in admin session only, never an API key, a feed token, an
//! export or a public route.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Method, Request, StatusCode, header};
use lodestar_api::ops::{
    BoxFuture, CrawlerRuntime, ExternalAddress, FetchStatus, NetworkCheck, OpsApi, OpsError,
    PortMapping, ReachReason, Source, SourcePatch, StatsResponse,
};
use lodestar_api::{ApiState, Auth, router};
use lodestar_bencode::{OwnedValue, to_vec};
use lodestar_metainfo::Info;
use lodestar_search::SearchIndex;
use lodestar_store::claims::{ClaimedFile, Claims};
use lodestar_store::unverified::{Network, NewUnverified};
use lodestar_store::{NewTorrent, SavedSearchInput, SearchHits, Store, StoreConfig, flags};
use serde_json::{Value, json};
use tower::ServiceExt;

const PASSWORD: &str = "correct horse battery";
/// Names a public caller must never see: the private source's id and
/// display name, and what it claimed.
const SECRET: [&str; 3] = ["prowlarr", "Prowlarr", "Private Tracker Title"];

static NEXT_IP: AtomicU32 = AtomicU32::new(1);

fn fresh_ip() -> SocketAddr {
    let n = NEXT_IP.fetch_add(1, Ordering::Relaxed);
    SocketAddr::from(([10, 60, (n >> 8) as u8, n as u8], 40000))
}

/// The pipeline, as far as provenance asks: its plugins' names and
/// consent classes.
#[derive(Debug)]
struct FakeOps;

fn source(id: u8, plugin: &str, name: &str, consent: &str) -> Source {
    serde_json::from_value(json!({
        "agents": { "count": 1, "enforced": false, "max": 1 },
        "consent": consent,
        "consent_required": false,
        "consent_version": "",
        "credentials": [],
        "editable": [],
        "egress": {
            "live": { "counts": { "answered": 0, "kill_switch_aborts": 0, "kill_switch_refusals": 0,
                "policy_refusals": 0, "transport_failures": 0, "trips": 0 },
                "notice": null, "reason": null, "state": "direct" },
            "setting": { "route": "direct" },
            "warnings": []
        },
        "enabled": true,
        "endpoints": [],
        "health": { "harvested_24h": 0, "last_error": null, "last_run": null,
            "running": true, "stored_24h": 0 },
        "id": id,
        "kinds": ["discovery"],
        "name": name,
        "needs": null,
        "networks": ["bt"],
        "not_yet_editable": [],
        "password_required": false,
        "plugin": plugin,
        "priority": "normal",
        "purge_pending": false,
        "restart_required": false,
        "risk": "low",
        "schedule": { "default": [], "enforced": false, "windows": [] },
        "state": "enabled",
        "version": "1"
    }))
    .unwrap()
}

impl OpsApi for FakeOps {
    fn crawler(&self) -> CrawlerRuntime {
        CrawlerRuntime {
            since: 0,
            dht: None,
            fetch: FetchStatus {
                in_flight: 0,
                lookups: 0,
                queued: 0,
            },
        }
    }

    fn sync_crawler(&self) -> BoxFuture<'_, ()> {
        Box::pin(async {})
    }

    fn stats(&self) -> BoxFuture<'_, StatsResponse> {
        Box::pin(async {
            StatsResponse {
                data_bytes: None,
                disk_free_bytes: None,
                fetched_per_hour: None,
                harvested_per_hour: None,
                index_lag_s: None,
                paused: false,
                queue_depth: 0,
                sampling_blocked: None,
                searchable_torrents: None,
                stored_torrents: 0,
                updated_at: 0,
                uptime_s: 0,
            }
        })
    }

    fn sources(&self) -> BoxFuture<'_, Result<Vec<Source>, OpsError>> {
        Box::pin(async {
            Ok(vec![
                source(0, "dht", "Mainline DHT", "none"),
                source(1, "nyaa", "Nyaa", "external"),
                source(2, "prowlarr", "Prowlarr", "account"),
            ])
        })
    }

    fn patch_source(
        &self,
        _: u8,
        _: SourcePatch,
        _: bool,
    ) -> BoxFuture<'_, Result<Source, OpsError>> {
        Box::pin(async { Err(OpsError::UnknownSource) })
    }

    fn patch_source_credentials(
        &self,
        _: u8,
        _: Vec<(String, Option<lodestar_api::ops::Secret>)>,
    ) -> BoxFuture<'_, Result<Source, OpsError>> {
        Box::pin(async { Err(OpsError::UnknownSource) })
    }

    fn network(&self) -> NetworkCheck {
        NetworkCheck {
            checked_at: 0,
            external: ExternalAddress {
                agreeing: 0,
                ip: None,
                port: None,
                voters: 0,
            },
            inbound_queries: None,
            mapping: PortMapping::Unavailable,
            measured_s: 0,
            min_window_s: 120,
            reachable: None,
            reason: ReachReason::DhtOff,
            unsolicited_queries: None,
            unsolicited_sources: None,
            window_s: 600,
            window_started_at: None,
        }
    }

    fn restart_network_check(&self) -> Result<NetworkCheck, OpsError> {
        Err(OpsError::Internal("not here".into()))
    }
}

fn metainfo(name: &str, files: &[(&str, i64)]) -> Vec<u8> {
    let d = |e: Vec<(&str, OwnedValue)>| {
        OwnedValue::Dict(
            e.into_iter()
                .map(|(k, v)| (k.as_bytes().to_vec(), v))
                .collect(),
        )
    };
    let list = files
        .iter()
        .map(|(p, len)| {
            d(vec![
                ("length", OwnedValue::Int(*len)),
                (
                    "path",
                    OwnedValue::List(vec![OwnedValue::Bytes(p.as_bytes().to_vec())]),
                ),
            ])
        })
        .collect();
    to_vec(&d(vec![
        ("files", OwnedValue::List(list)),
        ("name", OwnedValue::Bytes(name.as_bytes().to_vec())),
        ("piece length", OwnedValue::Int(1 << 16)),
        ("pieces", OwnedValue::Bytes(vec![0; 20])),
    ]))
}

struct Fixture {
    app: Router,
    auth: Arc<Auth>,
    /// The public torrent (DHT, Nyaa's claims, and Prowlarr's private
    /// claims), then the private-source one (Prowlarr only).
    hashes: [String; 2],
    saved_search: i64,
    _dir: tempfile::TempDir,
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("t.db");
    let mut store = Store::open(&db, StoreConfig::default()).unwrap();
    let dht = store.register_source("dht", "1", 1_700_000_000).unwrap();
    let nyaa = store.register_source("nyaa", "1", 1_700_000_000).unwrap();
    let prowlarr = store
        .register_source("prowlarr", "1", 1_700_000_000)
        .unwrap();
    let raws = [
        metainfo(
            "Public.Show.S01.1080p",
            &[("e01.mkv", 1 << 30), ("e02.mkv", 1 << 30)],
        ),
        metainfo("Members.Only.2024.2160p", &[("film.mkv", 4 << 30)]),
    ];
    let infos: Vec<Info> = raws.iter().map(|r| Info::parse(r).unwrap()).collect();
    for (i, (info, raw)) in infos.iter().zip(&raws).enumerate() {
        let (source, extra_flags) = if i == 0 {
            (dht, 0)
        } else {
            (prowlarr, flags::PRIVATE_SOURCE)
        };
        store
            .insert(&[NewTorrent {
                info,
                raw_info: Some(raw),
                discovered_at: 1_700_000_000 + i as i64,
                metadata_at: 1_700_000_001 + i as i64,
                extra_flags,
                source,
            }])
            .unwrap();
    }
    let public_key = infos[0].primary_hash().0;
    let feed_claims = Claims {
        name: Some("Public Show S01".into()),
        files: vec![ClaimedFile {
            path: "e01.mkv".into(),
            size: None,
        }],
        ..Claims::default()
    };
    let private_claims = Claims {
        name: Some("Private Tracker Title".into()),
        ..Claims::default()
    };
    for (source, claims, private_source) in [
        (nyaa, &feed_claims, false),
        (prowlarr, &private_claims, true),
    ] {
        store
            .put_unverified(&[NewUnverified {
                network: Network::Bt,
                key: &public_key,
                source,
                private_source,
                claims: Some(claims),
                at: 1_700_000_100,
            }])
            .unwrap();
    }
    let ids: Vec<i64> = infos
        .iter()
        .map(|i| store.find(&i.primary_hash()).unwrap().unwrap())
        .collect();
    let saved_search = store
        .create_saved_search(
            &SavedSearchInput {
                name: "everything",
                query: "",
                ast: "{}",
                sort: None,
                alert: false,
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
            ids[1],
            &[SearchHits {
                saved_search_id: saved_search,
                torrent_ids: &ids,
            }],
            1_700_000_200,
        )
        .unwrap();
    let index = SearchIndex::in_memory().unwrap();
    let mut w = index.writer(15 << 20).unwrap();
    w.catch_up(&mut store, 10).unwrap();
    w.commit().unwrap();
    drop(store);

    let auth = Arc::new(Auth::load(&dir.path().join("auth.json"), false).unwrap());
    let app = Router::new().nest(
        "/api/v1",
        router(ApiState {
            auth: auth.clone(),
            index: Some(index.into()),
            reader: Arc::new(Mutex::new(
                Store::open_reader(&db, StoreConfig::default()).unwrap(),
            )),
            filters: None,
            hot: None,
            events: None,
            settings: None,
            ops: Some(Arc::new(FakeOps)),
            user_data: Some(Arc::new(Mutex::new(
                Store::open(&db, StoreConfig::default()).unwrap(),
            ))),
            adult: Default::default(),
            insights: None,
        }),
    );
    Fixture {
        app,
        auth,
        hashes: [
            hex(&infos[0].primary_hash().0),
            hex(&infos[1].primary_hash().0),
        ],
        saved_search,
        _dir: dir,
    }
}

struct Reply {
    status: StatusCode,
    text: String,
    cookie: Option<String>,
}

impl Reply {
    fn json(&self) -> Value {
        serde_json::from_str(&self.text).unwrap_or_else(|e| panic!("{e}: {}", self.text))
    }
}

async fn send(f: &Fixture, mut req: Request<Body>) -> Reply {
    req.extensions_mut().insert(ConnectInfo(fresh_ip()));
    let resp = f.app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let cookie = resp
        .headers()
        .get(header::SET_COOKIE)
        .map(|v| v.to_str().unwrap().split(';').next().unwrap().to_string());
    let bytes = axum::body::to_bytes(resp.into_body(), 1 << 22)
        .await
        .unwrap();
    Reply {
        status,
        text: String::from_utf8_lossy(&bytes).into_owned(),
        cookie,
    }
}

fn req(
    method: Method,
    uri: &str,
    headers: &[(header::HeaderName, &str)],
    body: Option<Value>,
) -> Request<Body> {
    let mut b = Request::builder().method(method).uri(uri);
    for (k, v) in headers {
        b = b.header(k, *v);
    }
    match body {
        Some(v) => b
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(v.to_string()))
            .unwrap(),
        None => b.body(Body::empty()).unwrap(),
    }
}

async fn signed_in(f: &Fixture) -> String {
    let token = f.auth.setup_token().unwrap();
    let r = send(
        f,
        req(
            Method::POST,
            "/api/v1/setup",
            &[],
            Some(json!({ "token": token, "password": PASSWORD })),
        ),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    r.cookie.unwrap()
}

/// A new key or feed token with every scope it can have.
async fn make_key(f: &Fixture, cookie: &str, kind: &str) -> String {
    let scopes = if kind == "api_key" {
        json!(["search", "torznab", "feeds"])
    } else {
        json!([])
    };
    let r = send(
        f,
        req(
            Method::POST,
            "/api/v1/keys",
            &[(header::COOKIE, cookie)],
            Some(json!({ "name": "integration", "kind": kind, "scopes": scopes, "password": PASSWORD })),
        ),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    r.json()["secret"].as_str().unwrap().to_string()
}

async fn get_as(f: &Fixture, uri: &str, cookie: &str) -> Value {
    let r = send(f, req(Method::GET, uri, &[(header::COOKIE, cookie)], None)).await;
    assert_eq!(r.status, StatusCode::OK, "{uri}: {}", r.text);
    r.json()
}

#[tokio::test]
async fn the_admin_sees_every_source_and_claim() {
    let f = fixture();
    let c = signed_in(&f).await;
    let d = get_as(&f, &format!("/api/v1/torrents/{}", f.hashes[0]), &c).await;
    let p = &d["provenance"];
    assert_eq!(p["verified"], true);
    assert_eq!(p["network"], "bt");
    assert_eq!(p["withheld"], false);
    let sources = p["sources"].as_array().unwrap();
    let names: Vec<&str> = sources
        .iter()
        .map(|s| s["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["Mainline DHT", "Nyaa", "Prowlarr"]);
    assert_eq!(sources[0]["first"], true);
    assert_eq!(sources[0]["first_seen_at"], 1_700_000_000);
    assert_eq!(sources[0]["claim"], Value::Null);
    let private: Vec<bool> = sources
        .iter()
        .map(|s| s["private"].as_bool().unwrap())
        .collect();
    assert_eq!(private, [false, false, true]);
    // Nyaa claimed one file of two and another name; no file list.
    assert_eq!(
        sources[1]["claim"],
        json!({
            "file_count": { "claimed": 1, "verified": 2 },
            "matches": false,
            "name": { "claimed": "Public Show S01", "verified": "Public.Show.S01.1080p" },
            "size": null
        })
    );
    assert_eq!(
        sources[2]["claim"]["name"]["claimed"],
        "Private Tracker Title"
    );

    // The private-source torrent: everything, flagged.
    let d = get_as(&f, &format!("/api/v1/torrents/{}", f.hashes[1]), &c).await;
    let sources = d["provenance"]["sources"].as_array().unwrap();
    assert_eq!(sources.len(), 1);
    assert_eq!(sources[0]["plugin"], "prowlarr");
    assert_eq!(sources[0]["private"], true);

    // Hits carry `verified` and the first source.
    let s = get_as(&f, "/api/v1/search?sort=%2Bdiscovered", &c).await;
    let hits = s["hits"].as_array().unwrap();
    assert_eq!(hits.len(), 2);
    for h in hits {
        assert_eq!(h["verified"], true);
    }
    assert_eq!(
        hits[0]["source"],
        json!({ "name": "Mainline DHT", "plugin": "dht" })
    );
    assert_eq!(
        hits[1]["source"],
        json!({ "name": "Prowlarr", "plugin": "prowlarr" })
    );

    // The facets: every source by name, both items verified on `bt`.
    let fs = get_as(&f, "/api/v1/facets", &c).await;
    let count = |facet: &str, v: &str| {
        fs["facets"][facet]["values"]
            .as_array()
            .unwrap()
            .iter()
            .find(|x| x["value"] == v)
            .map_or(0, |x| x["count"].as_u64().unwrap())
    };
    assert_eq!(count("source", "dht"), 1);
    assert_eq!(count("source", "nyaa"), 1);
    assert_eq!(count("source", "prowlarr"), 2);
    assert_eq!(count("verified", "yes"), 2);
    assert_eq!(count("verified", "no"), 0);
    assert_eq!(count("network", "bt"), 2);
    assert_eq!(fs["source_names"]["prowlarr"], "Prowlarr");
    // The search hides unverified items by default: the facet's own
    // selection.
    let yes = fs["facets"]["verified"]["values"][0].clone();
    assert_eq!(yes["value"], "yes");
    assert_eq!(yes["selected"], true);
}

#[tokio::test]
async fn keys_and_tokens_never_reach_provenance() {
    let f = fixture();
    let c = signed_in(&f).await;
    let key = make_key(&f, &c, "api_key").await;
    let token = make_key(&f, &c, "feed_token").await;
    let bearer = format!("Bearer {key}");
    for uri in [
        "/api/v1/search".to_string(),
        "/api/v1/facets".to_string(),
        format!("/api/v1/torrents/{}", f.hashes[0]),
        format!("/api/v1/torrents/{}", f.hashes[1]),
    ] {
        // These routes are the session's only (§12): a key, even with the
        // search scope, is refused before anything is read.
        for r in [
            send(
                &f,
                req(
                    Method::GET,
                    &uri,
                    &[(header::AUTHORIZATION, bearer.as_str())],
                    None,
                ),
            )
            .await,
            send(
                &f,
                req(Method::GET, &format!("{uri}?apikey={key}"), &[], None),
            )
            .await,
            send(
                &f,
                req(Method::GET, &format!("{uri}?token={token}"), &[], None),
            )
            .await,
        ] {
            assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{uri}: {}", r.text);
            for s in SECRET.iter().chain(&["nyaa", "provenance"]) {
                assert!(!r.text.contains(s), "{uri}: {s} in {}", r.text);
            }
        }
    }
}

#[tokio::test]
async fn a_feed_never_names_a_source() {
    let f = fixture();
    let c = signed_in(&f).await;
    let token = make_key(&f, &c, "feed_token").await;
    let r = send(
        &f,
        req(
            Method::GET,
            &format!("/api/v1/feeds/{}?token={token}", f.saved_search),
            &[],
            None,
        ),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    // The public torrent is there; the private-source one isn't (§18.7).
    assert!(r.text.contains("Public.Show.S01.1080p"), "{}", r.text);
    assert!(!r.text.contains("Members.Only"), "{}", r.text);
    for s in SECRET.iter().chain(&["nyaa", "Nyaa", "Mainline DHT"]) {
        assert!(!r.text.contains(s), "{s} in {}", r.text);
    }
}

#[tokio::test]
async fn an_export_carries_no_provenance() {
    let f = fixture();
    let c = signed_in(&f).await;
    let r = send(
        &f,
        req(
            Method::GET,
            "/api/v1/export?format=json",
            &[(header::COOKIE, c.as_str())],
            None,
        ),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    // One row: the private-source torrent is never exported (§18.7).
    let rows: Vec<Value> = r
        .text
        .lines()
        .map(|l| serde_json::from_str::<Value>(l).unwrap())
        .filter(|v| v.get("trailer").is_none())
        .collect();
    assert_eq!(rows.len(), 1, "{}", r.text);
    for key in ["source", "verified", "provenance"] {
        assert!(rows[0].get(key).is_none(), "{key} in {}", r.text);
    }
    for s in SECRET.iter().chain(&["nyaa", "Mainline DHT"]) {
        assert!(!r.text.contains(s), "{s} in {}", r.text);
    }
    // And it's not a field one can ask for.
    let r = send(
        &f,
        req(
            Method::GET,
            "/api/v1/export?format=json&fields=name,source",
            &[(header::COOKIE, c.as_str())],
            None,
        ),
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text);
}
