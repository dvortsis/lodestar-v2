//! Insights' worker dimension (architecture §19.7; WK-15) through the
//! router, over a real store: `by_worker` on Discover and Fetch, the
//! marginal gain, the fleet summary for the status pill, single-machine
//! parity, the privacy rule (counts only), and the same numbers whether a
//! day is read live, from the sealed-day cache or compacted.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Request, header};
use axum::{Extension, Router};
use lodestar_api::dto::workers::{
    DhtPin, Worker, WorkerBudget, WorkerIntensity, WorkerList, WorkerPatch, WorkerRevoked,
    WorkerStatus,
};
use lodestar_api::insights::{InsightsSource, OverviewCache};
use lodestar_api::ops::{
    BoxFuture, CrawlerRuntime, ExternalAddress, FetchStatus, NetworkCheck, OpsApi, OpsError,
    PortMapping, ReachReason, Source, SourcePatch, StatsResponse,
};
use lodestar_api::workers::{NewToken, WorkersApi, WorkersError};
use lodestar_api::{AdultVisibility, ApiState, Auth, router};
use lodestar_store::{CompactOptions, Counter, Store, StoreConfig, WriteBatch};
use serde_json::{Value, json};
use tower::ServiceExt;

const NOW_H: i64 = 500_000;
const NOW: i64 = NOW_H * 3600 + 1800;
/// `metric::REDISCOVERED`, `DROPPED`, `COVERAGE`, `BY_WORKER`, `WORKER_LEASES`.
const REDISCOVERED: u32 = 2;
const DROPPED: u32 = 3;
const COVERAGE: u32 = 12;
const BY_WORKER: u32 = 17;
const LEASES: u32 = 18;
/// `marginal::key`.
const GRANTED: i64 = 0;
const MARGINAL: i64 = 1;
const COMMITTED: i64 = 2;
const LOST: i64 = 3;
const OWN_REDISCOVERED: i64 = 4;
const OWN_DROPPED: i64 = 5;

fn by_worker_key(metric: u32, dim: u32, key: i64) -> i64 {
    (i64::from(metric) << 16) | (i64::from(dim) << 8) | key
}

#[derive(Debug)]
struct FakeOps;

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
                index_lag_s: Some(0),
                paused: false,
                queue_depth: 0,
                sampling_blocked: None,
                searchable_torrents: Some(0),
                stored_torrents: 0,
                updated_at: NOW,
                uptime_s: 0,
            }
        })
    }
    fn sources(&self) -> BoxFuture<'_, Result<Vec<Source>, OpsError>> {
        Box::pin(async { Ok(Vec::new()) })
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

#[derive(Debug, Default)]
struct FakeSource {
    recorded: AtomicI64,
    cache: OverviewCache,
}

impl InsightsSource for FakeSource {
    fn last_flush(&self) -> Option<i64> {
        Some(NOW - 3)
    }
    fn started_at(&self) -> i64 {
        NOW - 86_400
    }
    fn last_recorded(&self) -> i64 {
        self.recorded.load(Ordering::Relaxed)
    }
    fn imported(&self) -> Option<u64> {
        Some(0)
    }
    fn cache(&self) -> &OverviewCache {
        &self.cache
    }
    fn now(&self) -> i64 {
        NOW
    }
}

/// `/workers` as the binary would answer, with these workers.
#[derive(Debug)]
struct Fleet(Vec<Worker>);

impl WorkersApi for Fleet {
    fn list(&self) -> BoxFuture<'_, Result<WorkerList, WorkersError>> {
        Box::pin(async move {
            Ok(WorkerList {
                allowed: true,
                workers: self.0.clone(),
            })
        })
    }
    fn create_token(&self, _: String) -> BoxFuture<'_, Result<NewToken, WorkersError>> {
        Box::pin(async { Err(WorkersError::Off) })
    }
    fn drain(&self, _: u32) -> BoxFuture<'_, Result<Worker, WorkersError>> {
        Box::pin(async { Err(WorkersError::UnknownWorker) })
    }
    fn resume(&self, _: u32) -> BoxFuture<'_, Result<Worker, WorkersError>> {
        Box::pin(async { Err(WorkersError::UnknownWorker) })
    }
    fn pause(&self, _: u32) -> BoxFuture<'_, Result<Worker, WorkersError>> {
        Box::pin(async { Err(WorkersError::UnknownWorker) })
    }
    fn revoke(&self, _: u32) -> BoxFuture<'_, Result<WorkerRevoked, WorkersError>> {
        Box::pin(async { Err(WorkersError::UnknownWorker) })
    }
    fn update(&self, _: u32, _: WorkerPatch) -> BoxFuture<'_, Result<Worker, WorkersError>> {
        Box::pin(async { Err(WorkersError::UnknownWorker) })
    }
}

fn worker(id: u32, status: WorkerStatus) -> Worker {
    Worker {
        budget: WorkerBudget {
            cpu_permille: 1000,
            down_kbps: 0,
            ram_mb: 0,
            up_kbps: 0,
        },
        claimed: Some(10),
        clock_offset_ms: Some(0),
        clock_skewed: false,
        cpu_permille: None,
        dht: DhtPin::Auto,
        external_ips: vec!["81.2.69.7".into()],
        fetched: Some(8),
        id,
        intensity: WorkerIntensity::Balanced,
        last_seen_at: None,
        leases: 0,
        name: Some(format!("worker-{id}")),
        paused: status == WorkerStatus::Paused,
        pool_cap: 50_000,
        processed: None,
        rss_mb: None,
        spool_bytes: None,
        status,
    }
}

struct Fixture {
    app: Router,
    auth: Arc<Auth>,
    source: Arc<FakeSource>,
    db: PathBuf,
    _dir: tempfile::TempDir,
}

fn fixture(fleet: Option<Vec<Worker>>) -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("t.db");
    drop(Store::open(&db, StoreConfig::default()).unwrap());
    let reader = Store::open_reader(&db, StoreConfig::default()).unwrap();
    let auth = Arc::new(Auth::load(&dir.path().join("auth.json"), false).unwrap());
    let source = Arc::new(FakeSource::default());
    let state = ApiState {
        auth: auth.clone(),
        index: None,
        reader: Arc::new(Mutex::new(reader)),
        filters: None,
        settings: None,
        ops: Some(Arc::new(FakeOps) as Arc<dyn OpsApi>),
        adult: AdultVisibility::follow(|| false),
        insights: Some(source.clone() as Arc<dyn InsightsSource>),
        user_data: None,
        hot: None,
        events: None,
    };
    let mut app = Router::new().nest("/api/v1", router(state));
    if let Some(fleet) = fleet {
        app = app.layer(Extension(Arc::new(Fleet(fleet)) as Arc<dyn WorkersApi>));
    }
    Fixture {
        app,
        auth,
        source,
        db,
        _dir: dir,
    }
}

impl Fixture {
    fn write(&self, counters: &[Counter]) {
        Store::open(&self.db, StoreConfig::default())
            .unwrap()
            .write(&WriteBatch {
                counters,
                ..WriteBatch::default()
            })
            .unwrap();
        self.source.recorded.fetch_add(1, Ordering::Relaxed);
    }

    async fn session(&self) -> String {
        let token = self.auth.setup_token().unwrap();
        let req = Request::post("/api/v1/setup")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                json!({ "token": token, "password": "correct horse battery", "notice_version": lodestar_api::legal::NOTICE_VERSION }).to_string(),
            ))
            .unwrap();
        let resp = self.send(req).await;
        let cookie = resp.headers()[header::SET_COOKIE].to_str().unwrap();
        cookie.split(';').next().unwrap().to_string()
    }

    async fn send(&self, mut req: Request<Body>) -> axum::response::Response {
        req.extensions_mut()
            .insert(ConnectInfo(SocketAddr::from(([10, 9, 0, 1], 1))));
        self.app.clone().oneshot(req).await.unwrap()
    }

    async fn get(&self, cookie: &str, uri: &str) -> Value {
        let req = Request::get(format!("/api/v1{uri}"))
            .header(header::COOKIE, cookie)
            .body(Body::empty())
            .unwrap();
        let resp = self.send(req).await;
        let status = resp.status().as_u16();
        let bytes = axum::body::to_bytes(resp.into_body(), 16 << 20)
            .await
            .unwrap();
        let body = String::from_utf8(bytes.to_vec()).unwrap();
        assert_eq!(status, 200, "{uri}: {body}");
        serde_json::from_str(&body).unwrap()
    }
}

fn c(hour: i64, metric: u32, dim: u32, key: i64, value: i64) -> Counter {
    Counter {
        hour,
        metric,
        dim,
        key,
        value,
    }
}

/// One hour of a fleet: the built-in crawler (0), worker 3, and worker 4
/// whose only key was claimed by another worker too (its marginal taken
/// back in the same hour). The fleet's rediscovered and dropped totals
/// include the remote workers' own counters, which the remote intake also
/// sums per worker under `WORKER_LEASES` (keys 4 and 5). Their packed
/// `BY_WORKER` rows aren't read by these pages.
fn fleet_hour(h: i64) -> Vec<Counter> {
    vec![
        c(h, COVERAGE, 1, 0, 3600),
        c(h, REDISCOVERED, 0, 1, 50),
        c(h, DROPPED, 0, 0, 5),
        c(h, BY_WORKER, 3, by_worker_key(REDISCOVERED, 0, 1), 20),
        c(h, BY_WORKER, 3, by_worker_key(DROPPED, 0, 0), 2),
        c(h, BY_WORKER, 4, by_worker_key(REDISCOVERED, 0, 1), 10),
        c(h, LEASES, 3, OWN_REDISCOVERED, 20),
        c(h, LEASES, 3, OWN_DROPPED, 2),
        c(h, LEASES, 4, OWN_REDISCOVERED, 10),
        c(h, LEASES, 0, GRANTED, 30),
        c(h, LEASES, 0, MARGINAL, 25),
        c(h, LEASES, 0, COMMITTED, 20),
        c(h, LEASES, 0, LOST, 5),
        c(h, LEASES, 3, GRANTED, 40),
        c(h, LEASES, 3, MARGINAL, 10),
        c(h, LEASES, 3, COMMITTED, 30),
        c(h, LEASES, 3, LOST, 8),
        c(h, LEASES, 4, GRANTED, 1),
        c(h, LEASES, 4, MARGINAL, 1),
        c(h, LEASES, 4, MARGINAL, 0),
    ]
}

/// Discover's and Fetch's `by_worker` for one copy of [`fleet_hour`], `n`
/// times.
fn expected(n: u64) -> (Value, Value) {
    let d = |w: u32, red: u64, drop: u64, granted: u64, marginal: u64| {
        json!({ "dropped": drop * n, "granted": granted * n, "marginal": marginal * n,
                "rediscovered": red * n, "worker": w })
    };
    let f = |w: u32, committed: u64, lost: u64| json!({ "committed": committed * n, "lost": lost * n, "worker": w });
    (
        json!([d(0, 20, 3, 30, 25), d(3, 20, 2, 40, 10), d(4, 10, 0, 1, 1)]),
        json!([f(0, 20, 5), f(3, 30, 8), f(4, 0, 0)]),
    )
}

/// The privacy rule: a worker's entry is numbers only, under these names.
fn counts_only(list: &Value, fields: &[&str]) {
    for entry in list.as_array().unwrap() {
        let obj = entry.as_object().unwrap();
        let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(keys, fields);
        assert!(obj.values().all(Value::is_u64), "{entry}");
    }
}

/// Single-machine parity: with no worker rows the stage pages have no
/// `by_worker` at all, and the summary's `fleet` is null, with or without
/// a cluster.
#[tokio::test]
async fn a_single_machine_shows_nothing_new() {
    for fleet in [None, Some(Vec::new())] {
        let f = fixture(fleet);
        let h = NOW_H - 2;
        f.write(&[
            c(h, COVERAGE, 1, 0, 3600),
            c(h, REDISCOVERED, 0, 1, 50),
            c(h, DROPPED, 0, 0, 5),
        ]);
        let cookie = f.session().await;
        for page in ["discover", "fetch"] {
            let v = f.get(&cookie, &format!("/insights/{page}?range=24h")).await;
            assert!(v.get("by_worker").is_none(), "{page}: {v}");
        }
        let v = f.get(&cookie, "/insights/workers/summary").await;
        assert_eq!(v, json!({ "fleet": null }));
    }
}

/// Each worker's counts, the built-in crawler's derived from the fleet's
/// totals, marginal gain net of take-backs, and nothing but counts.
#[tokio::test]
async fn discover_and_fetch_break_down_by_worker() {
    let f = fixture(None);
    f.write(&fleet_hour(NOW_H - 3));
    f.write(&fleet_hour(NOW_H - 2));
    let cookie = f.session().await;
    let discover = f.get(&cookie, "/insights/discover?range=24h").await;
    let fetch = f
        .get(&cookie, "/insights/fetch?range=24h&reason=timed_out")
        .await;
    let (d, fe) = expected(2);
    assert_eq!(discover["by_worker"], d);
    assert_eq!(fetch["by_worker"], fe, "`reason` doesn't narrow it");
    counts_only(
        &discover["by_worker"],
        &["dropped", "granted", "marginal", "rediscovered", "worker"],
    );
    counts_only(&fetch["by_worker"], &["committed", "lost", "worker"]);
    // The fleet's own numbers are as before.
    assert_eq!(discover["kpi"]["rediscovered"], 100);
    assert_eq!(discover["kpi"]["dropped"], 10);
}

/// A sealed day read live, then from the sealed-day cache, then compacted
/// into `rollup_day`, gives the same per-worker numbers.
#[tokio::test]
async fn sealed_cached_and_compacted_days_read_alike() {
    let f = fixture(None);
    // 12 and 40 days back: sealed (over 8 days), and compactable (30).
    let hours: Vec<i64> = [12, 40].iter().map(|d| (NOW_H / 24 - d) * 24 + 5).collect();
    for &h in &hours {
        f.write(&fleet_hour(h));
    }
    let cookie = f.session().await;
    let read = |label: &'static str| {
        let (f, cookie) = (&f, &cookie);
        async move {
            // A new flush each time: never a cached response.
            f.source.recorded.fetch_add(1, Ordering::Relaxed);
            let d = f.get(cookie, "/insights/discover?range=all").await;
            let fe = f.get(cookie, "/insights/fetch?range=all").await;
            let (wd, wf) = expected(2);
            assert_eq!(d["by_worker"], wd, "{label}");
            assert_eq!(fe["by_worker"], wf, "{label}");
        }
    };
    read("live").await;
    read("cached").await;
    let mut store = Store::open(&f.db, StoreConfig::default()).unwrap();
    let kind = |_| Some(lodestar_store::Aggregation::Sum);
    let r = store
        .compact_rollups(CompactOptions {
            now: NOW,
            keep_days: 30,
            hold_from_day: None,
            budget: std::time::Duration::from_secs(60),
            kind: &kind,
        })
        .unwrap();
    assert!(r.done && r.days >= 1, "{r:?}");
    f.source.cache.forget_days_before(r.compacted_before_day);
    read("compacted").await;
}

/// The pill: workers working (online and draining), paused and offline
/// ones apart, and the fleet's commits in the last full hour: their sum,
/// 0 for a measured hour without one, null for an unmeasured hour.
#[tokio::test]
async fn the_fleet_summary_counts_states_and_the_last_hour() {
    let fleet = vec![
        worker(3, WorkerStatus::Online),
        worker(4, WorkerStatus::Paused),
        worker(5, WorkerStatus::Offline),
        worker(6, WorkerStatus::Draining),
        worker(7, WorkerStatus::Online),
    ];
    let f = fixture(Some(fleet));
    let cookie = f.session().await;
    let summary = |n: Value| {
        json!({ "fleet": { "committed_per_hour": n, "draining": 1, "offline": 1,
                           "online": 2, "paused": 1, "workers": 3 } })
    };
    assert_eq!(
        f.get(&cookie, "/insights/workers/summary").await,
        summary(Value::Null)
    );
    f.write(&[c(NOW_H - 1, COVERAGE, 1, 0, 3600)]);
    assert_eq!(
        f.get(&cookie, "/insights/workers/summary").await,
        summary(json!(0))
    );
    // The hour under way doesn't count.
    f.write(&fleet_hour(NOW_H - 1));
    f.write(&fleet_hour(NOW_H));
    let v = f.get(&cookie, "/insights/workers/summary").await;
    assert_eq!(v, summary(json!(50)));
    // Counts only: no name, address or id of any worker.
    let text = v.to_string();
    for word in ["worker-", "81.2.69.7", "\"id\""] {
        assert!(!text.contains(word), "{text}");
    }
}
