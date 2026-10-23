//! `GET /digest/preview` through the router, over a real store and fakes
//! of the running process (the fixture and its story are
//! `tests/insights.rs`'s): the session guard, the error envelope, JSON
//! only, the numbers against the same request's `/insights/overview`, and
//! Adult. Response bytes are checked against `golden/digest_<name>.txt`;
//! `LODESTAR_BLESS=1 cargo test -p lodestar-api --test digest` rewrites
//! them (read the diff before committing it).
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Request, header};
use lodestar_api::insights::{InsightsSource, OverviewCache};
use lodestar_api::ops::{
    BoxFuture, CrawlerRuntime, ExternalAddress, FetchStatus, NetworkCheck, OpsApi, OpsError,
    PortMapping, ReachReason, Source, SourcePatch, StatsResponse,
};
use lodestar_api::{AdultVisibility, ApiState, Auth, router};
use lodestar_store::rusqlite::{self, params};
use lodestar_store::{
    CohortCount, Counter, Event, SearchableCodes, SourceId, Store, StoreConfig, WriteBatch, flags,
};
use serde_json::{Value, json};
use tower::ServiceExt;

/// 2027-01-15 08:30 UTC: the fixtures' "now", half past hour `NOW_H`.
const NOW_H: i64 = 500_000;
const NOW: i64 = NOW_H * 3600 + 1800;

const CODES: SearchableCodes = SearchableCodes {
    stage: 5,
    newly: 10,
    delay: 11,
};
const HARVESTED: u8 = 1;
const PEER_FOUND: u8 = 2;
const FETCHED: u8 = 3;
const STORED: u8 = 4;
const LOST: u8 = 6;
const COVERAGE: u32 = 12;
const COVER_INDEX: u32 = 0;
const COVER_CRAWL: u32 = 1;
/// `classify::ContentType` codes.
const MOVIE: i64 = 0;
const MUSIC: i64 = 2;
const ADULT: i64 = 11;

#[derive(Debug)]
struct FakeOps {
    paused: AtomicBool,
    searchable: Mutex<Option<u64>>,
    lag: Mutex<Option<u64>>,
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
        Box::pin(async move {
            StatsResponse {
                data_bytes: None,
                disk_free_bytes: None,
                fetched_per_hour: None,
                harvested_per_hour: None,
                index_lag_s: *self.lag.lock().unwrap(),
                paused: self.paused.load(Ordering::Relaxed),
                queue_depth: 0,
                searchable_torrents: *self.searchable.lock().unwrap(),
                stored_torrents: 0,
                updated_at: NOW,
                uptime_s: 0,
            }
        })
    }

    fn sources(&self) -> BoxFuture<'_, Result<Vec<Source>, OpsError>> {
        Box::pin(async { Ok(Vec::new()) })
    }

    fn patch_source(&self, _: u8, _: SourcePatch) -> BoxFuture<'_, Result<Source, OpsError>> {
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

#[derive(Debug)]
struct FakeSource {
    now: AtomicI64,
    last_flush: Mutex<Option<i64>>,
    recorded: AtomicI64,
    imported: Option<u64>,
    cache: OverviewCache,
}

impl InsightsSource for FakeSource {
    fn last_flush(&self) -> Option<i64> {
        *self.last_flush.lock().unwrap()
    }

    fn started_at(&self) -> i64 {
        NOW - 86_400
    }

    fn last_recorded(&self) -> i64 {
        self.recorded.load(Ordering::Relaxed)
    }

    fn imported(&self) -> Option<u64> {
        self.imported
    }

    fn cache(&self) -> &OverviewCache {
        &self.cache
    }

    fn now(&self) -> i64 {
        self.now.load(Ordering::Relaxed)
    }
}

struct Fixture {
    app: Router,
    auth: Arc<Auth>,
    db: PathBuf,
    _dir: tempfile::TempDir,
}

fn fixture_with(insights: bool) -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("t.db");
    drop(Store::open(&db, StoreConfig::default()).unwrap());
    let reader = Store::open_reader(&db, StoreConfig::default()).unwrap();
    let auth = Arc::new(Auth::load(&dir.path().join("auth.json"), false).unwrap());
    let ops = Arc::new(FakeOps {
        paused: AtomicBool::new(false),
        searchable: Mutex::new(Some(1_000)),
        lag: Mutex::new(Some(0)),
    });
    let source = Arc::new(FakeSource {
        now: AtomicI64::new(NOW),
        last_flush: Mutex::new(Some(NOW - 3)),
        recorded: AtomicI64::new(0),
        imported: Some(40),
        cache: OverviewCache::default(),
    });
    let shown = Arc::new(AtomicBool::new(false));
    let state = ApiState {
        auth: auth.clone(),
        index: None,
        reader: Arc::new(Mutex::new(reader)),
        filters: None,
        settings: None,
        ops: Some(ops.clone() as Arc<dyn OpsApi>),
        adult: AdultVisibility::follow(move || shown.load(Ordering::Relaxed)),
        insights: insights.then(|| source.clone() as Arc<dyn InsightsSource>),
        user_data: None,
    };
    let app = Router::new().nest("/api/v1", router(state));
    Fixture {
        app,
        auth,
        db,
        _dir: dir,
    }
}

fn fixture() -> Fixture {
    fixture_with(true)
}

impl Fixture {
    fn store(&self) -> Store {
        Store::open(&self.db, StoreConfig::default()).unwrap()
    }

    fn sql(&self) -> rusqlite::Connection {
        rusqlite::Connection::open(&self.db).unwrap()
    }
}

fn src(id: u8) -> SourceId {
    SourceId::new(id).unwrap()
}

fn cohort(cohort: i64, source: u8, stage: u8, reason: u8, count: i64) -> CohortCount {
    CohortCount {
        cohort,
        source: src(source),
        stage,
        reason,
        count,
    }
}

fn counter(hour: i64, metric: u32, dim: u32, key: i64, value: i64) -> Counter {
    Counter {
        hour,
        metric,
        dim,
        key,
        value,
    }
}

fn write(store: &mut Store, cohorts: &[CohortCount], counters: &[Counter], events: &[Event]) {
    store
        .write(&WriteBatch {
            cohorts,
            counters,
            events,
            ..WriteBatch::default()
        })
        .unwrap();
}

/// A torrent row with what Insights' recorder reads.
fn torrent(c: &rusqlite::Connection, id: i64, at: i64, source: u8, kind: Option<i64>, fl: i64) {
    c.execute(
        "INSERT INTO torrents (id, info_hash, name, size, piece_len, piece_count,
             file_count, info_size, flags, discovered_at, metadata_at, last_seen_at,
             first_source, content_type)
         VALUES (?1, ?2, 'x', 0, 0, 0, 0, 0, ?3, ?4, ?4, ?4, ?5, ?6)",
        params![
            id,
            id.to_be_bytes().repeat(3)[..20].to_vec(),
            fl,
            at,
            source,
            kind
        ],
    )
    .unwrap();
}

fn register_sources(c: &rusqlite::Connection) {
    for (id, plugin) in [(0, "dht"), (1, "bitmagnet")] {
        c.execute(
            "INSERT INTO sources (id, plugin, version, state, added_at) VALUES (?1, ?2, '1', 0, 0)",
            params![id, plugin],
        )
        .unwrap();
    }
}

fn coverage(hours: impl IntoIterator<Item = i64>, dims: &[u32]) -> Vec<Counter> {
    hours
        .into_iter()
        .flat_map(|h| dims.iter().map(move |&d| counter(h, COVERAGE, d, 0, 3600)))
        .collect()
}

/// The story the goldens tell: three cohorts (one past the retry
/// horizon, one complete, one still in progress), torrents made
/// searchable in two commits, a paused stretch with no coverage, events,
/// and Adult among the new torrents.
fn story(f: &Fixture) {
    let c = f.sql();
    register_sources(&c);
    let mut s = f.store();
    let old = NOW_H - 80;
    let done = NOW_H - 5;
    let open = NOW_H - 2;
    write(
        &mut s,
        &[
            // Past the horizon, 1 still in progress: final anyway.
            cohort(old, 0, HARVESTED, 0, 10),
            cohort(old, 0, PEER_FOUND, 0, 8),
            cohort(old, 0, FETCHED, 0, 5),
            cohort(old, 0, STORED, 0, 3),
            cohort(old, 0, LOST, 1, 2),
            cohort(old, 0, LOST, 2, 4),
            // Complete: final.
            cohort(done, 0, HARVESTED, 0, 6),
            cohort(done, 0, PEER_FOUND, 0, 5),
            cohort(done, 0, FETCHED, 0, 4),
            cohort(done, 0, STORED, 0, 3),
            cohort(done, 0, LOST, 1, 1),
            cohort(done, 0, LOST, 2, 1),
            cohort(done, 0, LOST, 7, 1),
            cohort(done, 1, HARVESTED, 0, 2),
            cohort(done, 1, PEER_FOUND, 0, 2),
            cohort(done, 1, FETCHED, 0, 1),
            cohort(done, 1, STORED, 0, 1),
            cohort(done, 1, LOST, 8, 1),
            // In progress.
            cohort(open, 0, HARVESTED, 0, 9),
            cohort(open, 0, PEER_FOUND, 0, 3),
            cohort(open, 0, FETCHED, 0, 2),
            cohort(open, 0, STORED, 0, 2),
            cohort(open, 0, LOST, 9, 1),
        ],
        &[
            counter(old, 4, 0, 0, 3),
            counter(done, 4, 0, 0, 3),
            counter(done, 4, 1, 0, 1),
            counter(open, 4, 0, 0, 2),
            // Searches (#48's metric 8): not read yet.
            counter(NOW_H - 1, 8, 0, 0, 77),
            // Fetches by event time (metric 14, dim = the Fetched stage).
            counter(done, 14, u32::from(FETCHED), 0, 5),
            counter(open, 14, u32::from(FETCHED), 0, 2),
            counter(open, 14, u32::from(LOST), 9, 1),
            counter(done, 14, u32::from(LOST), 2, 1),
            counter(done, 14, u32::from(LOST), 7, 1),
            // Keys reported (metric 1: dim = source, key = strategy),
            // rediscovered (2) and dropped (3): the Discover page.
            counter(done, 1, 0, 0, 60),
            counter(done, 1, 0, 1, 20),
            counter(done, 1, 1, 0, 5),
            counter(open, 1, 0, 0, 30),
            counter(open, 1, 0, 200, 2),
            counter(done, 2, 0, 0, 12),
            counter(open, 3, 0, 0, 1),
            // Removals (15, by type): the Index page; one Adult.
            counter(NOW_H - 1, 15, MOVIE as u32, 0, 2),
            counter(NOW_H - 1, 15, ADULT as u32, 0, 1),
        ],
        &[
            Event {
                at: NOW - 20 * 3600,
                kind: 1,
                detail: "v0.5.0".into(),
            },
            Event {
                at: (NOW_H - 10) * 3600,
                kind: 3,
                detail: String::new(),
            },
            Event {
                at: (NOW_H - 8) * 3600,
                kind: 4,
                detail: String::new(),
            },
            Event {
                at: NOW - 3600,
                kind: 5,
                detail: "aggressive".into(),
            },
            Event {
                at: NOW - 60,
                kind: 99,
                detail: "from a later build".into(),
            },
        ],
    );
    // Fetches have been counted by event time for two days: a range
    // reaching further back doesn't know its fetches.
    c.execute(
        "UPDATE insights_state SET value = ?1 WHERE key = 'funnel_events_since_hour'",
        [NOW_H - 48],
    )
    .unwrap();
    // The searchable count's hourly snapshots (a gauge), one in the day
    // before the 7-day range starts.
    s.set_gauges(&[
        counter(NOW_H - 180, 13, 0, 0, 900),
        counter(NOW_H - 3, 13, 0, 0, 995),
        counter(NOW_H - 1, 13, 0, 0, 1000),
    ])
    .unwrap();
    // Coverage: the last 30 hours, but not the paused 2.
    let running: Vec<i64> = (NOW_H - 29..=NOW_H)
        .filter(|h| !(NOW_H - 10..NOW_H - 8).contains(h))
        .collect();
    s.add_counters(&coverage(
        running.iter().copied(),
        &[COVER_INDEX, COVER_CRAWL],
    ))
    .unwrap();
    // The stored torrents, and two imports.
    let mut id = 0;
    let mut add = |at: i64, source: u8, kind: Option<i64>, fl: i64| -> i64 {
        id += 1;
        torrent(&c, id, at, source, kind, fl);
        id
    };
    let mut last = 0;
    for k in [Some(MOVIE), Some(MOVIE), Some(MUSIC)] {
        last = add(old * 3600 + 100, 0, k, 0);
    }
    s.record_searchable(last, old * 3600 + 1000, CODES).unwrap();
    for k in [Some(MOVIE), Some(ADULT), None] {
        add(done * 3600 + 200, 0, k, 0);
    }
    add(done * 3600 + 300, 1, Some(MUSIC), 0);
    add(done * 3600, 1, Some(MOVIE), flags::IMPORTED);
    last = add(done * 3600, 1, Some(MOVIE), flags::IMPORTED);
    s.record_searchable(last, done * 3600 + 900, CODES).unwrap();
    // The open cohort: stored, one not searchable yet.
    last = add(open * 3600 + 10, 0, Some(ADULT), 0);
    s.record_searchable(last, open * 3600 + 400, CODES).unwrap();
    add(open * 3600 + 20, 0, Some(MOVIE), 0);
}

async fn send(f: &Fixture, mut req: Request<Body>) -> (u16, String) {
    req.extensions_mut()
        .insert(ConnectInfo(SocketAddr::from(([10, 9, 0, 1], 1))));
    let resp = f.app.clone().oneshot(req).await.unwrap();
    let status = resp.status().as_u16();
    let bytes = axum::body::to_bytes(resp.into_body(), 16 << 20)
        .await
        .unwrap();
    (status, String::from_utf8(bytes.to_vec()).unwrap())
}

async fn session(f: &Fixture) -> String {
    let token = f.auth.setup_token().unwrap();
    let mut req = Request::post("/api/v1/setup")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            json!({ "token": token, "password": "correct horse battery" }).to_string(),
        ))
        .unwrap();
    req.extensions_mut()
        .insert(ConnectInfo(SocketAddr::from(([10, 9, 0, 2], 1))));
    let resp = f.app.clone().oneshot(req).await.unwrap();
    let cookie = resp.headers()[header::SET_COOKIE].to_str().unwrap();
    cookie.split(';').next().unwrap().to_string()
}

fn get(uri: &str, cookie: Option<&str>) -> Request<Body> {
    let mut b = Request::get(uri);
    if let Some(c) = cookie {
        b = b.header(header::COOKIE, c);
    }
    b.body(Body::empty()).unwrap()
}

async fn json_get(f: &Fixture, cookie: &str, uri: &str) -> Value {
    let (status, body) = send(f, get(uri, Some(cookie))).await;
    assert_eq!(status, 200, "{uri}: {body}");
    serde_json::from_str(&body).unwrap()
}

/// Checks the status and body against `golden/digest_{name}.txt`.
async fn golden(f: &Fixture, cookie: &str, name: &str, uri: &str) -> Value {
    let (status, body) = send(f, get(uri, Some(cookie))).await;
    let got = format!("{status}\n{body}\n");
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden")
        .join(format!("digest_{name}.txt"));
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
    serde_json::from_str(&body).unwrap()
}

#[tokio::test]
async fn needs_a_session() {
    let f = fixture();
    for uri in [
        "/api/v1/digest/preview",
        "/api/v1/digest/preview?period=daily",
    ] {
        let (status, body) = send(&f, get(uri, None)).await;
        assert_eq!(status, 401, "{uri}");
        assert!(body.contains("login_required"));
    }
}

#[tokio::test]
async fn without_insights_it_is_unavailable() {
    let f = fixture_with(false);
    let cookie = session(&f).await;
    let (status, body) = send(&f, get("/api/v1/digest/preview", Some(&cookie))).await;
    assert_eq!(status, 503);
    assert!(body.contains("insights_unavailable"));
}

#[tokio::test]
async fn a_bad_period_names_its_field() {
    let f = fixture();
    let cookie = session(&f).await;
    for query in [
        "period=monthly",
        "period=",
        "period=Daily",
        "period=weekly,daily",
    ] {
        let (status, body) = send(
            &f,
            get(&format!("/api/v1/digest/preview?{query}"), Some(&cookie)),
        )
        .await;
        assert_eq!(status, 400, "{query}: {body}");
        let v: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["error"]["code"], "bad_parameter", "{query}");
        assert_eq!(v["error"]["field"], "period", "{query}");
    }
}

#[tokio::test]
async fn it_is_json_and_nothing_else() {
    let f = fixture();
    story(&f);
    let cookie = session(&f).await;
    let mut req = get("/api/v1/digest/preview", Some(&cookie));
    req.extensions_mut()
        .insert(ConnectInfo(SocketAddr::from(([10, 9, 0, 1], 1))));
    let resp = f.app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), 200);
    let ct = resp.headers()[header::CONTENT_TYPE].to_str().unwrap();
    assert!(ct.starts_with("application/json"), "{ct}");
    assert!(!ct.contains("html"));
}

/// 1,000 as `1,000`.
fn grouped(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

#[tokio::test]
async fn previews_match_goldens_and_the_overview() {
    let f = fixture();
    story(&f);
    let cookie = session(&f).await;
    for (name, period, range) in [("weekly", "weekly", "7d"), ("daily", "daily", "24h")] {
        let v = golden(
            &f,
            &cookie,
            name,
            &format!("/api/v1/digest/preview?period={period}"),
        )
        .await;
        let o = json_get(
            &f,
            &cookie,
            &format!("/api/v1/insights/overview?range={range}&compare=1"),
        )
        .await;
        assert_eq!(v["period"], period);
        assert_eq!(v["range"], o["range"]);
        assert_eq!(v["generated_at"], o["updated_at"]);
        let text = v["text"].as_str().unwrap();
        let html = v["html"].as_str().unwrap();
        assert!(text.len() <= 4_000 && html.len() <= 96 * 1024);
        assert!(v["subject"].as_str().unwrap().chars().count() <= 120);
        assert!(text.starts_with(v["subject"].as_str().unwrap()));
        let k = &o["kpi"];
        let count = |x: &Value| grouped(x.as_u64().unwrap());
        assert!(text.contains(&format!(
            "- Searchable torrents: {}",
            count(&k["searchable"])
        )));
        assert!(text.contains(&format!(
            "- New searchable: {}",
            count(&k["new_searchable"])
        )));
        let ft = &o["funnel_total"];
        assert!(text.contains(&format!(
            "- Peer found: {}",
            count(&ft["stages"]["peer_found"])
        )));
        assert!(text.contains(&format!(
            "- Searchable: {}",
            count(&ft["stages"]["searchable"])
        )));
        assert!(
            text.contains(&format!(
                "({} cohorts, {} final",
                ft["cohorts"], ft["final_cohorts"]
            )) || text.contains(&format!(
                "({} cohort, {} final",
                ft["cohorts"], ft["final_cohorts"]
            ))
        );
        // Yield and its change are the server's, not recomputed.
        match k["yield"].as_f64() {
            Some(y) => {
                let want = format!("{:.1}%", (y * 1000.0).round() / 10.0);
                assert!(text.contains(&format!("- Yield: {want}")), "{want}\n{text}");
            }
            None => assert!(text.contains("- Yield: — (No completed cohorts yet)")),
        }
        // The changes are the overview's own.
        for (field, label) in [
            ("searchable_delta", "Searchable torrents"),
            ("new_searchable_delta", "New searchable"),
        ] {
            match k[field].as_i64() {
                Some(0) => {
                    assert!(text.contains(&format!("- {label}")) && text.contains("change 0"))
                }
                Some(d) => {
                    let (arrow, sign) = if d > 0 {
                        ("▲", "+")
                    } else {
                        ("▼", "\u{2212}")
                    };
                    let want = format!("change {arrow} {sign}{}", grouped(d.unsigned_abs()));
                    assert!(text.contains(&want), "{field}: {want}\n{text}");
                }
                None => {}
            }
        }
        assert!(text.contains(&format!("/insights?range={range}&compare=1")));
    }
}

#[tokio::test]
async fn the_default_is_weekly() {
    let f = fixture();
    story(&f);
    let cookie = session(&f).await;
    let a = json_get(&f, &cookie, "/api/v1/digest/preview").await;
    let b = json_get(&f, &cookie, "/api/v1/digest/preview?period=weekly").await;
    assert_eq!(a, b);
    assert_eq!(a["period"], "weekly");
}

#[tokio::test]
async fn adult_is_left_out_while_hidden() {
    let f = fixture();
    story(&f);
    let cookie = session(&f).await;
    for period in ["weekly", "daily"] {
        let v = json_get(
            &f,
            &cookie,
            &format!("/api/v1/digest/preview?period={period}"),
        )
        .await;
        for part in ["text", "html"] {
            let s = v[part].as_str().unwrap().to_lowercase();
            assert!(!s.contains("adult"), "{period} {part}: {s}");
        }
    }
}
