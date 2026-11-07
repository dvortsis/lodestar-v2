//! `/jobs` (§9, §18.8): the background jobs of this server (exports
//! now, imports with the importer framework) with their progress, for
//! the Jobs view.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use super::error::ErrorBody;
use super::export::ExportFormat;

/// What a job does. `import` is reserved for the importer framework
/// (§18.8): nothing starts one yet, but the Jobs view can render it.
/// `purge` removes a source's data (`POST /sources/{id}/purge`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum BackgroundJobKind {
    Export,
    Import,
    Purge,
}

/// Where a job is. `running` and `paused` hold one of the running
/// slots; the rest are finished and kept for 10 minutes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum BackgroundJobState {
    Running,
    /// Stopped reading by `POST /jobs/{id}/pause`; it holds no lock or
    /// transaction, and fails if it isn't resumed within 10 minutes.
    Paused,
    Done,
    /// Cancelled by `POST /jobs/{id}/cancel`, or (an export) the client
    /// stopped the download.
    Cancelled,
    /// Ended by an error: see `error`.
    Failed,
}

/// One job (`GET /jobs/{id}`, and each entry of `GET /jobs`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct BackgroundJob {
    /// Items done: rows written, for an export.
    pub done: u64,
    /// Seconds left at the current rate; null while it isn't running,
    /// before the first items, or when `total` is unknown.
    #[schema(required = true)]
    pub eta_s: Option<u64>,
    /// Once `failed`: what went wrong.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub error: Option<ErrorBody>,
    /// An export's own details.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub export: Option<ExportJob>,
    /// A purge's own details.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub purge: Option<PurgeJob>,
    /// Unix seconds; null while it runs.
    #[schema(required = true)]
    pub finished_at: Option<i64>,
    pub id: u64,
    pub kind: BackgroundJobKind,
    /// Items read per second (`done` and `skipped`), paused time left
    /// out; null before the first items.
    #[schema(required = true)]
    pub rate: Option<f64>,
    /// Items left out: for an export, torrents it may not include
    /// (withheld, private-source, deleted or unknown), counted without
    /// saying which or why.
    pub skipped: u64,
    /// Unix seconds.
    pub started_at: i64,
    pub state: BackgroundJobState,
    /// Items expected; null when unknown.
    #[schema(required = true)]
    pub total: Option<u64>,
    /// `total` is an estimate (a query with more than 100,000 matches).
    pub total_approx: bool,
}

/// What an export job reports besides its progress.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ExportJob {
    /// Rows one export writes at most.
    pub cap: u64,
    pub format: ExportFormat,
    /// The export stopped at `cap` with more rows left: the file holds
    /// the first `cap`.
    pub truncated: bool,
}

/// Where a purge is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum PurgeStage {
    /// Turning the source off and stopping its plugin.
    Stopping,
    /// Deleting what it found, in steps.
    Removing,
    /// Dropping its credentials, consent and settings, then scrubbing
    /// the store's snapshots.
    Finishing,
    /// Finished: the source is purged.
    Purged,
}

/// What a purge job reports besides its progress. Every count is of
/// changes already saved: a purge that stopped (cancelled, failed, or cut
/// short by a restart) did exactly this much, and the source stays off
/// until a purge run again finishes it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct PurgeJob {
    /// Torrents others found too, kept without this source.
    pub kept: u64,
    /// Keys waiting to be fetched that were dropped.
    pub keys_dropped: u64,
    pub plugin: String,
    /// Warnings the snapshot pass raised: every problem it met (a
    /// snapshot it couldn't read or write now, one of a schema it doesn't
    /// know, a file it couldn't delete), so this counts problems, not only
    /// snapshots left unscrubbed. The hourly pass tries again, and a
    /// rollback scrubs a snapshot before restoring it.
    pub snapshots_left: u64,
    /// Snapshots the pass changed: its credentials and consent removed,
    /// with anything else the pass was due to remove.
    pub snapshots_scrubbed: u64,
    pub source_id: u8,
    pub stage: PurgeStage,
    /// Torrents only this source found, deleted.
    pub torrents_deleted: u64,
    /// Items without metadata only this source found, deleted.
    pub unverified_deleted: u64,
}

/// `POST /sources/{id}/purge`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct PurgeRequest {
    /// The admin password, checked fresh: a purge can't be undone.
    #[serde(default)]
    pub password: Option<String>,
}

/// `GET /sources/{id}/purge/preview`: what purging the source would
/// remove if it ran now, counted with the purge's own rules. Nothing is
/// changed. The dialog shows it before you commit (§10.10.7).
///
/// While the source is off (`settled`), a purge started next removes
/// exactly this, as long as nothing else changes the store meanwhile. An
/// enabled source keeps finding until the purge turns it off, so the purge
/// may remove more. Keys still in memory waiting to be fetched aren't
/// counted: the purge reports them as `keys_dropped`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[schema(example = json!({
    "claims_deleted": 96_402,
    "kept": 88_120,
    "plugin": "nyaa",
    "queue_deleted": 12,
    "settled": true,
    "source_id": 3,
    "state_deleted": 4,
    "torrents_deleted": 1_204_331,
    "unverified_deleted": 5_210,
    "unverified_kept": 301
}))]
pub struct PurgePreview {
    /// Claims of stored torrents it made, deleted.
    pub claims_deleted: u64,
    /// Torrents others found too, kept without this source (`kept` in the
    /// purge job).
    pub kept: u64,
    pub plugin: String,
    /// Keys it queued to be fetched, saved across restarts, dropped.
    pub queue_deleted: u64,
    /// The source is off, so nothing it finds can add to the counts
    /// before the purge runs.
    pub settled: bool,
    pub source_id: u8,
    /// Its plugin's saved state entries, deleted.
    pub state_deleted: u64,
    /// Torrents only this source found, deleted.
    pub torrents_deleted: u64,
    /// Items without metadata only this source found, deleted.
    pub unverified_deleted: u64,
    /// Items without metadata others found too, kept without this source.
    pub unverified_kept: u64,
}

/// `GET /jobs`: every job kept, newest first.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct BackgroundJobs {
    pub jobs: Vec<BackgroundJob>,
}
