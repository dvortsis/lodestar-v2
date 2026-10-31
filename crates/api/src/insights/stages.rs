//! The stage detail pages (§10.9.5): `GET /insights/{discover|fetch|
//! understand|index|serve}`.
//!
//! - **Rollups and snapshots only** (§1.1.7), as the overview: each read
//!   is an indexed range of `rollup_hour`/`cohort_hour` or their daily
//!   twins below `compacted_before_day`, in one read transaction that also
//!   reads the boundary.
//! - **Every §6.8 metric is a field.** One with no counter yet is `null`
//!   (`docs/metrics.md` §7.7 says which counter it waits for), never 0.
//! - **Gaps, not zeros:** a bucket nothing measured (no coverage row: the
//!   crawler paused, the server down) is `null`; fetch, loss and block
//!   counts are also `null` before `funnel_events_since_hour`.
//! - **Cached** per (normalized query, stage, reason, last flush), like
//!   the overview; the snapshot fields are read for every request.

use std::collections::BTreeMap;
use std::sync::{Arc, PoisonError};

use axum::Json;
use axum::extract::State;
use lodestar_classify::ContentType;
use lodestar_store::{InsightsState, RollupBy, Store};

use super::{
    ApiError, ApiResult, ApiState, CohortCount, InsightsSource, MetricDays, OverviewCache,
    OverviewQuery, Sealed, Stale, Window, codes, compare_flag, funnel, internal, period,
    read_period, sources, stale_of, type_lists, window, windows,
};
use crate::dto::insights::{
    DiscoverKpi, DiscoverPoint, DiscoverPrevious, DiscoverSource, FetchKpi, FetchPoint,
    FetchPrevious, FetchResponse, IndexKpi, IndexPoint, IndexPrevious, IndexResponse,
    InsightsDiscoverResponse, LossCount, RemovedCount, ServeKpi, ServePoint, ServePrevious,
    ServeResponse, StageQuery, StrategyCount, UnderstandKpi, UnderstandPoint, UnderstandPrevious,
    UnderstandResponse,
};
use crate::extract::Query;

/// A stage page.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum Stage {
    Discover,
    Fetch,
    Understand,
    Index,
    Serve,
}

/// A built stage page: the JSON response, and what `export.csv` reads.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum StagePage {
    Discover(InsightsDiscoverResponse),
    Fetch(FetchResponse),
    Understand(UnderstandResponse),
    Index(IndexResponse),
    Serve(ServeResponse),
}

impl StagePage {
    pub(crate) fn to_value(&self) -> serde_json::Value {
        match self {
            StagePage::Discover(r) => serde_json::to_value(r),
            StagePage::Fetch(r) => serde_json::to_value(r),
            StagePage::Understand(r) => serde_json::to_value(r),
            StagePage::Index(r) => serde_json::to_value(r),
            StagePage::Serve(r) => serde_json::to_value(r),
        }
        .expect("a stage page serializes")
    }
}

/// The cache key of a stage page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PageKey {
    stage: Stage,
    window: Window,
    previous: Option<Window>,
    reason: Option<u8>,
    now_h: i64,
    adult: bool,
    last_flush: Option<i64>,
    last_recorded: i64,
}

/// `reason`: a §6.4 loss name, as its code.
pub(crate) fn reason_code(reason: Option<&str>) -> ApiResult<Option<u8>> {
    let Some(name) = reason else {
        return Ok(None);
    };
    codes::LOSSES
        .iter()
        .find(|(_, n)| *n == name)
        .map(|(c, _)| Some(*c))
        .ok_or_else(|| {
            ApiError::bad_request(
                "bad_parameter",
                "Unknown reason: use a loss name such as no_peers, timed_out or blocked.",
            )
            .field("reason")
        })
}

/// One metric's sealed days `from..to` by (dim, key): from the cache,
/// reading the days it lacks in one pass (`last`: a gauge, its last
/// snapshot in each day under (0, 0)), kept if no forget came since.
fn sealed_metric(
    store: &Store,
    sealed: &Sealed<'_>,
    metric: u32,
    last: bool,
    from: i64,
    to: i64,
) -> lodestar_store::Result<MetricDays> {
    let tag = if last { metric | super::LAST } else { metric };
    let (mut have, missing) = sealed.cache.metric_days(tag, from, to);
    if let (Some(&a), Some(&b)) = (missing.first(), missing.last()) {
        let mut read: BTreeMap<i64, BTreeMap<(u32, i64), i64>> =
            missing.iter().map(|&d| (d, BTreeMap::new())).collect();
        if last {
            for c in store.rollup_sums(metric, a, b + 24, RollupBy::Hour)? {
                // In time order: the day's last hour wins.
                if let Some(day) = read.get_mut(&(c.hour.div_euclid(24) * 24)) {
                    day.insert((0, 0), c.value);
                }
            }
        } else {
            for c in store.rollup_sums(metric, a, b + 24, RollupBy::All)? {
                if let Some(day) = read.get_mut(&(c.hour.div_euclid(24) * 24)) {
                    *day.entry((c.dim, c.key)).or_default() += c.value;
                }
            }
        }
        let read: MetricDays = read.into_iter().map(|(d, r)| (d, Arc::new(r))).collect();
        sealed.cache.keep_metric_days(sealed.generation, tag, &read);
        have.extend(read);
    }
    Ok(have)
}

/// The metrics the stage pages read by day: summed, and gauges.
const SUMMED: [u32; 8] = [
    codes::metric::COVERAGE,
    codes::metric::HARVESTED,
    codes::metric::REDISCOVERED,
    codes::metric::DROPPED,
    codes::metric::FUNNEL_EVENTS,
    codes::metric::STORED,
    codes::metric::NEWLY_SEARCHABLE,
    codes::metric::REMOVED,
];
const GAUGES: [u32; 1] = [codes::metric::SEARCHABLE];

/// Reads the stage pages' sealed days `from..to` that the cache lacks
/// (with the overview's, from [`super::warm`]).
pub(super) fn warm_metrics(
    store: &Store,
    sealed: &Sealed<'_>,
    from: i64,
    to: i64,
) -> lodestar_store::Result<()> {
    for metric in SUMMED {
        sealed_metric(store, sealed, metric, false, from, to)?;
    }
    for metric in GAUGES {
        sealed_metric(store, sealed, metric, true, from, to)?;
    }
    Ok(())
}

/// Where a window's rows come from: days below `cb` from the day tables,
/// sealed days `cb..split` from the cache, the rest hour by hour. Hourly
/// windows are hourly throughout.
fn spans(c: &Ctx<'_>) -> (i64, i64) {
    let cb = if c.w.grain == 24 {
        c.compacted.clamp(c.w.from_h, c.w.to_h)
    } else {
        c.w.from_h
    };
    let split = if c.w.grain == 24 {
        super::sealed_before(c.now_h, c.sealed.floor).clamp(cb, c.w.to_h)
    } else {
        cb
    };
    (cb, split)
}

/// A metric's rows over a window, at both grains, keyed by the window's
/// bucket: days below the boundary from `rollup_day` plus stragglers,
/// sealed days from the cache, the rest hour by hour. Values are summed
/// per (bucket, dim, key); only `Sum` metrics are read this way.
fn read_metric(c: &Ctx<'_>, metric: u32) -> lodestar_store::Result<BTreeMap<(i64, u32, i64), i64>> {
    let (store, w) = (c.store, &c.w);
    let (cb, split) = spans(c);
    let mut rows = Vec::new();
    if cb > w.from_h {
        rows.extend(store.rollup_day_sums(
            metric,
            w.from_h.div_euclid(24),
            cb.div_euclid(24),
            RollupBy::All,
        )?);
        rows.extend(store.rollup_straggler_sums(metric, w.from_h, cb, RollupBy::All)?);
    }
    rows.extend(store.rollup_sums(metric, split, w.to_h, RollupBy::All)?);
    let mut out = BTreeMap::new();
    for r in rows {
        *out.entry((w.bucket_of(r.hour), r.dim, r.key)).or_default() += r.value;
    }
    if split > cb {
        for (d, day) in sealed_metric(store, &c.sealed, metric, false, cb, split)? {
            for (&(dim, key), &v) in day.iter() {
                *out.entry((w.bucket_of(d), dim, key)).or_default() += v;
            }
        }
    }
    Ok(out)
}

/// A gauge's last snapshot in each bucket (`metric::SEARCHABLE`): the
/// last hourly row, or a day's (compacted: its last hour's). Stragglers
/// below the boundary aren't read: the gauge is only ever written at the
/// current hour.
fn last_per_bucket(c: &Ctx<'_>, metric: u32) -> lodestar_store::Result<BTreeMap<i64, i64>> {
    let (store, w) = (c.store, &c.w);
    let (cb, split) = spans(c);
    // In time order: a later snapshot replaces an earlier one's.
    let mut out = BTreeMap::new();
    if cb > w.from_h {
        for r in store.rollup_day_sums(
            metric,
            w.from_h.div_euclid(24),
            cb.div_euclid(24),
            RollupBy::Hour,
        )? {
            out.insert(w.bucket_of(r.hour), r.value);
        }
    }
    if split > cb {
        for (d, day) in sealed_metric(store, &c.sealed, metric, true, cb, split)? {
            if let Some(&v) = day.get(&(0, 0)) {
                out.insert(w.bucket_of(d), v);
            }
        }
    }
    for r in store.rollup_sums(metric, split, w.to_h, RollupBy::Hour)? {
        out.insert(w.bucket_of(r.hour), r.value);
    }
    Ok(out)
}

/// A cohort stage's counts per bucket (first harvests, for Discover):
/// sealed days from the overview's own day sums.
fn cohort_buckets(c: &Ctx<'_>, stage: u8) -> lodestar_store::Result<BTreeMap<i64, u64>> {
    let (store, w) = (c.store, &c.w);
    let (cb, split) = spans(c);
    let mut rows: Vec<CohortCount> = Vec::new();
    if cb > w.from_h {
        rows.extend(store.cohort_day_sums(w.from_h.div_euclid(24), cb.div_euclid(24))?);
        rows.extend(store.cohort_sums(w.from_h, cb, 24)?);
    }
    if split > cb {
        for day in super::sealed_days(store, c.sealed.cache, c.sealed.generation, cb, split)? {
            rows.extend_from_slice(&day.cohorts);
        }
    }
    rows.extend(store.cohort_sums(split, w.to_h, w.grain)?);
    let mut out = BTreeMap::new();
    for r in rows.iter().filter(|r| r.stage == stage) {
        *out.entry(w.bucket_of(r.cohort)).or_default() += r.count.max(0) as u64;
    }
    Ok(out)
}

/// Sums of `rows` per bucket, where `keep` says which (dim, key) count.
fn per_bucket(
    rows: &BTreeMap<(i64, u32, i64), i64>,
    keep: impl Fn(u32, i64) -> bool,
) -> BTreeMap<i64, u64> {
    let mut out = BTreeMap::new();
    for (&(b, dim, key), &v) in rows {
        if keep(dim, key) {
            *out.entry(b).or_default() += v.max(0) as u64;
        }
    }
    out
}

fn total(rows: &BTreeMap<i64, u64>) -> u64 {
    rows.values().sum()
}

/// What one window's builder reads beside the store.
struct Ctx<'a> {
    store: &'a Store,
    w: Window,
    now_h: i64,
    st: &'a InsightsState,
    compacted: i64,
    adult: bool,
    names: &'a BTreeMap<u8, String>,
    sealed: Sealed<'a>,
    reason: Option<u8>,
}

impl Ctx<'_> {
    /// Coverage seconds per (bucket, dim).
    fn coverage(&self) -> lodestar_store::Result<BTreeMap<(i64, u32), i64>> {
        let rows = read_metric(self, codes::metric::COVERAGE)?;
        let mut out = BTreeMap::new();
        for ((b, dim, _), v) in rows {
            *out.entry((b, dim)).or_default() += v;
        }
        Ok(out)
    }

    /// Whether fetches, losses and blocks were counted by event time for
    /// the whole of bucket `b` (or the whole window, for `None`).
    fn events_known(&self, b: Option<i64>) -> bool {
        b.unwrap_or(self.w.from_h) >= self.st.funnel_events_since_hour
    }
}

/// A bucket's count: rows win; otherwise 0 where measured, null where not.
fn point(rows: &BTreeMap<i64, u64>, b: i64, measured: bool) -> Option<u64> {
    rows.get(&b).copied().or(measured.then_some(0))
}

fn measured(cov: &BTreeMap<(i64, u32), i64>, b: i64, dim: u32) -> bool {
    cov.get(&(b, dim)).is_some_and(|&s| s > 0)
}

/// `lodestar_pipeline::strategy::REFETCH`: an import queued for its info
/// dict, whatever the source.
const REFETCH: i64 = 200;

fn strategy_name(key: i64) -> String {
    match codes::strategy_name(key) {
        Some(n) => n.to_string(),
        None => format!("strategy_{key}"),
    }
}

/// One window of Discover: (kpi, trend, by_source, by_strategy).
#[allow(clippy::type_complexity)]
fn discover(
    c: &Ctx<'_>,
) -> lodestar_store::Result<(
    DiscoverKpi,
    Vec<DiscoverPoint>,
    Vec<DiscoverSource>,
    Vec<StrategyCount>,
)> {
    use codes::metric;
    let reported = read_metric(c, metric::HARVESTED)?;
    let rediscovered = read_metric(c, metric::REDISCOVERED)?;
    let dropped = read_metric(c, metric::DROPPED)?;
    let harvested = cohort_buckets(c, codes::HARVESTED)?;
    let cov = c.coverage()?;
    let all = |_, _| true;
    let (rep_b, red_b, drop_b) = (
        per_bucket(&reported, all),
        per_bucket(&rediscovered, all),
        per_bucket(&dropped, all),
    );
    let trend =
        c.w.buckets()
            .map(|b| {
                let m = measured(&cov, b, metric::COVERAGE_CRAWL);
                DiscoverPoint {
                    at: b * 3600,
                    dropped: point(&drop_b, b, m),
                    harvested: point(&harvested, b, m),
                    rediscovered: point(&red_b, b, m),
                    reported: point(&rep_b, b, m),
                }
            })
            .collect();
    let mut by_source: BTreeMap<String, DiscoverSource> = BTreeMap::new();
    let name = |dim: u32| {
        u8::try_from(dim)
            .ok()
            .and_then(|d| c.names.get(&d).cloned())
            .unwrap_or_else(|| format!("source_{dim}"))
    };
    for (rows, which) in [(&reported, 0), (&rediscovered, 1), (&dropped, 2)] {
        for (&(_, dim, _), &v) in rows {
            let key = name(dim);
            let e = by_source.entry(key.clone()).or_insert(DiscoverSource {
                dropped: 0,
                key,
                rediscovered: 0,
                reported: 0,
            });
            let v = v.max(0) as u64;
            match which {
                0 => e.reported += v,
                1 => e.rediscovered += v,
                _ => e.dropped += v,
            }
        }
    }
    let mut by_source: Vec<DiscoverSource> = by_source.into_values().collect();
    by_source.sort_by(|a, b| b.reported.cmp(&a.reported).then(a.key.cmp(&b.key)));
    // Strategy codes are each plugin's own: only the DHT's are named from
    // its table; the pipeline's refetch (200) is the same for every source.
    let mut by_strategy: BTreeMap<String, u64> = BTreeMap::new();
    for (&(_, dim, key), &v) in &reported {
        let plugin = name(dim);
        let label = if plugin == "dht" || key == REFETCH {
            strategy_name(key)
        } else {
            format!("{plugin}:{key}")
        };
        *by_strategy.entry(label).or_default() += v.max(0) as u64;
    }
    let mut by_strategy: Vec<StrategyCount> = by_strategy
        .into_iter()
        .map(|(key, reported)| StrategyCount { key, reported })
        .collect();
    by_strategy.sort_by(|a, b| b.reported.cmp(&a.reported).then(a.key.cmp(&b.key)));
    let kpi = DiscoverKpi {
        bep51_sample_yield: None,
        dht_nodes: None,
        dropped: total(&drop_b),
        harvested: total(&harvested),
        keyspace_coverage: None,
        rediscovered: total(&red_b),
        reply_ratio: None,
        reported: total(&rep_b),
        seen_filter_load: None,
    };
    Ok((kpi, trend, by_source, by_strategy))
}

/// Fetches and losses by event time, for Fetch and Understand: the rows
/// of `FUNNEL_EVENTS`.
fn funnel_events(c: &Ctx<'_>) -> lodestar_store::Result<BTreeMap<(i64, u32, i64), i64>> {
    read_metric(c, codes::metric::FUNNEL_EVENTS)
}

/// One window of Fetch: (kpi, trend, losses, funnel, funnel_total).
#[allow(clippy::type_complexity)]
fn fetch(
    c: &Ctx<'_>,
) -> lodestar_store::Result<(
    FetchKpi,
    Vec<FetchPoint>,
    Option<Vec<LossCount>>,
    Vec<crate::dto::insights::FunnelCohort>,
    crate::dto::insights::FunnelTotal,
)> {
    let fetched_s = u32::from(codes::FETCHED);
    let lost_s = u32::from(codes::LOST);
    let ev = funnel_events(c)?;
    let cov = c.coverage()?;
    let reason = c.reason.map(i64::from);
    let fetched = per_bucket(&ev, |d, _| d == fetched_s);
    let lost = per_bucket(&ev, |d, k| d == lost_s && reason.is_none_or(|r| r == k));
    let trend =
        c.w.buckets()
            .map(|b| {
                let known = c.events_known(Some(b));
                let m = measured(&cov, b, codes::metric::COVERAGE_CRAWL);
                FetchPoint {
                    at: b * 3600,
                    fetched: known.then(|| point(&fetched, b, m)).flatten(),
                    lost: known.then(|| point(&lost, b, m)).flatten(),
                }
            })
            .collect();
    let known = c.events_known(None);
    let losses = known.then(|| {
        let mut by: BTreeMap<i64, u64> = codes::LOSSES
            .iter()
            .map(|(r, _)| (i64::from(*r), 0))
            .collect();
        for (&(_, dim, key), &v) in &ev {
            if dim == lost_s {
                *by.entry(key).or_default() += v.max(0) as u64;
            }
        }
        let all: u64 = by.values().sum();
        let mut list: Vec<LossCount> = by
            .into_iter()
            .filter(|(k, _)| reason.is_none_or(|r| r == *k))
            .map(|(k, count)| LossCount {
                count,
                key: u8::try_from(k)
                    .ok()
                    .and_then(codes::loss_name)
                    .map_or_else(|| format!("reason_{k}"), str::to_string),
                share: super::ratio(count, all),
            })
            .collect();
        list.sort_by(|a, b| b.count.cmp(&a.count).then(a.key.cmp(&b.key)));
        list
    });
    // The cohort funnel, as the overview reads it.
    let rows = read_period(c.store, &c.w, c.now_h, Some(c.sealed), c.compacted)?;
    let p = period(&c.w, c.now_h, &rows, c.st);
    let (mut list, mut tot) = funnel(&p.cohorts);
    if let Some(r) = c.reason.and_then(codes::loss_name) {
        for f in &mut list {
            f.losses.retain(|k, _| k == r);
        }
        tot.losses.retain(|k, _| k == r);
        tot.loss_share.retain(|k, _| k == r);
    }
    let kpi = FetchKpi {
        ephemeral_port_pressure: None,
        fetch_latency_p50_ms: None,
        fetch_latency_p95_ms: None,
        fetched: known.then(|| total(&fetched)),
        in_flight: None,
        limiter_rejections: None,
        lost: known.then(|| total(&lost)),
        mse_share: None,
        queue_age_s: None,
        queue_depth: None,
        utp_share: None,
    };
    Ok((kpi, trend, losses, list, tot))
}

/// One window of Understand: (kpi, trend, by_content_type, by_group).
#[allow(clippy::type_complexity)]
fn understand(
    c: &Ctx<'_>,
) -> lodestar_store::Result<(
    UnderstandKpi,
    Vec<UnderstandPoint>,
    Vec<crate::dto::insights::SegmentCount>,
    Vec<crate::dto::insights::SegmentCount>,
)> {
    use codes::metric;
    let stored = per_bucket(&read_metric(c, metric::STORED)?, |_, _| true);
    let ev = funnel_events(c)?;
    let blocked_code = codes::BLOCKED;
    let blocked = per_bucket(&ev, |d, k| {
        d == u32::from(codes::LOST) && k == i64::from(blocked_code)
    });
    // Crawled only: imports never went through Understand's pipeline.
    let mut newly = read_metric(c, metric::NEWLY_SEARCHABLE)?;
    newly.retain(|&(_, _, key), _| key == 0);
    let cov = c.coverage()?;
    let trend =
        c.w.buckets()
            .map(|b| {
                let m = measured(&cov, b, metric::COVERAGE_CRAWL);
                UnderstandPoint {
                    at: b * 3600,
                    blocked: c
                        .events_known(Some(b))
                        .then(|| point(&blocked, b, m))
                        .flatten(),
                    stored: point(&stored, b, m),
                }
            })
            .collect();
    let (types, groups) = type_lists(
        newly
            .iter()
            .map(|(&(_, dim, _), &v)| (dim, v.max(0) as u64)),
        c.adult,
    );
    let kpi = UnderstandKpi {
        blocked: c.events_known(None).then(|| total(&blocked)),
        junk_distribution: None,
        newly_searchable: newly.values().map(|&v| v.max(0) as u64).sum(),
        parser_confidence: None,
        recent_corrections: None,
        resolution_rate: None,
        stored: total(&stored),
    };
    Ok((kpi, trend, types, groups))
}

/// One window of Index: (kpi, trend, removed_by_type).
fn index(c: &Ctx<'_>) -> lodestar_store::Result<(IndexKpi, Vec<IndexPoint>, Vec<RemovedCount>)> {
    use codes::metric;
    let newly_rows = read_metric(c, metric::NEWLY_SEARCHABLE)?;
    let newly = per_bucket(&newly_rows, |_, key| key == 0);
    let imported = per_bucket(&newly_rows, |_, key| key != 0);
    let removed_rows = read_metric(c, metric::REMOVED)?;
    let removed = per_bucket(&removed_rows, |_, _| true);
    let searchable = last_per_bucket(c, metric::SEARCHABLE)?;
    let cov = c.coverage()?;
    let trend =
        c.w.buckets()
            .map(|b| {
                let m = measured(&cov, b, metric::COVERAGE_INDEX);
                IndexPoint {
                    at: b * 3600,
                    new_searchable: point(&newly, b, m),
                    new_searchable_imported: point(&imported, b, m),
                    // Deletes happen paused or not: a bucket with any coverage
                    // was measured.
                    removed: point(&removed, b, m || measured(&cov, b, metric::COVERAGE_CRAWL)),
                    searchable: searchable.get(&b).map(|&v| v.max(0) as u64),
                }
            })
            .collect();
    let mut by_type: BTreeMap<String, u64> = BTreeMap::new();
    for (&(_, dim, _), &v) in &removed_rows {
        let (name, t) = super::type_name(dim);
        // Left out entirely while Adult is hidden (§20.9).
        if !c.adult && t == Some(ContentType::Adult) {
            continue;
        }
        *by_type.entry(name).or_default() += v.max(0) as u64;
    }
    let mut by_type: Vec<RemovedCount> = by_type
        .into_iter()
        .map(|(key, removed)| RemovedCount { key, removed })
        .collect();
    by_type.sort_by(|a, b| b.removed.cmp(&a.removed).then(a.key.cmp(&b.key)));
    let kpi = IndexKpi {
        data_bytes: None,
        disk_free_bytes: None,
        file_index_coverage: None,
        index_lag_s: None,
        merges: None,
        new_searchable: total(&newly),
        new_searchable_imported: total(&imported),
        projection_days: None,
        rebuild: None,
        removed: total(&removed),
        scrape_coverage: None,
        scrape_freshness_s: None,
        searchable: None,
        segments: None,
        sizes: None,
    };
    Ok((kpi, trend, by_type))
}

fn serve_kpi() -> ServeKpi {
    ServeKpi {
        alerts_fired: None,
        cancel_rate: None,
        facet_sampling_rate: None,
        latency_p50_ms: None,
        latency_p95_ms: None,
        notification_failures: None,
        notifications_delivered: None,
        searches: None,
        sends: None,
        zero_result_rate: None,
    }
}

/// One window of Serve: every count is null until its counter exists.
fn serve(c: &Ctx<'_>) -> (ServeKpi, Vec<ServePoint>) {
    let trend =
        c.w.buckets()
            .map(|b| ServePoint {
                at: b * 3600,
                searches: None,
                zero_results: None,
            })
            .collect();
    (serve_kpi(), trend)
}

/// A stage page without its snapshot fields, inside the caller's read
/// transaction.
#[allow(clippy::too_many_arguments)]
fn build(
    store: &Store,
    cache: &OverviewCache,
    generation: u64,
    floor: Option<i64>,
    st: &InsightsState,
    stage: Stage,
    w: Window,
    prev: Option<Window>,
    reason: Option<u8>,
    reason_name: Option<String>,
    now: i64,
    adult: bool,
) -> lodestar_store::Result<StagePage> {
    let names: BTreeMap<u8, String> = store
        .sources()?
        .into_iter()
        .map(|s| (s.id.get(), s.plugin))
        .collect();
    let sealed = Sealed {
        cache,
        generation,
        floor,
    };
    let ctx = |w: Window| Ctx {
        store,
        w,
        now_h: now.div_euclid(3600),
        st,
        compacted: st.compacted_before_day * 24,
        adult,
        names: &names,
        sealed,
        reason,
    };
    let cur = ctx(w);
    Ok(match stage {
        Stage::Discover => {
            let (kpi, trend, by_source, by_strategy) = discover(&cur)?;
            let previous = prev
                .map(|p| -> lodestar_store::Result<_> {
                    let (kpi, trend, ..) = discover(&ctx(p))?;
                    Ok(DiscoverPrevious {
                        kpi,
                        range: p.dto(),
                        trend,
                    })
                })
                .transpose()?;
            StagePage::Discover(InsightsDiscoverResponse {
                by_ip_family: None,
                by_source,
                by_strategy,
                kpi,
                previous,
                range: w.dto(),
                stale: None,
                trend,
                updated_at: now,
            })
        }
        Stage::Fetch => {
            let (kpi, trend, losses, funnel, funnel_total) = fetch(&cur)?;
            let previous = prev
                .map(|p| -> lodestar_store::Result<_> {
                    let (kpi, trend, ..) = fetch(&ctx(p))?;
                    Ok(FetchPrevious {
                        kpi,
                        range: p.dto(),
                        trend,
                    })
                })
                .transpose()?;
            StagePage::Fetch(FetchResponse {
                funnel,
                funnel_total,
                kpi,
                losses,
                previous,
                range: w.dto(),
                reason: reason_name,
                stale: None,
                trend,
                updated_at: now,
            })
        }
        Stage::Understand => {
            let (kpi, trend, by_content_type, by_group) = understand(&cur)?;
            let previous = prev
                .map(|p| -> lodestar_store::Result<_> {
                    let (kpi, trend, ..) = understand(&ctx(p))?;
                    Ok(UnderstandPrevious {
                        kpi,
                        range: p.dto(),
                        trend,
                    })
                })
                .transpose()?;
            StagePage::Understand(UnderstandResponse {
                by_content_type,
                by_group,
                kpi,
                previous,
                range: w.dto(),
                stale: None,
                trend,
                updated_at: now,
            })
        }
        Stage::Index => {
            let (kpi, trend, removed_by_type) = index(&cur)?;
            let previous = prev
                .map(|p| -> lodestar_store::Result<_> {
                    let (kpi, trend, _) = index(&ctx(p))?;
                    Ok(IndexPrevious {
                        kpi,
                        range: p.dto(),
                        trend,
                    })
                })
                .transpose()?;
            StagePage::Index(IndexResponse {
                kpi,
                previous,
                range: w.dto(),
                removed_by_type,
                stale: None,
                trackers: None,
                trend,
                updated_at: now,
            })
        }
        Stage::Serve => {
            let (kpi, trend) = serve(&cur);
            let previous = prev.map(|p| {
                let (kpi, trend) = serve(&ctx(p));
                ServePrevious {
                    kpi,
                    range: p.dto(),
                    trend,
                }
            });
            StagePage::Serve(ServeResponse {
                kpi,
                previous,
                range: w.dto(),
                stale: None,
                top_queries: None,
                top_zero_result_queries: None,
                trend,
                updated_at: now,
            })
        }
    })
}

/// What a stage page reads fresh for every request.
#[derive(Debug, Clone)]
struct Snap {
    now: i64,
    stale: Option<Stale>,
    searchable: Option<u64>,
    index_lag_s: Option<u64>,
    data_bytes: Option<u64>,
    disk_free_bytes: Option<u64>,
    queue_depth: u64,
    in_flight: u64,
    dht_nodes: Option<u64>,
    reply_ratio: Option<f64>,
    rebuild: Option<String>,
}

fn apply(page: &mut StagePage, s: Snap) {
    match page {
        StagePage::Discover(r) => {
            r.stale = s.stale;
            r.updated_at = s.now;
            r.kpi.dht_nodes = s.dht_nodes;
            r.kpi.reply_ratio = s.reply_ratio;
        }
        StagePage::Fetch(r) => {
            r.stale = s.stale;
            r.updated_at = s.now;
            r.kpi.queue_depth = Some(s.queue_depth);
            r.kpi.in_flight = Some(s.in_flight);
        }
        StagePage::Understand(r) => {
            r.stale = s.stale;
            r.updated_at = s.now;
        }
        StagePage::Index(r) => {
            r.stale = s.stale;
            r.updated_at = s.now;
            r.kpi.searchable = s.searchable;
            r.kpi.index_lag_s = s.index_lag_s;
            r.kpi.data_bytes = s.data_bytes;
            r.kpi.disk_free_bytes = s.disk_free_bytes;
            r.kpi.rebuild = s.rebuild;
        }
        StagePage::Serve(r) => {
            r.stale = s.stale;
            r.updated_at = s.now;
        }
    }
}

fn rebuild_phase(s: &ApiState) -> Option<String> {
    use lodestar_search::RebuildPhase as P;
    let p = s.index.as_ref()?.progress()?;
    Some(
        match p.phase {
            P::Building => "building",
            P::Finishing => "finishing",
            P::Swapping => "swapping",
            P::Failed => "failed",
        }
        .into(),
    )
}

/// A stage page as its route serves it (and `export.csv` reads it).
pub(crate) async fn stage_response(
    s: &ApiState,
    stage: Stage,
    q: StageQuery,
) -> ApiResult<StagePage> {
    let compare = compare_flag(q.compare)?;
    let reason = reason_code(q.reason.as_deref())?;
    let (ops, source) = sources(s)?;
    let stats = ops.stats().await;
    let crawler = ops.crawler();
    let now = source.now();
    let snap = Snap {
        now,
        stale: stale_of(now, stats.paused, source.last_flush(), source.started_at()),
        searchable: stats.searchable_torrents,
        index_lag_s: stats.index_lag_s,
        data_bytes: stats.data_bytes,
        disk_free_bytes: stats.disk_free_bytes,
        queue_depth: stats.queue_depth,
        in_flight: crawler.fetch.in_flight,
        dht_nodes: crawler.dht.map(|d| d.routing_nodes),
        reply_ratio: source.stage_snapshot().dht_reply_ratio,
        rebuild: rebuild_phase(s),
    };
    let adult = s.adult.shown();
    let reader = s.reader.clone();
    let oq = OverviewQuery {
        range: q.range,
        from: q.from,
        to: q.to,
        compare: q.compare,
    };
    let reason_name = q.reason;
    let mut page = tokio::task::spawn_blocking(move || -> ApiResult<StagePage> {
        let store = reader.lock().unwrap_or_else(PoisonError::into_inner);
        // Before the snapshot begins (see the overview).
        let generation = source.cache().generation();
        store
            .read_snapshot(|store| {
                Ok(page_in_snapshot(
                    store,
                    generation,
                    &oq,
                    stage,
                    compare,
                    reason,
                    reason_name,
                    now,
                    adult,
                    &*source,
                ))
            })
            .map_err(internal)?
    })
    .await
    .map_err(internal)??;
    apply(&mut page, snap);
    Ok(page)
}

#[allow(clippy::too_many_arguments)]
fn page_in_snapshot(
    store: &Store,
    generation: u64,
    q: &OverviewQuery,
    stage: Stage,
    compare: bool,
    reason: Option<u8>,
    reason_name: Option<String>,
    now: i64,
    adult: bool,
    source: &dyn InsightsSource,
) -> ApiResult<StagePage> {
    // Only Fetch reads `reason`: the other pages share one cache entry.
    let (reason, reason_name) = if stage == Stage::Fetch {
        (reason, reason_name)
    } else {
        (None, None)
    };
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
    let key = PageKey {
        stage,
        window: w,
        previous: prev,
        reason,
        now_h: now.div_euclid(3600),
        adult,
        last_flush: source.last_flush(),
        last_recorded: source.last_recorded(),
    };
    let cache = source.cache();
    if let Some(hit) = cache.page(&key) {
        return Ok((*hit).clone());
    }
    let built = Arc::new(
        build(
            store,
            cache,
            generation,
            source.seal_floor(),
            &st,
            stage,
            w,
            prev,
            reason,
            reason_name,
            now,
            adult,
        )
        .map_err(internal)?,
    );
    cache.put_page(key, built.clone());
    Ok((*built).clone())
}

macro_rules! stage_route {
    ($fn:ident, $stage:ident, $resp:ident, $op:literal, $path:literal, $doc:literal) => {
        #[doc = $doc]
        #[utoipa::path(
                                                            operation_id = $op,
                                                            get,
                                                            path = $path,
                                                            tag = "insights",
                                                            params(StageQuery),
                                                            responses((status = 200, body = $resp))
                                                        )]
        pub(crate) async fn $fn(
            State(s): State<ApiState>,
            Query(q): Query<StageQuery>,
        ) -> ApiResult<Json<$resp>> {
            match stage_response(&s, Stage::$stage, q).await? {
                StagePage::$stage(r) => Ok(Json(r)),
                _ => unreachable!("the stage asked for"),
            }
        }
    };
}

stage_route!(
    discover_page,
    Discover,
    InsightsDiscoverResponse,
    "getInsightsDiscover",
    "/insights/discover",
    "Discover (§10.9.5): keys reported, rediscovered and dropped by source and strategy, first harvests, and the DHT's snapshot. Event time, except `harvested` (discovery cohort, ≈)."
);
stage_route!(
    fetch_page,
    Fetch,
    FetchResponse,
    "getInsightsFetch",
    "/insights/fetch",
    "Fetch (§10.9.5): fetched and lost by event time, losses by reason, the queue now, and the cohort funnel. `reason` keeps only that loss."
);
stage_route!(
    understand_page,
    Understand,
    UnderstandResponse,
    "getInsightsUnderstand",
    "/insights/understand",
    "Understand (§10.9.5): stored, blocked (counts only) and newly searchable by content type and group. Event time."
);
stage_route!(
    index_page,
    Index,
    IndexResponse,
    "getInsightsIndex",
    "/insights/index",
    "Index (§10.9.5): searchable now and per bucket, new searchable, removals (a lifecycle event, by type), lag, size and rebuild state."
);
stage_route!(
    serve_page,
    Serve,
    ServeResponse,
    "getInsightsServe",
    "/insights/serve",
    "Serve (§10.9.5): searches, latency, zero-result and cancel rates, alerts, notifications and sends. Every count is null until its counter exists."
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reasons_are_loss_names() {
        assert_eq!(reason_code(None).unwrap(), None);
        assert_eq!(reason_code(Some("timed_out")).unwrap(), Some(2));
        assert_eq!(reason_code(Some("unreachable")).unwrap(), Some(10));
        let e = reason_code(Some("lost")).unwrap_err();
        assert_eq!(e.body()["error"]["field"], "reason");
        assert!(reason_code(Some("")).is_err());
    }

    #[test]
    fn a_bucket_is_its_rows_or_zero_if_measured() {
        let rows: BTreeMap<i64, u64> = [(10, 4)].into();
        assert_eq!(point(&rows, 10, false), Some(4), "rows win");
        assert_eq!(point(&rows, 11, true), Some(0));
        assert_eq!(point(&rows, 11, false), None, "a gap");
    }
}
