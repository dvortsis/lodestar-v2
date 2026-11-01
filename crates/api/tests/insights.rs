//! `/insights/overview` and `/insights/events` through the router, over a
//! real store and fakes of the running process: the session guard, the
//! error envelope, the reconciliation rules of §10.17 against independent
//! counts, gaps against zeros, Compare, Adult, the stale flag, the
//! exceptions, the cache, and the 50 ms budget on a year of hourly
//! rollups. Response bytes are checked against `golden/insights_<name>.txt`;
//! `LODESTAR_BLESS=1 cargo test -p lodestar-api --test insights` rewrites
//! them (read the diff before committing it).

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use axum::Router;
use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Request, header};
use lodestar_api::dto::insights::PrivacySummary;
use lodestar_api::insights::{InsightsSource, OverviewCache};
use lodestar_api::ops::{
    BoxFuture, CrawlerRuntime, ExternalAddress, FetchStatus, NetworkCheck, OpsApi, OpsError,
    PortMapping, ReachReason, Source, SourcePatch, StatsResponse,
};
use lodestar_api::settings::{
    Imported, Saved, SectionValues, SettingsApi, SettingsBatch, SettingsFailure, SettingsImport,
    SettingsIndex, SettingsPreview,
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
                sampling_blocked: None,
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
    state: ApiState,
    auth: Arc<Auth>,
    ops: Arc<FakeOps>,
    source: Arc<FakeSource>,
    adult: Arc<AtomicBool>,
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
    let adult = Arc::new(AtomicBool::new(false));
    let shown = adult.clone();
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
        hot: None,
        events: None,
    };
    let app = Router::new().nest("/api/v1", router(state.clone()));
    Fixture {
        app,
        state,
        auth,
        ops,
        source,
        adult,
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

    /// Something was written: the next overview isn't a cached one.
    fn touched(&self) {
        self.source.recorded.fetch_add(1, Ordering::Relaxed);
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
                at: NOW - 2 * 3600,
                kind: lodestar_store::import::event::FINISHED,
                detail: "job=3 source=bitmagnet new=2 merged=1".into(),
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

/// Checks the status and body against `golden/insights_{name}.txt`.
async fn golden(f: &Fixture, cookie: &str, name: &str, uri: &str) -> Value {
    let (status, body) = send(f, get(uri, Some(cookie))).await;
    let got = format!("{status}\n{body}\n");
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden")
        .join(format!("insights_{name}.txt"));
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
async fn every_route_needs_a_session() {
    let f = fixture();
    for uri in [
        "/api/v1/insights/overview",
        "/api/v1/insights/events?from=0&to=10",
        "/api/v1/insights/discover",
        "/api/v1/insights/fetch",
        "/api/v1/insights/understand",
        "/api/v1/insights/index",
        "/api/v1/insights/serve",
        "/api/v1/insights/export.csv?panel=trend",
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
    for uri in [
        "/api/v1/insights/overview",
        "/api/v1/insights/events?from=0&to=10",
        "/api/v1/insights/fetch",
        "/api/v1/insights/export.csv?panel=index.trend",
    ] {
        let (status, body) = send(&f, get(uri, Some(&cookie))).await;
        assert_eq!(status, 503, "{uri}");
        assert!(body.contains("insights_unavailable"));
    }
}

#[tokio::test]
async fn bad_parameters_name_their_field() {
    let f = fixture();
    let cookie = session(&f).await;
    for (query, code, field) in [
        ("range=1y", "bad_range", "range"),
        ("range=custom&to=10", "bad_range", "from"),
        ("range=custom&from=10", "bad_range", "to"),
        ("range=custom&from=10&to=5", "bad_range", "to"),
        ("range=24h&from=10", "bad_range", "from"),
        ("compare=2", "bad_parameter", "compare"),
        ("compare=yes", "bad_parameter", "compare"),
        ("range=custom&from=x&to=5", "bad_parameter", "from"),
    ] {
        let (status, body) = send(
            &f,
            get(&format!("/api/v1/insights/overview?{query}"), Some(&cookie)),
        )
        .await;
        assert_eq!(status, 400, "{query}: {body}");
        let v: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["error"]["code"], code, "{query}");
        assert_eq!(v["error"]["field"], field, "{query}");
    }
    // The stage pages and the export take the overview's range rules,
    // plus `reason` and `panel`.
    for (path, query, field) in [
        ("discover", "range=1y", "range"),
        ("fetch", "reason=lost", "reason"),
        ("fetch", "reason=", "reason"),
        ("index", "reason=Timed_Out", "reason"),
        ("serve", "compare=3", "compare"),
        ("understand", "range=custom&from=10", "to"),
        ("export.csv", "", "panel"),
        ("export.csv", "panel=fetch", "panel"),
        ("export.csv", "panel=trend&range=1y", "range"),
        ("export.csv", "panel=fetch.trend&compare=2", "compare"),
        ("export.csv", "panel=trend&reason=nope", "reason"),
    ] {
        let (status, body) = send(
            &f,
            get(&format!("/api/v1/insights/{path}?{query}"), Some(&cookie)),
        )
        .await;
        assert_eq!(status, 400, "{path}?{query}: {body}");
        let v: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["error"]["field"], field, "{path}?{query}");
    }
    for (query, field) in [
        ("from=5&to=5", "to"),
        ("to=5", "from"),
        ("from=-1&to=5", "from"),
    ] {
        let (status, body) = send(
            &f,
            get(&format!("/api/v1/insights/events?{query}"), Some(&cookie)),
        )
        .await;
        assert_eq!(status, 400, "{query}: {body}");
        let v: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["error"]["field"], field, "{query}");
    }
}

#[tokio::test]
async fn goldens() {
    let f = fixture();
    story(&f);
    let cookie = session(&f).await;
    let day = golden(
        &f,
        &cookie,
        "overview_24h",
        "/api/v1/insights/overview?range=24h",
    )
    .await;
    let week = golden(
        &f,
        &cookie,
        "overview_7d_compare",
        "/api/v1/insights/overview?range=7d&compare=1",
    )
    .await;
    golden(
        &f,
        &cookie,
        "overview_30d",
        "/api/v1/insights/overview?range=30d",
    )
    .await;
    golden(
        &f,
        &cookie,
        "events",
        &format!("/api/v1/insights/events?from={}&to={}", NOW - 86_400, NOW),
    )
    .await;
    // The story's numbers, read back.
    assert_eq!(day["kpi"]["searchable"], 1000);
    assert_eq!(day["kpi"]["imported"], 40);
    assert_eq!(day["kpi"]["searchable_crawled"], 960);
    let import = day["annotations"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["kind"] == "import_finished")
        .expect("the import's annotation");
    assert_eq!(
        (
            &import["import"]["job"],
            &import["import"]["new"],
            &import["import"]["merged"]
        ),
        (&Value::from(3), &Value::from(2), &Value::from(1)),
        "novel against already known"
    );
    assert_eq!(day["kpi"]["searchable_delta"], Value::Null);
    assert!(day.get("previous").is_none());
    assert_eq!(day["stages"]["fetch"], 7);
    assert_eq!(
        week["stages"]["fetch"],
        Value::Null,
        "before fetches were counted"
    );
    assert_eq!(week["previous"]["stages"]["fetch"], Value::Null);
    assert_eq!(day["stages"]["serve"], Value::Null);
    assert_eq!(
        day["serve"]["searches"],
        Value::Null,
        "#48's counter isn't read yet"
    );
    assert_eq!(day["stale"], Value::Null);
    assert_eq!(
        day["kpi"]["health"],
        json!({ "reason": null, "state": "running" })
    );
}

/// A paused hour is a gap (null); a measured hour with nothing in it is
/// 0; and annotations name the pause.
#[tokio::test]
async fn gaps_are_null_and_zeros_are_zero() {
    let f = fixture();
    story(&f);
    let cookie = session(&f).await;
    let v = json_get(&f, &cookie, "/api/v1/insights/overview?range=24h").await;
    let trend = v["trend"].as_array().unwrap();
    assert_eq!(trend.len(), 24);
    let at = |h: i64| {
        trend
            .iter()
            .find(|p| p["at"] == h * 3600)
            .unwrap_or_else(|| panic!("no bucket for {h}"))
    };
    for h in [NOW_H - 10, NOW_H - 9] {
        assert_eq!(at(h)["searchable"], Value::Null, "paused hour {h}");
        assert_eq!(at(h)["harvested"], Value::Null, "paused hour {h}");
    }
    assert_eq!(at(NOW_H - 3)["searchable"], 0, "measured, nothing new");
    assert_eq!(at(NOW_H - 3)["harvested"], 0);
    // Crawled only: the hour's two imports are apart (IMP-5).
    assert_eq!(at(NOW_H - 5)["searchable"], 4);
    assert_eq!(at(NOW_H - 5)["searchable_imported"], 2);
    assert_eq!(at(NOW_H - 5)["harvested"], 8);
    assert_eq!(at(NOW_H - 2)["searchable"], 1);
    assert_eq!(v["ingest_per_hour"], 0, "the last full hour was measured");
    let kinds: Vec<&str> = v["annotations"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["kind"].as_str().unwrap())
        .collect();
    assert_eq!(
        kinds,
        [
            "restart",
            "paused",
            "resumed",
            "import_finished",
            "intensity"
        ]
    );
    let e = json_get(
        &f,
        &cookie,
        &format!(
            "/api/v1/insights/events?from={}&to={}",
            (NOW_H - 9) * 3600,
            NOW
        ),
    )
    .await;
    // Paused before the range starts: the span begins at the pause.
    assert_eq!(
        e["paused"],
        json!([{ "from": (NOW_H - 10) * 3600, "to": (NOW_H - 8) * 3600 }])
    );
}

/// §10.17's reconciliation over a generated history: funnel stages never
/// increase, harvested = Σ terminal + in progress, stored and searchable
/// equal independent `COUNT(*)`s over `torrents`, and yield and time to
/// searchable count final cohorts only.
#[tokio::test]
async fn the_funnel_reconciles() {
    let f = fixture();
    let c = f.sql();
    register_sources(&c);
    let mut s = f.store();
    let mut rng = 0x2545_f491_4f6c_dd1du64;
    let mut next = |n: u64| {
        rng ^= rng << 13;
        rng ^= rng >> 7;
        rng ^= rng << 17;
        rng % n
    };
    let first = NOW_H - 300;
    let mut id = 0i64;
    let mut rows = Vec::new();
    // Final cohorts' harvested and searchable, computed independently.
    let (mut final_harvested, mut final_searchable) = (0u64, 0u64);
    for hour in first..NOW_H {
        for source in 0..2u8 {
            let harvested = 1 + next(12);
            let mut stored_ids = Vec::new();
            let (mut peers, mut fetched, mut lost) = (0, 0, 0);
            let mut in_progress = 0;
            for _ in 0..harvested {
                match next(10) {
                    0..=2 => {
                        rows.push(cohort(hour, source, LOST, 1 + next(10) as u8, 1));
                        lost += 1;
                    }
                    3 if hour > NOW_H - 72 => in_progress += 1,
                    _ => {
                        peers += 1;
                        fetched += 1;
                        id += 1;
                        let kind = Some(next(12) as i64);
                        torrent(&c, id, hour * 3600 + next(3600) as i64, source, kind, 0);
                        stored_ids.push(id);
                    }
                }
            }
            let _ = (lost, in_progress);
            rows.extend([
                cohort(hour, source, HARVESTED, 0, harvested as i64),
                cohort(hour, source, PEER_FOUND, 0, peers),
                cohort(hour, source, FETCHED, 0, fetched),
                cohort(hour, source, STORED, 0, stored_ids.len() as i64),
            ]);
        }
        // An import now and then: never in the funnel.
        if next(5) == 0 {
            id += 1;
            torrent(&c, id, hour * 3600, 1, Some(MOVIE), flags::IMPORTED);
        }
        // Commits trail by an hour; the last two hours' aren't indexed.
        if hour < NOW_H - 2 {
            s.record_searchable(id, (hour + 1) * 3600 + 120, CODES)
                .unwrap();
        }
    }
    write(&mut s, &rows, &[], &[]);
    s.add_counters(&coverage(first..=NOW_H, &[COVER_INDEX, COVER_CRAWL]))
        .unwrap();
    let cookie = session(&f).await;
    let uri = format!(
        "/api/v1/insights/overview?range=custom&from={}&to={}",
        first * 3600,
        NOW
    );
    let v = json_get(&f, &cookie, &uri).await;
    assert_eq!(v["range"]["bucket"], "day");

    // Independent counts over `torrents`.
    let count = |sql: &str| -> u64 {
        c.query_row(sql, [flags::IMPORTED], |r| r.get::<_, i64>(0))
            .unwrap() as u64
    };
    let stored = count("SELECT count(*) FROM torrents WHERE flags & ?1 = 0");
    let searchable = count(
        "SELECT count(*) FROM torrents WHERE flags & ?1 = 0
         AND id <= (SELECT max(last_id) FROM index_watermark_hour)",
    );
    let total = &v["funnel_total"];
    assert_eq!(total["stages"]["stored"], stored);
    assert_eq!(total["stages"]["searchable"], searchable);
    let (mut sum_new, mut sum_imported) = (0, 0);
    for p in v["trend"].as_array().unwrap() {
        sum_new += p["searchable"].as_u64().unwrap_or(0);
        sum_imported += p["searchable_imported"].as_u64().unwrap_or(0);
    }
    let all_searchable = c
        .query_row(
            "SELECT count(*) FROM torrents WHERE id <= (SELECT max(last_id) FROM index_watermark_hour)",
            [],
            |r| r.get::<_, i64>(0),
        )
        .unwrap() as u64;
    // Crawled and imported are apart, in the same hours (IMP-5).
    let imported = all_searchable - searchable;
    assert!(imported > 0, "the story imports something");
    assert_eq!(sum_new, searchable, "the trend is the crawl's");
    assert_eq!(sum_imported, imported, "imports have their own series");
    assert_eq!(v["kpi"]["new_searchable"], searchable);
    assert_eq!(v["kpi"]["new_searchable_imported"], imported);

    for entry in v["funnel"].as_array().unwrap() {
        let st = &entry["stages"];
        let n = |k: &str| st[k].as_u64().unwrap();
        assert!(n("harvested") >= n("peer_found"), "{entry}");
        assert!(n("peer_found") >= n("fetched"), "{entry}");
        assert!(n("fetched") >= n("stored"), "{entry}");
        assert!(n("stored") >= n("searchable"), "{entry}");
        let lost: u64 = entry["losses"]
            .as_object()
            .unwrap()
            .values()
            .map(|v| v.as_u64().unwrap())
            .sum();
        assert_eq!(
            n("harvested"),
            lost + n("stored") + entry["in_progress"].as_u64().unwrap(),
            "{entry}"
        );
        if entry["final"] == true {
            final_harvested += n("harvested");
            final_searchable += n("searchable");
        }
    }
    // Daily buckets: a day is final only when every hour is, and the
    // KPI counts final hours, so it covers at least those days.
    let y = v["kpi"]["yield"].as_f64().unwrap();
    assert!(final_harvested > 0);
    let by_days = final_searchable as f64 / final_harvested as f64;
    assert!((y - by_days).abs() < 0.1, "{y} vs {by_days}");
    assert!(v["kpi"]["time_to_searchable_p50_s"].as_u64().unwrap() > 0);
    assert!(
        v["kpi"]["time_to_searchable_p95_s"].as_u64().unwrap()
            >= v["kpi"]["time_to_searchable_p50_s"].as_u64().unwrap()
    );

    // Hour by hour (the last 7 days), the KPI's yield is exactly the
    // final cohorts' searchable ÷ harvested.
    let v = json_get(&f, &cookie, "/api/v1/insights/overview?range=7d").await;
    let (mut h, mut s_) = (0, 0);
    for entry in v["funnel"].as_array().unwrap() {
        if entry["final"] == true {
            h += entry["stages"]["harvested"].as_u64().unwrap();
            s_ += entry["stages"]["searchable"].as_u64().unwrap();
        }
    }
    assert_eq!(v["kpi"]["yield"].as_f64().unwrap(), s_ as f64 / h as f64);
    let incomplete = v["funnel"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["final"] == false)
        .count();
    assert!(incomplete > 0, "recent cohorts are incomplete");
    // Segments add up to new searchable, and sources' yields are final.
    let seg: u64 = v["segments"]["by_content_type"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["searchable"].as_u64().unwrap())
        .sum();
    let adult_new = count_newly_adult(&c, v["range"]["from"].as_i64().unwrap() / 3600);
    assert_eq!(
        seg + adult_new,
        v["kpi"]["new_searchable"].as_u64().unwrap()
    );
    let sources: Vec<&str> = v["segments"]["by_source"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["key"].as_str().unwrap())
        .collect();
    assert_eq!(sources.len(), 2);
    assert!(sources.contains(&"dht") && sources.contains(&"bitmagnet"));
}

/// Adult torrents newly searchable from `from_h` on (left out of the
/// segments while hidden).
fn count_newly_adult(c: &rusqlite::Connection, from_h: i64) -> u64 {
    c.query_row(
        "SELECT coalesce(sum(value), 0) FROM rollup_hour WHERE metric = 10 AND dim = ?1 AND hour >= ?2",
        params![ADULT, from_h],
        |r| r.get::<_, i64>(0),
    )
    .unwrap() as u64
}

#[tokio::test]
async fn no_final_cohort_means_null_yield() {
    let f = fixture();
    let mut s = f.store();
    write(
        &mut s,
        &[
            cohort(NOW_H - 3, 0, HARVESTED, 0, 5),
            cohort(NOW_H - 3, 0, STORED, 0, 1),
        ],
        &[],
        &[],
    );
    let cookie = session(&f).await;
    let v = json_get(&f, &cookie, "/api/v1/insights/overview?range=24h").await;
    assert_eq!(v["kpi"]["yield"], Value::Null);
    assert_eq!(v["kpi"]["time_to_searchable_p50_s"], Value::Null);
    assert_eq!(v["kpi"]["time_to_searchable_p95_s"], Value::Null);
    assert_eq!(v["funnel"][0]["final"], false);
    assert_eq!(v["funnel"][0]["in_progress"], 4);
    assert_eq!(v["funnel_total"]["final_cohorts"], 0);
    assert_eq!(
        v["ingest_per_hour"],
        Value::Null,
        "the last hour wasn't measured"
    );
    // Nothing measured at all: every trend point is a gap.
    assert!(
        v["trend"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|p| p["at"] != (NOW_H - 3) * 3600)
            .all(|p| p["searchable"].is_null() && p["harvested"].is_null())
    );
}

#[tokio::test]
async fn compare_covers_the_period_before() {
    let f = fixture();
    let mut s = f.store();
    let c = f.sql();
    // 2 newly searchable in the previous 24 h, 3 in the current.
    for (id, at) in [(1, NOW - 30 * 3600), (2, NOW - 30 * 3600), (3, NOW - 3600)] {
        torrent(&c, id, at, 0, Some(MOVIE), 0);
        s.record_searchable(id, at + 60, CODES).unwrap();
    }
    for id in 4..=5 {
        torrent(&c, id, NOW - 600, 0, Some(MOVIE), 0);
    }
    s.record_searchable(5, NOW - 60, CODES).unwrap();
    let cookie = session(&f).await;
    let v = json_get(&f, &cookie, "/api/v1/insights/overview?range=24h&compare=1").await;
    let p = &v["previous"];
    assert_eq!(p["range"]["to"], v["range"]["from"]);
    assert_eq!(
        p["range"]["to"].as_i64().unwrap() - p["range"]["from"].as_i64().unwrap(),
        24 * 3600
    );
    assert_eq!(p["trend"].as_array().unwrap().len(), 24);
    assert_eq!(v["kpi"]["new_searchable"], 3);
    assert_eq!(p["kpi"]["new_searchable"], 2);
    // No snapshot from the previous period's end: the change is unknown.
    assert_eq!(v["kpi"]["searchable_delta"], Value::Null);
    assert_eq!(p["kpi"]["searchable"], Value::Null);
    // The recorder's hourly snapshots: the last one before the range
    // starts is the count then, removals included.
    let start_h = NOW_H - 23;
    s.set_gauges(&[
        counter(start_h - 5, 13, 0, 0, 900),
        counter(start_h - 1, 13, 0, 0, 1_004),
        counter(start_h, 13, 0, 0, 2_000),
    ])
    .unwrap();
    f.touched();
    let v = json_get(&f, &cookie, "/api/v1/insights/overview?range=24h&compare=1").await;
    assert_eq!(v["previous"]["kpi"]["searchable"], 1_004);
    assert_eq!(v["kpi"]["searchable_delta"], -4, "1,000 now: 4 removed net");
    // Without compare, or for `all`, there's no previous period.
    let v = json_get(&f, &cookie, "/api/v1/insights/overview?range=24h&compare=0").await;
    assert!(v.get("previous").is_none());
    assert_eq!(v["kpi"]["searchable_delta"], Value::Null);
    let v = json_get(&f, &cookie, "/api/v1/insights/overview?range=all&compare=1").await;
    assert!(v.get("previous").is_none());
    assert_eq!(v["kpi"]["searchable_delta"], Value::Null);
    assert_eq!(
        v["range"]["from"],
        (NOW_H - 30) * 3600,
        "all starts at the first rollup"
    );
}

#[tokio::test]
async fn adult_is_left_out_of_segments_while_hidden() {
    let f = fixture();
    story(&f);
    let cookie = session(&f).await;
    let keys = |v: &Value, seg: &str| -> Vec<String> {
        v["segments"][seg]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["key"].as_str().unwrap().to_string())
            .collect()
    };
    let v = json_get(&f, &cookie, "/api/v1/insights/overview?range=24h").await;
    for seg in ["by_content_type", "by_group"] {
        assert!(!keys(&v, seg).contains(&"adult".to_string()), "{seg}");
    }
    assert!(!v.to_string().contains("adult"));
    f.adult.store(true, Ordering::Relaxed);
    let v = json_get(&f, &cookie, "/api/v1/insights/overview?range=24h").await;
    for seg in ["by_content_type", "by_group"] {
        assert!(keys(&v, seg).contains(&"adult".to_string()), "{seg}");
    }
    // Sorted by value, then key.
    let rows = v["segments"]["by_content_type"].as_array().unwrap();
    for w in rows.windows(2) {
        let (a, b) = (&w[0], &w[1]);
        assert!(
            a["searchable"].as_u64() > b["searchable"].as_u64()
                || (a["searchable"] == b["searchable"] && a["key"].as_str() < b["key"].as_str())
        );
    }
}

#[tokio::test]
async fn stale_health_and_exceptions() {
    let f = fixture();
    let mut s = f.store();
    // In progress, the crawler ran the last two hours, nothing stored.
    write(
        &mut s,
        &[cohort(NOW_H - 6, 0, HARVESTED, 0, 5)],
        &coverage(NOW_H - 2..NOW_H, &[COVER_CRAWL]),
        &[],
    );
    let cookie = session(&f).await;
    let v = json_get(&f, &cookie, "/api/v1/insights/overview?range=24h").await;
    assert_eq!(
        v["exceptions"],
        json!([{
            "fix": { "kind": "stage", "target": "fetch" },
            "kind": "cohort_stalled",
            "severity": "warning",
            "since": (NOW_H - 2) * 3600,
            "unit": "torrents",
            "value": 5.0
        }])
    );
    *f.ops.lag.lock().unwrap() = Some(42);
    let v = json_get(&f, &cookie, "/api/v1/insights/overview?range=24h").await;
    assert_eq!(v["exceptions"][0]["kind"], "index_lag");
    assert_eq!(v["exceptions"][0]["value"], 42.0);
    assert_eq!(v["exceptions"][0]["since"], NOW - 42);
    assert_eq!(v["exceptions"][1]["kind"], "cohort_stalled");
    assert_eq!(
        v["kpi"]["health"],
        json!({ "reason": "index_lag", "state": "degraded" })
    );
    // Stale: more than 2 minutes since the last flush.
    *f.source.last_flush.lock().unwrap() = Some(NOW - 121);
    let v = json_get(&f, &cookie, "/api/v1/insights/overview?range=24h").await;
    assert_eq!(
        v["stale"],
        json!({ "reason": "writer_stalled", "since": NOW - 121 })
    );
    assert_eq!(v["kpi"]["health"]["reason"], "writer_stalled");
    *f.source.last_flush.lock().unwrap() = None;
    let v = json_get(&f, &cookie, "/api/v1/insights/overview?range=24h").await;
    assert_eq!(
        v["stale"]["since"],
        NOW - 86_400,
        "no flush since the start"
    );
    f.ops.paused.store(true, Ordering::Relaxed);
    let v = json_get(&f, &cookie, "/api/v1/insights/overview?range=24h").await;
    assert_eq!(v["stale"]["reason"], "paused");
    assert_eq!(
        v["kpi"]["health"],
        json!({ "reason": "paused", "state": "paused" })
    );
    *f.source.last_flush.lock().unwrap() = Some(NOW - 120);
    *f.ops.lag.lock().unwrap() = None;
    *f.ops.searchable.lock().unwrap() = None;
    f.ops.paused.store(false, Ordering::Relaxed);
    let v = json_get(&f, &cookie, "/api/v1/insights/overview?range=24h").await;
    assert_eq!(v["stale"], Value::Null, "exactly 2 minutes is fresh");
    assert_eq!(v["kpi"]["searchable"], Value::Null, "search is off");
    assert_eq!(v["stages"]["index"], Value::Null);
}

#[tokio::test]
async fn responses_are_cached_per_flush() {
    let f = fixture();
    story(&f);
    let cookie = session(&f).await;
    let a = json_get(&f, &cookie, "/api/v1/insights/overview?range=24h").await;
    assert_eq!(f.source.cache.len(), 1);
    // A row written behind the cache's back isn't seen until a flush.
    let mut s = f.store();
    write(&mut s, &[], &[counter(NOW_H, 4, 0, 0, 5)], &[]);
    let b = json_get(&f, &cookie, "/api/v1/insights/overview?range=24h").await;
    assert_eq!(a, b);
    assert_eq!(f.source.cache.len(), 1);
    f.touched();
    let c = json_get(&f, &cookie, "/api/v1/insights/overview?range=24h").await;
    assert_eq!(
        c["stages"]["understand"].as_u64().unwrap(),
        a["stages"]["understand"].as_u64().unwrap() + 5
    );
    *f.source.last_flush.lock().unwrap() = Some(NOW - 1);
    json_get(&f, &cookie, "/api/v1/insights/overview?range=24h").await;
    assert_eq!(f.source.cache.len(), 3);
    // Snapshot fields are read every time, cached or not.
    *f.ops.searchable.lock().unwrap() = Some(5);
    let d = json_get(&f, &cookie, "/api/v1/insights/overview?range=24h").await;
    assert_eq!(d["kpi"]["searchable"], 5);
}

/// A year of hourly rollups at a realistic density (§1.6.4: ≤ 50 ms
/// whatever the index size). Built with SQL, which is quicker than the
/// store's API at this size.
fn year_of_rollups(db: &Path) {
    let mut c = rusqlite::Connection::open(db).unwrap();
    let tx = c.transaction().unwrap();
    {
        let mut r = tx
            .prepare(
                "INSERT INTO rollup_hour (hour, metric, dim, key, value) VALUES (?1, ?2, ?3, ?4, ?5)",
            )
            .unwrap();
        let mut k = tx
            .prepare(
                "INSERT INTO cohort_hour (cohort, source, stage, reason, count) VALUES (?1, ?2, ?3, ?4, ?5)",
            )
            .unwrap();
        for h in NOW_H - 365 * 24..=NOW_H {
            // Harvested, rediscovered, dropped and stored by source and
            // strategy (1–4); M4's scrape and search counters (5–9);
            // newly searchable by type and import (10), the delay
            // histogram (11) and coverage (12): about 110 rows an hour.
            for source in 0..2 {
                for strategy in 0..3 {
                    r.execute(params![h, 1, source, strategy, 900]).unwrap();
                    r.execute(params![h, 2, source, strategy, 300]).unwrap();
                }
                r.execute(params![h, 3, source, 0, 2]).unwrap();
                r.execute(params![h, 4, source, 0, 120]).unwrap();
            }
            for tracker in 0..8 {
                for outcome in 0..3 {
                    r.execute(params![h, 5, tracker, outcome, 40]).unwrap();
                }
            }
            for key in 0..17 {
                r.execute(params![h, 9, 0, key, 3]).unwrap();
            }
            for t in 0..12 {
                r.execute(params![h, 10, t, 0, 10]).unwrap();
                // Imports in every hour: the crawled/imported split's
                // worst case (IMP-5).
                r.execute(params![h, 10, t, 1, 2]).unwrap();
            }
            for b in 80..110 {
                r.execute(params![h, 11, 0, b, 4]).unwrap();
            }
            for d in 0..2 {
                r.execute(params![h, 12, d, 0, 3600]).unwrap();
            }
            for source in 0..2 {
                for (stage, reason, n) in [
                    (1, 0, 1000),
                    (2, 0, 700),
                    (3, 0, 300),
                    (4, 0, 120),
                    (5, 0, 120),
                    (6, 1, 300),
                    (6, 2, 200),
                    (6, 3, 100),
                    (6, 4, 50),
                    (6, 5, 10),
                    (6, 6, 20),
                    (6, 7, 30),
                    (6, 8, 70),
                    (6, 9, 100),
                ] {
                    k.execute(params![h, source, stage, reason, n]).unwrap();
                }
            }
        }
        for i in 0..2000 {
            tx.execute(
                "INSERT INTO events (at, kind, detail) VALUES (?1, ?2, '')",
                params![NOW - i * 4000, 3 + i % 3],
            )
            .unwrap();
        }
    }
    tx.commit().unwrap();
}

#[tokio::test]
async fn a_year_of_rollups_answers_within_budget() {
    let f = fixture();
    year_of_rollups(&f.db);
    let cookie = session(&f).await;
    // What the recorder does at start: the sealed days' sums.
    let cold = Instant::now();
    let warmed = lodestar_api::insights::warm(&f.store(), &f.source.cache, NOW, None).unwrap();
    eprintln!("warm ({warmed} days): {:?}", cold.elapsed());
    assert!(warmed >= 355);
    let mut queries: Vec<String> = [
        "range=24h",
        "range=7d",
        "range=7d&compare=1",
        "range=30d",
        "range=30d&compare=1",
        "range=all",
        "range=all&compare=1",
    ]
    .iter()
    .map(|q| format!("overview?{q}"))
    .collect();
    // Every stage page, at the ranges that read the most.
    for stage in ["discover", "fetch", "understand", "index", "serve"] {
        for q in ["range=7d&compare=1", "range=30d&compare=1", "range=all"] {
            queries.push(format!("{stage}?{q}"));
        }
    }
    queries.push("fetch?range=30d&reason=timed_out".into());
    let timed = |label: &'static str| {
        let (f, cookie, queries) = (&f, &cookie, &queries);
        async move {
            let mut bodies = Vec::new();
            for query in queries {
                let uri = format!("/api/v1/insights/{query}");
                for i in 0..3 {
                    // A new flush each time: never a cached response.
                    f.touched();
                    let start = Instant::now();
                    let (status, body) = send(f, get(&uri, Some(cookie))).await;
                    let took = start.elapsed();
                    eprintln!("{label} {query}: {took:?}");
                    assert_eq!(status, 200);
                    let v: Value = serde_json::from_str(&body).unwrap();
                    if query.starts_with("overview") {
                        assert!(v["kpi"]["yield"].as_f64().is_some(), "{query}");
                        assert!(
                            v["annotations"].as_array().unwrap().len() <= 1000,
                            "{query}"
                        );
                    }
                    // The budget holds in release builds; others only report.
                    if !cfg!(debug_assertions) {
                        assert!(took.as_millis() <= 50, "{label} {query}: {took:?}");
                    }
                    // The first read of a page's sealed days fills the
                    // cache; the next reads them from it, alike.
                    if i == 0 {
                        bodies.push(body);
                    } else {
                        assert_eq!(bodies.last(), Some(&body), "{label} {query}");
                    }
                }
            }
            bodies
        }
    };
    let hourly = timed("hourly").await;
    // The nightly compaction's first run on the year, as the recorder
    // makes it: every response over compacted days is byte for byte the
    // same.
    let kind = |m: u32| {
        Some(if m == 13 {
            lodestar_store::Aggregation::Last
        } else {
            lodestar_store::Aggregation::Sum
        })
    };
    let r = f
        .store()
        .compact_rollups(lodestar_store::CompactOptions {
            now: NOW,
            keep_days: 30,
            hold_from_day: None,
            budget: std::time::Duration::from_secs(600),
            kind: &kind,
        })
        .unwrap();
    assert!(r.done && r.days >= 330, "{r:?}");
    f.source.cache.forget_days_before(r.compacted_before_day);
    let daily = timed("compacted").await;
    for ((q, a), b) in queries.iter().zip(&hourly).zip(&daily) {
        assert_eq!(a, b, "{q}: the response changed with compaction");
    }
    // A short range over compacted days is read by day.
    let from = (NOW_H - 60 * 24) * 3600;
    let v = json_get(
        &f,
        &cookie,
        &format!(
            "/api/v1/insights/overview?range=custom&from={from}&to={}",
            from + 3 * 86_400
        ),
    )
    .await;
    assert_eq!(v["range"]["bucket"], "day");
    assert_eq!(v["trend"].as_array().unwrap().len(), 4);
}

/// Every stage page, against its golden: 24 h, Compare, a `reason`, and
/// 30 days. The story's numbers read back, nulls where nothing was
/// measured or counted, and Adult left out of per-type lists.
#[tokio::test]
async fn stage_goldens() {
    let f = fixture();
    story(&f);
    let cookie = session(&f).await;
    let mut pages = BTreeMap::new();
    for (name, uri) in [
        ("discover_24h", "discover?range=24h"),
        ("discover_7d_compare", "discover?range=7d&compare=1"),
        ("fetch_24h", "fetch?range=24h"),
        ("fetch_24h_timed_out", "fetch?range=24h&reason=timed_out"),
        ("fetch_7d", "fetch?range=7d"),
        ("understand_24h", "understand?range=24h"),
        ("index_24h", "index?range=24h"),
        ("index_30d_compare", "index?range=30d&compare=1"),
        ("serve_24h", "serve?range=24h"),
    ] {
        let v = golden(&f, &cookie, name, &format!("/api/v1/insights/{uri}")).await;
        pages.insert(name, v);
    }
    let d = &pages["discover_24h"];
    assert_eq!(d["kpi"]["reported"], 117);
    assert_eq!(d["kpi"]["rediscovered"], 12);
    assert_eq!(d["kpi"]["dropped"], 1);
    assert_eq!(
        d["kpi"]["harvested"], 17,
        "first harvests, as the overview's"
    );
    assert_eq!(d["kpi"]["dht_nodes"], Value::Null, "the DHT is off");
    assert_eq!(d["by_ip_family"], Value::Null);
    assert_eq!(
        d["by_strategy"],
        json!([{"key": "sample", "reported": 90}, {"key": "get_peers", "reported": 20},
               {"key": "bitmagnet:0", "reported": 5}, {"key": "refetch", "reported": 2}]),
        "another source's code isn't a DHT strategy"
    );
    assert_eq!(d["by_source"][0]["key"], "dht");
    // The paused hours are gaps; a measured quiet hour is 0.
    let at = |v: &Value, h: i64| -> Value {
        v["trend"]
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["at"] == (NOW_H - h) * 3600)
            .cloned()
            .unwrap()
    };
    assert_eq!(at(d, 9)["reported"], Value::Null, "paused");
    assert_eq!(at(d, 12)["reported"], 0, "measured, nothing");
    let fe = &pages["fetch_24h"];
    assert_eq!(fe["kpi"]["fetched"], 7);
    assert_eq!(fe["kpi"]["lost"], 3);
    assert_eq!(fe["kpi"]["queue_depth"], 0, "a snapshot");
    assert_eq!(fe["kpi"]["fetch_latency_p50_ms"], Value::Null);
    assert_eq!(fe["losses"][0]["count"], 1);
    let t = &pages["fetch_24h_timed_out"];
    assert_eq!(t["reason"], "timed_out");
    assert_eq!(t["kpi"]["lost"], 1);
    assert_eq!(t["losses"].as_array().unwrap().len(), 1);
    assert_eq!(
        t["funnel_total"]["losses"]
            .as_object()
            .unwrap()
            .keys()
            .collect::<Vec<_>>(),
        ["timed_out"]
    );
    // Fetches have been counted for two days only: a week's are unknown.
    let w = &pages["fetch_7d"];
    assert_eq!(w["kpi"]["fetched"], Value::Null);
    assert_eq!(w["losses"], Value::Null);
    assert_eq!(at(w, 100)["fetched"], Value::Null, "before counting began");
    let u = &pages["understand_24h"];
    assert_eq!(u["kpi"]["stored"], 6, "the 80 h old cohort is outside");
    assert_eq!(u["kpi"]["blocked"], 1);
    assert_eq!(u["kpi"]["parser_confidence"], Value::Null);
    let keys = |v: &Value| -> Vec<String> {
        v.as_array()
            .unwrap()
            .iter()
            .map(|r| r["key"].as_str().unwrap().to_string())
            .collect()
    };
    assert!(!keys(&u["by_content_type"]).contains(&"adult".to_string()));
    assert!(!keys(&u["by_group"]).contains(&"adult".to_string()));
    let i = &pages["index_24h"];
    assert_eq!(i["kpi"]["searchable"], 1000);
    assert_eq!(i["kpi"]["removed"], 3, "totals count Adult (allowed)");
    assert_eq!(
        i["removed_by_type"],
        json!([{"key": "movie", "removed": 2}])
    );
    assert_eq!(at(i, 1)["searchable"], 1000);
    assert_eq!(at(i, 3)["searchable"], 995);
    assert_eq!(at(i, 2)["searchable"], Value::Null, "no snapshot that hour");
    assert_eq!(i["kpi"]["disk_free_bytes"], Value::Null);
    let c = &pages["index_30d_compare"];
    assert_eq!(c["range"]["bucket"], "day");
    assert_eq!(c["previous"]["range"]["to"], c["range"]["from"]);
    let sv = &pages["serve_24h"];
    assert_eq!(sv["kpi"]["searches"], Value::Null);
    assert_eq!(sv["top_queries"], Value::Null);
    assert!(
        sv["trend"]
            .as_array()
            .unwrap()
            .iter()
            .all(|p| p["searches"].is_null())
    );
    // The pages agree with the overview's stage strip and KPIs.
    let o = json_get(&f, &cookie, "/api/v1/insights/overview?range=24h").await;
    assert_eq!(pages["fetch_24h"]["kpi"]["fetched"], o["stages"]["fetch"]);
    assert_eq!(u["kpi"]["stored"], o["stages"]["understand"]);
    assert_eq!(i["kpi"]["new_searchable"], o["kpi"]["new_searchable"]);
    // With Adult shown, it's listed.
    f.adult.store(true, Ordering::Relaxed);
    f.touched();
    let i = json_get(&f, &cookie, "/api/v1/insights/index?range=24h").await;
    assert_eq!(
        i["removed_by_type"],
        json!([{"key": "movie", "removed": 2}, {"key": "adult", "removed": 1}])
    );
}

/// The overview's Compare deltas and previous segments.
#[tokio::test]
async fn compare_adds_deltas_and_previous_segments() {
    let f = fixture();
    story(&f);
    let cookie = session(&f).await;
    let v = json_get(&f, &cookie, "/api/v1/insights/overview?range=7d&compare=1").await;
    let k = &v["kpi"];
    let p = &v["previous"]["kpi"];
    assert_eq!(
        k["new_searchable_delta"],
        k["new_searchable"].as_i64().unwrap() - p["new_searchable"].as_i64().unwrap()
    );
    assert_eq!(k["searchable_delta"], 100, "1,000 now, 900 then");
    // No final cohort in the previous week: nothing to subtract.
    assert_eq!(k["yield_delta"], Value::Null);
    assert_eq!(k["time_to_searchable_p50_delta_s"], Value::Null);
    assert!(v["previous"]["segments"]["by_group"].is_array());
    let day = json_get(&f, &cookie, "/api/v1/insights/overview?range=24h").await;
    for field in [
        "new_searchable_delta",
        "yield_delta",
        "time_to_searchable_p50_delta_s",
        "time_to_searchable_p95_delta_s",
    ] {
        assert_eq!(day["kpi"][field], Value::Null, "{field} without compare");
    }
}

async fn send_full(f: &Fixture, mut req: Request<Body>) -> (u16, axum::http::HeaderMap, String) {
    req.extensions_mut()
        .insert(ConnectInfo(SocketAddr::from(([10, 9, 0, 1], 1))));
    let resp = f.app.clone().oneshot(req).await.unwrap();
    let status = resp.status().as_u16();
    let headers = resp.headers().clone();
    let bytes = axum::body::to_bytes(resp.into_body(), 16 << 20)
        .await
        .unwrap();
    (status, headers, String::from_utf8(bytes.to_vec()).unwrap())
}

/// Every panel's CSV against its golden, the attachment headers, and
/// the CSV holding the JSON's numbers.
#[tokio::test]
async fn export_goldens() {
    let f = fixture();
    story(&f);
    let cookie = session(&f).await;
    for panel in [
        "trend",
        "funnel",
        "losses",
        "segments.by_group",
        "segments.by_content_type",
        "segments.by_source",
        "exceptions",
        "annotations",
        "discover.trend",
        "discover.by_source",
        "discover.by_strategy",
        "fetch.trend",
        "fetch.losses",
        "fetch.funnel",
        "understand.trend",
        "understand.by_content_type",
        "understand.by_group",
        "index.trend",
        "index.removed_by_type",
        "serve.trend",
    ] {
        let uri = format!("/api/v1/insights/export.csv?panel={panel}&range=24h");
        let (status, headers, body) = send_full(&f, get(&uri, Some(&cookie))).await;
        assert_eq!(status, 200, "{panel}: {body}");
        assert_eq!(
            headers[header::CONTENT_TYPE],
            "text/csv; charset=utf-8; header=present"
        );
        assert_eq!(
            headers[header::CONTENT_DISPOSITION],
            format!(
                "attachment; filename=\"lodestar-insights-{panel}-{}-{}.csv\"",
                (NOW_H - 23) * 3600,
                (NOW_H + 1) * 3600
            )
            .as_str()
        );
        assert_eq!(headers[header::CACHE_CONTROL], "no-store");
        assert_eq!(headers["x-content-type-options"], "nosniff");
        assert!(
            body.ends_with("\r\n") && body.starts_with("period"),
            "{panel}"
        );
        let head = body.lines().next().unwrap();
        assert!(
            head.split(',').all(|h| !h.is_empty()
                && h.bytes()
                    .all(|b| matches!(b, b'a'..=b'z' | b'0'..=b'9' | b'_' | b'.'))),
            "{panel}: header {head:?}"
        );
        assert_eq!(
            body.matches("\r\n").count(),
            body.matches('\n').count(),
            "{panel}: every line ends with CRLF"
        );
        // A range with nothing in it has the same header: each panel
        // declares every column its rows carry.
        let empty =
            format!("/api/v1/insights/export.csv?panel={panel}&range=custom&from=3600&to=7200");
        let (status, _, none) = send_full(&f, get(&empty, Some(&cookie))).await;
        assert_eq!(status, 200, "{panel}: {none}");
        assert_eq!(
            none.lines().next(),
            body.lines().next(),
            "{panel}: the header depends on the rows"
        );
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/golden")
            .join(format!("insights_export_{}.txt", panel.replace('.', "_")));
        if std::env::var_os("LODESTAR_BLESS").is_some() {
            std::fs::write(&path, &body).unwrap();
        } else {
            let want = std::fs::read_to_string(&path).unwrap_or_else(|e| {
                panic!("{}: {e} (bless with LODESTAR_BLESS=1)", path.display())
            });
            // Git may check the golden out with LF line ends; the body's
            // are checked apart.
            assert_eq!(
                body.replace("\r\n", "\n"),
                want.replace("\r\n", "\n"),
                "{panel}: the CSV changed"
            );
        }
    }
    // With Compare, the previous period's rows follow, marked.
    let (_, _, body) = send_full(
        &f,
        get(
            "/api/v1/insights/export.csv?panel=index.trend&range=24h&compare=1",
            Some(&cookie),
        ),
    )
    .await;
    let periods: Vec<&str> = body
        .lines()
        .skip(1)
        .map(|l| l.split(',').next().unwrap())
        .collect();
    assert_eq!(periods.iter().filter(|p| **p == "current").count(), 24);
    assert_eq!(periods.iter().filter(|p| **p == "previous").count(), 24);
    // The CSV's numbers are the JSON's: nulls empty, never 0.
    let json = json_get(&f, &cookie, "/api/v1/insights/fetch?range=7d").await;
    let (_, _, csv) = send_full(
        &f,
        get(
            "/api/v1/insights/export.csv?panel=fetch.trend&range=7d",
            Some(&cookie),
        ),
    )
    .await;
    let header: Vec<&str> = csv.lines().next().unwrap().split(',').collect();
    assert_eq!(header, ["period", "at", "at_utc", "fetched", "lost"]);
    for (line, point) in csv.lines().skip(1).zip(json["trend"].as_array().unwrap()) {
        let cells: Vec<&str> = line.split(',').collect();
        assert_eq!(cells[1], point["at"].to_string());
        let want = |v: &Value| {
            if v.is_null() {
                String::new()
            } else {
                v.to_string()
            }
        };
        assert_eq!(cells[3], want(&point["fetched"]));
        assert_eq!(cells[4], want(&point["lost"]));
    }
    // A filtered Fetch page's CSV holds what its chart shows.
    let page = json_get(
        &f,
        &cookie,
        "/api/v1/insights/fetch?range=24h&reason=timed_out",
    )
    .await;
    let (_, _, csv) = send_full(
        &f,
        get(
            "/api/v1/insights/export.csv?panel=fetch.losses&range=24h&reason=timed_out",
            Some(&cookie),
        ),
    )
    .await;
    assert_eq!(
        csv.lines().count(),
        1 + page["losses"].as_array().unwrap().len()
    );
    assert!(
        csv.contains(",timed_out,") && !csv.contains("blocked"),
        "{csv}"
    );
    // Adult is left out of a per-type CSV while hidden.
    let (_, _, csv) = send_full(
        &f,
        get(
            "/api/v1/insights/export.csv?panel=index.removed_by_type&range=24h",
            Some(&cookie),
        ),
    )
    .await;
    assert!(!csv.contains("adult"), "{csv}");
}

/// `insights` events (§10.9.6): the default overview's stage strip, sent
/// every `insights_every` only while someone listens, with the nullable
/// stages as `null`.
#[tokio::test]
async fn insights_events_go_out_only_while_someone_listens() {
    use lodestar_api::events::{EventHub, EventSources, EventsConfig};
    let f = fixture();
    story(&f);
    let cookie = session(&f).await;
    let every = std::time::Duration::from_millis(100);
    let hub = EventHub::start(
        EventsConfig {
            insights_every: every,
            ..EventsConfig::default()
        },
        EventSources {
            insights: Some(f.state.clone()),
            ..EventSources::default()
        },
    );
    // Several ticks with nobody listening: nothing is built or sent.
    // Time passing for the code under test: ticks must elapse unheard.
    tokio::time::sleep(every * 4).await;
    let app = Router::new()
        .nest("/api/v1", router(f.state.clone()))
        .layer(axum::Extension(hub.clone()));
    let mut req = get("/api/v1/events?topics=insights", Some(&cookie));
    req.extensions_mut()
        .insert(ConnectInfo(SocketAddr::from(([10, 9, 0, 1], 1))));
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), 200);
    let mut body = resp.into_body().into_data_stream();
    let mut text = String::new();
    let start = Instant::now();
    while text.matches("event: insights").count() < 2 {
        assert!(
            start.elapsed() < std::time::Duration::from_secs(15),
            "{text}"
        );
        let next = std::future::poll_fn(|cx| {
            futures_core::Stream::poll_next(std::pin::Pin::new(&mut body), cx)
        });
        let chunk = tokio::time::timeout(std::time::Duration::from_secs(15), next)
            .await
            .expect("an event")
            .expect("open")
            .unwrap();
        text.push_str(std::str::from_utf8(&chunk).unwrap());
    }
    // The first event published is the first one sent: none went out
    // while nobody listened.
    assert!(
        text.contains(&format!("id: {}-1\nevent: insights\n", hub.epoch())),
        "{text}"
    );
    let data = text.lines().find_map(|l| l.strip_prefix("data: ")).unwrap();
    let v: Value = serde_json::from_str(data).unwrap();
    assert_eq!(v["updated_at"], NOW);
    // The 7-day range starts before fetches were counted: null, not 0.
    assert_eq!(v["stages"]["fetch"], Value::Null);
    assert_eq!(v["stages"]["serve"], Value::Null);
    assert_eq!(v["stages"]["index"], 1000);
    assert_eq!(v["stages"]["discover"], 27, "the week's first harvests");
    hub.close();
}

/// Settings that answer only the privacy summary (S-privacy-banner).
#[derive(Debug, Default)]
struct PrivacySettings(Mutex<PrivacySummary>);

impl SettingsApi for PrivacySettings {
    fn index(&self) -> SettingsIndex {
        SettingsIndex {
            sections: Vec::new(),
        }
    }
    fn section(&self, _: &str) -> Option<SectionValues> {
        None
    }
    fn save(&self, _: SettingsBatch) -> Result<Saved, SettingsFailure> {
        unreachable!("the overview doesn't save")
    }
    fn preview(&self, _: SettingsBatch) -> Result<SettingsPreview, SettingsFailure> {
        unreachable!("the overview doesn't preview")
    }
    fn export(&self, _: bool) -> String {
        unreachable!("the overview doesn't export")
    }
    fn import(&self, _: SettingsImport, _: bool) -> Result<Imported, SettingsFailure> {
        unreachable!("the overview doesn't import")
    }
    fn privacy(&self, _: i64) -> PrivacySummary {
        *self.0.lock().unwrap()
    }
}

#[tokio::test]
async fn the_overview_carries_the_privacy_summary_to_a_session_only() {
    let mut f = fixture();
    let cookie = session(&f).await;
    // No settings wired: nothing can be on.
    let o = json_get(&f, &cookie, "/api/v1/insights/overview").await;
    assert_eq!(
        o["privacy"],
        json!({ "identifying_until": null, "query_logging": false })
    );

    let settings = Arc::new(PrivacySettings::default());
    let mut state = f.state.clone();
    state.settings = Some(settings.clone() as Arc<dyn SettingsApi>);
    f.app = Router::new().nest("/api/v1", router(state));
    *settings.0.lock().unwrap() = PrivacySummary {
        identifying_until: Some(NOW + 3600),
        query_logging: true,
    };
    let o = json_get(&f, &cookie, "/api/v1/insights/overview").await;
    // Booleans and a time, nothing else.
    assert_eq!(
        o["privacy"],
        json!({ "identifying_until": NOW + 3600, "query_logging": true })
    );
    // It follows the setting at each request, never from the rollups' cache.
    *settings.0.lock().unwrap() = PrivacySummary {
        identifying_until: Some(-1),
        query_logging: false,
    };
    let o = json_get(&f, &cookie, "/api/v1/insights/overview").await;
    assert_eq!(o["privacy"]["identifying_until"], -1);

    // Without a session: the same 401 as before, with nothing about privacy.
    let (status, body) = send(&f, get("/api/v1/insights/overview", None)).await;
    assert_eq!(status, 401);
    assert!(
        !body.contains("privacy") && !body.contains("query_logging"),
        "{body}"
    );
    let (status, _) = send(
        &f,
        get("/api/v1/insights/overview", Some("lodestar_session=forged")),
    )
    .await;
    assert_eq!(status, 401);
}
