//! Updates (architecture §12.2, §10.10.14): `GET /updates`,
//! `POST /updates/check`, `POST /updates/skip`, `POST /updates/pause` and
//! `POST /updates/history/clear` **(§2.19, a new route)**.
//!
//! - **The binary answers** through [`UpdatesApi`], an `Extension`
//!   beside `SystemApi`; without it every route is
//!   `503 updates_unavailable`.
//! - **Security:** every route needs a session (API keys never reach
//!   them), and the writes are same-origin JSON. No route needs the fresh
//!   password: nothing installs that isn't signed, and Roll back is a
//!   Danger-zone action (§10.10.7), not a Security one.
//! - **The check** runs at most once a minute (`429 rate_limited` with
//!   `Retry-After`) and one at a time, at any mode, `off` included.

use std::fmt;
use std::sync::Arc;

use axum::extract::Extension;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use utoipa::OpenApi;

use crate::ApiState;
use crate::auth;
use crate::dto::error::ErrorEnvelope;
pub use crate::dto::updates::{
    Blocked, Capability, CheckState, CheckStatus, HistoryEntry, PauseRequest, PendingInstall,
    Policy, ReleaseInfo, RollbackInfo, Running, SkipRequest, UpdatesStatus,
};
use crate::error::{ApiError, ApiResult, internal};
use crate::extract::{Json as JsonBody, same_origin_json};
use crate::ops::BoxFuture;

/// This module's half of `/openapi.json`.
#[derive(OpenApi)]
#[openapi(paths(status, check, skip, pause, clear_history))]
pub(crate) struct ApiDoc;

/// What these routes ask of the update engine.
pub trait UpdatesApi: Send + Sync + fmt::Debug {
    fn status(&self) -> BoxFuture<'_, UpdatesStatus>;
    /// Runs the signed check now, then reports.
    fn check(&self) -> BoxFuture<'_, Result<UpdatesStatus, UpdatesError>>;
    /// Never installs `version` automatically again (`skipped`, reason
    /// `user`).
    fn skip(&self, version: String) -> BoxFuture<'_, Result<UpdatesStatus, UpdatesError>>;
    /// Pauses automatic installs for `days` (0 resumes).
    fn pause(&self, days: u32) -> BoxFuture<'_, Result<UpdatesStatus, UpdatesError>>;
    fn clear_history(&self) -> BoxFuture<'_, Result<UpdatesStatus, UpdatesError>>;
}

/// Why an updates route refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdatesError {
    /// A check ran less than a minute ago, or one is running.
    RateLimited { retry_after: u64 },
    /// The version isn't one a check found.
    UnknownVersion,
    /// Updates aren't available in this process (`fixture`).
    Unavailable(String),
    /// A value the route doesn't take; the message says which.
    Invalid {
        field: &'static str,
        message: String,
    },
    /// Logged, never shown.
    Internal(String),
}

impl From<UpdatesError> for ApiError {
    fn from(e: UpdatesError) -> ApiError {
        match e {
            UpdatesError::RateLimited { .. } => ApiError::new(
                StatusCode::TOO_MANY_REQUESTS,
                "rate_limited",
                "A check ran moments ago. Try again in a minute.",
            ),
            UpdatesError::UnknownVersion => ApiError::new(
                StatusCode::NOT_FOUND,
                "unknown_version",
                "That version isn't one the last check found.",
            ),
            UpdatesError::Unavailable(why) => ApiError::new(
                StatusCode::CONFLICT,
                "updates_unavailable",
                format!("Updates aren't available here ({why})."),
            ),
            UpdatesError::Invalid { field, message } => {
                ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, "invalid_value", message)
                    .field(field)
            }
            UpdatesError::Internal(e) => internal(format!("updates: {e}")),
        }
    }
}

/// `e` as a response, with `Retry-After` on a `429`.
fn refusal(e: UpdatesError) -> Response {
    let retry = match &e {
        UpdatesError::RateLimited { retry_after } => Some(*retry_after),
        _ => None,
    };
    let mut response = ApiError::from(e).into_response();
    if let Some(secs) = retry {
        response
            .headers_mut()
            .insert(header::RETRY_AFTER, HeaderValue::from(secs.max(1)));
    }
    response
}

/// The routes: all behind a session.
pub fn routes() -> Router<ApiState> {
    auth::protected(
        Router::new()
            .route("/updates", get(status))
            .route("/updates/check", post(check))
            .route("/updates/skip", post(skip))
            .route("/updates/pause", post(pause))
            .route("/updates/history/clear", post(clear_history)),
    )
}

type Handle = Option<Extension<Arc<dyn UpdatesApi>>>;

fn api(handle: Handle) -> ApiResult<Arc<dyn UpdatesApi>> {
    handle.map(|Extension(s)| s).ok_or_else(|| {
        ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "updates_unavailable",
            "Updates aren't available in this process.",
        )
    })
}

fn answer(r: Result<UpdatesStatus, UpdatesError>) -> Response {
    match r {
        Ok(s) => Json(s).into_response(),
        Err(e) => refusal(e),
    }
}

/// The update engine's state: the last check, the running version, what
/// this installation can do, the policy in effect, the releases found
/// (with why each does or doesn't install automatically), any version in
/// probation, Roll back, the notices and the history.
#[utoipa::path(
    operation_id = "getUpdates",
    get,
    path = "/updates",
    tag = "settings",
    responses((status = 200, body = UpdatesStatus))
)]
pub(crate) async fn status(handle: Handle) -> ApiResult<Json<UpdatesStatus>> {
    Ok(Json(api(handle)?.status().await))
}

/// Checks for updates now: the one signed request, at any mode. At most
/// once a minute.
#[utoipa::path(
    operation_id = "checkUpdates",
    post,
    path = "/updates/check",
    tag = "settings",
    responses(
        (status = 200, body = UpdatesStatus),
        (status = 429, description = "`rate_limited`, with `Retry-After`.", body = ErrorEnvelope),
        (status = 409, description = "`updates_unavailable`.", body = ErrorEnvelope),
    )
)]
pub(crate) async fn check(handle: Handle, headers: HeaderMap) -> ApiResult<Response> {
    same_origin_json(&headers)?;
    Ok(answer(api(handle)?.check().await))
}

/// Skips a release: it's never installed automatically, and Install now
/// refuses it (`version_skipped`).
#[utoipa::path(
    operation_id = "skipUpdate",
    post,
    path = "/updates/skip",
    tag = "settings",
    request_body = SkipRequest,
    responses(
        (status = 200, body = UpdatesStatus),
        (status = 404, description = "`unknown_version`.", body = ErrorEnvelope),
    )
)]
pub(crate) async fn skip(
    handle: Handle,
    headers: HeaderMap,
    JsonBody(req): JsonBody<SkipRequest>,
) -> ApiResult<Response> {
    same_origin_json(&headers)?;
    Ok(answer(api(handle)?.skip(req.version).await))
}

/// Pauses automatic installs for 1, 7 or 30 days; 0 resumes. Checks and
/// Install now carry on.
#[utoipa::path(
    operation_id = "pauseUpdates",
    post,
    path = "/updates/pause",
    tag = "settings",
    request_body = PauseRequest,
    responses(
        (status = 200, body = UpdatesStatus),
        (status = 422, description = "`invalid_value` for any other number of days.", body = ErrorEnvelope),
    )
)]
pub(crate) async fn pause(
    handle: Handle,
    headers: HeaderMap,
    JsonBody(req): JsonBody<PauseRequest>,
) -> ApiResult<Response> {
    same_origin_json(&headers)?;
    if ![0, 1, 7, 30].contains(&req.days) {
        return Ok(refusal(UpdatesError::Invalid {
            field: "days",
            message: "Pause for 1, 7 or 30 days, or 0 to resume.".into(),
        }));
    }
    Ok(answer(api(handle)?.pause(req.days).await))
}

/// Clears the update history (`history.json`).
#[utoipa::path(
    operation_id = "clearUpdateHistory",
    post,
    path = "/updates/history/clear",
    tag = "settings",
    responses((status = 200, body = UpdatesStatus))
)]
pub(crate) async fn clear_history(handle: Handle, headers: HeaderMap) -> ApiResult<Response> {
    same_origin_json(&headers)?;
    Ok(answer(api(handle)?.clear_history().await))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refusals_map_to_the_envelope() {
        let r = refusal(UpdatesError::RateLimited { retry_after: 42 });
        assert_eq!(r.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(r.headers()[header::RETRY_AFTER], "42");
        let e = ApiError::from(UpdatesError::UnknownVersion);
        assert_eq!(
            (e.status(), e.code()),
            (StatusCode::NOT_FOUND, "unknown_version")
        );
        let e = ApiError::from(UpdatesError::Internal("/secret".into()));
        assert!(!e.body().to_string().contains("secret"));
    }
}
