//! `GET /insights/workers/summary`: the fleet for the top bar's status
//! pill ("Crawling · 3 workers · 4.2k/h", architecture §19.7; WK-15).
//!
//! - **Single-machine parity:** with no worker joined (or no cluster at
//!   all) `fleet` is `null`, and the pill shows what it shows today.
//! - **How workers are counted:** `workers` is the remote workers doing
//!   work now: online, plus draining ones finishing their leases. Paused
//!   and offline workers aren't in it; each state has its own count, so
//!   the pill can say "1 paused" beside it. The built-in worker is the
//!   coordinator itself and is never counted.
//! - **The rate** is the fleet's commits (every worker's, the built-in
//!   one's included) in the last full UTC hour, from the rollups
//!   (`metric::WORKER_LEASES`, key `COMMITTED`), as the overview's
//!   `ingest_per_hour` is. `null` when that hour wasn't measured.
//! - **Counts only:** no name, address, key or torrent appears here.

use std::sync::{Arc, PoisonError};

use axum::extract::{Extension, State};
use axum::routing::get;
use axum::{Json, Router};
use lodestar_store::{RollupBy, Store};
use serde::{Deserialize, Serialize};
use utoipa::{OpenApi, ToSchema};

use crate::dto::workers::{WorkerList, WorkerStatus};
use crate::error::{ApiResult, internal};
use crate::insights::codes;
use crate::workers::WorkersApi;
use crate::{ApiState, auth};

/// This module's half of `/openapi.json`.
#[derive(OpenApi)]
#[openapi(
    paths(fleet_summary),
    components(schemas(FleetSummary, Fleet)),
    tags((name = "insights", description = "Insights (§10.9)."))
)]
pub(crate) struct ApiDoc;

/// `GET /insights/workers/summary`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[schema(example = json!({
    "fleet": {
        "committed_per_hour": 4200,
        "draining": 1,
        "offline": 0,
        "online": 2,
        "paused": 1,
        "workers": 3
    }
}))]
pub struct FleetSummary {
    /// `null` when no remote worker has joined: the pill shows nothing
    /// new.
    #[schema(required = true)]
    pub fleet: Option<Fleet>,
}

/// The fleet's counts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct Fleet {
    /// Torrents the whole fleet (the coordinator's own crawler included)
    /// fetched and committed in the last full UTC hour; `null` when that
    /// hour wasn't measured.
    #[schema(required = true)]
    pub committed_per_hour: Option<u64>,
    /// Finishing their leases, then stopping.
    pub draining: u32,
    /// Joined, not connected now.
    pub offline: u32,
    /// Connected and taking work.
    pub online: u32,
    /// Connected, taking no new work until resumed.
    pub paused: u32,
    /// Remote workers doing work now: `online` plus `draining` (the pill's
    /// "3 workers").
    pub workers: u32,
}

pub fn routes() -> Router<ApiState> {
    auth::protected(Router::new().route("/insights/workers/summary", get(fleet_summary)))
}

/// The fleet from `/workers`' list and the last full hour's commits.
pub(crate) fn summarize(list: &WorkerList, committed: Option<u64>) -> FleetSummary {
    if list.workers.is_empty() {
        return FleetSummary { fleet: None };
    }
    let count = |s: WorkerStatus| {
        let n = list.workers.iter().filter(|w| w.status == s).count();
        u32::try_from(n).unwrap_or(u32::MAX)
    };
    let (online, draining) = (count(WorkerStatus::Online), count(WorkerStatus::Draining));
    FleetSummary {
        fleet: Some(Fleet {
            committed_per_hour: committed,
            draining,
            offline: count(WorkerStatus::Offline),
            online,
            paused: count(WorkerStatus::Paused),
            workers: online.saturating_add(draining),
        }),
    }
}

/// Commits in hour `h`: the rows' sum, 0 if the crawler ran that hour
/// without one, `null` if nothing measured it. Two primary-key seeks.
pub(crate) fn committed_in_hour(store: &Store, h: i64) -> lodestar_store::Result<Option<u64>> {
    let committed = store
        .rollup_sums(codes::metric::WORKER_LEASES, h, h + 1, RollupBy::Key)?
        .into_iter()
        .find(|c| c.key == codes::WORKER_COMMITTED)
        .map(|c| c.value.max(0) as u64);
    if committed.is_some() {
        return Ok(committed);
    }
    let covered = store
        .rollup_sums(codes::metric::COVERAGE, h, h + 1, RollupBy::Dim)?
        .iter()
        .any(|c| c.dim == codes::metric::COVERAGE_CRAWL && c.value > 0);
    Ok(covered.then_some(0))
}

/// The fleet for the status pill: how many remote workers are working,
/// in each state, and the fleet's commits in the last full UTC hour.
/// `fleet` is `null` when no worker has joined (a single machine).
#[utoipa::path(
    operation_id = "insightsFleetSummary",
    get,
    path = "/insights/workers/summary",
    tag = "insights",
    responses((status = 200, body = FleetSummary))
)]
pub(crate) async fn fleet_summary(
    State(s): State<ApiState>,
    handle: Option<Extension<Arc<dyn WorkersApi>>>,
) -> ApiResult<Json<FleetSummary>> {
    let Some(Extension(api)) = handle else {
        return Ok(Json(FleetSummary { fleet: None }));
    };
    let list = api.list().await?;
    if list.workers.is_empty() {
        return Ok(Json(FleetSummary { fleet: None }));
    }
    let now = s
        .insights
        .as_ref()
        .map_or_else(crate::unix_now, |i| i.now());
    let last_hour = now.div_euclid(3600) - 1;
    let reader = s.reader.clone();
    let committed = tokio::task::spawn_blocking(move || {
        let store = reader.lock().unwrap_or_else(PoisonError::into_inner);
        committed_in_hour(&store, last_hour)
    })
    .await
    .map_err(|e| internal(e.to_string()))?
    .map_err(|e| internal(e.to_string()))?;
    Ok(Json(summarize(&list, committed)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dto::workers::{DhtPin, Worker, WorkerBudget, WorkerIntensity};

    fn worker(id: u32, status: WorkerStatus) -> Worker {
        Worker {
            budget: WorkerBudget {
                cpu_permille: 1000,
                down_kbps: 0,
                ram_mb: 0,
                up_kbps: 0,
            },
            claimed: None,
            clock_offset_ms: None,
            clock_skewed: false,
            cpu_permille: None,
            dht: DhtPin::Auto,
            external_ips: Vec::new(),
            fetched: None,
            id,
            intensity: WorkerIntensity::Balanced,
            last_seen_at: None,
            leases: 0,
            name: None,
            paused: status == WorkerStatus::Paused,
            pool_cap: 1,
            processed: None,
            rss_mb: None,
            spool_bytes: None,
            status,
        }
    }

    #[test]
    fn an_empty_fleet_is_null() {
        let list = WorkerList {
            allowed: true,
            workers: Vec::new(),
        };
        assert_eq!(summarize(&list, Some(5)), FleetSummary { fleet: None });
    }

    /// Online and draining workers work; paused and offline ones are
    /// counted apart and not in `workers`.
    #[test]
    fn workers_are_counted_by_state() {
        let list = WorkerList {
            allowed: true,
            workers: vec![
                worker(1, WorkerStatus::Online),
                worker(2, WorkerStatus::Online),
                worker(3, WorkerStatus::Draining),
                worker(4, WorkerStatus::Paused),
                worker(5, WorkerStatus::Offline),
                worker(6, WorkerStatus::Offline),
            ],
        };
        let f = summarize(&list, None).fleet.unwrap();
        assert_eq!(
            f,
            Fleet {
                committed_per_hour: None,
                draining: 1,
                offline: 2,
                online: 2,
                paused: 1,
                workers: 3,
            }
        );
    }
}
