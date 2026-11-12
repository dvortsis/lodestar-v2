//! The detail view's "Identified as" section (ENR-12, architecture §6,
//! §10.5): each general enrichment plugin's stored answer (ENR-10), surest
//! first, with the one that names the title marked; absent when there's
//! none, and for a hidden, held, private or Adult torrent; never from a
//! source that found the torrent; every list and text within its cap;
//! `resolved_via` only from the listed methods.

use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use lodestar_api::ops::{
    BoxFuture, CrawlerRuntime, ExternalAddress, FetchStatus, NetworkCheck, OpsApi, OpsError,
    PortMapping, ReachReason, Source, SourcePatch, StatsResponse,
};
use lodestar_api::{AdultVisibility, ApiState, Auth, openapi, router};
use lodestar_bencode::{OwnedValue, to_vec};
use lodestar_classify::Correction;
use lodestar_metainfo::Info;
use lodestar_store::claims::{AltTitle, Claims, Details, ExternalId, MAX_ALT_TITLES, MAX_GENRES};
use lodestar_store::{NewTorrent, SourceId, Store, StoreConfig, flags};
use serde_json::{Value, json};
use tokio::sync::watch;
use tower::ServiceExt;

const PASSWORD: &str = "correct horse battery";

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
        "finds_private": false,
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
            // Every find of `adultsite` is Adult, as its manifest says.
            let mut adult = source(5, "adultsite", "Adult Site", "external", "discovery", &[]);
            adult.always_adult = true;
            Ok(vec![
                adult,
                source(0, "dht", "Mainline DHT", "none", "discovery", &[]),
                source(
                    1,
                    "wikidata",
                    "Wikidata",
                    "external",
                    "enrichment",
                    &["https://www.wikidata.org"],
                ),
                source(
                    2,
                    "tmdb",
                    "TMDB",
                    "external",
                    "enrichment",
                    &["https://api.themoviedb.org"],
                ),
                source(3, "nyaa", "Nyaa", "external", "discovery", &[]),
                source(
                    4,
                    "acctenr",
                    "Account Enricher",
                    "account",
                    "enrichment",
                    &[],
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

/// A multi-file info dict named `name` with one large video `file` in
/// it and a small text file.
fn with_files(name: &str, file: &str) -> Vec<u8> {
    let d = |e: Vec<(&str, OwnedValue)>| {
        OwnedValue::Dict(
            e.into_iter()
                .map(|(k, v)| (k.as_bytes().to_vec(), v))
                .collect(),
        )
    };
    let f = |p: &str, len: i64| {
        d(vec![
            ("length", OwnedValue::Int(len)),
            (
                "path",
                OwnedValue::List(vec![OwnedValue::Bytes(p.as_bytes().to_vec())]),
            ),
        ])
    };
    to_vec(&d(vec![
        (
            "files",
            OwnedValue::List(vec![f(file, 1 << 30), f("info.txt", 100)]),
        ),
        ("name", OwnedValue::Bytes(name.as_bytes().to_vec())),
        ("piece length", OwnedValue::Int(1 << 16)),
        ("pieces", OwnedValue::Bytes(vec![0; 20])),
    ]))
}

/// The torrents, by index into `Fixture::hashes`.
mod item {
    /// A film two plugins answered for: TMDB surer than Wikidata.
    pub const TWO: usize = 0;
    /// Nothing answered.
    pub const NONE: usize = 1;
    /// Answered, but found only through a private source.
    pub const PRIVATE: usize = 2;
    /// Answered, then classified Adult.
    pub const ADULT: usize = 3;
    /// Answered, but hidden by a filter rule.
    pub const HIDDEN: usize = 4;
    /// Answered, but its swarm hash is quarantined.
    pub const HELD: usize = 5;
    /// A feed that found it claims a canonical title: not an answer.
    pub const FINDER: usize = 6;
    /// One answer at every list's cap.
    pub const FULL: usize = 7;
    /// An ambiguous name whose record is resolved from its files.
    pub const RESOLVED: usize = 8;
    /// A BEP 27 private torrent with an answer.
    pub const BEP27: usize = 9;
    /// Answered by an account-class enricher only.
    pub const ACCOUNT: usize = 10;
    /// An ambiguous name over a show's episode, resolved via the plugin.
    pub const PLUGIN: usize = 11;
    /// Found by the always-adult source, answered, and not reclassified
    /// Adult yet (the window before `adult_by_source`).
    pub const ADULT_FINDER: usize = 12;
    /// Found by the always-adult source, answered, then marked Not adult.
    pub const NOT_ADULT_FINDER: usize = 13;
    /// Found by a registered source this build doesn't run.
    pub const GHOST: usize = 14;
}

const NAMES: [&str; 15] = [
    "Some.Film.1994.1080p.BluRay.x264",
    "Other.Film.2001.1080p.BluRay.x264",
    "Private.Film.2002.1080p.BluRay.x264",
    "Adult.Film.2003.1080p.BluRay.x264",
    "Hidden.Film.2004.1080p.BluRay.x264",
    "Held.Film.2005.1080p.BluRay.x264",
    "Feed.Film.2006.1080p.BluRay.x264",
    "Full.Film.2007.1080p.BluRay.x264",
    "New Folder",
    "Bep27.Film.2008.1080p.BluRay.x264",
    "Account.Film.2009.1080p.BluRay.x264",
    "New Folder (2)",
    "Finder.Film.2010.1080p.BluRay.x264",
    "NotAdult.Film.2011.1080p.BluRay.x264",
    "Ghost.Film.2012.1080p.BluRay.x264",
];

struct Fixture {
    app: Router,
    auth: Arc<Auth>,
    show: watch::Sender<bool>,
    hashes: Vec<String>,
    _dir: tempfile::TempDir,
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn answer(canonical: &str, confidence: u8) -> Claims {
    Claims {
        ids: vec![
            ExternalId {
                scheme: "imdb".into(),
                value: "tt0111161".into(),
            },
            ExternalId {
                scheme: "wikidata".into(),
                value: "Q172241".into(),
            },
        ],
        details: Some(Box::new(Details {
            canonical_title: Some(canonical.into()),
            alt_titles: vec![AltTitle {
                language: "ja".into(),
                title: "某映画".into(),
            }],
            genres: vec!["drama".into()],
            year: Some(1994),
            match_confidence: Some(confidence),
            ..Details::default()
        })),
        ..Claims::default()
    }
}

fn full() -> Claims {
    let mut c = answer("Full Film", 70);
    let d = c.details.as_mut().unwrap();
    d.alt_titles = (0..MAX_ALT_TITLES)
        .map(|i| AltTitle {
            language: "en".into(),
            title: format!("Title {i}"),
        })
        .collect();
    d.genres = (0..MAX_GENRES).map(|i| format!("genre {i}")).collect();
    c
}

/// `interest`: whether the host says which sources are always adult
/// through `DrawerInterest` too, as well as through its manifests.
fn fixture_with(interest: bool) -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("t.db");
    let mut store = Store::open(&db, StoreConfig::default()).unwrap();
    let ids_of: Vec<SourceId> = [
        "dht",
        "wikidata",
        "tmdb",
        "nyaa",
        "acctenr",
        "adultsite",
        "ghost",
    ]
    .iter()
    .map(|p| store.register_source(p, "1", 1_700_000_000).unwrap())
    .collect();
    let [dht, wikidata, tmdb, nyaa, acct, adultsite, ghost] = ids_of[..] else {
        panic!()
    };
    assert_eq!(
        [dht, wikidata, tmdb, nyaa, acct].map(SourceId::get),
        [0, 1, 2, 3, 4]
    );
    let raws: Vec<Vec<u8>> = NAMES
        .iter()
        .enumerate()
        .map(|(i, n)| match i {
            item::RESOLVED | item::PLUGIN => with_files(n, "Some.Show.S01E02.1080p.WEB-DL.mkv"),
            _ => raw(n),
        })
        .collect();
    let infos: Vec<Info> = raws.iter().map(|r| Info::parse(r).unwrap()).collect();
    for (i, (info, raw)) in infos.iter().zip(&raws).enumerate() {
        let (source, extra_flags) = match i {
            item::PRIVATE => (dht, flags::PRIVATE_SOURCE),
            item::FINDER => (nyaa, 0),
            item::ADULT_FINDER | item::NOT_ADULT_FINDER => (adultsite, 0),
            item::GHOST => (ghost, 0),
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
    for (i, id) in ids.iter().enumerate() {
        let row = store.torrent(*id).unwrap().unwrap();
        let tree = store.files(*id).unwrap().unwrap();
        let files = lodestar_classify::file_infos(&tree.files, tree.encoding);
        let fix = Correction {
            adult: (i == item::ADULT).then_some(true),
            not_junk: false,
        };
        let (e, _) = lodestar_filter::reclassify::enrich_corrected(&row.name, &files, fix);
        store.set_enrichments(&[(*id, e)]).unwrap();
    }
    for (i, src, claims) in [
        (item::TWO, wikidata, answer("The Film", 80)),
        (item::TWO, tmdb, answer("The Film (TMDB)", 92)),
        (item::PRIVATE, wikidata, answer("Private Answer", 90)),
        (item::ADULT, wikidata, answer("Adult Answer", 90)),
        (item::HIDDEN, wikidata, answer("Hidden Answer", 90)),
        (item::HELD, wikidata, answer("Held Answer", 90)),
        (item::FINDER, nyaa, answer("Feed Answer", 90)),
        (item::FULL, wikidata, full()),
        (item::RESOLVED, tmdb, answer("Resolved Answer", 60)),
        (item::BEP27, wikidata, answer("Bep27 Answer", 90)),
        (item::ACCOUNT, acct, answer("Account Answer", 90)),
        (item::PLUGIN, wikidata, answer("Plugin Answer", 90)),
        (item::PLUGIN, tmdb, answer("Weaker Answer", 50)),
        (item::ADULT_FINDER, wikidata, answer("Finder Answer", 90)),
        (
            item::NOT_ADULT_FINDER,
            wikidata,
            answer("NotAdult Answer", 90),
        ),
        (item::GHOST, wikidata, answer("Ghost Answer", 90)),
    ] {
        assert!(store.put_claims(ids[i], src, &claims, false).unwrap());
    }
    assert!(
        store
            .set_correction(
                ids[item::NOT_ADULT_FINDER],
                lodestar_store::ClassCorrection {
                    adult: Some(false),
                    not_junk: false,
                },
            )
            .unwrap()
    );
    // The backfill resolves the ambiguous name with the surest answer, as
    // ENR-10 does.
    {
        let id = ids[item::PLUGIN];
        let row = store.torrent(id).unwrap().unwrap();
        let tree = store.files(id).unwrap().unwrap();
        let files = lodestar_classify::file_infos(&tree.files, tree.encoding);
        let class = lodestar_classify::classify(&row.name, &files);
        let before = store.enrichment(id).unwrap().unwrap();
        let after =
            lodestar_pipeline::identify::with_identified(&store, &row, &class, before).unwrap();
        let rec = lodestar_classify::Record::decode(&after.data).unwrap();
        assert_eq!(rec.resolved, Some(lodestar_classify::ResolvedVia::Plugin));
        store.set_enrichments(&[(id, after)]).unwrap();
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
        // The same mask the manifests give: `adultsite` (source 5).
        app = app.layer(axum::Extension(lodestar_api::DrawerInterest::new(
            1 << 5,
            |_| true,
        )));
    }
    Fixture {
        app,
        auth,
        show,
        hashes: infos.iter().map(|i| hex(&i.primary_hash().0)).collect(),
        _dir: dir,
    }
}

fn fixture() -> Fixture {
    fixture_with(true)
}

struct Reply {
    status: StatusCode,
    text: String,
    cookie: Option<String>,
}

async fn send(f: &Fixture, req: Request<Body>) -> Reply {
    let mut req = req;
    req.extensions_mut()
        .insert(axum::extract::ConnectInfo(std::net::SocketAddr::from((
            [10, 0, 0, 1],
            4000,
        ))));
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

async fn session(f: &Fixture) -> String {
    let token = f.auth.setup_token().unwrap();
    let body = json!({ "token": token, "password": PASSWORD,
        "notice_version": lodestar_api::legal::NOTICE_VERSION });
    let r = send(
        f,
        Request::builder()
            .method(Method::POST)
            .uri("/api/v1/setup")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body.to_string()))
            .unwrap(),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    r.cookie.unwrap()
}

async fn detail(f: &Fixture, cookie: &str, i: usize) -> Reply {
    let uri = format!("/api/v1/torrents/{}", f.hashes[i]);
    send(
        f,
        Request::builder()
            .uri(uri)
            .header("cookie", cookie)
            .body(Body::empty())
            .unwrap(),
    )
    .await
}

async fn ok(f: &Fixture, cookie: &str, i: usize) -> Value {
    let r = detail(f, cookie, i).await;
    assert_eq!(r.status, StatusCode::OK, "{i}: {}", r.text);
    serde_json::from_str(&r.text).unwrap()
}

/// The key is missing, not null and not empty.
fn absent(body: &Value) -> bool {
    body.as_object()
        .is_some_and(|o| !o.contains_key("identified_as"))
}

#[tokio::test]
async fn each_plugins_answer_is_an_alternative_surest_first() {
    let f = fixture();
    let c = session(&f).await;
    let body = ok(&f, &c, item::TWO).await;
    let s = &body["identified_as"];
    assert_eq!(s["resolved_via"], Value::Null);
    let alts = s["alternatives"].as_array().unwrap();
    assert_eq!(alts.len(), 2);
    assert_eq!(alts[0]["source"]["plugin"], "tmdb");
    assert_eq!(alts[0]["confidence"], 92);
    // An informative name keeps its own title: no answer is chosen.
    assert_eq!(alts[0]["chosen"], false);
    assert_eq!(alts[0]["canonical_title"], "The Film (TMDB)");
    assert_eq!(alts[1]["source"]["plugin"], "wikidata");
    assert_eq!(alts[1]["chosen"], false);
    assert_eq!(
        alts[1],
        json!({
            "alt_titles": [{"language": "ja", "title": "某映画"}],
            "canonical_title": "The Film",
            "chosen": false,
            "confidence": 80,
            "genres": ["drama"],
            "ids": [{"scheme": "imdb", "value": "tt0111161"}, {"scheme": "wikidata", "value": "Q172241"}],
            "source": {"id": 1, "name": "Wikidata", "plugin": "wikidata"},
            "year": 1994
        })
    );
    // Stable: the same order every time.
    for _ in 0..3 {
        assert_eq!(ok(&f, &c, item::TWO).await["identified_as"], *s);
    }
    // The admin sees an account-class enricher's answer too.
    let body = ok(&f, &c, item::ACCOUNT).await;
    assert_eq!(
        body["identified_as"]["alternatives"][0]["source"]["plugin"],
        "acctenr"
    );
}

#[tokio::test]
async fn the_section_is_absent_where_it_must_not_show() {
    let f = fixture();
    let c = session(&f).await;
    for i in [
        item::NONE,
        item::PRIVATE,
        item::HIDDEN,
        item::HELD,
        item::FINDER,
        item::BEP27,
    ] {
        let r = detail(&f, &c, i).await;
        assert_eq!(r.status, StatusCode::OK, "{i}: {}", r.text);
        let body: Value = serde_json::from_str(&r.text).unwrap();
        assert!(absent(&body), "{i}: {body}");
        assert!(!r.text.contains("Answer"), "{i}: {}", r.text);
    }
    // A private-source torrent names no enricher anywhere.
    let r = detail(&f, &c, item::PRIVATE).await;
    assert!(
        !r.text.contains("Wikidata") && !r.text.contains("wikidata"),
        "{}",
        r.text
    );
    // Adult: hidden with the detail while Show adult content is off...
    let r = detail(&f, &c, item::ADULT).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "{}", r.text);
    assert!(!r.text.contains("Adult Answer"));
    // ...and Adult goes only through ENR-1's section, so not with it on.
    f.show.send(true).unwrap();
    let body = ok(&f, &c, item::ADULT).await;
    assert!(absent(&body), "{body}");
}

#[tokio::test]
async fn lists_stay_within_their_caps() {
    let f = fixture();
    let c = session(&f).await;
    let body = ok(&f, &c, item::FULL).await;
    let a = &body["identified_as"]["alternatives"][0];
    assert_eq!(a["alt_titles"].as_array().unwrap().len(), MAX_ALT_TITLES);
    assert_eq!(a["genres"].as_array().unwrap().len(), MAX_GENRES);
    assert!(a["ids"].as_array().unwrap().len() <= lodestar_store::claims::MAX_IDS);
    for id in a["ids"].as_array().unwrap() {
        let scheme = id["scheme"].as_str().unwrap();
        assert!(
            lodestar_store::claims::ENRICHMENT_SCHEMES.contains(&scheme),
            "{scheme}"
        );
    }
}

#[tokio::test]
async fn resolved_via_lists_only_the_listed_methods() {
    let f = fixture();
    let c = session(&f).await;
    // Resolved from its files: listed.
    let body = ok(&f, &c, item::RESOLVED).await;
    assert_eq!(body["identified_as"]["resolved_via"], "files");
    assert!(
        body["identified_as"]["alternatives"]
            .as_array()
            .unwrap()
            .iter()
            .all(|a| a["chosen"] == false)
    );
    // Resolved via the plugin: `plugin` isn't listed, so null, and the
    // answer that named the title is the chosen one.
    let body = ok(&f, &c, item::PLUGIN).await;
    let s = &body["identified_as"];
    assert_eq!(s["resolved_via"], Value::Null);
    let alts = s["alternatives"].as_array().unwrap();
    assert_eq!(alts[0]["canonical_title"], "Plugin Answer");
    assert_eq!(alts[0]["chosen"], true);
    assert_eq!(alts[1]["chosen"], false);
}

/// In the binary's configuration, with no drawer installed, the
/// always-adult sources come from the plugins' manifests: the section
/// shows for an ordinary torrent, and never for one an always-adult
/// source found (before its reclassification, or after you said Not
/// adult) or one a source this build doesn't run found.
#[tokio::test]
async fn the_manifests_say_which_finders_are_always_adult() {
    for interest in [false, true] {
        let f = fixture_with(interest);
        let c = session(&f).await;
        for i in [item::TWO, item::PLUGIN, item::ACCOUNT] {
            let body = ok(&f, &c, i).await;
            assert!(!absent(&body), "{interest} {i}: {body}");
        }
        for i in [item::ADULT_FINDER, item::NOT_ADULT_FINDER, item::GHOST] {
            let r = detail(&f, &c, i).await;
            assert_eq!(r.status, StatusCode::OK, "{interest} {i}: {}", r.text);
            let body: Value = serde_json::from_str(&r.text).unwrap();
            assert!(absent(&body), "{interest} {i}: {body}");
            assert!(!r.text.contains("Answer"), "{interest} {i}: {}", r.text);
        }
    }
}

/// The document says what the route serves: the key optional, never
/// null, and the section's schema with its required fields.
#[test]
fn the_openapi_document_describes_the_section() {
    let doc: Value = serde_json::from_str(&openapi::json()).unwrap();
    let schemas = &doc["components"]["schemas"];
    let detail = &schemas["TorrentDetail"];
    let required: Vec<&str> = detail["required"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert!(!required.contains(&"identified_as"));
    // A plain reference: no `oneOf` with null, no null type.
    let prop = &detail["properties"]["identified_as"];
    assert_eq!(prop["$ref"], "#/components/schemas/IdentifiedAs");
    assert!(
        prop.get("oneOf").is_none() && prop.get("type").is_none(),
        "{prop}"
    );
    for (name, fields) in [
        ("IdentifiedAs", &["alternatives", "resolved_via"][..]),
        (
            "Identification",
            &[
                "alt_titles",
                "canonical_title",
                "chosen",
                "confidence",
                "genres",
                "ids",
                "source",
                "year",
            ][..],
        ),
        ("IdentifiedTitle", &["language", "title"][..]),
        ("IdentifiedId", &["scheme", "value"][..]),
    ] {
        let req = schemas[name]["required"].to_string();
        for f in fields {
            assert!(req.contains(f), "{name}.{f} in {req}");
        }
    }
}
