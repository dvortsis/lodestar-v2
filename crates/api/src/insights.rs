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
    CohortCount, Counter, Event, RollupBy, Store, UNCLASSIFIED, log_bucket_bounds,
};
use utoipa::OpenApi;

pub use crate::dto::insights::{
    Annotation, Bucket, Conversion, EventsQuery, EventsResponse, Fix, FunnelCohort, FunnelStages,
    FunnelTotal, Health, HealthState, InsightsException, InsightsRange, Kpi, OverviewQuery,
    OverviewResponse, PausedSpan, PreviousKpi, PreviousPeriod, PreviousStages, SegmentCount,
    SegmentYield, Segments, Serve, Severity, Stages, Stale, TrendPoint,
};
use crate::error::{ApiError, ApiResult, internal};
use crate::extract::Query;
use crate::ops::OpsApi;
use crate::{ApiState, auth};

/// This module's half of `/openapi.json`.
#[derive(OpenApi)]
#[openapi(
    paths(overview, events),
    tags((name = "insights", description = "The Insights dashboard's rollups (§10.9)."))
)]
pub(crate) struct ApiDoc;

pub fn routes() -> Router<ApiState> {
    auth::protected(
        Router::new()
            .route("/insights/overview", get(overview))
            .route("/insights/events", get(events)),
    )
}

/// Codes this module reads, as the pipeline persists them (append only).
pub mod codes {
    /// `rollup_hour.metric` (`lodestar_pipeline::metric`).
    pub mod metric {
        pub const STORED: u32 = 4;
        pub const NEWLY_SEARCHABLE: u32 = 10;
        pub const TIME_TO_SEARCHABLE: u32 = 11;
        pub const COVERAGE: u32 = 12;
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

    /// `events.kind` → annotation kind (`lodestar_pipeline::event`).
    pub const EVENTS: [(u32, &str); 5] = [
        (1, "restart"),
        (2, "stopped"),
        (3, "paused"),
        (4, "resumed"),
        (5, "intensity"),
    ];
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
}

/// Recent overview responses, by normalized query and the flush they were
/// built after; and the sealed days' sums, which no flush changes.
#[derive(Debug, Default)]
pub struct OverviewCache {
    entries: Mutex<VecDeque<(CacheKey, Arc<OverviewResponse>)>>,
    days: Mutex<Days>,
}

/// The sealed days' sums, and a generation [`OverviewCache::forget_days_from`]
/// bumps, so sums read from a snapshot older than a forget are never kept.
#[derive(Debug, Default)]
struct Days {
    generation: u64,
    days: BTreeMap<i64, Arc<DayRows>>,
}

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
            let stale = days.days.split_off(&(hour.div_euclid(24) * 24));
            drop(stale);
        }
        self.entries
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clear();
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
        let from = first.div_euclid(24) * 24;
        if from >= to {
            return Ok(0);
        }
        let to = to.min(from + MAX_SPAN_HOURS);
        sealed_days(store, cache, generation, from, to)?;
        Ok(cache.days())
    })
}

/// One sealed UTC day's sums: what [`read_period`] would read for it.
#[derive(Debug, Default)]
struct DayRows {
    /// Newly searchable in the day; `None` with no row.
    newly: Option<i64>,
    newly_by_dim: BTreeMap<u32, i64>,
    stored: i64,
    coverage: BTreeMap<u32, i64>,
    /// Summed over the day (`cohort` = the day's first hour).
    cohorts: Vec<CohortCount>,
    delays: BTreeMap<i64, i64>,
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
        for c in store.rollup_sums(metric::NEWLY_SEARCHABLE, a, b, RollupBy::HourDim)? {
            let d = read.get_mut(&day(c.hour)).expect("in range");
            *d.newly.get_or_insert(0) += c.value;
            *d.newly_by_dim.entry(c.dim).or_default() += c.value;
        }
        for c in store.rollup_sums(metric::STORED, a, b, RollupBy::Hour)? {
            read.get_mut(&day(c.hour)).expect("in range").stored += c.value;
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
    fn new(from_h: i64, to_h: i64) -> Window {
        if to_h - from_h <= HOURLY_MAX {
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
/// one, or whole UTC days past 7 days. `first_hour` is the earliest
/// rollup hour, for `all`.
fn window(q: &OverviewQuery, now: i64, first_hour: Option<i64>) -> ApiResult<Window> {
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
            let from_h = first_hour.unwrap_or(end_h - 24).min(end_h - 1);
            return Ok(Window::new(from_h, end_h));
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
            return Ok(Window::new(from_h, to_h));
        }
        _ => {
            return Err(bad(
                "range",
                "Unknown range: use 24h, 7d, 30d, all or custom.",
            ));
        }
    };
    if hours <= HOURLY_MAX {
        Ok(Window::new(end_h - hours, end_h))
    } else {
        // Whole UTC days, the current one last.
        let to_h = ceil_to(end_h, 24);
        Ok(Window::new(to_h - hours, to_h))
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
    newly_by_hour: Vec<Counter>,
    newly_by_dim: Vec<Counter>,
    stored_by_hour: Vec<Counter>,
    coverage: Vec<Counter>,
    cohorts_old: Vec<CohortCount>,
    cohorts_recent: Vec<CohortCount>,
    delays_old: Vec<Counter>,
    delays_recent: Vec<Counter>,
}

/// Where [`read_period`] takes sealed days' sums from.
#[derive(Debug, Clone, Copy)]
struct Sealed<'a> {
    cache: &'a OverviewCache,
    /// The cache's generation before the snapshot began.
    generation: u64,
    floor: Option<i64>,
}

fn read_period(
    store: &Store,
    w: &Window,
    now_h: i64,
    sealed: Option<Sealed<'_>>,
) -> lodestar_store::Result<PeriodRows> {
    use codes::metric;
    // Day buckets: sealed days from their sums, the rest hour by hour.
    let split = if let (24, Some(sealed)) = (w.grain, &sealed) {
        sealed_before(now_h, sealed.floor).clamp(w.from_h, w.to_h)
    } else {
        w.from_h
    };
    let cut = cutoff(w, now_h).max(split);
    let mut rows = PeriodRows {
        newly_by_hour: store.rollup_sums(
            metric::NEWLY_SEARCHABLE,
            split,
            w.to_h,
            RollupBy::Hour,
        )?,
        newly_by_dim: store.rollup_sums(metric::NEWLY_SEARCHABLE, split, w.to_h, RollupBy::Dim)?,
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
    };
    if let Some(sealed) = sealed.filter(|_| split > w.from_h) {
        let days = sealed_days(store, sealed.cache, sealed.generation, w.from_h, split)?;
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
            for (&dim, &n) in &day.newly_by_dim {
                rows.newly_by_dim.push(row(0, dim, 0, n));
            }
            rows.stored_by_hour.push(row(d, 0, 0, day.stored));
            for (&dim, &n) in &day.coverage {
                rows.coverage.push(row(d, dim, 0, n));
            }
            rows.cohorts_old.extend_from_slice(&day.cohorts);
            for (&key, &n) in &day.delays {
                rows.delays_old.push(row(0, 0, key, n));
            }
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
    new_searchable: u64,
    yield_: Option<f64>,
    p50: Option<u64>,
    p95: Option<u64>,
    trend: Vec<TrendPoint>,
    cohorts: Cohorts,
}

fn period(w: &Window, now_h: i64, rows: &PeriodRows) -> Period {
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
    let sum_by_bucket = |rows: &[Counter]| -> BTreeMap<i64, u64> {
        let mut out = BTreeMap::new();
        for c in rows {
            *out.entry(w.bucket_of(c.hour)).or_default() += c.value.max(0) as u64;
        }
        out
    };
    let newly = sum_by_bucket(&rows.newly_by_hour);
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
            let searchable = newly
                .get(&b)
                .copied()
                .or_else(|| is_covered(b, metric::COVERAGE_INDEX).then_some(0));
            TrendPoint {
                at: b * 3600,
                harvested,
                searchable,
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
        new_searchable: newly.values().sum(),
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

fn segments(rows: &PeriodRows, c: &Cohorts, names: &BTreeMap<u8, String>, adult: bool) -> Segments {
    let mut types: BTreeMap<String, u64> = BTreeMap::new();
    let mut groups: BTreeMap<String, u64> = BTreeMap::new();
    for r in &rows.newly_by_dim {
        let (name, t) = type_name(r.dim);
        // Left out entirely while Adult is hidden (§20.9).
        if !adult && t == Some(ContentType::Adult) {
            continue;
        }
        let n = r.value.max(0) as u64;
        *types.entry(name.clone()).or_default() += n;
        let group = t.map_or(name, |t| group_of(t).to_string());
        *groups.entry(group).or_default() += n;
    }
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
        by_content_type: sorted_counts(types),
        by_group: sorted_counts(groups),
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
            })
        })
        .collect()
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
        severity: Severity::Warning,
        since: from * 3600,
        unit: Some("torrents".into()),
        value: Some(in_progress as f64),
    })
}

/// Everything but the snapshot fields, which [`overview`] fills in.
fn build(
    store: &Store,
    cache: &OverviewCache,
    floor: Option<i64>,
    w: Window,
    prev: Option<Window>,
    now: i64,
    adult: bool,
) -> lodestar_store::Result<OverviewResponse> {
    let now_h = now.div_euclid(3600);
    let sealed = Sealed {
        cache,
        generation: cache.generation(),
        floor,
    };
    store.read_snapshot(|store| {
        let rows = read_period(store, &w, now_h, Some(sealed))?;
        let prev_rows = prev
            .map(|p| read_period(store, &p, now_h, Some(sealed)))
            .transpose()?;
        let now_rows = read_now(store, now_h)?;
        let events = store.events(w.from_h * 3600, w.to_h * 3600)?;
        let names: BTreeMap<u8, String> = store
            .sources()?
            .into_iter()
            .map(|s| (s.id.get(), s.plugin))
            .collect();
        let p = period(&w, now_h, &rows);
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
        let previous = prev.zip(prev_rows).map(|(pw, pr)| {
            let q = period(&pw, now_h, &pr);
            PreviousPeriod {
                kpi: PreviousKpi {
                    new_searchable: q.new_searchable,
                    searchable: None,
                    time_to_searchable_p50_s: q.p50,
                    time_to_searchable_p95_s: q.p95,
                    yield_: q.yield_,
                },
                range: pw.dto(),
                serve: Serve::default(),
                stages: PreviousStages {
                    discover: q.stages.0,
                    fetch: None,
                    serve: None,
                    understand: q.stages.1,
                },
                trend: q.trend,
            }
        });
        Ok(OverviewResponse {
            annotations: annotations(&events),
            exceptions: stalled(&now_rows, now_h).into_iter().collect(),
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
                searchable: None,
                searchable_delta: None,
                time_to_searchable_p50_s: p.p50,
                time_to_searchable_p95_s: p.p95,
                yield_: p.yield_,
            },
            previous,
            range: w.dto(),
            segments: segments(&rows, &p.cohorts, &names, adult),
            serve: Serve::default(),
            stages: Stages {
                discover: p.stages.0,
                fetch: None,
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

fn apply_snapshot(r: &mut OverviewResponse, s: Snapshot) {
    let last = s.last_flush.unwrap_or(s.started_at);
    r.stale = (s.now - last > STALE_AFTER_S).then(|| Stale {
        reason: if s.paused { "paused" } else { "writer_stalled" }.into(),
        since: last,
    });
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
    r.stages.index = s.searchable;
    r.updated_at = s.now;
    // `kpi.searchable_delta` and `previous.kpi.searchable` stay null:
    // the change since the previous period's end needs removals, which
    // nothing counts until B11b's lifecycle counters.
}

/// The Insights overview (§10.9.2): stage strip, KPIs, trend with
/// annotations, the cohort funnel, segments, exceptions and Serve, from
/// the rollups. `compare=1` adds `previous`.
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
    let compare = match q.compare {
        None | Some(0) => false,
        Some(1) => true,
        Some(_) => {
            return Err(
                ApiError::bad_request("bad_parameter", "compare is 0 or 1.").field("compare")
            );
        }
    };
    let (ops, source) = sources(&s)?;
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
        let first = if q.range.as_deref() == Some("all") {
            store.first_rollup_hour().map_err(internal)?
        } else {
            None
        };
        let w = window(&q, now, first)?;
        let prev = (compare && q.range.as_deref() != Some("all"))
            .then(|| Window::new(w.from_h - w.span(), w.from_h));
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
                &store,
                source.cache(),
                source.seal_floor(),
                w,
                prev,
                now,
                adult,
            )
            .map_err(internal)?,
        );
        source.cache().put(key, built.clone());
        Ok((*built).clone())
    })
    .await
    .map_err(internal)??;
    apply_snapshot(&mut response, snap);
    Ok(Json(response))
}

/// Trend annotations (`restart`, `stopped`, `paused`, `resumed`,
/// `intensity`; other events are skipped) and the paused spans
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

    fn q(range: &str) -> OverviewQuery {
        OverviewQuery {
            range: Some(range.into()),
            ..OverviewQuery::default()
        }
    }

    #[test]
    fn ranges_normalize_to_whole_buckets() {
        let now_h = NOW / 3600;
        let w = window(&q("24h"), NOW, None).unwrap();
        assert_eq!((w.from_h, w.to_h, w.grain), (now_h - 23, now_h + 1, 1));
        let w = window(&OverviewQuery::default(), NOW, None).unwrap();
        assert_eq!((w.span(), w.grain), (168, 1));
        let w = window(&q("30d"), NOW, None).unwrap();
        assert_eq!((w.span(), w.grain, w.from_h % 24), (720, 24, 0));
        assert!(w.to_h > now_h && w.to_h - now_h <= 24);
        let w = window(&q("all"), NOW, Some(now_h - 5)).unwrap();
        assert_eq!((w.from_h, w.to_h, w.grain), (now_h - 5, now_h + 1, 1));
        let w = window(&q("all"), NOW, Some(now_h - 1000)).unwrap();
        assert_eq!((w.grain, w.from_h % 24), (24, 0));
        let w = window(&q("all"), NOW, None).unwrap();
        assert_eq!(w.span(), 24);
        let custom = |from, to| OverviewQuery {
            range: Some("custom".into()),
            from,
            to,
            compare: None,
        };
        let w = window(&custom(Some(NOW - 7200 - 5), Some(NOW + 99_999)), NOW, None).unwrap();
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
            let e = window(&query, NOW, None).unwrap_err();
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
            window(&q("30d"), NOW, None).unwrap(),
            Window::new(now_h - 40 * 24, now_h - 10 * 24),
        ] {
            let summed = read_period(&store, &w, now_h, Some(sealed)).unwrap();
            let direct = read_period(&store, &w, now_h, None).unwrap();
            let (a, b) = (period(&w, now_h, &summed), period(&w, now_h, &direct));
            assert_eq!(a.trend, b.trend);
            assert_eq!(
                (a.stages, a.new_searchable, a.yield_, a.p50, a.p95),
                (b.stages, b.new_searchable, b.yield_, b.p50, b.p95)
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

    #[test]
    fn content_types_group() {
        assert_eq!(type_name(UNCLASSIFIED).0, "unclassified");
        assert_eq!(type_name(200).0, "type_200");
        for t in ContentType::ALL {
            assert_eq!(type_name(u32::from(t.code())).1, Some(t));
            assert!(!group_of(t).is_empty());
        }
    }
}
