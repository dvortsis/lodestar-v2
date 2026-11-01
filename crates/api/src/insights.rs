//! Insights (§10.9): `GET /insights/overview` and `GET /insights/events`.
//!
//! - **Rollups only.** Every number comes from the pre-aggregated tables
//!   (`rollup_hour`, `cohort_hour`, `events`, `index_watermark_hour`
//!   through what it fed) or a snapshot the process already keeps
//!   ([`OpsApi::stats`], [`InsightsSource`]); never `torrents` or the
//!   index (§1.1.7). Each read is an indexed range scan (store tests check
//!   the plans), run in one read transaction under the reader lock.
//! - **Cached per (normalized query, last flush).** A response's rollup
//!   part is reused until the writer flushes or Insights records again;
//!   the snapshot fields (searchable, health, stale, index lag) are read
//!   every time.
//! - **Two time bases** (`docs/metrics.md` §2): the trend, new searchable,
//!   the stage strip and Serve by event time; the funnel, yield and time
//!   to searchable by discovery cohort.
//! - **Null, not 0,** where nothing was measured: a trend bucket no
//!   coverage row backs (paused, not running), and every field whose
//!   counter doesn't exist yet.
//! - **Every rate, share, percentile and Δ is computed here.**
//!
//! The code → name tables for stages, losses, events and the metric codes
//! read here are this module's [`codes`]; `crates/lodestar` checks them
//! against `lodestar_sources::{Stage, Loss}` and `lodestar_pipeline`.

use std::collections::{BTreeMap, VecDeque};
use std::fmt;
use std::sync::{Arc, Mutex, PoisonError};

use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::get;
use axum::{Json, Router};
use lodestar_classify::ContentType;
use lodestar_store::{
    CohortCount, Counter, Event, InsightsState, RollupBy, Store, UNCLASSIFIED, log_bucket_bounds,
};
use utoipa::OpenApi;

pub use crate::dto::insights::{
    Annotation, Bucket, Conversion, EventsQuery, EventsResponse, Fix, FunnelCohort, FunnelStages,
    FunnelTotal, Health, HealthState, ImportNote, InsightsException, InsightsRange, Kpi,
    OverviewQuery, OverviewResponse, PausedSpan, PreviousKpi, PreviousPeriod, PreviousStages,
    SegmentCount, SegmentMover, SegmentYield, Segments, Serve, Severity, Stages, Stale, TrendPoint,
};
use crate::error::{ApiError, ApiResult, internal};

mod csv;
mod stages;

use crate::extract::Query;
use crate::ops::OpsApi;
use crate::{ApiState, auth};
#[allow(unused_imports)] // B11c's digest dates its lines with it.
pub(crate) use csv::civil_from_days;
pub(crate) use stages::{Stage, StagePage, stage_response};

/// This module's half of `/openapi.json`.
#[derive(OpenApi)]
#[openapi(
    paths(
        overview,
        events,
        stages::discover_page,
        stages::fetch_page,
        stages::understand_page,
        stages::index_page,
        stages::serve_page,
        csv::export_csv
    ),
    tags((name = "insights", description = "The Insights dashboard's rollups (§10.9)."))
)]
pub(crate) struct ApiDoc;

pub fn routes() -> Router<ApiState> {
    auth::protected(
        Router::new()
            .route("/insights/overview", get(overview))
            .route("/insights/events", get(events))
            .route("/insights/discover", get(stages::discover_page))
            .route("/insights/fetch", get(stages::fetch_page))
            .route("/insights/understand", get(stages::understand_page))
            .route("/insights/index", get(stages::index_page))
            .route("/insights/serve", get(stages::serve_page))
            .route("/insights/export.csv", get(csv::export_csv)),
    )
}

/// Codes this module reads, as the pipeline persists them (append only).
pub mod codes {
    /// `rollup_hour.metric` (`lodestar_pipeline::metric`).
    pub mod metric {
        pub const HARVESTED: u32 = 1;
        pub const REDISCOVERED: u32 = 2;
        pub const DROPPED: u32 = 3;
        pub const STORED: u32 = 4;
        pub const NEWLY_SEARCHABLE: u32 = 10;
        pub const TIME_TO_SEARCHABLE: u32 = 11;
        pub const COVERAGE: u32 = 12;
        pub const SEARCHABLE: u32 = 13;
        pub const FUNNEL_EVENTS: u32 = 14;
        pub const REMOVED: u32 = 15;
        /// `COVERAGE` dims.
        pub const COVERAGE_INDEX: u32 = 0;
        pub const COVERAGE_CRAWL: u32 = 1;
    }

    /// `cohort_hour.stage` (`lodestar_sources::Stage`).
    pub const HARVESTED: u8 = 1;
    pub const PEER_FOUND: u8 = 2;
    pub const FETCHED: u8 = 3;
    pub const STORED: u8 = 4;
    pub const SEARCHABLE: u8 = 5;
    pub const LOST: u8 = 6;
    /// `Loss::Blocked`.
    pub const BLOCKED: u8 = 7;

    /// `rollup_hour.key` of `metric::HARVESTED` (`lodestar_pipeline::dht::
    /// strategy`), by name.
    pub const STRATEGIES: [(i64, &str); 5] = [
        (0, "sample"),
        (1, "get_peers"),
        (2, "announce"),
        (3, "lookup"),
        (200, "refetch"),
    ];

    pub fn strategy_name(key: i64) -> Option<&'static str> {
        STRATEGIES.iter().find(|(k, _)| *k == key).map(|(_, n)| *n)
    }

    /// Stage names, by code.
    pub const STAGES: [(u8, &str); 6] = [
        (HARVESTED, "harvested"),
        (PEER_FOUND, "peer_found"),
        (FETCHED, "fetched"),
        (STORED, "stored"),
        (SEARCHABLE, "searchable"),
        (LOST, "lost"),
    ];

    /// `cohort_hour.reason` names for [`LOST`] (`lodestar_sources::Loss`),
    /// as the Prometheus labels.
    pub const LOSSES: [(u8, &str); 10] = [
        (1, "no_peers"),
        (2, "timed_out"),
        (3, "refused"),
        (4, "encryption_failed"),
        (5, "oversize"),
        (6, "invalid"),
        (7, "blocked"),
        (8, "evicted"),
        (9, "gave_up"),
        (10, "unreachable"),
    ];

    /// `events.kind` → annotation kind (`lodestar_pipeline::event`, then
    /// the store's import annotations, `lodestar_store::import::event`).
    pub const EVENTS: [(u32, &str); 8] = [
        (1, "restart"),
        (2, "stopped"),
        (3, "paused"),
        (4, "resumed"),
        (5, "intensity"),
        (IMPORT_STARTED, "import_started"),
        (IMPORT_FINISHED, "import_finished"),
        (IMPORT_REMOVED, "import_removed"),
    ];
    pub const IMPORT_STARTED: u32 = lodestar_store::import::event::STARTED;
    pub const IMPORT_FINISHED: u32 = lodestar_store::import::event::FINISHED;
    pub const IMPORT_REMOVED: u32 = lodestar_store::import::event::REMOVED;
    pub const PAUSE: u32 = 3;
    pub const RESUME: u32 = 4;

    pub fn event_name(kind: u32) -> Option<&'static str> {
        EVENTS.iter().find(|(k, _)| *k == kind).map(|(_, n)| *n)
    }

    pub fn loss_name(reason: u8) -> Option<&'static str> {
        LOSSES.iter().find(|(r, _)| *r == reason).map(|(_, n)| *n)
    }
}

/// A cohort is final this long after its hour, whatever is still in
/// progress: the fetch pool's retry horizon (§10.9.3, 72 h).
pub const RETRY_HORIZON_HOURS: i64 = 72;
/// Stale after this long without a rollup flush (§10.9.6).
pub const STALE_AFTER_S: i64 = 120;
/// `index_lag` is an exception past this (§10.9.2).
pub const INDEX_LAG_S: u64 = 30;
/// ... and an error past this.
const INDEX_LAG_ERROR_S: u64 = 600;
/// `cohort_stalled`: nothing stored for this many full hours while the
/// crawler ran and hashes were in progress.
const STALL_HOURS: i64 = 2;
/// An hour counts as "the crawler ran" for `cohort_stalled` with at
/// least this much coverage.
const STALL_COVERAGE_S: i64 = 3000;
/// Hourly buckets up to this span; UTC days beyond.
const HOURLY_MAX: i64 = 7 * 24;
/// The widest range served (20 years of hours).
const MAX_SPAN_HOURS: i64 = 20 * 366 * 24;
const MAX_ANNOTATIONS: usize = 1000;
const CACHE_ENTRIES: usize = 8;
/// UTC days that ended this many days ago or more are "sealed": past the
/// retry horizon with room to spare, so nothing writes into their hours
/// any more (the recorder says when it does:
/// [`OverviewCache::forget_days_from`]). Their sums are kept in memory, so
/// a long range reads them once.
const SEALED_DAYS: i64 = 8;

/// The first hour that isn't sealed at `now_h`: [`SEALED_DAYS`] back, and
/// never past `floor` (the oldest cohort still in flight), as a UTC day.
fn sealed_before(now_h: i64, floor: Option<i64>) -> i64 {
    let by_age = (now_h.div_euclid(24) - SEALED_DAYS) * 24;
    floor.map_or(by_age, |f| by_age.min(f.div_euclid(24) * 24))
}

/// What the overview needs from the process beyond [`OpsApi`]: the
/// binary implements it.
pub trait InsightsSource: Send + Sync + fmt::Debug {
    /// Unix seconds of the writer's last flush (its last commit), or
    /// `None` if it hasn't committed since the start.
    fn last_flush(&self) -> Option<i64>;
    /// When the pipeline started (unix seconds).
    fn started_at(&self) -> i64;
    /// A value that changes whenever Insights writes rollups of its own
    /// (searchable time, coverage): the cache key's second half.
    fn last_recorded(&self) -> i64;
    /// Searchable torrents that were imported: a count taken at start
    /// plus the imports made searchable since; `None` until that count
    /// is in.
    fn imported(&self) -> Option<u64>;
    /// The overview cache; the implementer keeps one.
    fn cache(&self) -> &OverviewCache;
    /// The oldest discovery cohort that may still be written to (keys
    /// restored after a long downtime keep their cohort): days from it on
    /// aren't sealed. `None` when nothing older than [`SEALED_DAYS`] is
    /// in flight.
    fn seal_floor(&self) -> Option<i64> {
        None
    }
    /// Unix seconds now (a fixed clock in tests).
    fn now(&self) -> i64 {
        crate::unix_now()
    }
    /// What the stage pages read from the process beside `OpsApi`; all
    /// null by default.
    fn stage_snapshot(&self) -> StageSnapshot {
        StageSnapshot::default()
    }
}

/// [`InsightsSource::stage_snapshot`].
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct StageSnapshot {
    /// DHT responses per query sent since the start (as `/metrics`'
    /// `dht_reply_ratio`); `None` with the DHT off.
    pub dht_reply_ratio: Option<f64>,
}

/// Recent overview responses, by normalized query and the flush they were
/// built after; and the sealed days' sums, which no flush changes.
#[derive(Debug, Default)]
pub struct OverviewCache {
    entries: Mutex<VecDeque<(CacheKey, Arc<OverviewResponse>)>>,
    /// Stage pages, keyed the same way.
    pages: Mutex<VecDeque<(stages::PageKey, Arc<StagePage>)>>,
    days: Mutex<Days>,
}

/// The sealed days' sums, and a generation [`OverviewCache::forget_days_from`]
/// bumps, so sums read from a snapshot older than a forget are never kept.
#[derive(Debug, Default)]
struct Days {
    generation: u64,
    days: BTreeMap<i64, Arc<DayRows>>,
    /// The stage pages' sealed days: one metric's day, by (dim, key);
    /// keyed (day's first hour, metric). [`LAST`] marks a gauge's entry
    /// (its last snapshot, under (0, 0)).
    metrics: BTreeMap<(i64, u32), MetricDay>,
}

/// Marks a gauge's entry in [`Days::metrics`].
const LAST: u32 = 1 << 31;

/// One metric's sealed day, by (dim, key).
type MetricDay = Arc<BTreeMap<(u32, i64), i64>>;
/// A metric's sealed days, by the day's first hour.
type MetricDays = BTreeMap<i64, MetricDay>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CacheKey {
    window: Window,
    previous: Option<Window>,
    now_h: i64,
    adult: bool,
    last_flush: Option<i64>,
    last_recorded: i64,
}

impl OverviewCache {
    fn get(&self, key: &CacheKey) -> Option<Arc<OverviewResponse>> {
        let entries = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
        entries
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.clone())
    }

    fn put(&self, key: CacheKey, value: Arc<OverviewResponse>) {
        let mut entries = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
        entries.retain(|(k, _)| k != &key);
        if entries.len() >= CACHE_ENTRIES {
            entries.pop_front();
        }
        entries.push_back((key, value));
    }

    fn page(&self, key: &stages::PageKey) -> Option<Arc<StagePage>> {
        let pages = self.pages.lock().unwrap_or_else(PoisonError::into_inner);
        pages.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone())
    }

    fn put_page(&self, key: stages::PageKey, value: Arc<StagePage>) {
        let mut pages = self.pages.lock().unwrap_or_else(PoisonError::into_inner);
        pages.retain(|(k, _)| k != &key);
        if pages.len() >= CACHE_ENTRIES {
            pages.pop_front();
        }
        pages.push_back((key, value));
    }

    /// How many responses it holds, for tests.
    pub fn len(&self) -> usize {
        self.entries
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Drops the sums of the sealed days from `hour`'s on, and every
    /// response: something was written into an hour thought sealed (the
    /// recorder making old cohorts searchable after an index backlog).
    /// The caller warms them again ([`warm`]).
    pub fn forget_days_from(&self, hour: i64) {
        {
            let mut days = self.days.lock().unwrap_or_else(PoisonError::into_inner);
            days.generation += 1;
            let day = hour.div_euclid(24) * 24;
            let stale = days.days.split_off(&day);
            drop(stale);
            let stale = days.metrics.split_off(&(day, 0));
            drop(stale);
        }
        self.entries
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clear();
        self.pages
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clear();
    }

    /// Drops the sums of days below `day` (compacted: read from the day
    /// tables from now on), to keep the cache to the hourly band.
    pub fn forget_days_before(&self, day: i64) {
        let mut days = self.days.lock().unwrap_or_else(PoisonError::into_inner);
        days.days = days.days.split_off(&(day * 24));
        days.metrics = days.metrics.split_off(&(day * 24, 0));
    }

    /// The sealed days `from..to` of one metric (`metric | LAST` for a
    /// gauge), by day: those it has, and the days it lacks.
    fn metric_days(&self, metric: u32, from: i64, to: i64) -> (MetricDays, Vec<i64>) {
        let days = self.days.lock().unwrap_or_else(PoisonError::into_inner);
        let mut have = BTreeMap::new();
        let mut missing = Vec::new();
        for d in (from..to).step_by(24) {
            match days.metrics.get(&(d, metric)) {
                Some(rows) => {
                    have.insert(d, rows.clone());
                }
                None => missing.push(d),
            }
        }
        (have, missing)
    }

    /// Keeps sealed metric days read under `generation`, unless a forget
    /// came since.
    fn keep_metric_days(&self, generation: u64, metric: u32, read: &MetricDays) {
        let mut days = self.days.lock().unwrap_or_else(PoisonError::into_inner);
        if days.generation != generation {
            return;
        }
        for (&d, rows) in read {
            days.metrics
                .entry((d, metric))
                .or_insert_with(|| rows.clone());
        }
    }

    fn generation(&self) -> u64 {
        self.days
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .generation
    }

    /// How many sealed days it holds, for tests.
    pub fn days(&self) -> usize {
        self.days
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .days
            .len()
    }
}

/// The hour before which a write reaches sealed days at `now`: a
/// [`lodestar_store::Searchable`] whose `first_cohort` is earlier calls
/// for [`OverviewCache::forget_days_from`].
pub fn sealed_hour(now: i64, floor: Option<i64>) -> i64 {
    sealed_before(now.div_euclid(3600), floor)
}

/// Reads every sealed day's sums that `cache` lacks (at start, and after
/// a forget, so a long range is as quick as the rest). Returns how many
/// sealed days it holds.
pub fn warm(
    store: &Store,
    cache: &OverviewCache,
    now: i64,
    floor: Option<i64>,
) -> lodestar_store::Result<usize> {
    let generation = cache.generation();
    store.read_snapshot(|store| {
        let Some(first) = store.first_rollup_hour()? else {
            return Ok(0);
        };
        let to = sealed_before(now.div_euclid(3600), floor);
        // Compacted days are read from the day tables, never kept here.
        let compacted = store.insights_state()?.compacted_before_day * 24;
        let from = (first.div_euclid(24) * 24).max(compacted);
        if from >= to {
            return Ok(0);
        }
        let from = from.max(to - MAX_SPAN_HOURS);
        sealed_days(store, cache, generation, from, to)?;
        let sealed = Sealed {
            cache,
            generation,
            floor,
        };
        stages::warm_metrics(store, &sealed, from, to)?;
        Ok(cache.days())
    })
}

/// One sealed UTC day's sums: what [`read_period`] would read for it.
#[derive(Debug, Default)]
struct DayRows {
    /// Crawled torrents newly searchable in the day; `None` with no row.
    newly: Option<i64>,
    /// Imported ones (`key` 1), apart; `None` with no row.
    newly_imported: Option<i64>,
    /// Crawled only.
    newly_by_dim: BTreeMap<u32, i64>,
    stored: i64,
    /// Fetches by event time (`FUNNEL_EVENTS`, the `Fetched` stage).
    fetched: i64,
    coverage: BTreeMap<u32, i64>,
    /// Summed over the day (`cohort` = the day's first hour).
    cohorts: Vec<CohortCount>,
    delays: BTreeMap<i64, i64>,
}

impl DayRows {
    /// One `NEWLY_SEARCHABLE` row (by hour, dim and key): imports apart.
    fn add_newly(&mut self, c: &Counter) {
        if c.key == 0 {
            *self.newly.get_or_insert(0) += c.value;
            *self.newly_by_dim.entry(c.dim).or_default() += c.value;
        } else {
            *self.newly_imported.get_or_insert(0) += c.value;
        }
    }
}

/// The sealed days `from..to` (whole days), from the cache, reading the
/// missing ones in one pass. What it reads is kept only if no forget came
/// since `generation`, taken before the caller's snapshot began.
fn sealed_days(
    store: &Store,
    cache: &OverviewCache,
    generation: u64,
    from: i64,
    to: i64,
) -> lodestar_store::Result<Vec<Arc<DayRows>>> {
    use codes::metric;
    let mut out: BTreeMap<i64, Arc<DayRows>> = {
        let days = cache.days.lock().unwrap_or_else(PoisonError::into_inner);
        days.days
            .range(from..to)
            .map(|(&d, r)| (d, r.clone()))
            .collect()
    };
    let missing: Vec<i64> = (from..to)
        .step_by(24)
        .filter(|d| !out.contains_key(d))
        .collect();
    if let (Some(&a), Some(&b)) = (missing.first(), missing.last()) {
        let (a, b) = (a, b + 24);
        let mut read: BTreeMap<i64, DayRows> = (a..b)
            .step_by(24)
            .map(|d| (d, DayRows::default()))
            .collect();
        let day = |h: i64| h.div_euclid(24) * 24;
        for c in store.rollup_sums(metric::NEWLY_SEARCHABLE, a, b, RollupBy::All)? {
            read.get_mut(&day(c.hour)).expect("in range").add_newly(&c);
        }
        for c in store.rollup_sums(metric::STORED, a, b, RollupBy::Hour)? {
            read.get_mut(&day(c.hour)).expect("in range").stored += c.value;
        }
        for c in store.rollup_sums(metric::FUNNEL_EVENTS, a, b, RollupBy::HourDim)? {
            if c.dim == u32::from(codes::FETCHED) {
                read.get_mut(&day(c.hour)).expect("in range").fetched += c.value;
            }
        }
        for c in store.rollup_sums(metric::COVERAGE, a, b, RollupBy::HourDim)? {
            *read
                .get_mut(&day(c.hour))
                .expect("in range")
                .coverage
                .entry(c.dim)
                .or_default() += c.value;
        }
        for c in store.cohort_sums(a, b, 24)? {
            read.get_mut(&c.cohort).expect("in range").cohorts.push(c);
        }
        for c in store.rollup_sums(metric::TIME_TO_SEARCHABLE, a, b, RollupBy::HourKey)? {
            *read
                .get_mut(&day(c.hour))
                .expect("in range")
                .delays
                .entry(c.key)
                .or_default() += c.value;
        }
        let mut days = cache.days.lock().unwrap_or_else(PoisonError::into_inner);
        let keep = days.generation == generation;
        for (d, rows) in read {
            if out.contains_key(&d) {
                continue;
            }
            let rows = Arc::new(rows);
            if keep {
                days.days.entry(d).or_insert_with(|| rows.clone());
            }
            out.insert(d, rows);
        }
    }
    Ok(out.into_values().collect())
}

/// Compacted days `from..to` (whole days below `compacted_before_day`):
/// their `rollup_day`/`cohort_day` rows plus any hourly stragglers waiting
/// for the next compaction (only `Sum` metrics are read here). One seek
/// per (day, metric), and a range scan of the sparse hours below the
/// boundary.
fn compacted_days(store: &Store, from: i64, to: i64) -> lodestar_store::Result<Vec<DayRows>> {
    use codes::metric;
    let (a, b) = (from.div_euclid(24), to.div_euclid(24));
    let mut read: BTreeMap<i64, DayRows> = (from..to)
        .step_by(24)
        .map(|d| (d, DayRows::default()))
        .collect();
    let both = |m: u32, by: RollupBy| -> lodestar_store::Result<Vec<Counter>> {
        let mut v = store.rollup_day_sums(m, a, b, by)?;
        v.extend(store.rollup_straggler_sums(m, from, to, by)?);
        Ok(v)
    };
    fn at(read: &mut BTreeMap<i64, DayRows>, h: i64) -> &mut DayRows {
        read.get_mut(&h).expect("in range")
    }
    for c in both(metric::NEWLY_SEARCHABLE, RollupBy::All)? {
        at(&mut read, c.hour).add_newly(&c);
    }
    for c in both(metric::STORED, RollupBy::Hour)? {
        at(&mut read, c.hour).stored += c.value;
    }
    for c in both(metric::FUNNEL_EVENTS, RollupBy::HourDim)? {
        if c.dim == u32::from(codes::FETCHED) {
            at(&mut read, c.hour).fetched += c.value;
        }
    }
    for c in both(metric::COVERAGE, RollupBy::HourDim)? {
        *at(&mut read, c.hour).coverage.entry(c.dim).or_default() += c.value;
    }
    for c in store
        .rollup_day_sums(metric::TIME_TO_SEARCHABLE, a, b, RollupBy::HourKey)?
        .into_iter()
        .chain(store.rollup_straggler_sums(
            metric::TIME_TO_SEARCHABLE,
            from,
            to,
            RollupBy::HourKey,
        )?)
    {
        *at(&mut read, c.hour).delays.entry(c.key).or_default() += c.value;
    }
    for c in store
        .cohort_day_sums(a, b)?
        .into_iter()
        .chain(store.cohort_sums(from, to, 24)?)
    {
        at(&mut read, c.cohort).cohorts.push(c);
    }
    Ok(read.into_values().collect())
}

fn unavailable() -> ApiError {
    ApiError::new(
        StatusCode::SERVICE_UNAVAILABLE,
        "insights_unavailable",
        "Insights isn't running in this process.",
    )
}

fn sources(s: &ApiState) -> ApiResult<(Arc<dyn OpsApi>, Arc<dyn InsightsSource>)> {
    match (&s.ops, &s.insights) {
        (Some(ops), Some(insights)) => Ok((ops.clone(), insights.clone())),
        _ => Err(unavailable()),
    }
}

/// A range in whole hours, `from_h..to_h`, bucketed by `grain` hours.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Window {
    from_h: i64,
    to_h: i64,
    grain: i64,
}

impl Window {
    /// Hourly buckets up to [`HOURLY_MAX`] and from `compacted` (the
    /// first hour that's still hourly) on; UTC days otherwise.
    fn new(from_h: i64, to_h: i64, compacted: i64) -> Window {
        if to_h - from_h <= HOURLY_MAX && from_h >= compacted {
            Window {
                from_h,
                to_h,
                grain: 1,
            }
        } else {
            Window {
                from_h: from_h.div_euclid(24) * 24,
                to_h: ceil_to(to_h, 24),
                grain: 24,
            }
        }
    }

    fn span(&self) -> i64 {
        self.to_h - self.from_h
    }

    fn dto(&self) -> InsightsRange {
        InsightsRange {
            bucket: if self.grain == 1 {
                Bucket::Hour
            } else {
                Bucket::Day
            },
            from: self.from_h * 3600,
            to: self.to_h * 3600,
            tz: "UTC".into(),
        }
    }

    fn bucket_of(&self, hour: i64) -> i64 {
        hour.div_euclid(self.grain) * self.grain
    }

    fn buckets(&self) -> impl Iterator<Item = i64> {
        (self.from_h..self.to_h).step_by(self.grain as usize)
    }
}

fn ceil_to(v: i64, step: i64) -> i64 {
    (v + step - 1).div_euclid(step) * step
}

fn bad(field: &'static str, message: impl Into<std::borrow::Cow<'static, str>>) -> ApiError {
    ApiError::bad_request("bad_range", message).field(field)
}

/// The query's range, normalized: whole UTC hours ending with the current
/// one, or whole UTC days past 7 days or when it reaches a compacted day
/// (`compacted`, the first hour still hourly). `first_hour` is the
/// earliest rollup hour, for `all`.
fn window(
    q: &OverviewQuery,
    now: i64,
    first_hour: Option<i64>,
    compacted: i64,
) -> ApiResult<Window> {
    let end_h = now.div_euclid(3600) + 1;
    let range = q.range.as_deref().unwrap_or("7d");
    if range != "custom" && (q.from.is_some() || q.to.is_some()) {
        let field = if q.from.is_some() { "from" } else { "to" };
        return Err(bad(field, "from and to go with range=custom."));
    }
    let hours = match range {
        "24h" => 24,
        "7d" => 7 * 24,
        "30d" => 30 * 24,
        "all" => {
            // At most 20 years back: one stray hour from a clock that was
            // unset doesn't make the range absurd.
            let from_h = first_hour
                .unwrap_or(end_h - 24)
                .clamp(end_h - MAX_SPAN_HOURS, end_h - 1);
            return Ok(Window::new(from_h, end_h, compacted));
        }
        "custom" => {
            let from = q
                .from
                .ok_or_else(|| bad("from", "range=custom needs from."))?;
            let to = q.to.ok_or_else(|| bad("to", "range=custom needs to."))?;
            if from < 0 {
                return Err(bad("from", "from is a unix time, at least 0."));
            }
            if to <= from {
                return Err(bad("to", "to must be after from."));
            }
            let from_h = from.div_euclid(3600);
            let to_h = ceil_to(to.min(end_h * 3600), 3600).div_euclid(3600);
            if from_h >= to_h {
                return Err(bad("from", "The range starts after the current hour."));
            }
            if to_h - from_h > MAX_SPAN_HOURS {
                return Err(bad("from", "The range is wider than 20 years."));
            }
            return Ok(Window::new(from_h, to_h, compacted));
        }
        _ => {
            return Err(bad(
                "range",
                "Unknown range: use 24h, 7d, 30d, all or custom.",
            ));
        }
    };
    if hours <= HOURLY_MAX {
        Ok(Window::new(end_h - hours, end_h, compacted))
    } else {
        // Whole UTC days, the current one last.
        let to_h = ceil_to(end_h, 24);
        Ok(Window::new(to_h - hours, to_h, compacted))
    }
}

/// One cohort's (or bucket's) funnel counts.
#[derive(Debug, Clone, Default)]
struct Agg {
    /// By stage code, 1–5.
    stages: [u64; 6],
    /// Lost, by reason code.
    lost: BTreeMap<u8, u64>,
    harvested_rows: bool,
}

impl Agg {
    fn add(&mut self, c: &CohortCount) {
        let n = c.count.max(0) as u64;
        match c.stage {
            codes::LOST => *self.lost.entry(c.reason).or_default() += n,
            s @ codes::HARVESTED..=codes::SEARCHABLE => {
                self.stages[usize::from(s)] += n;
                self.harvested_rows |= s == codes::HARVESTED;
            }
            // A stage this build doesn't know: not counted.
            _ => {}
        }
    }

    fn merge(&mut self, o: &Agg) {
        for (a, b) in self.stages.iter_mut().zip(o.stages) {
            *a += b;
        }
        for (r, n) in &o.lost {
            *self.lost.entry(*r).or_default() += n;
        }
        self.harvested_rows |= o.harvested_rows;
    }

    fn get(&self, stage: u8) -> u64 {
        self.stages[usize::from(stage)]
    }

    fn lost_total(&self) -> u64 {
        self.lost.values().sum()
    }

    fn in_progress(&self) -> u64 {
        self.get(codes::HARVESTED)
            .saturating_sub(self.get(codes::STORED) + self.lost_total())
    }

    fn funnel_stages(&self) -> FunnelStages {
        FunnelStages {
            fetched: self.get(codes::FETCHED),
            harvested: self.get(codes::HARVESTED),
            peer_found: self.get(codes::PEER_FOUND),
            searchable: self.get(codes::SEARCHABLE),
            stored: self.get(codes::STORED),
        }
    }

    fn conversion(&self) -> Conversion {
        let s = |stage| self.get(stage);
        Conversion {
            fetched: ratio(s(codes::FETCHED), s(codes::PEER_FOUND)),
            peer_found: ratio(s(codes::PEER_FOUND), s(codes::HARVESTED)),
            searchable: ratio(s(codes::SEARCHABLE), s(codes::STORED)),
            stored: ratio(s(codes::STORED), s(codes::FETCHED)),
        }
    }

    /// Every known reason, 0 where none; unknown reasons as `reason_<n>`.
    fn losses(&self) -> BTreeMap<String, u64> {
        let mut out: BTreeMap<String, u64> = codes::LOSSES
            .iter()
            .map(|(r, name)| (name.to_string(), self.lost.get(r).copied().unwrap_or(0)))
            .collect();
        for (r, n) in &self.lost {
            if codes::loss_name(*r).is_none() {
                out.insert(format!("reason_{r}"), *n);
            }
        }
        out
    }

    fn is_empty(&self) -> bool {
        self.stages.iter().all(|&n| n == 0) && self.lost.is_empty()
    }
}

fn ratio(n: u64, d: u64) -> Option<f64> {
    (d > 0).then(|| n as f64 / d as f64)
}

/// Whether a cohort hour is final at `now_h`: past the retry horizon, or
/// ended with nothing in progress and everything stored searchable.
fn hour_is_final(hour: i64, agg: &Agg, now_h: i64) -> bool {
    hour < now_h - RETRY_HORIZON_HOURS
        || (hour < now_h
            && agg.in_progress() == 0
            && agg.get(codes::SEARCHABLE) >= agg.get(codes::STORED))
}

/// The funnel side of one period.
#[derive(Debug, Default)]
struct Cohorts {
    /// By bucket start hour: counts and whether every hour is final.
    buckets: BTreeMap<i64, (Agg, bool)>,
    /// Over final cohort hours only.
    finals: Agg,
    /// Harvested and searchable by source, over final cohort hours.
    by_source: BTreeMap<u8, (u64, u64)>,
    /// Final cohort hours whose delay histogram counts (recent part).
    final_hours: Vec<i64>,
    any_final: bool,
}

/// Splits `w` at the retry horizon: hours before `cutoff` are final by
/// age, so they're read summed by bucket; the rest hour by hour.
fn cutoff(w: &Window, now_h: i64) -> i64 {
    let c = w.bucket_of(now_h - RETRY_HORIZON_HOURS);
    c.clamp(w.from_h, w.to_h)
}

fn cohorts(w: &Window, now_h: i64, old: &[CohortCount], recent: &[CohortCount]) -> Cohorts {
    let mut out = Cohorts::default();
    for c in old {
        let entry = out
            .buckets
            .entry(c.cohort)
            .or_insert((Agg::default(), true));
        entry.0.add(c);
        out.finals.add(c);
        add_source(&mut out.by_source, c);
        out.any_final |= c.stage == codes::HARVESTED && c.count > 0;
    }
    let mut hours: BTreeMap<i64, Agg> = BTreeMap::new();
    for c in recent {
        hours.entry(c.cohort).or_default().add(c);
    }
    for (&hour, agg) in &hours {
        let fin = hour_is_final(hour, agg, now_h);
        let entry = out
            .buckets
            .entry(w.bucket_of(hour))
            .or_insert((Agg::default(), true));
        entry.0.merge(agg);
        entry.1 &= fin;
        if fin {
            out.finals.merge(agg);
            out.final_hours.push(hour);
            out.any_final |= agg.get(codes::HARVESTED) > 0;
        }
    }
    for c in recent {
        if out.final_hours.binary_search(&c.cohort).is_ok() {
            add_source(&mut out.by_source, c);
        }
    }
    // A bucket of the current day may be final hour by hour so far, but
    // hours still to come can't be.
    let now_bucket = w.bucket_of(now_h);
    if let Some(b) = out.buckets.get_mut(&now_bucket) {
        b.1 &= w.grain == 1 && now_bucket < now_h;
    }
    out
}

fn add_source(by: &mut BTreeMap<u8, (u64, u64)>, c: &CohortCount) {
    let n = c.count.max(0) as u64;
    let e = by.entry(c.source.get()).or_default();
    match c.stage {
        codes::HARVESTED => e.0 += n,
        codes::SEARCHABLE => e.1 += n,
        _ => {}
    }
}

/// A percentile of a log-bucket histogram (`(bucket, count)`, any order):
/// the geometric midpoint of the bucket holding the rank, rounded.
fn percentile(hist: &BTreeMap<i64, u64>, q: f64) -> Option<u64> {
    let total: u64 = hist.values().sum();
    if total == 0 {
        return None;
    }
    let rank = ((q * total as f64).ceil() as u64).max(1);
    let mut seen = 0;
    for (&bucket, &n) in hist {
        seen += n;
        if seen >= rank {
            let (lo, hi) = log_bucket_bounds(bucket);
            return Some((lo * hi).sqrt().round() as u64);
        }
    }
    None
}

/// The rollup rows of one period, read in one snapshot.
#[derive(Debug, Default)]
struct PeriodRows {
    /// `NEWLY_SEARCHABLE` by hour and key: `key` 0 crawled, 1 imported.
    newly_by_hour: Vec<Counter>,
    /// By dim and key.
    newly_by_dim: Vec<Counter>,
    stored_by_hour: Vec<Counter>,
    coverage: Vec<Counter>,
    cohorts_old: Vec<CohortCount>,
    cohorts_recent: Vec<CohortCount>,
    delays_old: Vec<Counter>,
    delays_recent: Vec<Counter>,
    /// Fetches by event time over the whole window.
    fetched: i64,
}

/// Where [`read_period`] takes sealed days' sums from.
#[derive(Debug, Clone, Copy)]
struct Sealed<'a> {
    cache: &'a OverviewCache,
    /// The cache's generation before the snapshot began.
    generation: u64,
    floor: Option<i64>,
}

/// One period's rows, in the caller's snapshot. With day buckets, days
/// below `compacted` (an hour: `compacted_before_day`'s first) come from
/// the day tables, sealed days from `sealed`'s cache, the rest hour by
/// hour.
fn read_period(
    store: &Store,
    w: &Window,
    now_h: i64,
    sealed: Option<Sealed<'_>>,
    compacted: i64,
) -> lodestar_store::Result<PeriodRows> {
    use codes::metric;
    let cb = if w.grain == 24 {
        compacted.clamp(w.from_h, w.to_h)
    } else {
        w.from_h
    };
    let split = if let (24, Some(sealed)) = (w.grain, &sealed) {
        sealed_before(now_h, sealed.floor).clamp(cb, w.to_h)
    } else {
        cb
    };
    let cut = cutoff(w, now_h).max(split);
    let mut rows = PeriodRows {
        newly_by_hour: store.rollup_sums(
            metric::NEWLY_SEARCHABLE,
            split,
            w.to_h,
            RollupBy::HourKey,
        )?,
        newly_by_dim: store.rollup_sums(
            metric::NEWLY_SEARCHABLE,
            split,
            w.to_h,
            RollupBy::DimKey,
        )?,
        stored_by_hour: store.rollup_sums(metric::STORED, split, w.to_h, RollupBy::Hour)?,
        coverage: store.rollup_sums(metric::COVERAGE, split, w.to_h, RollupBy::HourDim)?,
        cohorts_old: store.cohort_sums(split, cut, w.grain)?,
        cohorts_recent: store.cohorts(cut, w.to_h)?,
        delays_old: store.rollup_sums(metric::TIME_TO_SEARCHABLE, split, cut, RollupBy::Key)?,
        delays_recent: store.rollup_sums(
            metric::TIME_TO_SEARCHABLE,
            cut,
            w.to_h,
            RollupBy::HourKey,
        )?,
        fetched: store
            .rollup_sums(metric::FUNNEL_EVENTS, split, w.to_h, RollupBy::Dim)?
            .iter()
            .filter(|c| c.dim == u32::from(codes::FETCHED))
            .map(|c| c.value)
            .sum(),
    };
    let mut days: Vec<Arc<DayRows>> = if cb > w.from_h {
        compacted_days(store, w.from_h, cb)?
            .into_iter()
            .map(Arc::new)
            .collect()
    } else {
        Vec::new()
    };
    if let Some(sealed) = sealed.filter(|_| split > cb) {
        days.extend(sealed_days(
            store,
            sealed.cache,
            sealed.generation,
            cb,
            split,
        )?);
    }
    let row = |hour, dim, key, value| Counter {
        hour,
        metric: 0,
        dim,
        key,
        value,
    };
    for (d, day) in (w.from_h..split).step_by(24).zip(&days) {
        if let Some(n) = day.newly {
            rows.newly_by_hour.push(row(d, 0, 0, n));
        }
        if let Some(n) = day.newly_imported {
            rows.newly_by_hour.push(row(d, 0, 1, n));
        }
        for (&dim, &n) in &day.newly_by_dim {
            rows.newly_by_dim.push(row(0, dim, 0, n));
        }
        rows.stored_by_hour.push(row(d, 0, 0, day.stored));
        rows.fetched += day.fetched;
        for (&dim, &n) in &day.coverage {
            rows.coverage.push(row(d, dim, 0, n));
        }
        rows.cohorts_old.extend_from_slice(&day.cohorts);
        for (&key, &n) in &day.delays {
            rows.delays_old.push(row(0, 0, key, n));
        }
    }
    Ok(rows)
}

/// What the exceptions read, whatever the range: the cohorts that can
/// still be in progress, and the last full hours' stores and coverage.
#[derive(Debug, Default)]
struct NowRows {
    cohorts: Vec<CohortCount>,
    stored: Vec<Counter>,
    coverage: Vec<Counter>,
}

fn read_now(store: &Store, now_h: i64) -> lodestar_store::Result<NowRows> {
    use codes::metric;
    let from = now_h - STALL_HOURS;
    Ok(NowRows {
        cohorts: store.cohorts(now_h - RETRY_HORIZON_HOURS, now_h + 1)?,
        stored: store.rollup_sums(metric::STORED, from - 1, now_h, RollupBy::Hour)?,
        coverage: store.rollup_sums(metric::COVERAGE, from - 1, now_h, RollupBy::HourDim)?,
    })
}

/// A period computed: what the overview and `previous` share.
#[derive(Debug)]
struct Period {
    stages: (u64, u64),
    /// Fetched by event time; `None` when the window starts before fetches
    /// were counted (an unknown, not a 0).
    fetch: Option<u64>,
    /// Crawled only.
    new_searchable: u64,
    new_searchable_imported: u64,
    yield_: Option<f64>,
    p50: Option<u64>,
    p95: Option<u64>,
    trend: Vec<TrendPoint>,
    cohorts: Cohorts,
}

fn period(w: &Window, now_h: i64, rows: &PeriodRows, st: &InsightsState) -> Period {
    use codes::metric;
    let cohorts = cohorts(w, now_h, &rows.cohorts_old, &rows.cohorts_recent);
    let mut hist: BTreeMap<i64, u64> = BTreeMap::new();
    for c in &rows.delays_old {
        *hist.entry(c.key).or_default() += c.value.max(0) as u64;
    }
    for c in &rows.delays_recent {
        if cohorts.final_hours.binary_search(&c.hour).is_ok() {
            *hist.entry(c.key).or_default() += c.value.max(0) as u64;
        }
    }
    let (yield_, p50, p95) = if cohorts.any_final {
        (
            ratio(
                cohorts.finals.get(codes::SEARCHABLE),
                cohorts.finals.get(codes::HARVESTED),
            ),
            percentile(&hist, 0.5),
            percentile(&hist, 0.95),
        )
    } else {
        (None, None, None)
    };
    // Trend: rows win; otherwise 0 where measured, a gap where not.
    let sum_by_bucket = |rows: &[Counter], imported: bool| -> BTreeMap<i64, u64> {
        let mut out = BTreeMap::new();
        for c in rows.iter().filter(|c| (c.key != 0) == imported) {
            *out.entry(w.bucket_of(c.hour)).or_default() += c.value.max(0) as u64;
        }
        out
    };
    let newly = sum_by_bucket(&rows.newly_by_hour, false);
    let imported = sum_by_bucket(&rows.newly_by_hour, true);
    let mut covered: BTreeMap<(i64, u32), i64> = BTreeMap::new();
    for c in &rows.coverage {
        *covered.entry((w.bucket_of(c.hour), c.dim)).or_default() += c.value;
    }
    let is_covered = |b: i64, dim: u32| covered.get(&(b, dim)).is_some_and(|&s| s > 0);
    let trend = w
        .buckets()
        .map(|b| {
            let harvested = cohorts
                .buckets
                .get(&b)
                .filter(|(a, _)| a.harvested_rows)
                .map(|(a, _)| a.get(codes::HARVESTED))
                .or_else(|| is_covered(b, metric::COVERAGE_CRAWL).then_some(0));
            let indexed = |rows: &BTreeMap<i64, u64>| {
                rows.get(&b)
                    .copied()
                    .or_else(|| is_covered(b, metric::COVERAGE_INDEX).then_some(0))
            };
            TrendPoint {
                at: b * 3600,
                harvested,
                searchable: indexed(&newly),
                searchable_imported: indexed(&imported),
            }
        })
        .collect();
    let discover = cohorts
        .buckets
        .values()
        .map(|(a, _)| a.get(codes::HARVESTED))
        .sum();
    let understand = rows
        .stored_by_hour
        .iter()
        .map(|c| c.value.max(0) as u64)
        .sum();
    Period {
        stages: (discover, understand),
        fetch: (w.from_h >= st.funnel_events_since_hour).then_some(rows.fetched.max(0) as u64),
        new_searchable: newly.values().sum(),
        new_searchable_imported: imported.values().sum(),
        yield_,
        p50,
        p95,
        trend,
        cohorts,
    }
}

fn funnel(c: &Cohorts) -> (Vec<FunnelCohort>, FunnelTotal) {
    let mut total = Agg::default();
    let mut list = Vec::with_capacity(c.buckets.len());
    let mut finals = 0;
    for (&b, (agg, fin)) in &c.buckets {
        if agg.is_empty() {
            continue;
        }
        total.merge(agg);
        finals += u64::from(*fin);
        list.push(FunnelCohort {
            cohort: b * 3600,
            conversion: agg.conversion(),
            is_final: *fin,
            in_progress: agg.in_progress(),
            losses: agg.losses(),
            stages: agg.funnel_stages(),
        });
    }
    let losses = total.losses();
    let lost: u64 = losses.values().sum();
    let total = FunnelTotal {
        cohorts: list.len() as u64,
        conversion: total.conversion(),
        final_cohorts: finals,
        in_progress: list.iter().map(|f| f.in_progress).sum(),
        loss_share: losses
            .iter()
            .map(|(k, &n)| (k.clone(), ratio(n, lost)))
            .collect(),
        losses,
        stages: total.funnel_stages(),
    };
    (list, total)
}

/// A content type's group (§6.5 "category group").
fn group_of(t: ContentType) -> &'static str {
    use ContentType::*;
    match t {
        Movie | Tv | Video => "video",
        Music | Audiobook => "audio",
        Ebook | Comic => "documents",
        Software | Game => "software",
        Images => "images",
        Other => "other",
        Adult => "adult",
    }
}

fn type_name(dim: u32) -> (String, Option<ContentType>) {
    if dim == UNCLASSIFIED {
        return ("unclassified".into(), None);
    }
    match u8::try_from(dim).ok().and_then(ContentType::from_code) {
        Some(t) => (t.as_str().into(), Some(t)),
        None => (format!("type_{dim}"), None),
    }
}

fn sorted_counts(m: BTreeMap<String, u64>) -> Vec<SegmentCount> {
    let mut v: Vec<SegmentCount> = m
        .into_iter()
        .map(|(key, searchable)| SegmentCount { key, searchable })
        .collect();
    v.sort_by(|a, b| b.searchable.cmp(&a.searchable).then(a.key.cmp(&b.key)));
    v
}

/// Counts by content type and by group, sorted, Adult left out entirely
/// while it's hidden (§20.9).
fn type_lists(
    rows: impl Iterator<Item = (u32, u64)>,
    adult: bool,
) -> (Vec<SegmentCount>, Vec<SegmentCount>) {
    let mut types: BTreeMap<String, u64> = BTreeMap::new();
    let mut groups: BTreeMap<String, u64> = BTreeMap::new();
    for (dim, n) in rows {
        let (name, t) = type_name(dim);
        if !adult && t == Some(ContentType::Adult) {
            continue;
        }
        *types.entry(name.clone()).or_default() += n;
        let group = t.map_or(name, |t| group_of(t).to_string());
        *groups.entry(group).or_default() += n;
    }
    (sorted_counts(types), sorted_counts(groups))
}

fn segments(rows: &PeriodRows, c: &Cohorts, names: &BTreeMap<u8, String>, adult: bool) -> Segments {
    let (by_content_type, by_group) = type_lists(
        // Crawled only: imports are apart (`kpi.new_searchable_imported`).
        rows.newly_by_dim
            .iter()
            .filter(|r| r.key == 0)
            .map(|r| (r.dim, r.value.max(0) as u64)),
        adult,
    );
    let mut by_source: Vec<SegmentYield> = c
        .by_source
        .iter()
        .map(|(id, &(harvested, searchable))| SegmentYield {
            harvested,
            key: names
                .get(id)
                .cloned()
                .unwrap_or_else(|| format!("source_{id}")),
            searchable,
            yield_: ratio(searchable, harvested),
        })
        .collect();
    by_source.sort_by(|a, b| {
        b.yield_
            .unwrap_or(-1.0)
            .total_cmp(&a.yield_.unwrap_or(-1.0))
            .then(a.key.cmp(&b.key))
    });
    Segments {
        by_content_type,
        by_group,
        by_source,
    }
}

fn annotations(events: &[Event]) -> Vec<Annotation> {
    events
        .iter()
        .filter_map(|e| {
            codes::event_name(e.kind).map(|kind| Annotation {
                at: e.at,
                kind: kind.into(),
                text: e.detail.clone(),
                import: import_note(e),
            })
        })
        .collect()
}

/// An import annotation's `key=value` detail (`lodestar_store::import::
/// event`); `None` for other kinds, or a detail without a job.
fn import_note(e: &Event) -> Option<ImportNote> {
    if !matches!(
        e.kind,
        codes::IMPORT_STARTED | codes::IMPORT_FINISHED | codes::IMPORT_REMOVED
    ) {
        return None;
    }
    let mut note = ImportNote {
        job: 0,
        source: String::new(),
        new: None,
        merged: None,
        removed: None,
    };
    let mut job = None;
    for (k, v) in e.detail.split(' ').filter_map(|w| w.split_once('=')) {
        match k {
            "job" => job = v.parse().ok(),
            "source" => note.source = v.into(),
            "new" => note.new = v.parse().ok(),
            "merged" => note.merged = v.parse().ok(),
            "removed" => note.removed = v.parse().ok(),
            _ => {}
        }
    }
    note.job = job?;
    Some(note)
}

/// `cohort_stalled`: hashes in progress, the crawler running the last
/// [`STALL_HOURS`] full hours, and nothing stored in them.
fn stalled(rows: &NowRows, now_h: i64) -> Option<InsightsException> {
    use codes::metric;
    let from = now_h - STALL_HOURS;
    let ran = (from..now_h).all(|h| {
        rows.coverage
            .iter()
            .any(|c| c.hour == h && c.dim == metric::COVERAGE_CRAWL && c.value >= STALL_COVERAGE_S)
    });
    let stored: i64 = rows
        .stored
        .iter()
        .filter(|c| (from..now_h).contains(&c.hour))
        .map(|c| c.value)
        .sum();
    let mut hours: BTreeMap<i64, Agg> = BTreeMap::new();
    for c in &rows.cohorts {
        hours.entry(c.cohort).or_default().add(c);
    }
    let in_progress: u64 = hours
        .iter()
        .filter(|(h, a)| !hour_is_final(**h, a, now_h))
        .map(|(_, a)| a.in_progress())
        .sum();
    (ran && stored == 0 && in_progress > 0).then(|| InsightsException {
        fix: Fix {
            kind: "stage".into(),
            target: "fetch".into(),
        },
        kind: "cohort_stalled".into(),
        rows: Vec::new(),
        severity: Severity::Warning,
        since: from * 3600,
        unit: Some("torrents".into()),
        value: Some(in_progress as f64),
    })
}

fn delta(now: Option<u64>, then: Option<u64>) -> Option<i64> {
    Some(now? as i64 - then? as i64)
}

/// `yield_below_baseline` compares the newest 24 cohort hours final by
/// age (72–96 h ago) with the 7 days of cohort hours before them.
const RECENT_TO_H: i64 = RETRY_HORIZON_HOURS;
const BASELINE_FROM_H: i64 = RETRY_HORIZON_HOURS + 24 + 7 * 24;
/// ... when the recent hours harvested at least this many keys, the
/// baseline at least 7 times as many, ...
const BASELINE_MIN_HARVESTED: u64 = 500;
/// ... and the recent yield is below this share of the baseline's (an
/// error below half of it).
const BASELINE_WARN: f64 = 0.75;
const BASELINE_ERROR: f64 = 0.5;

/// `yield_below_baseline` (§10.9.2 level 5): end-to-end yield of the
/// newest day of cohorts past the retry horizon (so every one is final)
/// against the 7 days before it. `rows` are the cohort rows of both.
fn yield_below_baseline(rows: &[CohortCount], now_h: i64) -> Option<InsightsException> {
    let split = now_h - RECENT_TO_H - 24;
    let (mut recent, mut base) = (Agg::default(), Agg::default());
    for c in rows {
        if c.cohort >= split {
            recent.add(c);
        } else {
            base.add(c);
        }
    }
    let (rh, bh) = (recent.get(codes::HARVESTED), base.get(codes::HARVESTED));
    if rh < BASELINE_MIN_HARVESTED || bh < BASELINE_MIN_HARVESTED * 7 {
        return None;
    }
    let recent_yield = ratio(recent.get(codes::SEARCHABLE), rh)?;
    let base_yield = ratio(base.get(codes::SEARCHABLE), bh).filter(|&y| y > 0.0)?;
    (recent_yield < base_yield * BASELINE_WARN).then(|| InsightsException {
        fix: Fix {
            kind: "stage".into(),
            target: "fetch".into(),
        },
        kind: "yield_below_baseline".into(),
        rows: Vec::new(),
        severity: if recent_yield < base_yield * BASELINE_ERROR {
            Severity::Error
        } else {
            Severity::Warning
        },
        since: split * 3600,
        unit: Some("ratio".into()),
        value: Some(recent_yield),
    })
}

/// The segment rows that moved most between `previous` and `current`
/// (the digest's "top segment movers", §10.9.7): rows of `by_group` and
/// `by_content_type` by |Δ|, largest first, ties by key (then list). A
/// key missing on one side counts as 0 there; rows that didn't move are
/// left out. At most `n`.
#[cfg_attr(not(test), allow(dead_code))] // B11c's digest calls it.
pub(crate) fn segment_movers(
    current: &Segments,
    previous: &Segments,
    n: usize,
) -> Vec<SegmentMover> {
    let mut out = Vec::new();
    for (list, now, then) in [
        ("by_group", &current.by_group, &previous.by_group),
        (
            "by_content_type",
            &current.by_content_type,
            &previous.by_content_type,
        ),
    ] {
        let mut keys: BTreeMap<&str, (u64, u64)> = BTreeMap::new();
        for r in now {
            keys.entry(&r.key).or_default().0 = r.searchable;
        }
        for r in then {
            keys.entry(&r.key).or_default().1 = r.searchable;
        }
        for (key, (c, p)) in keys {
            let delta = c as i64 - p as i64;
            if delta != 0 {
                out.push(SegmentMover {
                    list,
                    key: key.to_string(),
                    current: c,
                    previous: p,
                    delta,
                });
            }
        }
    }
    out.sort_by(|a, b| {
        b.delta
            .unsigned_abs()
            .cmp(&a.delta.unsigned_abs())
            .then_with(|| a.key.cmp(&b.key))
            .then_with(|| a.list.cmp(b.list))
    });
    out.truncate(n);
    out
}

/// Everything but the snapshot fields, which [`overview`] fills in. `st`
/// is read in the caller's snapshot, which this one joins.
#[allow(clippy::too_many_arguments)]
fn build(
    store: &Store,
    cache: &OverviewCache,
    generation: u64,
    floor: Option<i64>,
    st: &InsightsState,
    w: Window,
    prev: Option<Window>,
    now: i64,
    adult: bool,
) -> lodestar_store::Result<OverviewResponse> {
    let now_h = now.div_euclid(3600);
    let sealed = Sealed {
        cache,
        generation,
        floor,
    };
    let compacted = st.compacted_before_day * 24;
    store.read_snapshot(|store| {
        let rows = read_period(store, &w, now_h, Some(sealed), compacted)?;
        let prev_rows = prev
            .map(|p| read_period(store, &p, now_h, Some(sealed), compacted))
            .transpose()?;
        let now_rows = read_now(store, now_h)?;
        let events = store.events(w.from_h * 3600, w.to_h * 3600)?;
        let names: BTreeMap<u8, String> = store
            .sources()?
            .into_iter()
            .map(|s| (s.id.get(), s.plugin))
            .collect();
        let p = period(&w, now_h, &rows, st);
        let (funnel, funnel_total) = funnel(&p.cohorts);
        let last_hour = now_h - 1;
        let ingest = now_rows
            .stored
            .iter()
            .find(|c| c.hour == last_hour)
            .map(|c| c.value.max(0) as u64)
            .or_else(|| {
                now_rows
                    .coverage
                    .iter()
                    .any(|c| {
                        c.hour == last_hour && c.dim == codes::metric::COVERAGE_CRAWL && c.value > 0
                    })
                    .then_some(0)
            });
        // Searchable at the previous period's end: the last hourly
        // snapshot in the day before the range starts (a compacted day
        // keeps its last hour's).
        let searchable_then = match prev {
            Some(_) => searchable_before(store, w.from_h, compacted)?,
            None => None,
        };
        let previous = prev.zip(prev_rows).map(|(pw, pr)| {
            let q = period(&pw, now_h, &pr, st);
            PreviousPeriod {
                segments: segments(&pr, &q.cohorts, &names, adult),
                kpi: PreviousKpi {
                    new_searchable: q.new_searchable,
                    new_searchable_imported: q.new_searchable_imported,
                    searchable: searchable_then,
                    time_to_searchable_p50_s: q.p50,
                    time_to_searchable_p95_s: q.p95,
                    yield_: q.yield_,
                },
                range: pw.dto(),
                serve: Serve::default(),
                stages: PreviousStages {
                    discover: q.stages.0,
                    fetch: q.fetch,
                    serve: None,
                    understand: q.stages.1,
                },
                trend: q.trend,
            }
        });
        Ok(OverviewResponse {
            annotations: {
                let mut a = annotations(&events);
                a.truncate(MAX_ANNOTATIONS);
                a
            },
            exceptions: stalled(&now_rows, now_h)
                .into_iter()
                .chain(yield_below_baseline(
                    &store.cohorts(now_h - BASELINE_FROM_H, now_h - RECENT_TO_H)?,
                    now_h,
                ))
                .collect(),
            funnel,
            funnel_total,
            ingest_per_hour: ingest,
            kpi: Kpi {
                health: Health {
                    reason: None,
                    state: HealthState::Running,
                },
                imported: None,
                new_searchable: p.new_searchable,
                new_searchable_delta: previous
                    .as_ref()
                    .map(|q| p.new_searchable as i64 - q.kpi.new_searchable as i64),
                new_searchable_imported: p.new_searchable_imported,
                searchable: None,
                searchable_crawled: None,
                searchable_delta: None,
                time_to_searchable_p50_delta_s: previous
                    .as_ref()
                    .and_then(|q| delta(p.p50, q.kpi.time_to_searchable_p50_s)),
                time_to_searchable_p50_s: p.p50,
                time_to_searchable_p95_delta_s: previous
                    .as_ref()
                    .and_then(|q| delta(p.p95, q.kpi.time_to_searchable_p95_s)),
                time_to_searchable_p95_s: p.p95,
                yield_: p.yield_,
                yield_delta: previous
                    .as_ref()
                    .and_then(|q| Some(p.yield_? - q.kpi.yield_?)),
            },
            previous,
            privacy: None,
            range: w.dto(),
            segments: segments(&rows, &p.cohorts, &names, adult),
            serve: Serve::default(),
            stages: Stages {
                discover: p.stages.0,
                fetch: p.fetch,
                index: None,
                serve: None,
                understand: p.stages.1,
            },
            stale: None,
            trend: p.trend,
            updated_at: now,
        })
    })
}

/// The last `SEARCHABLE` snapshot in the 24 hours before `from_h`:
/// hourly where those hours are, else the compacted day's (`Last`: its
/// latest hour's value).
fn searchable_before(
    store: &Store,
    from_h: i64,
    compacted: i64,
) -> lodestar_store::Result<Option<u64>> {
    let m = codes::metric::SEARCHABLE;
    let hourly = store
        .rollup_sums(m, (from_h - 24).max(compacted), from_h, RollupBy::Hour)?
        .last()
        .map(|c| c.value);
    let then = match hourly {
        Some(v) => Some(v),
        None if from_h - 24 < compacted => store
            .rollup_day_sums(
                m,
                (from_h - 24).div_euclid(24),
                ceil_to(from_h.min(compacted), 24).div_euclid(24),
                RollupBy::Hour,
            )?
            .last()
            .map(|c| c.value),
        None => None,
    };
    Ok(then.map(|v| v.max(0) as u64))
}

/// The snapshot fields, read for every request.
#[derive(Debug, Clone, Copy)]
struct Snapshot {
    now: i64,
    paused: bool,
    searchable: Option<u64>,
    index_lag_s: Option<u64>,
    imported: Option<u64>,
    last_flush: Option<i64>,
    started_at: i64,
}

/// Stale (§10.9.6): more than [`STALE_AFTER_S`] since the writer's last
/// flush (the start, when there was none).
fn stale_of(now: i64, paused: bool, last_flush: Option<i64>, started_at: i64) -> Option<Stale> {
    let last = last_flush.unwrap_or(started_at);
    (now - last > STALE_AFTER_S).then(|| Stale {
        reason: if paused { "paused" } else { "writer_stalled" }.into(),
        since: last,
    })
}

fn apply_snapshot(r: &mut OverviewResponse, s: Snapshot) {
    r.stale = stale_of(s.now, s.paused, s.last_flush, s.started_at);
    let lagging = s.index_lag_s.filter(|&l| l > INDEX_LAG_S);
    r.kpi.health = match (s.paused, &r.stale, lagging) {
        (true, _, _) => Health {
            reason: Some("paused".into()),
            state: HealthState::Paused,
        },
        (false, Some(_), _) => Health {
            reason: Some("writer_stalled".into()),
            state: HealthState::Degraded,
        },
        (false, None, Some(_)) => Health {
            reason: Some("index_lag".into()),
            state: HealthState::Degraded,
        },
        (false, None, None) => Health {
            reason: None,
            state: HealthState::Running,
        },
    };
    if let Some(lag) = lagging {
        r.exceptions.insert(
            0,
            InsightsException {
                fix: Fix {
                    kind: "stage".into(),
                    target: "index".into(),
                },
                kind: "index_lag".into(),
                rows: Vec::new(),
                severity: if lag > INDEX_LAG_ERROR_S {
                    Severity::Error
                } else {
                    Severity::Warning
                },
                since: s.now - lag as i64,
                unit: Some("s".into()),
                value: Some(lag as f64),
            },
        );
    }
    r.kpi.searchable = s.searchable;
    r.kpi.imported = s.imported;
    r.kpi.searchable_crawled = s
        .searchable
        .zip(s.imported)
        .map(|(all, imported)| all.saturating_sub(imported));
    r.stages.index = s.searchable;
    r.updated_at = s.now;
    // The change since the previous period's end, removals included:
    // now's count less the snapshot then.
    r.kpi.searchable_delta = match (
        s.searchable,
        r.previous.as_ref().and_then(|p| p.kpi.searchable),
    ) {
        (Some(now), Some(then)) => Some(now as i64 - then as i64),
        _ => None,
    };
}

/// The Insights overview (§10.9.2): stage strip, KPIs, trend with
/// annotations, the cohort funnel, segments, exceptions and Serve, from
/// the rollups. `compare=1` adds `previous` and `kpi.searchable_delta`.
/// Event time for the trend, new searchable, stages and Serve; discovery
/// cohort for the funnel, yield and time to searchable.
#[utoipa::path(
    operation_id = "getInsightsOverview",
    get,
    path = "/insights/overview",
    tag = "insights",
    params(OverviewQuery),
    responses((status = 200, body = OverviewResponse))
)]
pub(crate) async fn overview(
    State(s): State<ApiState>,
    Query(q): Query<OverviewQuery>,
) -> ApiResult<Json<OverviewResponse>> {
    let mut response = overview_response(&s, q).await?;
    // Only here: the digest, the CSV and the `insights` event don't carry it. On the clock
    // of `updated_at`, which the client counts down from.
    let now = response.updated_at;
    response.privacy = s
        .settings
        .as_ref()
        .map(|settings| settings.privacy(now))
        .filter(|p| p.query_logging || p.identifying_until.is_some());
    Ok(Json(response))
}

fn compare_flag(compare: Option<u8>) -> ApiResult<bool> {
    match compare {
        None | Some(0) => Ok(false),
        Some(1) => Ok(true),
        Some(_) => {
            Err(ApiError::bad_request("bad_parameter", "compare is 0 or 1.").field("compare"))
        }
    }
}

/// The overview as `GET /insights/overview` serves it (the digest, B11c,
/// and the `insights` event read it too).
pub(crate) async fn overview_response(
    s: &ApiState,
    q: OverviewQuery,
) -> ApiResult<OverviewResponse> {
    let compare = compare_flag(q.compare)?;
    let (ops, source) = sources(s)?;
    let stats = ops.stats().await;
    let now = source.now();
    let snap = Snapshot {
        now,
        paused: stats.paused,
        searchable: stats.searchable_torrents,
        index_lag_s: stats.index_lag_s,
        imported: source.imported(),
        last_flush: source.last_flush(),
        started_at: source.started_at(),
    };
    let adult = s.adult.shown();
    let reader = s.reader.clone();
    let mut response = tokio::task::spawn_blocking(move || -> ApiResult<OverviewResponse> {
        let store = reader.lock().unwrap_or_else(PoisonError::into_inner);
        // Before the snapshot begins, so sums read from it are never kept
        // past a forget made meanwhile.
        let generation = source.cache().generation();
        // The grain is decided by `compacted_before_day` read in the same
        // snapshot as the rows, so a compaction beside it can't make a
        // day count twice or not at all.
        store
            .read_snapshot(|store| {
                Ok(overview_in_snapshot(
                    store, &q, compare, now, adult, &*source, generation,
                ))
            })
            .map_err(internal)?
    })
    .await
    .map_err(internal)??;
    apply_snapshot(&mut response, snap);
    if let Some(e) = s
        .filters
        .as_ref()
        .and_then(|f| unreadable_rows(&f.engine.current().unreadable))
    {
        response.exceptions.insert(0, e);
    }
    Ok(response)
}

/// [`unreadable_rows`] names at most this many.
const MAX_ROWS_NAMED: usize = 20;

/// Stored filter rows the engine couldn't read (§20.7): an error while
/// any of them fails closed (your rules and classification rules hold
/// back more than they would), a warning while they're only entries
/// that release, skipped. It clears once every row reads again.
fn unreadable_rows(rows: &[lodestar_filter::load::Unreadable]) -> Option<InsightsException> {
    let since = rows.iter().map(|u| u.since).min()?;
    let mut labels: Vec<String> = rows.iter().map(|u| u.label()).collect();
    labels.sort();
    labels.dedup();
    // A rule whose query and `unless` both fail is one row.
    let count = labels.len();
    // The count says how many; the list names enough to start with.
    labels.truncate(MAX_ROWS_NAMED);
    Some(InsightsException {
        fix: Fix {
            kind: "settings".into(),
            target: "filters".into(),
        },
        kind: "filter_rows_unreadable".into(),
        rows: labels,
        severity: if rows.iter().any(|u| u.kind.loosening()) {
            Severity::Error
        } else {
            Severity::Warning
        },
        since,
        unit: Some("rows".into()),
        value: Some(count as f64),
    })
}

/// The overview's rollup part, inside the caller's read transaction.
fn overview_in_snapshot(
    store: &Store,
    q: &OverviewQuery,
    compare: bool,
    now: i64,
    adult: bool,
    source: &dyn InsightsSource,
    generation: u64,
) -> ApiResult<OverviewResponse> {
    let st = store.insights_state().map_err(internal)?;
    let compacted = st.compacted_before_day * 24;
    let first = if q.range.as_deref() == Some("all") {
        store.first_rollup_hour().map_err(internal)?
    } else {
        None
    };
    let (w, prev) = windows(
        window(q, now, first, compacted)?,
        compare && q.range.as_deref() != Some("all"),
        compacted,
    );
    let key = CacheKey {
        window: w,
        previous: prev,
        now_h: now.div_euclid(3600),
        adult,
        last_flush: source.last_flush(),
        last_recorded: source.last_recorded(),
    };
    if let Some(hit) = source.cache().get(&key) {
        return Ok((*hit).clone());
    }
    let built = Arc::new(
        build(
            store,
            source.cache(),
            generation,
            source.seal_floor(),
            &st,
            w,
            prev,
            now,
            adult,
        )
        .map_err(internal)?,
    );
    source.cache().put(key, built.clone());
    Ok((*built).clone())
}

/// The window and, with Compare, the one of equal length before it. When
/// the previous one reaches a compacted day it's bucketed by day, and the
/// current one is too, so their trends line up.
fn windows(w: Window, compare: bool, compacted: i64) -> (Window, Option<Window>) {
    if !compare {
        return (w, None);
    }
    let prev = Window::new(w.from_h - w.span(), w.from_h, compacted);
    if prev.grain == w.grain {
        return (w, Some(prev));
    }
    // Hourly now, daily before: both by day.
    let w = Window::new(w.from_h, w.to_h, i64::MAX);
    (
        w,
        Some(Window::new(w.from_h - w.span(), w.from_h, i64::MAX)),
    )
}

/// Trend annotations (`restart`, `stopped`, `paused`, `resumed`,
/// `intensity`, and an import's `import_started`, `import_finished` and
/// `import_removed` with its counts; other events are skipped) and the
/// paused spans
/// overlapping `from..to`, so the client draws gaps.
#[utoipa::path(
    operation_id = "getInsightsEvents",
    get,
    path = "/insights/events",
    tag = "insights",
    params(EventsQuery),
    responses((status = 200, body = EventsResponse))
)]
pub(crate) async fn events(
    State(s): State<ApiState>,
    Query(q): Query<EventsQuery>,
) -> ApiResult<Json<EventsResponse>> {
    if q.from < 0 {
        return Err(bad("from", "from is a unix time, at least 0."));
    }
    if q.to <= q.from {
        return Err(bad("to", "to must be after from."));
    }
    if (q.to - q.from) / 3600 > MAX_SPAN_HOURS {
        return Err(bad("from", "The range is wider than 20 years."));
    }
    sources(&s)?;
    let reader = s.reader.clone();
    let (before, list) = tokio::task::spawn_blocking(move || {
        let store = reader.lock().unwrap_or_else(PoisonError::into_inner);
        store.read_snapshot(|store| {
            Ok((
                store.last_event_before(&[codes::PAUSE, codes::RESUME], q.from)?,
                store.events(q.from, q.to)?,
            ))
        })
    })
    .await
    .map_err(internal)?
    .map_err(internal)?;
    Ok(Json(events_response(before.as_ref(), &list)))
}

fn events_response(before: Option<&Event>, list: &[Event]) -> EventsResponse {
    let mut paused = Vec::new();
    let mut open = before.filter(|e| e.kind == codes::PAUSE).map(|e| e.at);
    for e in list {
        match e.kind {
            codes::PAUSE if open.is_none() => open = Some(e.at),
            codes::RESUME => {
                if let Some(from) = open.take() {
                    paused.push(PausedSpan {
                        from,
                        to: Some(e.at),
                    });
                }
            }
            _ => {}
        }
    }
    if let Some(from) = open {
        paused.push(PausedSpan { from, to: None });
    }
    let mut annotations = annotations(list);
    let truncated = annotations.len() > MAX_ANNOTATIONS;
    annotations.truncate(MAX_ANNOTATIONS);
    EventsResponse {
        annotations,
        paused,
        truncated,
    }
}

#[cfg(test)]
mod tests {
    use lodestar_store::SourceId;

    use super::*;

    const NOW: i64 = 1_800_000_000 + 1800;
    /// Nothing compacted; fetches counted since forever.
    const ST: InsightsState = InsightsState {
        compacted_before_day: 0,
        last_compaction_at: 0,
        funnel_events_since_hour: 0,
        imported: 0,
    };

    fn q(range: &str) -> OverviewQuery {
        OverviewQuery {
            range: Some(range.into()),
            ..OverviewQuery::default()
        }
    }

    #[test]
    fn ranges_normalize_to_whole_buckets() {
        let now_h = NOW / 3600;
        let w = window(&q("24h"), NOW, None, 0).unwrap();
        assert_eq!((w.from_h, w.to_h, w.grain), (now_h - 23, now_h + 1, 1));
        let w = window(&OverviewQuery::default(), NOW, None, 0).unwrap();
        assert_eq!((w.span(), w.grain), (168, 1));
        let w = window(&q("30d"), NOW, None, 0).unwrap();
        assert_eq!((w.span(), w.grain, w.from_h % 24), (720, 24, 0));
        assert!(w.to_h > now_h && w.to_h - now_h <= 24);
        let w = window(&q("all"), NOW, Some(now_h - 5), 0).unwrap();
        assert_eq!((w.from_h, w.to_h, w.grain), (now_h - 5, now_h + 1, 1));
        let w = window(&q("all"), NOW, Some(now_h - 1000), 0).unwrap();
        assert_eq!((w.grain, w.from_h % 24), (24, 0));
        let w = window(&q("all"), NOW, None, 0).unwrap();
        assert_eq!(w.span(), 24);
        // A stray hour from an unset clock: at most 20 years back.
        let w = window(&q("all"), NOW, Some(1), 0).unwrap();
        assert!(w.span() <= MAX_SPAN_HOURS + 48, "{}", w.span());
        let custom = |from, to| OverviewQuery {
            range: Some("custom".into()),
            from,
            to,
            compare: None,
        };
        let w = window(
            &custom(Some(NOW - 7200 - 5), Some(NOW + 99_999)),
            NOW,
            None,
            0,
        )
        .unwrap();
        assert_eq!((w.from_h, w.to_h), (now_h - 2, now_h + 1));
        for (query, field) in [
            (q("1y"), "range"),
            (custom(None, Some(NOW)), "from"),
            (custom(Some(NOW), None), "to"),
            (custom(Some(NOW), Some(NOW)), "to"),
            (custom(Some(-1), Some(NOW)), "from"),
            (custom(Some(NOW + 7200), Some(NOW + 9000)), "from"),
            (custom(Some(0), Some(NOW)), "from"),
            (
                OverviewQuery {
                    from: Some(1),
                    ..q("7d")
                },
                "from",
            ),
        ] {
            let e = window(&query, NOW, None, 0).unwrap_err();
            assert_eq!(e.status(), StatusCode::BAD_REQUEST);
            assert_eq!(e.body()["error"]["field"], field, "{query:?}");
        }
    }

    fn row(cohort: i64, stage: u8, reason: u8, count: i64) -> CohortCount {
        CohortCount {
            cohort,
            source: SourceId::new(0).unwrap(),
            stage,
            reason,
            count,
        }
    }

    #[test]
    fn finality_follows_progress_and_the_horizon() {
        let now_h = NOW / 3600;
        let mut done = Agg::default();
        for c in [
            row(0, codes::HARVESTED, 0, 10),
            row(0, codes::STORED, 0, 4),
            row(0, codes::SEARCHABLE, 0, 4),
            row(0, codes::LOST, 1, 6),
        ] {
            done.add(&c);
        }
        assert_eq!(done.in_progress(), 0);
        assert!(hour_is_final(now_h - 1, &done, now_h));
        assert!(!hour_is_final(now_h, &done, now_h), "the hour isn't over");
        let mut open = done.clone();
        open.add(&row(0, codes::HARVESTED, 0, 1));
        assert_eq!(open.in_progress(), 1);
        assert!(!hour_is_final(now_h - 72, &open, now_h));
        assert!(hour_is_final(now_h - 73, &open, now_h), "past the horizon");
        let mut unsearchable = done.clone();
        unsearchable.add(&row(0, codes::STORED, 0, 1));
        unsearchable.add(&row(0, codes::LOST, 1, 0));
        assert!(!hour_is_final(now_h - 2, &unsearchable, now_h));
    }

    #[test]
    fn percentiles_read_bucket_midpoints() {
        let hist: BTreeMap<i64, u64> = [(0, 1), (10, 1), (50, 8)].into();
        assert_eq!(percentile(&hist, 0.05), Some(0));
        let (lo, hi) = log_bucket_bounds(50);
        assert_eq!(
            percentile(&hist, 0.5),
            Some((lo * hi).sqrt().round() as u64)
        );
        assert_eq!(percentile(&BTreeMap::new(), 0.5), None);
    }

    #[test]
    fn paused_spans_close_and_carry_over() {
        let e = |at, kind| Event {
            at,
            kind,
            detail: String::new(),
        };
        let before = e(5, codes::PAUSE);
        let r = events_response(
            Some(&before),
            &[
                e(10, codes::RESUME),
                e(20, 1),
                e(30, codes::PAUSE),
                e(31, 99),
            ],
        );
        assert_eq!(
            r.paused,
            [
                PausedSpan {
                    from: 5,
                    to: Some(10)
                },
                PausedSpan { from: 30, to: None }
            ]
        );
        let kinds: Vec<_> = r.annotations.iter().map(|a| a.kind.as_str()).collect();
        assert_eq!(
            kinds,
            ["resumed", "restart", "paused"],
            "unknown codes skipped"
        );
        let r = events_response(Some(&e(5, codes::RESUME)), &[e(10, codes::RESUME)]);
        assert!(r.paused.is_empty());
    }

    #[test]
    fn import_annotations_carry_their_counts() {
        let e = |kind, detail: &str| Event {
            at: 7,
            kind,
            detail: detail.into(),
        };
        let note = |e: &Event| annotations(std::slice::from_ref(e))[0].import.clone();
        assert_eq!(
            note(&e(
                codes::IMPORT_FINISHED,
                "job=4 source=magnetico new=10 merged=3"
            )),
            Some(ImportNote {
                job: 4,
                source: "magnetico".into(),
                new: Some(10),
                merged: Some(3),
                removed: None,
            })
        );
        assert_eq!(
            note(&e(
                codes::IMPORT_REMOVED,
                "job=4 source=magnetico removed=9"
            ))
            .map(|n| n.removed),
            Some(Some(9))
        );
        let started = note(&e(codes::IMPORT_STARTED, "job=5 source=bitmagnet")).unwrap();
        assert_eq!((started.job, started.new), (5, None));
        // No job: no note, but the annotation stays.
        assert_eq!(note(&e(codes::IMPORT_STARTED, "garbled")), None);
        assert_eq!(note(&e(5, "job=1")), None, "other kinds have none");
        // The store's codes follow the pipeline's without overlap.
        for (code, _) in &codes::EVENTS[5..] {
            assert!(*code > lodestar_pipeline::event::INTENSITY);
        }
    }

    /// The sealed days' sums give exactly what reading every hour gives.
    #[test]
    fn sealed_days_read_as_their_hours() {
        let mut store = Store::open_in_memory(lodestar_store::StoreConfig::default()).unwrap();
        let now_h = NOW / 3600;
        let (mut counters, mut cohorts) = (Vec::new(), Vec::new());
        for h in now_h - 40 * 24..now_h {
            let k = h % 7;
            if k == 3 {
                continue; // a gap
            }
            let c = |metric, dim, key, value| Counter {
                hour: h,
                metric,
                dim,
                key,
                value,
            };
            counters.extend([
                c(codes::metric::NEWLY_SEARCHABLE, (k % 3) as u32, 0, k + 1),
                // Imports apart (key 1), in the same hours (IMP-5).
                c(codes::metric::NEWLY_SEARCHABLE, (k % 2) as u32, 1, k % 4),
                c(codes::metric::STORED, 0, 0, k),
                c(
                    codes::metric::COVERAGE,
                    codes::metric::COVERAGE_CRAWL,
                    0,
                    3600,
                ),
                c(codes::metric::TIME_TO_SEARCHABLE, 0, 40 + k, 2),
            ]);
            cohorts.extend([
                row(h, codes::HARVESTED, 0, 5 + k),
                row(h, codes::STORED, 0, 2),
                row(h, codes::SEARCHABLE, 0, 1 + k % 2),
                row(h, codes::LOST, (k % 3 + 1) as u8, 1),
            ]);
        }
        store.add_counters(&counters).unwrap();
        store
            .write(&lodestar_store::WriteBatch {
                cohorts: &cohorts,
                ..lodestar_store::WriteBatch::default()
            })
            .unwrap();
        let cache = OverviewCache::default();
        let sealed = Sealed {
            cache: &cache,
            generation: 0,
            floor: None,
        };
        for w in [
            window(&q("30d"), NOW, None, 0).unwrap(),
            Window::new(now_h - 40 * 24, now_h - 10 * 24, 0),
        ] {
            let summed = read_period(&store, &w, now_h, Some(sealed), 0).unwrap();
            let direct = read_period(&store, &w, now_h, None, 0).unwrap();
            let (a, b) = (
                period(&w, now_h, &summed, &ST),
                period(&w, now_h, &direct, &ST),
            );
            assert_eq!(a.trend, b.trend);
            assert_eq!(
                (
                    a.stages,
                    a.new_searchable,
                    a.new_searchable_imported,
                    a.yield_,
                    a.p50,
                    a.p95
                ),
                (
                    b.stages,
                    b.new_searchable,
                    b.new_searchable_imported,
                    b.yield_,
                    b.p50,
                    b.p95
                )
            );
            assert_eq!(funnel(&a.cohorts), funnel(&b.cohorts));
            let names = BTreeMap::new();
            assert_eq!(
                segments(&summed, &a.cohorts, &names, true),
                segments(&direct, &b.cohorts, &names, true)
            );
        }
        assert!(cache.days() >= 22);
        // A forget drops the days from its hour on, and sums read from a
        // snapshot older than it aren't kept.
        let from = now_h - 20 * 24;
        let before = cache.days();
        cache.forget_days_from(from);
        let dropped = before - cache.days();
        assert!(dropped > 0 && cache.days() > 0);
        sealed_days(
            &store,
            &cache,
            0,
            from.div_euclid(24) * 24,
            sealed_before(now_h, None),
        )
        .unwrap();
        assert_eq!(
            cache.days(),
            before - dropped,
            "an old generation's sums aren't kept"
        );
        assert!(warm(&store, &cache, NOW, None).unwrap() >= 32);
        // The floor keeps in-flight cohorts' days unsealed.
        let floor = Some(now_h - 15 * 24);
        assert_eq!(
            sealed_before(now_h, floor),
            (now_h - 15 * 24).div_euclid(24) * 24
        );
        assert_eq!(
            sealed_before(now_h, Some(now_h)),
            sealed_before(now_h, None)
        );
    }

    /// A window reaching a compacted day is bucketed by day, however
    /// short; with Compare, both windows share the grain.
    #[test]
    fn compacted_days_are_read_by_day() {
        let now_h = NOW / 3600;
        let compacted = (now_h / 24 - 31) * 24;
        let custom = |from: i64, to: i64| OverviewQuery {
            range: Some("custom".into()),
            from: Some(from * 3600),
            to: Some(to * 3600),
            compare: None,
        };
        let w = window(&custom(compacted - 30, compacted + 5), NOW, None, compacted).unwrap();
        assert_eq!(w.grain, 24);
        assert_eq!((w.from_h % 24, w.to_h % 24), (0, 0));
        let w = window(&custom(compacted, compacted + 5), NOW, None, compacted).unwrap();
        assert_eq!((w.grain, w.from_h), (1, compacted));
        let (w2, prev) = windows(w, true, compacted);
        let prev = prev.unwrap();
        assert_eq!((w2.grain, prev.grain), (24, 24));
        assert_eq!(prev.to_h, w2.from_h);
        assert_eq!(prev.span(), w2.span());
        // Far from the boundary nothing changes.
        let w = window(&q("24h"), NOW, None, compacted).unwrap();
        assert_eq!(windows(w, true, compacted).0, w);
        assert_eq!(windows(w, false, compacted), (w, None));
    }

    /// The overview's reads over compacted days equal the same reads of
    /// the hours before compaction, fetches included; fetches before
    /// they were counted are unknown, not 0.
    #[test]
    fn reads_over_compacted_days_equal_their_hours() {
        let mut store = Store::open_in_memory(lodestar_store::StoreConfig::default()).unwrap();
        let now_h = NOW / 3600;
        let (mut counters, mut cohorts) = (Vec::new(), Vec::new());
        for h in now_h - 60 * 24..now_h {
            let k = h % 5;
            if k == 2 {
                continue;
            }
            let c = |metric, dim, key, value| Counter {
                hour: h,
                metric,
                dim,
                key,
                value,
            };
            counters.extend([
                c(codes::metric::NEWLY_SEARCHABLE, (k % 3) as u32, 0, k + 1),
                // Imports apart (key 1), in the same hours (IMP-5).
                c(codes::metric::NEWLY_SEARCHABLE, (k % 2) as u32, 1, k % 4),
                c(codes::metric::STORED, 0, 0, k),
                c(
                    codes::metric::COVERAGE,
                    codes::metric::COVERAGE_CRAWL,
                    0,
                    3600,
                ),
                c(
                    codes::metric::COVERAGE,
                    codes::metric::COVERAGE_INDEX,
                    0,
                    3000,
                ),
                c(codes::metric::TIME_TO_SEARCHABLE, 0, 30 + k, 3),
                c(
                    codes::metric::FUNNEL_EVENTS,
                    u32::from(codes::FETCHED),
                    0,
                    2 + k,
                ),
                c(codes::metric::FUNNEL_EVENTS, u32::from(codes::LOST), 2, 1),
            ]);
            cohorts.extend([
                row(h, codes::HARVESTED, 0, 6 + k),
                row(h, codes::STORED, 0, 3),
                row(h, codes::SEARCHABLE, 0, 2),
                row(h, codes::LOST, (k % 3 + 1) as u8, 1),
            ]);
        }
        store.add_counters(&counters).unwrap();
        store
            .set_gauges(&[Counter {
                // In the day before the second range starts.
                hour: (now_h - 50 * 24).div_euclid(24) * 24 - 24 + 7,
                metric: codes::metric::SEARCHABLE,
                dim: 0,
                key: 0,
                value: 77,
            }])
            .unwrap();
        store
            .write(&lodestar_store::WriteBatch {
                cohorts: &cohorts,
                ..lodestar_store::WriteBatch::default()
            })
            .unwrap();
        let ranges = [
            Window::new(now_h - 60 * 24, now_h + 1, 0),
            Window::new(now_h - 50 * 24, now_h - 40 * 24, 0),
            window(&q("30d"), NOW, None, 0).unwrap(),
        ];
        let read = |store: &Store, st: &InsightsState| {
            let cache = OverviewCache::default();
            let sealed = Sealed {
                cache: &cache,
                generation: 0,
                floor: None,
            };
            ranges
                .iter()
                .map(|w| {
                    let rows =
                        read_period(store, w, now_h, Some(sealed), st.compacted_before_day * 24)
                            .unwrap();
                    let p = period(w, now_h, &rows, st);
                    let names = BTreeMap::new();
                    (
                        p.trend.clone(),
                        p.stages,
                        p.fetch,
                        p.new_searchable,
                        p.new_searchable_imported,
                        p.yield_,
                        p.p50,
                        p.p95,
                        funnel(&p.cohorts),
                        segments(&rows, &p.cohorts, &names, true),
                        searchable_before(store, w.from_h, st.compacted_before_day * 24).unwrap(),
                    )
                })
                .collect::<Vec<_>>()
        };
        let before = read(&store, &ST);
        let kind = |m: u32| {
            Some(if m == codes::metric::SEARCHABLE {
                lodestar_store::Aggregation::Last
            } else {
                lodestar_store::Aggregation::Sum
            })
        };
        let done = store
            .compact_rollups(lodestar_store::CompactOptions {
                now: NOW,
                keep_days: 30,
                hold_from_day: None,
                budget: std::time::Duration::from_secs(60),
                kind: &kind,
            })
            .unwrap();
        assert!(done.done && done.days >= 29, "{done:?}");
        let st = store.insights_state().unwrap();
        assert!(st.compacted_before_day * 24 > now_h - 32 * 24);
        let after = read(
            &store,
            &InsightsState {
                funnel_events_since_hour: 0,
                ..st
            },
        );
        assert_eq!(after, before);
        assert_eq!(before[1].10, Some(77), "the compacted day's gauge");
        assert!(before[0].2.is_some_and(|n| n > 0));
        // Fetches weren't counted before `funnel_events_since_hour`.
        let late = InsightsState {
            funnel_events_since_hour: now_h - 10 * 24,
            ..st
        };
        let rows = read_period(
            &store,
            &ranges[2],
            now_h,
            None,
            st.compacted_before_day * 24,
        )
        .unwrap();
        assert_eq!(period(&ranges[2], now_h, &rows, &late).fetch, None);
    }

    #[test]
    fn segment_movers_rank_by_change() {
        let count = |key: &str, searchable| SegmentCount {
            key: key.into(),
            searchable,
        };
        let seg = |groups: Vec<SegmentCount>, types: Vec<SegmentCount>| Segments {
            by_content_type: types,
            by_group: groups,
            by_source: Vec::new(),
        };
        let now = seg(
            vec![count("video", 50), count("audio", 5)],
            vec![count("movie", 40), count("tv", 10)],
        );
        let then = seg(
            vec![count("video", 30), count("audio", 5), count("images", 8)],
            vec![count("movie", 20), count("tv", 10)],
        );
        let got: Vec<_> = segment_movers(&now, &then, 10)
            .into_iter()
            .map(|m| (m.list, m.key, m.current, m.previous, m.delta))
            .collect();
        assert_eq!(
            got,
            [
                ("by_content_type", "movie".into(), 40, 20, 20),
                ("by_group", "video".into(), 50, 30, 20),
                // Gone this period: 0 now.
                ("by_group", "images".into(), 0, 8, -8),
            ]
        );
        assert_eq!(segment_movers(&now, &then, 1).len(), 1);
        assert!(segment_movers(&now, &now, 5).is_empty(), "nothing moved");
    }

    #[test]
    fn yield_below_baseline_needs_volume_and_a_drop() {
        let now_h = NOW / 3600;
        let split = now_h - RECENT_TO_H - 24;
        let rows = |recent: (i64, i64), base: (i64, i64)| {
            vec![
                row(split, codes::HARVESTED, 0, recent.0),
                row(split, codes::SEARCHABLE, 0, recent.1),
                row(split - 30, codes::HARVESTED, 0, base.0),
                row(split - 30, codes::SEARCHABLE, 0, base.1),
            ]
        };
        // 28% now against 40%: below 75% of the baseline is a warning,
        // below half of it an error.
        let e = yield_below_baseline(&rows((1000, 280), (7000, 2800)), now_h).unwrap();
        assert_eq!(
            (e.kind.as_str(), e.severity),
            ("yield_below_baseline", Severity::Warning)
        );
        assert_eq!(e.value, Some(0.28));
        assert_eq!(e.since, split * 3600);
        let e = yield_below_baseline(&rows((1000, 150), (7000, 2800)), now_h).unwrap();
        assert_eq!(e.severity, Severity::Error);
        // Within 75% of the baseline: nothing.
        assert!(yield_below_baseline(&rows((1000, 310), (7000, 2800)), now_h).is_none());
        // Too few keys on either side: nothing.
        assert!(yield_below_baseline(&rows((499, 0), (7000, 2800)), now_h).is_none());
        assert!(yield_below_baseline(&rows((1000, 0), (3499, 2800)), now_h).is_none());
        // A baseline that found nothing has nothing to fall below.
        assert!(yield_below_baseline(&rows((1000, 0), (7000, 0)), now_h).is_none());
    }

    #[test]
    fn content_types_group() {
        assert_eq!(type_name(UNCLASSIFIED).0, "unclassified");
        assert_eq!(type_name(200).0, "type_200");
        for t in ContentType::ALL {
            assert_eq!(type_name(u32::from(t.code())).1, Some(t));
            assert!(!group_of(t).is_empty());
        }
    }

    /// F-filter-load-fail-closed: an error while a row fails closed, a
    /// warning while they only release, nothing once all read again.
    #[test]
    fn unreadable_filter_rows_are_an_exception_until_fixed() {
        use lodestar_filter::load::{RowKind, Unreadable};
        let row = |kind, id, since| Unreadable { kind, id, since };
        assert_eq!(unreadable_rows(&[]), None);
        let e =
            unreadable_rows(&[row(RowKind::Allow, 6, 50), row(RowKind::Override, 2, 40)]).unwrap();
        assert_eq!(e.kind, "filter_rows_unreadable");
        assert_eq!(e.severity, Severity::Warning);
        assert_eq!((e.since, e.value), (40, Some(2.0)));
        assert_eq!(e.rows, ["A-6", "O-2"]);
        assert_eq!(
            (e.fix.kind.as_str(), e.fix.target.as_str()),
            ("settings", "filters")
        );
        let many: Vec<_> = (0..30)
            .map(|i| row(RowKind::Rule, i, 10))
            .chain([row(RowKind::Unless, 3, 10)])
            .collect();
        let e = unreadable_rows(&many).unwrap();
        assert_eq!(e.severity, Severity::Error);
        assert_eq!(e.value, Some(30.0), "U-3 counts once");
        assert_eq!(e.rows.len(), MAX_ROWS_NAMED, "U-3 is named once");
        let json = serde_json::to_value(&e).unwrap();
        assert_eq!(json["rows"].as_array().unwrap().len(), MAX_ROWS_NAMED);
    }
}
