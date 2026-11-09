//! Guided and detected companions (plan
//! `docs/plan/2026-11-06-v06-companions.md` CMP-5g, Q12, Q19; architecture
//! §18.10), admin only:
//!
//! - `POST /companions/detect`: what detection finds on its fixed targets
//!   (Q12: loopback and, in a container, the catalogue's container names,
//!   on their catalogue ports). Never a scan.
//! - `POST /companions/{id}/target`: adds the companion at a detected
//!   target or an address you typed, once detection confirms it's there.
//! - `POST /companions/{id}/paste`: qBittorrent's temporary WebUI password
//!   (Q19), sealed at once and never shown or logged.
//! - `POST /companions/{id}/plan`: the settings Lodestar would change, by
//!   name. A POST because it signs in to qBittorrent with the temporary
//!   password, which costs a failed sign-in; at most one a minute.
//! - `POST /companions/{id}/confirm`: applies that plan, echoed exactly,
//!   replaces the temporary password with a generated one, and hands
//!   qBittorrent to Integrations.
//! - `POST /companions/{id}/guided/forget`: deletes Lodestar's record of a
//!   companion you added, with `confirm` naming it.
//!
//! **Prowlarr's hand-over waits** for `X-prowlarr-settings`: its target,
//! paste, plan and confirm answer `409 handover_later`.
//!
//! **The binary answers** through [`CompanionsGuidedApi`], an `Extension`
//! beside `/companions`. Without one every route is
//! `503 companions_unavailable`. Security is as for `/companions`: a
//! session, same-origin JSON writes, stable codes, and no value you pasted
//! in any response, error or log line.

use std::sync::Arc;

use axum::extract::Extension;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use utoipa::OpenApi;

use crate::companions::CompanionsError;
use crate::dto::companions::Companion;
use crate::dto::companions_guided::{
    CompanionConfirmRequest, CompanionForgetRequest, CompanionForgotten, CompanionPasteRequest,
    CompanionPlan, CompanionTarget, DetectedCompanion, DetectedCompanions,
};
use crate::dto::error::ErrorEnvelope;
use crate::error::{ApiError, ApiResult};
use crate::extract::{self, Path, same_origin_json};
use crate::ops::BoxFuture;
use crate::{ApiState, auth};

/// The longest value accepted on paste, in bytes (qBittorrent takes up to
/// 128).
pub const PASTE_MAX: usize = 128;

/// This module's half of `/openapi.json`.
#[derive(OpenApi)]
#[openapi(paths(
    detect_companions,
    set_target,
    paste_credential,
    plan_configuration,
    confirm_configuration,
    forget_companion
))]
pub(crate) struct ApiDoc;

/// Why a guided route refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GuidedError {
    Companions(CompanionsError),
    /// A plan was made less than a minute ago.
    RateLimited {
        retry_after: u64,
    },
}

impl From<CompanionsError> for GuidedError {
    fn from(e: CompanionsError) -> GuidedError {
        GuidedError::Companions(e)
    }
}

fn message(code: &str) -> Option<&'static str> {
    Some(match code {
        "not_detected" => "The companion didn't answer there as itself.",
        "address_refused" => {
            "Lodestar doesn't reach that address: link-local and cloud metadata addresses are refused."
        }
        "handover_later" => {
            "Setting up Prowlarr from here comes in a later version. Add it in its source settings."
        }
        "installed" => "Lodestar installed this companion; it's managed on its own.",
        "generated_pending" => {
            "qBittorrent may already use the new password Lodestar set. Confirm again to finish; to start over, reveal the password, then forget the companion."
        }
        "sign_in_refused" => {
            "qBittorrent refused the password three times, and it bans an address after five. Restart qBittorrent for a new temporary password, then forget this companion and add it again."
        }
        "target_changed" => {
            "The companion's address changed after Lodestar set its new password. Forget it and add it again."
        }
        "already_added" => "This companion is already added. Forget it first to add it again.",
        "no_target" => "Add the companion's address first.",
        "no_temporary_password" => "Paste qBittorrent's temporary password first.",
        "bad_value" => "That doesn't look like qBittorrent's temporary password.",
        "busy" => "That is already running. Try again in a moment.",
        _ => return None,
    })
}

impl From<GuidedError> for ApiError {
    fn from(e: GuidedError) -> ApiError {
        match e {
            GuidedError::RateLimited { .. } => ApiError::new(
                StatusCode::TOO_MANY_REQUESTS,
                "rate_limited",
                "A plan costs qBittorrent a sign-in; wait a minute between them.",
            ),
            GuidedError::Companions(CompanionsError::Refused(code)) if message(code).is_some() => {
                ApiError::new(
                    StatusCode::CONFLICT,
                    code,
                    message(code).unwrap_or_default(),
                )
            }
            GuidedError::Companions(e) => e.into(),
        }
    }
}

/// What these routes ask of the binary. `id` is as the request gave it.
pub trait CompanionsGuidedApi: Send + Sync + std::fmt::Debug {
    fn detect(&self) -> BoxFuture<'_, Result<Vec<DetectedCompanion>, GuidedError>>;
    fn set_target(
        &self,
        id: String,
        target: CompanionTarget,
    ) -> BoxFuture<'_, Result<DetectedCompanion, GuidedError>>;
    /// Seals `value`; it's never returned or logged.
    fn paste(&self, id: String, value: String) -> BoxFuture<'_, Result<(), GuidedError>>;
    fn plan(&self, id: String) -> BoxFuture<'_, Result<Vec<String>, GuidedError>>;
    /// Applies the plan whose settings are `settings` exactly.
    fn confirm(
        &self,
        id: String,
        settings: Vec<String>,
    ) -> BoxFuture<'_, Result<Companion, GuidedError>>;
    /// Deletes Lodestar's record of it: the row keys it deleted, and
    /// whether the credential was the one qBittorrent may use now.
    fn forget(&self, id: String) -> BoxFuture<'_, Result<CompanionForgotten, GuidedError>>;
}

pub fn routes() -> Router<ApiState> {
    auth::protected(
        Router::new()
            .route("/companions/detect", post(detect_companions))
            .route("/companions/{id}/target", post(set_target))
            .route("/companions/{id}/paste", post(paste_credential))
            .route("/companions/{id}/plan", post(plan_configuration))
            .route("/companions/{id}/confirm", post(confirm_configuration))
            .route("/companions/{id}/guided/forget", post(forget_companion)),
    )
}

type Handle = Option<Extension<Arc<dyn CompanionsGuidedApi>>>;

fn api(handle: Handle) -> ApiResult<Arc<dyn CompanionsGuidedApi>> {
    handle.map(|Extension(c)| c).ok_or_else(|| {
        ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "companions_unavailable",
            "Companions aren't available in this process.",
        )
    })
}

/// As `/companions`' own check: `[a-z0-9-]`, 1–32.
fn check_id(id: &str) -> ApiResult<()> {
    let ok = (1..=32).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
    if ok {
        Ok(())
    } else {
        Err(CompanionsError::Unknown.into())
    }
}

/// Runs detection on its fixed targets and lists what answered.
#[utoipa::path(
    operation_id = "detectCompanions",
    post,
    path = "/companions/detect",
    tag = "companions",
    responses(
        (status = 200, body = DetectedCompanions),
        (status = 403, description = "`cross_origin`.", body = ErrorEnvelope),
        (status = 409, description = "`busy`: detection is already running.", body = ErrorEnvelope),
        (status = 415, description = "`unsupported_media_type`: not JSON.", body = ErrorEnvelope),
        (status = 503, description = "`companions_unavailable`.", body = ErrorEnvelope),
    )
)]
pub(crate) async fn detect_companions(
    handle: Handle,
    headers: HeaderMap,
) -> ApiResult<Json<DetectedCompanions>> {
    same_origin_json(&headers)?;
    let found = api(handle)?.detect().await.map_err(ApiError::from)?;
    Ok(Json(DetectedCompanions { found }))
}

/// Adds the companion at `target`, once detection confirms it answers
/// there as itself.
#[utoipa::path(
    operation_id = "setCompanionTarget",
    post,
    path = "/companions/{id}/target",
    tag = "companions",
    params(("id" = String, Path, description = "The catalogue id.")),
    request_body = CompanionTarget,
    responses(
        (status = 200, body = DetectedCompanion),
        (status = 400, description = "`bad_template_input` with `field`: a port or address that can't be used.", body = ErrorEnvelope),
        (status = 403, description = "`cross_origin`.", body = ErrorEnvelope),
        (status = 404, description = "`unknown_companion`.", body = ErrorEnvelope),
        (status = 409, description = "`not_detected`, `address_refused`, `handover_later`, `installed`, `already_added`, `generated_pending`, `busy`, or the companion's own refusal code (`companion_unreachable`, `companion_unauthorized`, `companion_protocol`).", body = ErrorEnvelope),
        (status = 415, description = "`unsupported_media_type`: not JSON.", body = ErrorEnvelope),
        (status = 422, description = "`bad_body`.", body = ErrorEnvelope),
        (status = 503, description = "`companions_unavailable`.", body = ErrorEnvelope),
    )
)]
pub(crate) async fn set_target(
    handle: Handle,
    headers: HeaderMap,
    Path(id): Path<String>,
    extract::Json(body): extract::Json<CompanionTarget>,
) -> ApiResult<Json<DetectedCompanion>> {
    same_origin_json(&headers)?;
    check_id(&id)?;
    Ok(Json(
        api(handle)?
            .set_target(id, body)
            .await
            .map_err(ApiError::from)?,
    ))
}

/// Seals qBittorrent's temporary WebUI password (Q19). Answers `204`; the
/// value is never shown again or logged.
#[utoipa::path(
    operation_id = "pasteCompanionCredential",
    post,
    path = "/companions/{id}/paste",
    tag = "companions",
    params(("id" = String, Path, description = "The catalogue id.")),
    request_body = CompanionPasteRequest,
    responses(
        (status = 204, description = "Sealed."),
        (status = 400, description = "`bad_value` with `field`: empty, too long, or not printable.", body = ErrorEnvelope),
        (status = 403, description = "`cross_origin`.", body = ErrorEnvelope),
        (status = 404, description = "`unknown_companion`.", body = ErrorEnvelope),
        (status = 409, description = "`no_target`, `handover_later`, `installed`, `already_added`, `generated_pending` (a new password may already be qBittorrent's: confirm again, or reveal it first), `busy`.", body = ErrorEnvelope),
        (status = 415, description = "`unsupported_media_type`: not JSON.", body = ErrorEnvelope),
        (status = 422, description = "`bad_body`.", body = ErrorEnvelope),
        (status = 503, description = "`companions_unavailable`.", body = ErrorEnvelope),
    )
)]
pub(crate) async fn paste_credential(
    handle: Handle,
    headers: HeaderMap,
    Path(id): Path<String>,
    extract::Json(body): extract::Json<CompanionPasteRequest>,
) -> ApiResult<StatusCode> {
    same_origin_json(&headers)?;
    check_id(&id)?;
    let api = api(handle)?;
    let v = &body.value;
    if v.is_empty() || v.len() > PASTE_MAX || v.chars().any(char::is_control) {
        return Err(ApiError::bad_request(
            "bad_value",
            "That doesn't look like qBittorrent's temporary password.",
        )
        .field("value"));
    }
    api.paste(id, body.value).await.map_err(ApiError::from)?;
    Ok(StatusCode::NO_CONTENT)
}

/// The settings Lodestar would change, by name. Signs in to qBittorrent
/// with the temporary password, so at most one a minute.
#[utoipa::path(
    operation_id = "planCompanionConfiguration",
    post,
    path = "/companions/{id}/plan",
    tag = "companions",
    params(("id" = String, Path, description = "The catalogue id.")),
    responses(
        (status = 200, body = CompanionPlan),
        (status = 403, description = "`cross_origin`.", body = ErrorEnvelope),
        (status = 404, description = "`unknown_companion`.", body = ErrorEnvelope),
        (status = 409, description = "`no_target`, `no_temporary_password`, `handover_later`, `installed`, `busy`, `sign_in_refused` (three refused sign-ins in a row), `target_changed`, or the companion's own refusal code.", body = ErrorEnvelope),
        (status = 415, description = "`unsupported_media_type`: not JSON.", body = ErrorEnvelope),
        (status = 429, description = "`rate_limited`, with `Retry-After`.", body = ErrorEnvelope),
        (status = 503, description = "`companions_unavailable`.", body = ErrorEnvelope),
    )
)]
pub(crate) async fn plan_configuration(
    handle: Handle,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> ApiResult<Response> {
    same_origin_json(&headers)?;
    check_id(&id)?;
    Ok(match api(handle)?.plan(id.clone()).await {
        Ok(settings) => Json(CompanionPlan { id, settings }).into_response(),
        Err(e) => refusal(e),
    })
}

/// Applies the plan you were shown (its settings echoed exactly), replaces
/// the temporary password with a generated one, and adds qBittorrent to
/// Integrations as `lodestar-qbittorrent`.
#[utoipa::path(
    operation_id = "confirmCompanionConfiguration",
    post,
    path = "/companions/{id}/confirm",
    tag = "companions",
    params(("id" = String, Path, description = "The catalogue id.")),
    request_body = CompanionConfirmRequest,
    responses(
        (status = 200, body = Companion),
        (status = 403, description = "`cross_origin`.", body = ErrorEnvelope),
        (status = 404, description = "`unknown_companion`.", body = ErrorEnvelope),
        (status = 409, description = "`confirmation_mismatch`, `no_target`, `no_temporary_password`, `handover_later`, `installed`, `busy`, `sign_in_refused`, `target_changed`, or the companion's own refusal code.", body = ErrorEnvelope),
        (status = 415, description = "`unsupported_media_type`: not JSON.", body = ErrorEnvelope),
        (status = 422, description = "`bad_body`.", body = ErrorEnvelope),
        (status = 503, description = "`companions_unavailable`.", body = ErrorEnvelope),
    )
)]
pub(crate) async fn confirm_configuration(
    handle: Handle,
    headers: HeaderMap,
    Path(id): Path<String>,
    extract::Json(body): extract::Json<CompanionConfirmRequest>,
) -> ApiResult<Json<Companion>> {
    same_origin_json(&headers)?;
    check_id(&id)?;
    Ok(Json(
        api(handle)?
            .confirm(id, body.settings)
            .await
            .map_err(ApiError::from)?,
    ))
}

/// Deletes Lodestar's record of a companion you added and its stored
/// credential. `confirm` must name it. The Integrations entry stays. A
/// generated credential is the password qBittorrent uses once it's set:
/// reveal it first if you'll sign in to qBittorrent yourself
/// (`credential_in_use` says it was one).
#[utoipa::path(
    operation_id = "forgetCompanion",
    post,
    path = "/companions/{id}/guided/forget",
    tag = "companions",
    params(("id" = String, Path, description = "The catalogue id.")),
    request_body = CompanionForgetRequest,
    responses(
        (status = 200, body = CompanionForgotten),
        (status = 403, description = "`cross_origin`.", body = ErrorEnvelope),
        (status = 404, description = "`unknown_companion`.", body = ErrorEnvelope),
        (status = 409, description = "`confirmation_mismatch`, `installed`, `busy`.", body = ErrorEnvelope),
        (status = 415, description = "`unsupported_media_type`: not JSON.", body = ErrorEnvelope),
        (status = 422, description = "`bad_body`.", body = ErrorEnvelope),
        (status = 503, description = "`companions_unavailable`.", body = ErrorEnvelope),
    )
)]
pub(crate) async fn forget_companion(
    handle: Handle,
    headers: HeaderMap,
    Path(id): Path<String>,
    extract::Json(body): extract::Json<CompanionForgetRequest>,
) -> ApiResult<Json<CompanionForgotten>> {
    same_origin_json(&headers)?;
    check_id(&id)?;
    let api = api(handle)?;
    if body.confirm != id {
        return Err(CompanionsError::Mismatch.into());
    }
    Ok(Json(api.forget(id).await.map_err(ApiError::from)?))
}

/// `e` as a response, with `Retry-After` on a `429`.
fn refusal(e: GuidedError) -> Response {
    let retry = match &e {
        GuidedError::RateLimited { retry_after } => Some(*retry_after),
        GuidedError::Companions(_) => None,
    };
    let mut response = ApiError::from(e).into_response();
    if let Some(secs) = retry {
        response
            .headers_mut()
            .insert(header::RETRY_AFTER, HeaderValue::from(secs.max(1)));
    }
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guided_refusals_have_their_codes() {
        for code in [
            "not_detected",
            "address_refused",
            "handover_later",
            "installed",
            "already_added",
            "generated_pending",
            "target_changed",
            "sign_in_refused",
            "no_target",
            "no_temporary_password",
            "busy",
        ] {
            let e = ApiError::from(GuidedError::from(CompanionsError::Refused(code)));
            assert_eq!((e.status().as_u16(), e.code()), (409, code));
        }
        let e = ApiError::from(GuidedError::RateLimited { retry_after: 42 });
        assert_eq!((e.status().as_u16(), e.code()), (429, "rate_limited"));
    }
}
