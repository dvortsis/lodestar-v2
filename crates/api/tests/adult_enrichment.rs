//! The detail view's adult-enrichment section and drawer interest
//! (ENR-7, architecture §20.9, plan 2026-11-04 §4.3): the section is in
//! `GET /torrents/{hash}` only while "Show adult content" is on, and the
//! key is absent otherwise; never for a hidden, held or private item or a
//! private claim; a link only on the matching source's own host; and an
//! opened drawer reaches the runtime only with the preference on, within
//! a per-client and an overall limit, on a clock the test controls.

use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Method, Request, StatusCode, header};
use axum::{Extension, Router};
use lodestar_api::ops::{
    BoxFuture, CrawlerRuntime, ExternalAddress, FetchStatus, NetworkCheck, OpsApi, OpsError,
    PortMapping, ReachReason, Source, SourcePatch, StatsResponse,
};
use lodestar_api::{AdultVisibility, ApiState, Auth, DrawerInterest, router};
use lodestar_bencode::{OwnedValue, to_vec};
use lodestar_classify::Correction;
use lodestar_metainfo::Info;
use lodestar_store::claims::{AdultEnrichment, AdultKind, Claims, Provenance, ReleaseDate};
use lodestar_store::{NewTorrent, SourceId, Store, StoreConfig, flags};
use serde_json::{Value, json};
use tokio::sync::watch;
use tower::ServiceExt;

const PASSWORD: &str = "correct horse battery";
const STASH_SCENES: &str = "https://stashdb.org/scenes/";

/// The pipeline, as far as the detail view asks: its plugins' names,
/// classes and hosts.
#[derive(Debug)]
struct FakeOps;

fn source(id: u8, plugin: &str, name: &str, consent: &str, kind: &str, hosts: &[&str]) -> Source {
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
        "endpoints": hosts,
        "health": { "harvested_24h": 0, "last_error": null, "last_run": null,
            "running": true, "stored_24h": 0 },
        "id": id,
        "kinds": [kind],
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
                source(0, "dht", "Mainline DHT", "none", "discovery", &[]),
                source(
                    1,
                    "stashdb",
                    "StashDB",
                    "account",
                    "enrichment",
                    &["https://stashdb.org"],
                ),
                source(2, "prowlarr", "Prowlarr", "account", "discovery", &[]),
                source(
                    3,
                    "tpdb",
                    "ThePornDB",
                    "account",
                    "enrichment",
                    &["https://api.theporndb.net"],
                ),
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

fn raw(name: &str) -> Vec<u8> {
    let d = |e: Vec<(&str, OwnedValue)>| {
        OwnedValue::Dict(
            e.into_iter()
                .map(|(k, v)| (k.as_bytes().to_vec(), v))
                .collect(),
        )
    };
    to_vec(&d(vec![
        ("length", OwnedValue::Int(1 << 30)),
        ("name", OwnedValue::Bytes(name.as_bytes().to_vec())),
        ("piece length", OwnedValue::Int(1 << 16)),
        ("pieces", OwnedValue::Bytes(vec![0; 20])),
    ]))
}

/// The torrents, by index into `Fixture::hashes`.
mod item {
    /// Adult, StashDB's match with its own link.
    pub const MATCHED: usize = 0;
    /// Adult, StashDB's match linking to another host.
    pub const FOREIGN_LINK: usize = 1;
    /// Adult, the only match came through a private source.
    pub const PRIVATE_CLAIM: usize = 2;
    /// Adult and matched, but hidden by a filter rule.
    pub const HIDDEN: usize = 3;
    /// Adult and matched, but its swarm hash is quarantined.
    pub const HELD: usize = 4;
    /// Adult, no match yet.
    pub const UNMATCHED: usize = 5;
    /// Not adult.
    pub const PLAIN: usize = 6;
    /// Adult, two matches: ThePornDB's (source 3) names a kind,
    /// StashDB's (source 1) doesn't.
    pub const TWO: usize = 7;
    /// Adult and matched, from a private-source discovery.
    pub const PRIVATE_ITEM: usize = 8;
    /// Adult and matched, a BEP 27 private torrent.
    pub const BEP27: usize = 9;
}

const NAMES: [&str; 10] = [
    "Garden.Scene.XXX.1080p.mp4",
    "Foreign.Link.XXX.1080p.mp4",
    "Private.Claim.XXX.1080p.mp4",
    "Hidden.Scene.XXX.1080p.mp4",
    "Held.Scene.XXX.1080p.mp4",
    "Unmatched.Scene.XXX.1080p.mp4",
    "Ordinary.Movie.2019.1080p.mkv",
    "Two.Matches.XXX.1080p.mp4",
    "Private.Item.XXX.1080p.mp4",
    "Bep27.Item.XXX.1080p.mp4",
];

/// The drawer, as a test sees it.
#[derive(Clone, Default)]
struct Opened(Arc<Mutex<Vec<i64>>>);

impl Opened {
    fn take(&self) -> Vec<i64> {
        std::mem::take(&mut *self.0.lock().unwrap())
    }
}

/// A clock the test moves.
#[derive(Clone)]
struct Clock {
    base: Instant,
    secs: Arc<AtomicU64>,
}

impl Clock {
    fn new() -> Clock {
        Clock {
            base: Instant::now(),
            secs: Arc::default(),
        }
    }

    fn now(&self) -> Instant {
        self.base + Duration::from_secs(self.secs.load(Ordering::SeqCst))
    }

    fn advance(&self, d: Duration) {
        self.secs.fetch_add(d.as_secs(), Ordering::SeqCst);
    }
}

struct Fixture {
    app: Router,
    auth: Arc<Auth>,
    show: watch::Sender<bool>,
    hashes: Vec<String>,
    ids: Vec<i64>,
    opened: Opened,
    clock: Clock,
    _dir: tempfile::TempDir,
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn section(scene: &str) -> AdultEnrichment {
    AdultEnrichment {
        kind: None,
        title: Some("Garden Scene".into()),
        studio: Some("Some Studio".into()),
        released: ReleaseDate::new(2021, Some(6), None),
        performers: vec!["Performer One".into(), "Performer Two".into()],
        performers_truncated: true,
        ..AdultEnrichment::new(scene)
    }
}

fn claims_of(section: AdultEnrichment, link: Option<String>) -> Claims {
    Claims {
        adult: Some(section),
        provenance: Provenance {
            external_ref: link,
            ..Provenance::default()
        },
        ..Claims::default()
    }
}

/// `interest`: whether the host serves drawer interest at all.
fn fixture(interest: bool) -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("t.db");
    let mut store = Store::open(&db, StoreConfig::default()).unwrap();
    let dht = store.register_source("dht", "1", 1_700_000_000).unwrap();
    let stash = store
        .register_source("stashdb", "1", 1_700_000_000)
        .unwrap();
    let prowlarr = store
        .register_source("prowlarr", "1", 1_700_000_000)
        .unwrap();
    let tpdb = store.register_source("tpdb", "1", 1_700_000_000).unwrap();
    assert_eq!(
        [dht, stash, prowlarr, tpdb].map(SourceId::get),
        [0, 1, 2, 3]
    );
    let raws: Vec<Vec<u8>> = NAMES.iter().map(|n| raw(n)).collect();
    let infos: Vec<Info> = raws.iter().map(|r| Info::parse(r).unwrap()).collect();
    for (i, (info, raw)) in infos.iter().zip(&raws).enumerate() {
        let (source, extra_flags) = match i {
            item::PRIVATE_ITEM => (prowlarr, flags::PRIVATE_SOURCE),
            _ => (dht, 0),
        };
        store
            .insert(&[NewTorrent {
                info,
                raw_info: Some(raw),
                discovered_at: 1_700_000_000 + i as i64,
                metadata_at: 1_700_000_100,
                extra_flags,
                source,
            }])
            .unwrap();
    }
    let ids: Vec<i64> = infos
        .iter()
        .map(|i| store.find(&i.primary_hash()).unwrap().unwrap())
        .collect();
    // Classified, as the pipeline would have: everything but PLAIN adult.
    for (i, id) in ids.iter().enumerate() {
        let row = store.torrent(*id).unwrap().unwrap();
        let tree = store.files(*id).unwrap().unwrap();
        let files = lodestar_classify::file_infos(&tree.files, tree.encoding);
        let fix = Correction {
            adult: Some(i != item::PLAIN),
            not_junk: false,
        };
        let (e, _) = lodestar_filter::reclassify::enrich_corrected(&row.name, &files, fix);
        store.set_enrichments(&[(*id, e)]).unwrap();
    }
    let own_link = Some(format!("{STASH_SCENES}scene-1"));
    for (i, src, claims, private) in [
        (
            item::MATCHED,
            stash,
            claims_of(section("scene-1"), own_link.clone()),
            false,
        ),
        (
            item::FOREIGN_LINK,
            stash,
            claims_of(
                section("scene-2"),
                Some("https://evil.example/scenes/scene-2".into()),
            ),
            false,
        ),
        (
            item::PRIVATE_CLAIM,
            prowlarr,
            claims_of(section("scene-3"), own_link.clone()),
            true,
        ),
        (
            item::HIDDEN,
            stash,
            claims_of(section("scene-4"), own_link.clone()),
            false,
        ),
        (
            item::HELD,
            stash,
            claims_of(section("scene-5"), own_link.clone()),
            false,
        ),
        (
            item::TWO,
            stash,
            claims_of(section("scene-6"), own_link.clone()),
            false,
        ),
        (
            item::TWO,
            tpdb,
            claims_of(
                AdultEnrichment {
                    kind: Some(AdultKind::Movie),
                    title: Some("The Movie".into()),
                    ..AdultEnrichment::new("movie-7")
                },
                Some("https://api.theporndb.net/movies/movie-7".into()),
            ),
            false,
        ),
        (
            item::PRIVATE_ITEM,
            stash,
            claims_of(section("scene-8"), own_link.clone()),
            false,
        ),
        (
            item::BEP27,
            stash,
            claims_of(section("scene-9"), own_link.clone()),
            false,
        ),
    ] {
        assert!(store.put_claims(ids[i], src, &claims, private).unwrap());
    }
    let conn = store.connection();
    for (i, flag) in [(item::HIDDEN, flags::HIDDEN), (item::BEP27, flags::PRIVATE)] {
        conn.execute(
            "UPDATE torrents SET flags = flags | ?1 WHERE id = ?2",
            (flag, ids[i]),
        )
        .unwrap();
    }
    conn.execute(
        "INSERT INTO quarantine (hash, rule, sealed, at, expires_at) VALUES (?1, 'CS-2a', NULL, 0, 9)",
        [&infos[item::HELD].primary_hash().0[..]],
    )
    .unwrap();
    drop(store);

    let (show, rx) = watch::channel(false);
    let auth = Arc::new(Auth::load(&dir.path().join("auth.json"), false).unwrap());
    let opened = Opened::default();
    let clock = Clock::new();
    let mut app = Router::new().nest(
        "/api/v1",
        router(ApiState {
            auth: auth.clone(),
            index: None,
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
            adult: AdultVisibility::follow(move || *rx.borrow()),
            insights: None,
        }),
    );
    if interest {
        let sink = opened.clone();
        let c = clock.clone();
        app = app.layer(Extension(DrawerInterest::with_clock(
            0,
            move |id| {
                sink.0.lock().unwrap().push(id);
                true
            },
            move || c.now(),
        )));
    }
    Fixture {
        app,
        auth,
        show,
        hashes: infos.iter().map(|i| hex(&i.primary_hash().0)).collect(),
        ids,
        opened,
        clock,
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

async fn send(f: &Fixture, mut req: Request<Body>, from: IpAddr) -> Reply {
    req.extensions_mut()
        .insert(ConnectInfo(SocketAddr::new(from, 40000)));
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

fn ip(n: u8) -> IpAddr {
    IpAddr::from([10, 70, 0, n])
}

fn req(method: Method, uri: &str, headers: &[(&str, &str)], body: Option<Value>) -> Request<Body> {
    let mut b = Request::builder().method(method).uri(uri);
    for (k, v) in headers {
        b = b.header(*k, *v);
    }
    match body {
        Some(v) => b
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(v.to_string()))
            .unwrap(),
        None => b.body(Body::empty()).unwrap(),
    }
}

async fn session(f: &Fixture) -> String {
    let token = f.auth.setup_token().unwrap();
    let body = json!({ "token": token, "password": PASSWORD,
        "notice_version": lodestar_api::legal::NOTICE_VERSION });
    let r = send(
        f,
        req(Method::POST, "/api/v1/setup", &[], Some(body)),
        ip(250),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    r.cookie.unwrap()
}

/// `GET /torrents/{hash}` for item `i`, as the admin from `from`.
async fn detail(f: &Fixture, cookie: &str, i: usize, from: IpAddr) -> Reply {
    let uri = format!("/api/v1/torrents/{}", f.hashes[i]);
    send(f, req(Method::GET, &uri, &[("cookie", cookie)], None), from).await
}

/// The detail of item `i`, which must be a 200.
async fn ok(f: &Fixture, cookie: &str, i: usize, from: IpAddr) -> Value {
    let r = detail(f, cookie, i, from).await;
    assert_eq!(r.status, StatusCode::OK, "{i}: {}", r.text);
    r.json()
}

/// The key is missing, not null and not empty.
fn absent(body: &Value) -> bool {
    body.as_object()
        .is_some_and(|o| !o.contains_key("adult_enrichment"))
}

#[tokio::test]
async fn the_section_is_there_only_with_show_adult_content_on() {
    let f = fixture(true);
    let cookie = session(&f).await;
    // Off: adult torrents are 404 `adult_hidden`, with no section, and the
    // ordinary one has no key.
    for i in [item::MATCHED, item::TWO] {
        let r = detail(&f, &cookie, i, ip(1)).await;
        assert_eq!(r.status, StatusCode::NOT_FOUND, "{}", r.text);
        assert_eq!(r.json()["error"]["code"], "adult_hidden");
        assert!(!r.text.contains("adult_enrichment"), "{}", r.text);
        assert!(!r.text.contains("Garden Scene"), "{}", r.text);
    }
    assert!(absent(&ok(&f, &cookie, item::PLAIN, ip(1)).await));

    f.show.send(true).unwrap();
    let body = ok(&f, &cookie, item::MATCHED, ip(1)).await;
    assert_eq!(
        body["adult_enrichment"],
        json!({
            "link": format!("{STASH_SCENES}scene-1"),
            "performers": ["Performer One", "Performer Two"],
            "performers_truncated": true,
            "released": "2021-06",
            "scene_title": "Garden Scene",
            "source": { "id": 1, "name": "StashDB", "plugin": "stashdb" },
            "studio": "Some Studio"
        })
    );
    // Only what ENR-0 stores: no scene id, no kind, no info hash.
    let text = body["adult_enrichment"].to_string();
    assert!(!text.contains(&f.hashes[item::MATCHED]));
    assert!(absent(&ok(&f, &cookie, item::UNMATCHED, ip(1)).await));
    assert!(absent(&ok(&f, &cookie, item::PLAIN, ip(1)).await));

    // Off again: the next request follows it.
    f.show.send(false).unwrap();
    let r = detail(&f, &cookie, item::MATCHED, ip(1)).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    assert!(!r.text.contains("adult_enrichment"));
}

#[tokio::test]
async fn hidden_held_and_private_items_and_private_claims_get_no_section() {
    let f = fixture(true);
    let cookie = session(&f).await;
    f.show.send(true).unwrap();
    for i in [
        item::PRIVATE_CLAIM,
        item::HIDDEN,
        item::HELD,
        item::PRIVATE_ITEM,
    ] {
        let body = ok(&f, &cookie, i, ip(2)).await;
        assert!(absent(&body), "{i}: {body}");
        assert!(!body.to_string().contains("Garden Scene"), "{i}: {body}");
    }
    // The held item is still shown as today: the detail itself is there.
    assert_eq!(
        ok(&f, &cookie, item::HELD, ip(2)).await["name"],
        NAMES[item::HELD]
    );
    // A BEP 27 private torrent's match is shown (it isn't looked up again,
    // below), since nothing holds it.
    assert!(!absent(&ok(&f, &cookie, item::BEP27, ip(2)).await));
}

#[tokio::test]
async fn a_link_on_another_host_is_dropped_and_the_rest_kept() {
    let f = fixture(true);
    let cookie = session(&f).await;
    f.show.send(true).unwrap();
    let body = ok(&f, &cookie, item::FOREIGN_LINK, ip(3)).await;
    let s = &body["adult_enrichment"];
    assert_eq!(s["link"], Value::Null, "{s}");
    assert_eq!(s["scene_title"], "Garden Scene");
    assert_eq!(s["source"]["plugin"], "stashdb");
    assert!(!body.to_string().contains("evil.example"));
}

#[tokio::test]
async fn two_matches_show_the_one_that_pins_the_kind() {
    let f = fixture(true);
    let cookie = session(&f).await;
    f.show.send(true).unwrap();
    let s = ok(&f, &cookie, item::TWO, ip(4)).await["adult_enrichment"].clone();
    // ENR-1's rule: the first by source id that names a kind, so
    // ThePornDB's (source 3) over StashDB's kindless one (source 1).
    assert_eq!(s["source"]["plugin"], "tpdb", "{s}");
    assert_eq!(s["scene_title"], "The Movie");
    assert_eq!(s["link"], "https://api.theporndb.net/movies/movie-7");
    assert_eq!(s["performers"], json!([]));
    assert_eq!(s["studio"], Value::Null);
    assert_eq!(s["released"], Value::Null);
}

#[tokio::test]
async fn a_public_caller_gets_nothing() {
    let f = fixture(true);
    let cookie = session(&f).await;
    f.show.send(true).unwrap();
    let r = send(
        &f,
        req(
            Method::POST,
            "/api/v1/keys",
            &[("cookie", &cookie)],
            Some(json!({ "name": "Prowlarr", "kind": "api_key",
                "scopes": ["search", "torznab", "feeds"], "password": PASSWORD })),
        ),
        ip(5),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    let key = r.json()["secret"].as_str().unwrap().to_string();
    let uri = format!("/api/v1/torrents/{}", f.hashes[item::MATCHED]);
    for headers in [
        vec![("x-api-key", key.as_str())],
        vec![("authorization", &*format!("Bearer {key}"))],
        vec![],
    ] {
        let r = send(&f, req(Method::GET, &uri, &headers, None), ip(5)).await;
        assert_ne!(r.status, StatusCode::OK, "{}", r.text);
        assert!(!r.text.contains("adult_enrichment"), "{}", r.text);
        assert!(!r.text.contains("Garden Scene"), "{}", r.text);
    }
    assert!(f.opened.take().is_empty());
}

#[tokio::test]
async fn interest_is_offered_only_with_show_adult_content_on() {
    let f = fixture(true);
    let cookie = session(&f).await;
    // Off: nothing, whatever is opened.
    for i in 0..NAMES.len() {
        let _ = detail(&f, &cookie, i, ip(6)).await;
    }
    assert!(f.opened.take().is_empty());

    f.show.send(true).unwrap();
    for i in 0..NAMES.len() {
        let _ = detail(&f, &cookie, i, ip(6)).await;
    }
    // Only adult items the gate could pass: not hidden, held, private,
    // BEP 27 private, or not adult. With or without a match.
    let want: Vec<i64> = [
        item::MATCHED,
        item::FOREIGN_LINK,
        item::PRIVATE_CLAIM,
        item::UNMATCHED,
        item::TWO,
    ]
    .iter()
    .map(|&i| f.ids[i])
    .collect();
    assert_eq!(f.opened.take(), want);

    // Without interest wired, the detail is the same.
    let bare = fixture(false);
    let bare_cookie = session(&bare).await;
    bare.show.send(true).unwrap();
    let a = ok(&f, &cookie, item::MATCHED, ip(7)).await;
    let b = ok(&bare, &bare_cookie, item::MATCHED, ip(7)).await;
    assert_eq!(a["adult_enrichment"], b["adult_enrichment"]);
}

#[tokio::test]
async fn one_client_is_limited_and_refusals_change_nothing() {
    let f = fixture(true);
    let cookie = session(&f).await;
    f.show.send(true).unwrap();
    let per = DrawerInterest::PER_CLIENT;
    let mut bodies = Vec::new();
    for _ in 0..per.burst + 5 {
        bodies.push(detail(&f, &cookie, item::UNMATCHED, ip(8)).await.text);
    }
    assert_eq!(f.opened.take().len(), per.burst as usize);
    // Taken and refused answers are the same bytes.
    assert!(bodies.windows(2).all(|w| w[0] == w[1]));

    // One back per `every`, not before.
    f.clock.advance(per.every - Duration::from_secs(1));
    let _ = detail(&f, &cookie, item::UNMATCHED, ip(8)).await;
    assert!(f.opened.take().is_empty());
    f.clock.advance(Duration::from_secs(1));
    for _ in 0..3 {
        let _ = detail(&f, &cookie, item::UNMATCHED, ip(8)).await;
    }
    assert_eq!(f.opened.take().len(), 1);

    // Another client isn't held back by this one.
    let _ = detail(&f, &cookie, item::UNMATCHED, ip(9)).await;
    assert_eq!(f.opened.take().len(), 1);

    // Addresses in one IPv6 /64 count as one client.
    let v6 = |n: u16| IpAddr::from([0x2001, 0xdb8, 0, 1, 0, 0, 0, n]);
    for n in 0..per.burst as u16 + 4 {
        let _ = detail(&f, &cookie, item::UNMATCHED, v6(n)).await;
    }
    assert_eq!(f.opened.take().len(), per.burst as usize);
}

#[tokio::test]
async fn every_client_together_is_limited() {
    let f = fixture(true);
    let cookie = session(&f).await;
    f.show.send(true).unwrap();
    let per = DrawerInterest::PER_CLIENT;
    let overall = DrawerInterest::OVERALL;
    let clients = overall.burst.div_ceil(per.burst) as u8 + 2;
    for c in 0..clients {
        for _ in 0..per.burst {
            let _ = detail(&f, &cookie, item::UNMATCHED, ip(100 + c)).await;
        }
    }
    assert_eq!(f.opened.take().len(), overall.burst as usize);

    // A fresh client still waits for the overall bucket.
    let _ = detail(&f, &cookie, item::UNMATCHED, ip(200)).await;
    assert!(f.opened.take().is_empty());
    f.clock.advance(overall.every);
    for c in 0..3 {
        let _ = detail(&f, &cookie, item::UNMATCHED, ip(210 + c)).await;
    }
    assert_eq!(f.opened.take().len(), 1);
}
