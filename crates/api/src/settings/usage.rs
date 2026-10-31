//! Usage data (§10.9.5 Serve, §10.10.8 Insights & digest): the query log,
//! kept only while query logging is on, and Clear usage data.
//!
//! Clearing is a destructive action (§10.10.7): the UI's Danger zone runs
//! it on its own, after a dialog that shows the count from `GET /usage`,
//! and the request must say `confirm: true`. §10.10.7 puts it in the
//! destructive class, which asks for a dialog, not the password.

use axum::Json;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};

use crate::ApiState;
use crate::dto::error::ErrorEnvelope;
use crate::dto::settings::{UsageClear, UsageCleared, UsageStats};
use crate::error::{ApiError, ApiResult, internal};
use crate::extract::{Json as JsonBody, same_origin_json};

/// How many searches are logged.
#[utoipa::path(
    operation_id = "usage",
    get,
    path = "/usage",
    tag = "settings",
    responses((status = 200, body = UsageStats, description = "What Clear usage data would delete."))
)]
pub(crate) async fn usage(State(s): State<ApiState>) -> ApiResult<Json<UsageStats>> {
    let reader = s.reader.clone();
    let queries =
        tokio::task::spawn_blocking(move || reader.lock().expect("reader lock").query_log_len())
            .await
            .map_err(internal)?
            .map_err(internal)?;
    Ok(Json(UsageStats { queries }))
}

/// Deletes every logged search. Settings and everything else stay.
#[utoipa::path(
    operation_id = "clearUsage",
    post,
    path = "/usage/clear",
    tag = "settings",
    request_body = UsageClear,
    responses(
        (status = 200, body = UsageCleared, description = "Deleted."),
        (status = 403, body = ErrorEnvelope, description = "`cross_origin`."),
        (status = 428, body = ErrorEnvelope, description = "`confirmation_required`: `confirm` isn't true."),
    )
)]
pub(crate) async fn clear(
    State(s): State<ApiState>,
    headers: HeaderMap,
    JsonBody(body): JsonBody<UsageClear>,
) -> ApiResult<Json<UsageCleared>> {
    same_origin_json(&headers)?;
    if !body.confirm {
        return Err(ApiError::new(
            StatusCode::PRECONDITION_REQUIRED,
            "confirmation_required",
            "Confirm that the usage data should be deleted: it can't be undone.",
        ));
    }
    let store = crate::user_items::user_data(&s)?;
    let queries =
        tokio::task::spawn_blocking(move || store.lock().expect("store lock").clear_query_log())
            .await
            .map_err(internal)?
            .map_err(internal)?;
    Ok(Json(UsageCleared {
        queries: queries as u64,
    }))
}
