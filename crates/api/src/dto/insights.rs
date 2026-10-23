//! `/insights/overview` and `/insights/events` (§10.9, `docs/metrics.md`
//! §6 and §7.5). Every rate, share, percentile and Δ here is computed by
//! the server; the client only formats. A value with nothing behind it yet
//! is `null`, never 0.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};

/// `GET /insights/overview`.
#[derive(Debug, Clone, Default, Deserialize, IntoParams)]
#[serde(default)]
#[into_params(parameter_in = Query)]
pub struct OverviewQuery {
    /// `24h`, `7d` (the default), `30d`, `all` or `custom` (with `from`
    /// and `to`).
    pub range: Option<String>,
    /// Unix seconds; `custom` only. Rounded down to its UTC hour (day,
    /// for ranges over 7 days).
    pub from: Option<i64>,
    /// Unix seconds, exclusive; `custom` only. Rounded up to its UTC hour
    /// (day), and to the current one at most.
    pub to: Option<i64>,
    /// `1` adds `previous`, the period of equal length before, and
    /// `kpi.searchable_delta`.
    pub compare: Option<u8>,
}

/// `GET /insights/events`.
#[derive(Debug, Clone, Default, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct EventsQuery {
    /// Unix seconds.
    pub from: i64,
    /// Unix seconds, exclusive.
    pub to: i64,
}

/// The overview (§10.9.2): status, movement, the funnel, segments,
/// exceptions and Serve, for one range.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct OverviewResponse {
    /// Trend annotations in the range, oldest first. Event time.
    pub annotations: Vec<Annotation>,
    /// What needs attention now, whatever the range (§10.9.2 level 5).
    pub exceptions: Vec<InsightsException>,
    /// The discovery cohorts in the range that have any count, oldest
    /// first: hours up to 7 days, UTC days beyond (`range.bucket`).
    pub funnel: Vec<FunnelCohort>,
    /// The funnel over every cohort in the range.
    pub funnel_total: FunnelTotal,
    /// Torrents newly stored in the last full UTC hour (§10.12). Event
    /// time. Null when that hour wasn't measured (the crawler paused or
    /// not running) and stored nothing.
    #[schema(required = true)]
    pub ingest_per_hour: Option<u64>,
    pub kpi: Kpi,
    /// The period of equal length just before, in the same shape;
    /// present only with `compare=1`, and never for `all`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub previous: Option<PreviousPeriod>,
    pub range: InsightsRange,
    pub segments: Segments,
    pub serve: Serve,
    pub stages: Stages,
    /// Set when the rollups haven't been flushed for over 2 minutes
    /// (§10.9.6); null when fresh.
    #[schema(required = true)]
    pub stale: Option<Stale>,
    /// Movement, one point per bucket (`range.bucket`), oldest first.
    /// Event time.
    pub trend: Vec<TrendPoint>,
    /// When the numbers were read (unix seconds).
    pub updated_at: i64,
}

/// The range as the server normalized it: whole UTC hours, or whole UTC
/// days past 7 days.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct InsightsRange {
    /// `hour` up to 7 days, `day` (UTC days) beyond.
    pub bucket: Bucket,
    /// Unix seconds, inclusive.
    pub from: i64,
    /// Unix seconds, exclusive. The current hour (day) counts so far.
    pub to: i64,
    /// The time zone buckets are in: always `UTC`.
    pub tz: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum Bucket {
    Hour,
    Day,
}

/// The stage strip (§10.9.1, `docs/metrics.md` §6.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct Stages {
    /// Unique hashes harvested in the range (first harvests: the cohorts'
    /// harvested counts). Always approximate (≈).
    pub discover: u64,
    /// Metadata retrieved in the range, by event time. Null until the
    /// pipeline counts fetches by event time (B11b).
    #[schema(required = true)]
    pub fetch: Option<u64>,
    /// Searchable torrents now; null when search is off.
    #[schema(required = true)]
    pub index: Option<u64>,
    /// Searches answered in the range; null until search counters exist
    /// (#48).
    #[schema(required = true)]
    pub serve: Option<u64>,
    /// Torrents newly stored in the range. Event time.
    pub understand: u64,
}

/// The KPI row (§10.9.2 level 1, `docs/metrics.md` §6.2).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct Kpi {
    pub health: Health,
    /// Searchable torrents that were imported (bitmagnet import), part of
    /// `searchable`; null until the count taken at start is in.
    #[schema(required = true)]
    pub imported: Option<u64>,
    /// Torrents that first became searchable in the range, imports
    /// included. Event time; 0 is real.
    pub new_searchable: u64,
    /// Documents in the search index now; null when search is off.
    #[schema(required = true)]
    pub searchable: Option<u64>,
    /// Torrents made searchable in this range, as the change since the
    /// previous period's end; null without `compare=1` (and for `all`).
    /// Removals aren't counted yet (B11b).
    #[schema(required = true)]
    pub searchable_delta: Option<i64>,
    /// First harvest → searchable, p50 and p95 seconds over the final
    /// cohorts in the range. Always approximate (log buckets ≤ 10%).
    /// Null when no cohort in the range is final.
    #[schema(required = true)]
    pub time_to_searchable_p50_s: Option<u64>,
    #[schema(required = true)]
    pub time_to_searchable_p95_s: Option<u64>,
    /// Searchable ÷ harvested over the final cohorts in the range, 0–1.
    /// Imports excluded. Null when no cohort in the range is final.
    #[serde(rename = "yield")]
    #[schema(required = true)]
    pub yield_: Option<f64>,
}

/// Health (§10.9.2): running, paused or degraded, with the reason.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct Health {
    /// `paused`, `writer_stalled` or `index_lag`; null while running.
    #[schema(required = true)]
    pub reason: Option<String>,
    pub state: HealthState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum HealthState {
    Running,
    Paused,
    Degraded,
}

/// One trend bucket. A count is null where nothing was measured (a gap:
/// paused, or not running), and 0 where it was measured and was 0.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct TrendPoint {
    /// The bucket's start, unix seconds.
    pub at: i64,
    /// Unique hashes first harvested in the bucket. Always ≈.
    #[schema(required = true)]
    pub harvested: Option<u64>,
    /// Torrents newly searchable in the bucket.
    #[schema(required = true)]
    pub searchable: Option<u64>,
}

/// A trend annotation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct Annotation {
    /// Unix seconds.
    pub at: i64,
    /// `restart`, `stopped`, `paused`, `resumed` or `intensity`.
    pub kind: String,
    /// The event's detail: the version for `restart`, the new level for
    /// `intensity`; empty when there's none.
    pub text: String,
}

/// One discovery cohort (or a UTC day of them) in the funnel (§10.9.3).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct FunnelCohort {
    /// The cohort's first hour, unix seconds.
    pub cohort: i64,
    pub conversion: Conversion,
    /// Final: nothing in progress and everything stored searchable, or
    /// past the 72 h retry horizon. A day is final when all its hours
    /// are. Not final is "incomplete" (hatched).
    #[serde(rename = "final")]
    pub is_final: bool,
    /// Harvested − stored − lost, at least 0.
    pub in_progress: u64,
    /// Losses by reason (`no_peers`, `timed_out`, `refused`,
    /// `encryption_failed`, `oversize`, `invalid`, `blocked`, `evicted`,
    /// `gave_up`, `unreachable`), every reason listed.
    pub losses: BTreeMap<String, u64>,
    pub stages: FunnelStages,
}

/// Hashes of the cohort that reached each stage.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct FunnelStages {
    pub fetched: u64,
    pub harvested: u64,
    pub peer_found: u64,
    pub searchable: u64,
    pub stored: u64,
}

/// Each stage ÷ the one before, 0–1; null when the one before is 0.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct Conversion {
    #[schema(required = true)]
    pub fetched: Option<f64>,
    #[schema(required = true)]
    pub peer_found: Option<f64>,
    #[schema(required = true)]
    pub searchable: Option<f64>,
    #[schema(required = true)]
    pub stored: Option<f64>,
}

/// The funnel over every cohort in the range.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct FunnelTotal {
    /// Cohort hours in the range with any count.
    pub cohorts: u64,
    pub conversion: Conversion,
    /// Of those, final.
    pub final_cohorts: u64,
    pub in_progress: u64,
    /// Each reason's share of all losses, 0–1; null when nothing was
    /// lost.
    pub loss_share: BTreeMap<String, Option<f64>>,
    pub losses: BTreeMap<String, u64>,
    pub stages: FunnelStages,
}

/// Segments (§10.9.2 level 4). Rows are sorted by value, descending, then
/// key. Adult is left out entirely while "Show adult content" is off.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct Segments {
    /// Newly searchable by content type (`movie`, `tv`, …, `adult`, and
    /// `unclassified` for none yet). Event time.
    pub by_content_type: Vec<SegmentCount>,
    /// The same by the content type's group: `video` (movie, tv, video),
    /// `audio` (music, audiobook), `documents` (ebook, comic),
    /// `software` (software, game), `images`, `other`, `adult`,
    /// `unclassified`. Event time.
    pub by_group: Vec<SegmentCount>,
    /// Yield by the source that found each hash first, over final
    /// cohorts. Discovery cohort.
    pub by_source: Vec<SegmentYield>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct SegmentCount {
    pub key: String,
    pub searchable: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct SegmentYield {
    pub harvested: u64,
    /// The source plugin's name.
    pub key: String,
    pub searchable: u64,
    /// Searchable ÷ harvested, 0–1; null when it harvested nothing.
    #[serde(rename = "yield")]
    #[schema(required = true)]
    pub yield_: Option<f64>,
}

/// Serve (§10.9.2 level 6). Every field is null until its counter exists:
/// searches and latency with #48, alerts with B9, sends with the
/// integrations' counters.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct Serve {
    #[schema(required = true)]
    pub alerts_fired: Option<u64>,
    /// Always approximate (log buckets).
    #[schema(required = true)]
    pub latency_p95_ms: Option<f64>,
    #[schema(required = true)]
    pub searches: Option<u64>,
    #[schema(required = true)]
    pub sends: Option<u64>,
    /// Searches that found nothing ÷ searches, 0–1; null with no searches.
    #[schema(required = true)]
    pub zero_result_rate: Option<f64>,
}

/// The stale banner (§10.9.6).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct Stale {
    /// `paused` (nothing to flush while paused) or `writer_stalled`.
    pub reason: String,
    /// The last flush, or the start when there was none since.
    pub since: i64,
}

/// An exception (§10.9.2 level 5): text + icon + a fix action.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct InsightsException {
    pub fix: Fix,
    /// `index_lag` (the index more than 30 s behind) or `cohort_stalled`
    /// (hashes in progress, the crawler running, and nothing stored for
    /// 2 full hours).
    pub kind: String,
    pub severity: Severity,
    /// Unix seconds.
    pub since: i64,
    /// `s` or `torrents`.
    #[schema(required = true)]
    pub unit: Option<String>,
    #[schema(required = true)]
    pub value: Option<f64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Warning,
    Error,
}

/// Where an exception's fix is: a settings section or a stage page.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct Fix {
    /// `settings` or `stage`.
    pub kind: String,
    /// The section or stage (`index`, `fetch`, …).
    pub target: String,
}

/// The previous period, for Compare: the fields that depend on the range.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct PreviousPeriod {
    pub kpi: PreviousKpi,
    pub range: InsightsRange,
    pub serve: Serve,
    pub stages: PreviousStages,
    pub trend: Vec<TrendPoint>,
}

/// The stage strip over the previous period.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct PreviousStages {
    pub discover: u64,
    #[schema(required = true)]
    pub fetch: Option<u64>,
    #[schema(required = true)]
    pub serve: Option<u64>,
    pub understand: u64,
}

/// The KPIs over the previous period.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct PreviousKpi {
    pub new_searchable: u64,
    /// Searchable at the period's end: now's, less what this range made
    /// searchable. Null when search is off.
    #[schema(required = true)]
    pub searchable: Option<u64>,
    #[schema(required = true)]
    pub time_to_searchable_p50_s: Option<u64>,
    #[schema(required = true)]
    pub time_to_searchable_p95_s: Option<u64>,
    #[serde(rename = "yield")]
    #[schema(required = true)]
    pub yield_: Option<f64>,
}

/// `GET /insights/events`: annotations and paused spans.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct EventsResponse {
    /// Oldest first; at most 1,000 (`truncated`).
    pub annotations: Vec<Annotation>,
    /// The paused spans overlapping the range, oldest first, so the
    /// client draws gaps.
    pub paused: Vec<PausedSpan>,
    /// More than 1,000 annotations: the oldest 1,000 are listed.
    pub truncated: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct PausedSpan {
    /// Unix seconds: when it paused (before the range if it was already
    /// paused then).
    pub from: i64,
    /// When it resumed; null while still paused.
    #[schema(required = true)]
    pub to: Option<i64>,
}
