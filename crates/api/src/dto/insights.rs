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
    /// Trend annotations in the range, oldest first, at most 1,000
    /// (`/insights/events` says when there were more). Event time.
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
    /// Null while nothing identifying is recorded (the Overview then loads
    /// no banner code).
    #[schema(required = true)]
    pub privacy: Option<PrivacySummary>,
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

/// Whether something that identifies torrents or people is being recorded
/// now (§10.10.7 Privacy, §10.10.12), for the Overview's banner. Booleans
/// and a time only: never which kinds, a path, a query or a name. Read at
/// each request, never cached with the rollups.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
#[schema(example = json!({ "identifying_until": 1_700_003_600i64, "query_logging": false }))]
pub struct PrivacySummary {
    /// When the log stops writing data kinds that identify torrents or
    /// people (the opt-ins included): -1 not until turned off, else Unix
    /// seconds. Null while none is on.
    #[schema(required = true)]
    pub identifying_until: Option<i64>,
    /// Searches are being saved for Insights (`insights.query_logging`).
    pub query_logging: bool,
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
    /// Metadata retrieved in the range, by event time (the writer's
    /// flush). Null when the range starts before fetches were counted by
    /// event time (the first whole hour after the upgrade that added it):
    /// unknown, not 0.
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
    /// `searchable`; null until Insights' state is read at start.
    #[schema(required = true)]
    pub imported: Option<u64>,
    /// Crawled torrents that first became searchable in the range:
    /// imports excluded (`new_searchable_imported`). Event time; 0 is
    /// real.
    pub new_searchable: u64,
    /// `new_searchable` less `previous.kpi.new_searchable`; null without
    /// `compare=1` (and for `all`).
    #[schema(required = true)]
    pub new_searchable_delta: Option<i64>,
    /// Imported torrents that first became searchable in the range (an
    /// import's records, indexed). Event time; 0 is real.
    pub new_searchable_imported: u64,
    /// Documents in the search index now; null when search is off.
    #[schema(required = true)]
    pub searchable: Option<u64>,
    /// Of `searchable`, the crawled ones: `searchable` less `imported`;
    /// null when either is null.
    #[schema(required = true)]
    pub searchable_crawled: Option<u64>,
    /// The change since the previous period's end: now's count less
    /// `previous.kpi.searchable`. Null without `compare=1`, for `all`,
    /// with search off, or with no snapshot from then.
    #[schema(required = true)]
    pub searchable_delta: Option<i64>,
    /// First harvest → searchable, p50 and p95 seconds over the final
    /// cohorts in the range. Always approximate (log buckets ≤ 10%).
    /// Null when no cohort in the range is final.
    #[schema(required = true)]
    pub time_to_searchable_p50_s: Option<u64>,
    /// `time_to_searchable_p50_s` less the previous period's, seconds;
    /// null without `compare=1` or when either side is null.
    #[schema(required = true)]
    pub time_to_searchable_p50_delta_s: Option<i64>,
    #[schema(required = true)]
    pub time_to_searchable_p95_s: Option<u64>,
    /// As `time_to_searchable_p50_delta_s`, for p95.
    #[schema(required = true)]
    pub time_to_searchable_p95_delta_s: Option<i64>,
    /// Searchable ÷ harvested over the final cohorts in the range, 0–1.
    /// Imports excluded. Null when no cohort in the range is final.
    #[serde(rename = "yield")]
    #[schema(required = true)]
    pub yield_: Option<f64>,
    /// `yield` less the previous period's, in ratio points (−1 to 1); null
    /// without `compare=1` or when either side is null.
    #[schema(required = true)]
    pub yield_delta: Option<f64>,
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
    /// Crawled torrents newly searchable in the bucket (imports
    /// excluded).
    #[schema(required = true)]
    pub searchable: Option<u64>,
    /// Imported torrents newly searchable in the bucket; null and 0 as
    /// for `searchable`.
    #[schema(required = true)]
    pub searchable_imported: Option<u64>,
}

/// A trend annotation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct Annotation {
    /// Unix seconds.
    pub at: i64,
    /// `restart`, `stopped`, `paused`, `resumed`, `intensity`,
    /// `import_started`, `import_finished` or `import_removed`.
    pub kind: String,
    /// The event's detail: the version for `restart`, the new level for
    /// `intensity`, `key=value` words for an import (as `import`); empty
    /// when there's none.
    pub text: String,
    /// An import annotation's job and counts; null for other kinds.
    #[schema(required = true)]
    pub import: Option<ImportNote>,
}

/// What an import annotation says, so a step in the trend is explained.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ImportNote {
    /// The import job's id.
    pub job: i64,
    /// Its adapter: `magnetico` or `bitmagnet`.
    pub source: String,
    /// `import_finished`: records the job stored new (novel). Null
    /// otherwise.
    #[schema(required = true)]
    pub new: Option<u64>,
    /// `import_finished`: records the store already had, merged as
    /// provenance only (already known). Null otherwise.
    #[schema(required = true)]
    pub merged: Option<u64>,
    /// `import_removed`: torrents the removal deleted. Null otherwise.
    #[schema(required = true)]
    pub removed: Option<u64>,
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
    /// `index_lag` (the index more than 30 s behind), `cohort_stalled`
    /// (hashes in progress, the crawler running, and nothing stored for
    /// 2 full hours), `yield_below_baseline`, or `filter_rows_unreadable`
    /// (stored filter rows that no longer read; filtering fails closed
    /// for them until they're fixed in Filters & Safety).
    pub kind: String,
    /// The rows an exception is about, as the Filters & Safety screens
    /// name them (`U-3`, `C-2`, …); empty for the other kinds.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rows: Vec<String>,
    pub severity: Severity,
    /// Unix seconds.
    pub since: i64,
    /// `s`, `torrents`, `ratio` or `rows`.
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
    /// The previous period's segments, in the same shape and with the
    /// same Adult rule.
    pub segments: Segments,
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
    pub new_searchable_imported: u64,
    /// Searchable at the period's end: the last hourly snapshot in the
    /// day before it ends. Null when there's none (search off, or not
    /// running then).
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

/// A segment row whose count moved most between the previous period and
/// this one (the digest's "top segment movers", §10.9.7). Not served
/// directly: B11c reads [`crate::insights::segment_movers`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SegmentMover {
    /// `by_group` or `by_content_type`.
    pub list: &'static str,
    pub key: String,
    pub current: u64,
    pub previous: u64,
    /// `current − previous`.
    pub delta: i64,
}

/// `GET /insights/{stage}`: the overview's range parameters, plus
/// `reason`.
#[derive(Debug, Clone, Default, Deserialize, IntoParams)]
#[serde(default)]
#[into_params(parameter_in = Query)]
pub struct StageQuery {
    /// As the overview's.
    pub range: Option<String>,
    pub from: Option<i64>,
    pub to: Option<i64>,
    /// `1` adds `previous`.
    pub compare: Option<u8>,
    /// A loss reason's name (`docs/metrics.md` §6.4: `no_peers`, …,
    /// `unreachable`): the Fetch page's losses and funnel show only that
    /// reason (the drill-through from a funnel loss, §10.9.4). Accepted
    /// and checked on every stage; only Fetch reads it.
    pub reason: Option<String>,
}

/// One bin of a distribution (parser confidence, junk score).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct InsightsHistogramBin {
    pub count: u64,
    /// The bin's edges.
    pub hi: f64,
    pub lo: f64,
}

/// A logged query and how often it ran (Serve, when query logging is on).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct QueryRow {
    pub count: u64,
    pub query: String,
    /// Of those runs, how many found nothing.
    pub zero: u64,
}

/// `GET /insights/discover` (§10.9.5, `docs/metrics.md` §6.8, §7.7).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct InsightsDiscoverResponse {
    /// Yield by IPv4 / IPv6. Null until leases carry the address family.
    #[schema(required = true)]
    pub by_ip_family: Option<Vec<SegmentYield>>,
    /// Keys reported, rediscovered and dropped, by the source that
    /// reported them. Event time.
    pub by_source: Vec<DiscoverSource>,
    /// Keys reported, by harvest strategy: the DHT's (BEP 51 sample,
    /// get_peers, announce, lookup), refetch, and each other source's own
    /// codes. Event time: volume, not yield.
    pub by_strategy: Vec<StrategyCount>,
    pub kpi: DiscoverKpi,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub previous: Option<DiscoverPrevious>,
    pub range: InsightsRange,
    #[schema(required = true)]
    pub stale: Option<Stale>,
    pub trend: Vec<DiscoverPoint>,
    pub updated_at: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct DiscoverKpi {
    /// BEP 51 sample yield. Null until the DHT counts samples.
    #[schema(required = true)]
    pub bep51_sample_yield: Option<f64>,
    /// Nodes in the routing tables now; null with the DHT off. Snapshot.
    #[schema(required = true)]
    pub dht_nodes: Option<u64>,
    /// Keys dropped because the pipeline was full. Event time.
    pub dropped: u64,
    /// First harvests (the cohorts' harvested counts). Always ≈.
    pub harvested: u64,
    /// Virtual-ID keyspace coverage. Null until the crawler reports it.
    #[schema(required = true)]
    pub keyspace_coverage: Option<f64>,
    /// Keys stored already, seen again. Event time.
    pub rediscovered: u64,
    /// DHT responses per query sent since the start (the NAT-saturation
    /// signal); null with the DHT off. Snapshot.
    #[schema(required = true)]
    pub reply_ratio: Option<f64>,
    /// Keys the sources reported. Event time.
    pub reported: u64,
    /// The seen filter's load factor. Null until the filter reports it.
    #[schema(required = true)]
    pub seen_filter_load: Option<f64>,
}

/// One bucket; a count is null where nothing was measured.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct DiscoverPoint {
    pub at: i64,
    #[schema(required = true)]
    pub dropped: Option<u64>,
    /// First harvests in the bucket's cohorts.
    #[schema(required = true)]
    pub harvested: Option<u64>,
    #[schema(required = true)]
    pub rediscovered: Option<u64>,
    #[schema(required = true)]
    pub reported: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct DiscoverSource {
    pub dropped: u64,
    /// The source's plugin name.
    pub key: String,
    pub rediscovered: u64,
    pub reported: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct StrategyCount {
    /// The DHT's strategies by name (`sample`, `get_peers`, `announce`,
    /// `lookup`, or `strategy_<n>`), `refetch` from any source, and other
    /// sources' own codes as `<plugin>:<n>`.
    pub key: String,
    pub reported: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct DiscoverPrevious {
    /// Snapshot fields are null here.
    pub kpi: DiscoverKpi,
    pub range: InsightsRange,
    pub trend: Vec<DiscoverPoint>,
}

/// `GET /insights/fetch`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct FetchResponse {
    /// The discovery-cohort funnel, as the overview's (with `reason`, only
    /// that loss).
    pub funnel: Vec<FunnelCohort>,
    pub funnel_total: FunnelTotal,
    pub kpi: FetchKpi,
    /// Losses by reason, by event time, most first; null when the range
    /// starts before fetches were counted by event time.
    #[schema(required = true)]
    pub losses: Option<Vec<LossCount>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub previous: Option<FetchPrevious>,
    pub range: InsightsRange,
    /// The `reason` filter, as asked; null for all.
    #[schema(required = true)]
    pub reason: Option<String>,
    #[schema(required = true)]
    pub stale: Option<Stale>,
    pub trend: Vec<FetchPoint>,
    pub updated_at: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct FetchKpi {
    /// Ephemeral-port pressure (Windows). Null until counted.
    #[schema(required = true)]
    pub ephemeral_port_pressure: Option<f64>,
    /// Fetch latency percentiles. Null until a fetch latency histogram
    /// exists.
    #[schema(required = true)]
    pub fetch_latency_p50_ms: Option<u64>,
    #[schema(required = true)]
    pub fetch_latency_p95_ms: Option<u64>,
    /// Info dicts fetched in the range, by event time; null when the range
    /// starts before they were counted so.
    #[schema(required = true)]
    pub fetched: Option<u64>,
    /// Fetches running now. Snapshot.
    #[schema(required = true)]
    pub in_flight: Option<u64>,
    /// Per-IP limiter rejections. Null until counted.
    #[schema(required = true)]
    pub limiter_rejections: Option<u64>,
    /// Keys lost in the range (with `reason`, for that reason only), by
    /// event time; null as `fetched`.
    #[schema(required = true)]
    pub lost: Option<u64>,
    /// MSE (encrypted) share of successful fetches. Null until counted.
    #[schema(required = true)]
    pub mse_share: Option<f64>,
    /// Age of the oldest queued key. Null until the pool reports it.
    #[schema(required = true)]
    pub queue_age_s: Option<u64>,
    /// Keys queued or in flight now. Snapshot.
    #[schema(required = true)]
    pub queue_depth: Option<u64>,
    /// uTP share of successful fetches. Null until counted.
    #[schema(required = true)]
    pub utp_share: Option<f64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct FetchPoint {
    pub at: i64,
    #[schema(required = true)]
    pub fetched: Option<u64>,
    #[schema(required = true)]
    pub lost: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct LossCount {
    pub count: u64,
    /// The reason's name (§6.4).
    pub key: String,
    /// Of all losses in the range.
    #[schema(required = true)]
    pub share: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct FetchPrevious {
    pub kpi: FetchKpi,
    pub range: InsightsRange,
    pub trend: Vec<FetchPoint>,
}

/// `GET /insights/understand`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct UnderstandResponse {
    /// Newly searchable by content type and group, as the overview's
    /// segments (Adult left out while hidden). Event time.
    pub by_content_type: Vec<SegmentCount>,
    pub by_group: Vec<SegmentCount>,
    pub kpi: UnderstandKpi,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub previous: Option<UnderstandPrevious>,
    pub range: InsightsRange,
    #[schema(required = true)]
    pub stale: Option<Stale>,
    pub trend: Vec<UnderstandPoint>,
    pub updated_at: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct UnderstandKpi {
    /// Dropped by a blocklist or filter rule before storage (counts only),
    /// by event time; null as `fetch.kpi.fetched`.
    #[schema(required = true)]
    pub blocked: Option<u64>,
    /// Junk score distribution. Null until scores are rolled up.
    #[schema(required = true)]
    pub junk_distribution: Option<Vec<InsightsHistogramBin>>,
    /// Crawled torrents that became searchable in the range, imports
    /// excluded. Event time.
    pub newly_searchable: u64,
    /// Parser confidence distribution. Null until rolled up.
    #[schema(required = true)]
    pub parser_confidence: Option<Vec<InsightsHistogramBin>>,
    /// Recent "Fix parse" corrections. Null until counted.
    #[schema(required = true)]
    pub recent_corrections: Option<u64>,
    /// Ambiguous names resolved ÷ ambiguous. Null until a resolution
    /// counter exists.
    #[schema(required = true)]
    pub resolution_rate: Option<f64>,
    /// Torrents newly stored in the range. Event time.
    pub stored: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct UnderstandPoint {
    pub at: i64,
    #[schema(required = true)]
    pub blocked: Option<u64>,
    #[schema(required = true)]
    pub stored: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct UnderstandPrevious {
    pub kpi: UnderstandKpi,
    pub range: InsightsRange,
    pub trend: Vec<UnderstandPoint>,
}

/// `GET /insights/index`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct IndexResponse {
    pub kpi: IndexKpi,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub previous: Option<IndexPrevious>,
    pub range: InsightsRange,
    /// Searchable torrents deleted in the range (a lifecycle event,
    /// §10.9.3), by content type, Adult left out while hidden. Event time.
    pub removed_by_type: Vec<RemovedCount>,
    #[schema(required = true)]
    pub stale: Option<Stale>,
    /// Per-tracker health. Null until it's rolled up.
    #[schema(required = true)]
    pub trackers: Option<Vec<TrackerHealth>>,
    pub trend: Vec<IndexPoint>,
    pub updated_at: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct IndexKpi {
    /// Bytes in the data directory. Snapshot.
    #[schema(required = true)]
    pub data_bytes: Option<u64>,
    /// Free bytes on its volume: always null in this build.
    #[schema(required = true)]
    pub disk_free_bytes: Option<u64>,
    /// File-index coverage (smart mode). Null until measured.
    #[schema(required = true)]
    pub file_index_coverage: Option<f64>,
    /// NRT commit lag (as `/stats`). Snapshot.
    #[schema(required = true)]
    pub index_lag_s: Option<u64>,
    /// Segment merges. Null until the index reports them.
    #[schema(required = true)]
    pub merges: Option<u64>,
    /// Crawled torrents that became searchable in the range, imports
    /// excluded. Event time.
    pub new_searchable: u64,
    /// Imported torrents that became searchable in the range. Event time.
    pub new_searchable_imported: u64,
    /// Days until the disk is full. Null while free space is unknown.
    #[schema(required = true)]
    pub projection_days: Option<u64>,
    /// A background rebuild's phase (`building`, `finishing`, `swapping`,
    /// `failed`); null when none runs. Snapshot.
    #[schema(required = true)]
    pub rebuild: Option<String>,
    /// Searchable torrents deleted in the range. Event time.
    pub removed: u64,
    /// Share of torrents with a swarm row. Null until rolled up.
    #[schema(required = true)]
    pub scrape_coverage: Option<f64>,
    /// Median age of scrape data. Null until rolled up.
    #[schema(required = true)]
    pub scrape_freshness_s: Option<u64>,
    /// Documents in the index now; null with search off. Snapshot.
    #[schema(required = true)]
    pub searchable: Option<u64>,
    /// Index segments now. Null until the index reports them.
    #[schema(required = true)]
    pub segments: Option<u64>,
    /// Database, index and file-index sizes against the §3 budget. Null
    /// until each part is measured.
    #[schema(required = true)]
    pub sizes: Option<IndexSizes>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct IndexSizes {
    pub database_bytes: u64,
    pub file_index_bytes: u64,
    pub index_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct TrackerHealth {
    /// `scheme://host:port` only: a URL path or query can carry a passkey.
    pub key: String,
    pub reporting: u64,
    /// `ok`, `backoff`, `down` or `banned`.
    pub state: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct IndexPoint {
    pub at: i64,
    /// Crawled torrents newly searchable (imports excluded).
    #[schema(required = true)]
    pub new_searchable: Option<u64>,
    /// Imported torrents newly searchable.
    #[schema(required = true)]
    pub new_searchable_imported: Option<u64>,
    #[schema(required = true)]
    pub removed: Option<u64>,
    /// The bucket's last searchable snapshot (a gauge: the last hour's);
    /// null with none.
    #[schema(required = true)]
    pub searchable: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct RemovedCount {
    /// Content type, or `unclassified`.
    pub key: String,
    pub removed: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct IndexPrevious {
    pub kpi: IndexKpi,
    pub range: InsightsRange,
    pub trend: Vec<IndexPoint>,
}

/// `GET /insights/serve`. Every count is null until the search, alert,
/// notification and send counters exist.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct ServeResponse {
    pub kpi: ServeKpi,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub previous: Option<ServePrevious>,
    pub range: InsightsRange,
    #[schema(required = true)]
    pub stale: Option<Stale>,
    /// Most frequent queries, only with query logging on (it's off by
    /// default, and nothing logs queries yet): null.
    #[schema(required = true)]
    pub top_queries: Option<Vec<QueryRow>>,
    #[schema(required = true)]
    pub top_zero_result_queries: Option<Vec<QueryRow>>,
    pub trend: Vec<ServePoint>,
    pub updated_at: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct ServeKpi {
    #[schema(required = true)]
    pub alerts_fired: Option<u64>,
    #[schema(required = true)]
    pub cancel_rate: Option<f64>,
    #[schema(required = true)]
    pub facet_sampling_rate: Option<f64>,
    #[schema(required = true)]
    pub latency_p50_ms: Option<f64>,
    #[schema(required = true)]
    pub latency_p95_ms: Option<f64>,
    #[schema(required = true)]
    pub notification_failures: Option<u64>,
    #[schema(required = true)]
    pub notifications_delivered: Option<u64>,
    #[schema(required = true)]
    pub searches: Option<u64>,
    #[schema(required = true)]
    pub sends: Option<u64>,
    #[schema(required = true)]
    pub zero_result_rate: Option<f64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ServePoint {
    pub at: i64,
    #[schema(required = true)]
    pub searches: Option<u64>,
    #[schema(required = true)]
    pub zero_results: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct ServePrevious {
    pub kpi: ServeKpi,
    pub range: InsightsRange,
    pub trend: Vec<ServePoint>,
}

/// `GET /insights/export.csv`.
#[derive(Debug, Clone, Default, Deserialize, IntoParams)]
#[serde(default)]
#[into_params(parameter_in = Query)]
pub struct ExportQuery {
    /// The panel: `trend`, `funnel`, `losses`, `segments.by_group`,
    /// `segments.by_content_type`, `segments.by_source`, `exceptions`,
    /// `annotations` (the overview's), or a stage's `<stage>.trend` and
    /// tables (`discover.by_source`, `discover.by_strategy`,
    /// `fetch.losses`, `fetch.funnel`, `understand.by_content_type`,
    /// `understand.by_group`, `index.removed_by_type`, `serve.trend`, …).
    pub panel: Option<String>,
    pub range: Option<String>,
    pub from: Option<i64>,
    pub to: Option<i64>,
    /// `1` adds the previous period's rows (`period` = `previous`) where
    /// the panel has them.
    pub compare: Option<u8>,
    /// As the stage pages' `reason`: checked for every panel, and Fetch's
    /// panels keep only that loss, as the page does.
    pub reason: Option<String>,
}
