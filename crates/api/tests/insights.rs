//! `/insights/overview` and `/insights/events` through the router, over a
//! real store and fakes of the running process: the session guard, the
//! error envelope, the reconciliation rules of §10.17 against independent
//! counts, gaps against zeros, Compare, Adult, the stale flag, the
//! exceptions, the cache, and the 50 ms budget on a year of hourly
//! rollups. Response bytes are checked against `golden/insights_<name>.txt`;
//! `LODESTAR_BLESS=1 cargo test -p lodestar-api --test insights` rewrites
//! them (read the diff before committing it).

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

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
    let app = Router::new().nest(
        "/api/v1",
        router(ApiState {
            auth: auth.clone(),
            index: None,
            reader: Arc::new(Mutex::new(reader)),
            filters: None,
            settings: None,
            ops: Some(ops.clone() as Arc<dyn OpsApi>),
            adult: AdultVisibility::follow(move || shown.load(Ordering::Relaxed)),
            insights: insights.then(|| source.clone() as Arc<dyn InsightsSource>),
        }),
    );
    Fixture {
        app,
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
    golden(
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
    assert_eq!(day["kpi"]["searchable_delta"], Value::Null);
    assert!(day.get("previous").is_none());
    assert_eq!(day["stages"]["fetch"], Value::Null);
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
    assert_eq!(at(NOW_H - 5)["searchable"], 6);
    assert_eq!(at(NOW_H - 5)["harvested"], 8);
    assert_eq!(at(NOW_H - 2)["searchable"], 1);
    assert_eq!(v["ingest_per_hour"], 0, "the last full hour was measured");
    let kinds: Vec<&str> = v["annotations"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["kind"].as_str().unwrap())
        .collect();
    assert_eq!(kinds, ["restart", "paused", "resumed", "intensity"]);
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
    let mut sum_new = 0;
    for p in v["trend"].as_array().unwrap() {
        sum_new += p["searchable"].as_u64().unwrap_or(0);
    }
    let all_searchable = c
        .query_row(
            "SELECT count(*) FROM torrents WHERE id <= (SELECT max(last_id) FROM index_watermark_hour)",
            [],
            |r| r.get::<_, i64>(0),
        )
        .unwrap() as u64;
    assert_eq!(sum_new, all_searchable, "imports are newly searchable too");
    assert_eq!(v["kpi"]["new_searchable"], all_searchable);

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
    assert_eq!(v["kpi"]["searchable_delta"], 3);
    assert_eq!(p["kpi"]["searchable"], 997);
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
            // newly searchable by type (10), the delay histogram (11) and
            // coverage (12): about 100 rows an hour.
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
    let warmed = lodestar_api::insights::warm(&f.store(), &f.source.cache, NOW).unwrap();
    eprintln!("warm ({warmed} days): {:?}", cold.elapsed());
    assert!(warmed >= 355);
    for query in [
        "range=24h",
        "range=7d",
        "range=7d&compare=1",
        "range=30d",
        "range=30d&compare=1",
        "range=all",
        "range=all&compare=1",
    ] {
        let uri = format!("/api/v1/insights/overview?{query}");
        for _ in 0..3 {
            // A new flush each time: never a cached response.
            f.touched();
            let start = Instant::now();
            let v = json_get(&f, &cookie, &uri).await;
            let took = start.elapsed();
            eprintln!("{query}: {took:?}");
            assert!(v["kpi"]["yield"].as_f64().is_some(), "{query}");
            // The budget holds in release builds; others only report.
            if !cfg!(debug_assertions) {
                assert!(took.as_millis() <= 50, "{query}: {took:?}");
            }
        }
    }
}
