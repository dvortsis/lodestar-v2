//! The running build and installation, the restart and the HTTP-address
//! keep-or-revert timer (architecture §10.10.6, §10.10.9, §10.10.13):
//! `GET /system`, `POST /system/restart`, `POST /system/address/keep` and
//! `POST /system/address/revert`.
//!
//! - **One request:** About, the Overview and General all read
//!   `GET /system`. The update status isn't here: it's the update
//!   engine's signed check (`/updates`, §12.2).
//! - **The binary answers** through [`SystemApi`], an `Extension` the
//!   server layers beside the event hub; without it every route is
//!   `503 system_unavailable`.
//! - **Restart answers first:** `202`, then the restart is asked for once
//!   the answer's body has been handed to the connection, so the browser
//!   learns it's coming before the server goes away.
//! - **Security:** every route is behind a session, and the writes are
//!   same-origin JSON. Nothing returned is a secret, a URL credential or
//!   a torrent detail.

use std::fmt;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use axum::body::{Body, Bytes};
use axum::extract::Extension;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use utoipa::OpenApi;

use crate::ApiState;
use crate::auth;
use crate::dto::error::ErrorEnvelope;
pub use crate::dto::system::{
    AddressChange, AddressOutcome, AddressState, BuildInfo, ChangedSetting, Channel, Connection,
    ConnectionKind, ConnectionState, DataFormats, DataInfo, Deployment, DeploymentKind,
    Diagnostics, HostInfo, KeepOrRevertKind, RestartAccepted, RestartInfo, RestartMethod,
    SystemInfo,
};
use crate::error::{ApiError, ApiResult, internal};
use crate::extract::same_origin_json;
use crate::ops::BoxFuture;

/// This module's half of `/openapi.json`.
#[derive(OpenApi)]
#[openapi(paths(system, restart, keep_address, revert_address))]
pub(crate) struct ApiDoc;

/// What these routes ask of the running process.
pub trait SystemApi: Send + Sync + fmt::Debug {
    /// Everything `GET /system` reports.
    fn info(&self) -> BoxFuture<'_, SystemInfo>;
    /// Whether and how this process can restart itself.
    fn restart_info(&self) -> RestartInfo;
    /// Asks for a restart: the graceful shutdown starts, then the process
    /// comes back. Called once `POST /system/restart`'s answer is out.
    fn restart(&self);
    /// Confirms the pending HTTP address from this process, which is
    /// serving on it.
    fn keep_address(&self) -> BoxFuture<'_, Result<AddressOutcome, SystemError>>;
    /// Goes back to the previous HTTP address now. With `restarting`, the
    /// caller then asks for a restart, as `POST /system/restart` does.
    fn revert_address(&self) -> BoxFuture<'_, Result<AddressOutcome, SystemError>>;
}

/// Why a system route refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SystemError {
    /// This process can't restart itself; the hint says how instead.
    RestartUnavailable { hint: String },
    /// There's no address change to keep or revert in that state.
    NoAddressChange,
    /// Logged, never shown.
    Internal(String),
}

impl From<SystemError> for ApiError {
    fn from(e: SystemError) -> ApiError {
        match e {
            SystemError::RestartUnavailable { hint } => {
                ApiError::new(StatusCode::CONFLICT, "restart_unavailable", hint)
            }
            SystemError::NoAddressChange => ApiError::new(
                StatusCode::CONFLICT,
                "no_address_change",
                "There's no address change waiting for confirmation.",
            ),
            SystemError::Internal(e) => internal(format!("system: {e}")),
        }
    }
}

/// The routes: all behind a session.
pub fn routes() -> Router<ApiState> {
    auth::protected(
        Router::new()
            .route("/system", get(system))
            .route("/system/restart", post(restart))
            .route("/system/address/keep", post(keep_address))
            .route("/system/address/revert", post(revert_address)),
    )
}

type Handle = Option<Extension<Arc<dyn SystemApi>>>;

fn api(handle: Handle) -> ApiResult<Arc<dyn SystemApi>> {
    handle.map(|Extension(s)| s).ok_or_else(|| {
        ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "system_unavailable",
            "System information isn't available in this process.",
        )
    })
}

/// The build, this installation, the restart and any pending address
/// change: everything About, the Overview and General show. A value this
/// platform can't measure is null, never 0.
#[utoipa::path(
    operation_id = "getSystem",
    get,
    path = "/system",
    tag = "settings",
    responses((status = 200, body = SystemInfo))
)]
pub(crate) async fn system(handle: Handle) -> ApiResult<Json<SystemInfo>> {
    Ok(Json(api(handle)?.info().await))
}

/// Restarts Lodestar: a graceful shutdown (the queue, DHT state and last
/// commit are saved), then the same program again. It answers first;
/// poll `/healthz` until it's back. `409 restart_unavailable` where it
/// can't restart itself, with how to restart instead.
#[utoipa::path(
    operation_id = "restartSystem",
    post,
    path = "/system/restart",
    tag = "settings",
    responses(
        (status = 202, body = RestartAccepted),
        (status = 409, description = "`restart_unavailable`: restart it the way it was started (the message says how).", body = ErrorEnvelope),
    )
)]
pub(crate) async fn restart(handle: Handle, headers: HeaderMap) -> ApiResult<Response> {
    same_origin_json(&headers)?;
    let api = api(handle)?;
    let info = api.restart_info();
    if !info.available {
        return Err(SystemError::RestartUnavailable {
            hint: info.hint.unwrap_or_default(),
        }
        .into());
    }
    let body = RestartAccepted {
        estimated_seconds: info.estimated_seconds,
        restarting: true,
    };
    then_restart(StatusCode::ACCEPTED, &body, api)
}

/// Keeps the new HTTP address: this process is serving on it, so the
/// request itself shows it's reachable (whatever `Host` says: a reverse
/// proxy may rewrite it). `409 no_address_change` unless one is awaiting
/// confirmation.
#[utoipa::path(
    operation_id = "keepAddress",
    post,
    path = "/system/address/keep",
    tag = "settings",
    responses(
        (status = 200, body = AddressOutcome),
        (status = 409, description = "`no_address_change`.", body = ErrorEnvelope),
    )
)]
pub(crate) async fn keep_address(
    handle: Handle,
    headers: HeaderMap,
) -> ApiResult<Json<AddressOutcome>> {
    same_origin_json(&headers)?;
    Ok(Json(api(handle)?.keep_address().await?))
}

/// Goes back to the previous HTTP address now. Awaiting confirmation,
/// the previous value is saved again and the server restarts onto it
/// (`restarting`); awaiting the restart, the saved change is undone and
/// nothing restarts. `409 no_address_change` when there's no change.
#[utoipa::path(
    operation_id = "revertAddress",
    post,
    path = "/system/address/revert",
    tag = "settings",
    responses(
        (status = 200, body = AddressOutcome),
        (status = 409, description = "`no_address_change`.", body = ErrorEnvelope),
    )
)]
pub(crate) async fn revert_address(handle: Handle, headers: HeaderMap) -> ApiResult<Response> {
    same_origin_json(&headers)?;
    let api = api(handle)?;
    let outcome = api.revert_address().await?;
    if outcome.restarting {
        then_restart(StatusCode::OK, &outcome, api)
    } else {
        Ok(Json(outcome).into_response())
    }
}

/// `body` as JSON, asking `api` for a restart once the body has been
/// handed to the connection (or dropped with it).
fn then_restart(
    status: StatusCode,
    body: &impl serde::Serialize,
    api: Arc<dyn SystemApi>,
) -> ApiResult<Response> {
    let bytes = Bytes::from(serde_json::to_vec(body).map_err(internal)?);
    let len = bytes.len();
    let stream = ThenRestart {
        chunk: Some(bytes),
        api: Some(api),
    };
    let mut response = (status, Body::from_stream(stream)).into_response();
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    headers.insert(header::CONTENT_LENGTH, HeaderValue::from(len));
    Ok(response)
}

/// A one-chunk body that asks for the restart when it's dropped: after
/// the server has written it, or when the client went away first.
struct ThenRestart {
    chunk: Option<Bytes>,
    api: Option<Arc<dyn SystemApi>>,
}

impl futures_core::Stream for ThenRestart {
    type Item = Result<Bytes, std::convert::Infallible>;

    fn poll_next(mut self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        Poll::Ready(self.chunk.take().map(Ok))
    }
}

impl Drop for ThenRestart {
    fn drop(&mut self) {
        if let Some(api) = self.api.take() {
            api.restart();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refusals_map_to_the_envelope() {
        let e = ApiError::from(SystemError::RestartUnavailable {
            hint: "Restart the service.".into(),
        });
        assert_eq!(
            (e.status(), e.code()),
            (StatusCode::CONFLICT, "restart_unavailable")
        );
        assert_eq!(e.body()["error"]["message"], "Restart the service.");
        let e = ApiError::from(SystemError::NoAddressChange);
        assert_eq!(
            (e.status(), e.code()),
            (StatusCode::CONFLICT, "no_address_change")
        );
        let e = ApiError::from(SystemError::Internal("/secret/path".into()));
        assert_eq!(e.status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert!(!e.body().to_string().contains("secret"));
    }
}
