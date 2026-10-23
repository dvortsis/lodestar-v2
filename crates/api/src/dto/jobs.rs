//! `/jobs` (§9, §18.8): the background jobs of this server (exports
//! now, imports with the importer framework) with their progress, for
//! the Jobs view.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use super::error::ErrorBody;
use super::export::ExportFormat;

/// What a job does. `import` is reserved for the importer framework
/// (§18.8): nothing starts one yet, but the Jobs view can render both.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum BackgroundJobKind {
    Export,
    Import,
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
    /// Unix seconds; null while it runs.
    #[schema(required = true)]
    pub finished_at: Option<i64>,
    pub id: u64,
    pub kind: BackgroundJobKind,
    /// Items per second while running (paused time left out); null
    /// before the first items.
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

/// `GET /jobs`: every job kept, newest first.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct BackgroundJobs {
    pub jobs: Vec<BackgroundJob>,
}
