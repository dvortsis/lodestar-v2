//! `/companions` (architecture §18.10, §12; plan
//! `docs/plan/2026-11-06-v06-companions.md` CMP-6, Q5): Settings →
//! Companions' list, detail and actions, admin only.
//!
//! - `GET /companions`, `GET /companions/{id}`: every catalogue companion,
//!   what Lodestar installed, its health, and whether it can be installed
//!   here (or why not, as a stable code).
//! - `GET /companions/{id}/install`: the pinned version, SHA-256, size and
//!   licence an install or update would fetch. `POST` installs or updates
//!   only when the body echoes that version and SHA-256, as
//!   `lodestar companion install --sha256` does; the manager checks again.
//! - `GET /companions/{id}/remove`: exactly what removal deletes. `POST`
//!   deletes it only with `confirm` naming the companion.
//! - `POST /companions/{id}/restart`, `GET /companions/{id}/logs`.
//! - `POST /companions/{id}/template`: CMP-7's compose service or Unraid
//!   template, from checked inputs. Nothing is stored.
//! - `POST /companions/{id}/credentials/{field}/reveal`: one stored
//!   credential, after the admin password is entered again; logged as
//!   `companions.credential_revealed` with names only, never the value
//!   (Q5). `…/regenerate` replaces it.
//!
//! **The binary answers** through [`CompanionsApi`], an `Extension` the
//! server layers beside `/system`. Without one (a fixture, or companions
//! off for this run) every route is `503 companions_unavailable`. Until
//! CMP-5g wires the supervisor, the binary's actions answer
//! `409 not_wired` before anything is downloaded or changed.
//!
//! **Security:** every route is behind a session (API keys, feed tokens
//! and anything else that isn't the signed-in admin are refused), writes
//! are same-origin JSON, and errors carry stable codes: never a path, a
//! URL or a credential. A credential leaves only in the reveal's body,
//! with `Cache-Control: no-store`.

use std::sync::Arc;

use axum::extract::{Extension, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::routing::{get, post};
use axum::{Json, Router};
use lodestar_log::{Client, log_event};
use utoipa::OpenApi;

use crate::dto::companions::{
    Companion, CompanionAccepted, CompanionInstallRequest, CompanionList, CompanionLogs,
    CompanionTemplate, InstallOffer, RemovalPlan, RemoveRequest, RevealRequest, RevealedCredential,
    TemplateRequest,
};
use crate::dto::error::ErrorEnvelope;
use crate::error::{ApiError, ApiResult, internal};
use crate::extract::{self, ClientIp, Path, same_origin_json};
use crate::log_events::COMPANIONS_CREDENTIAL_REVEALED;
use crate::ops::BoxFuture;
use crate::{ApiState, auth};

/// This module's half of `/openapi.json`.
#[derive(OpenApi)]
#[openapi(
    paths(
        list_companions,
        get_companion,
        install_offer,
        install_companion,
        removal_plan,
        remove_companion,
        restart_companion,
        companion_logs,
        companion_template,
        reveal_credential,
        regenerate_credential
    ),
    tags((name = "companions", description = "Companions: install, update, remove, templates and the credential reveal (§18.10, Q5)."))
)]
pub(crate) struct ApiDoc;

/// A credential the binary opened for the reveal. `id` and `field` are
/// the catalogue's own names, so they can be logged.
pub struct Revealed {
    pub id: &'static str,
    pub field: &'static str,
    pub value: String,
}

impl std::fmt::Debug for Revealed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Revealed")
            .field("id", &self.id)
            .field("field", &self.field)
            .finish_non_exhaustive()
    }
}

/// What these routes ask of the companion manager. `id` and `field` are
/// as the request gave them: the binary checks them against the
/// catalogue.
pub trait CompanionsApi: Send + Sync + std::fmt::Debug {
    fn list(&self) -> BoxFuture<'_, Result<Vec<Companion>, CompanionsError>>;
    fn get(&self, id: String) -> BoxFuture<'_, Result<Companion, CompanionsError>>;
    fn offer(&self, id: String) -> BoxFuture<'_, Result<InstallOffer, CompanionsError>>;
    /// Installs or updates to the confirmed pin. The binary passes both
    /// values to the manager, which compares them with the pin again.
    fn install(
        &self,
        id: String,
        version: String,
        sha256: String,
    ) -> BoxFuture<'_, Result<Companion, CompanionsError>>;
    /// What removing it deletes, without deleting anything.
    fn removal(&self, id: String) -> BoxFuture<'_, Result<RemovalPlan, CompanionsError>>;
    fn remove(&self, id: String) -> BoxFuture<'_, Result<RemovalPlan, CompanionsError>>;
    fn restart(&self, id: String) -> BoxFuture<'_, Result<(), CompanionsError>>;
    /// Recent output, already redacted by the binary with the companion's
    /// own credentials.
    fn logs(&self, id: String) -> BoxFuture<'_, Result<Vec<String>, CompanionsError>>;
    fn template(
        &self,
        id: String,
        request: TemplateRequest,
    ) -> BoxFuture<'_, Result<CompanionTemplate, CompanionsError>>;
    /// Opens one stored credential. Only called after the fresh-password
    /// check has passed.
    fn reveal(&self, id: String, field: String)
    -> BoxFuture<'_, Result<Revealed, CompanionsError>>;
    /// Replaces one credential with a new random one, written into the
    /// companion's own config and stored sealed. Never reads the old one.
    fn regenerate(&self, id: String, field: String) -> BoxFuture<'_, Result<(), CompanionsError>>;
}

/// Why a companions route refused. Every variant is a stable code; none
/// carries a path, a URL or a value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompanionsError {
    /// No catalogue entry has this id.
    Unknown,
    /// That credential isn't stored for it.
    NoCredential,
    /// The confirmation doesn't name what was shown.
    Mismatch,
    /// The manager refused (`not_wired`, `busy`, `native_in_container`,
    /// `awaiting_pin`, `not_installed`, …). Nothing was changed.
    Refused(&'static str),
    /// A template input was refused: which field, when one alone is to
    /// blame, and why.
    BadInput {
        field: Option<&'static str>,
        code: &'static str,
    },
    /// Logged by code, never shown.
    Internal(&'static str),
}

/// The sentence for a refusal code; a code without one gets a general
/// sentence (the code itself is always in the envelope).
fn refusal_message(code: &str) -> &'static str {
    match code {
        "not_wired" => "This version of Lodestar can't run companions yet, so nothing was changed.",
        "native_in_container" => {
            "Lodestar runs in a container, where companions aren't installed for you. Use its compose service or Unraid template instead."
        }
        "unsupported_platform" => "Companions can't be installed on this platform.",
        "awaiting_pin" | "no_build" => {
            "This release of Lodestar has no checked build of it for this machine."
        }
        "guided_only" => "On Windows its own installer is offered for you to run.",
        "busy" => "Another change to this companion is under way. Try again when it's done.",
        "already_current" => "That version is already installed.",
        "not_installed" => "Lodestar didn't install it.",
        "credential_present" => "A key or password you added is stored for it. Remove that first.",
        "folder_in_use" => {
            "Its folder in the data folder holds files Lodestar didn't put there, so it's left alone."
        }
        "no_launch_profile" => "This version of Lodestar can't run it yet.",
        "no_template" => "It doesn't offer that template.",
        _ => "The companion manager refused this; nothing was changed.",
    }
}

impl From<CompanionsError> for ApiError {
    fn from(e: CompanionsError) -> ApiError {
        match e {
            CompanionsError::Unknown => ApiError::new(
                StatusCode::NOT_FOUND,
                "unknown_companion",
                "There's no companion with that id.",
            ),
            CompanionsError::NoCredential => ApiError::new(
                StatusCode::NOT_FOUND,
                "no_credential",
                "That credential isn't stored for this companion.",
            ),
            CompanionsError::Mismatch => ApiError::new(
                StatusCode::CONFLICT,
                "confirmation_mismatch",
                "The confirmation doesn't match what was shown. Look again, then confirm.",
            ),
            CompanionsError::Refused(code) => {
                ApiError::new(StatusCode::CONFLICT, code, refusal_message(code))
            }
            CompanionsError::BadInput { field, code } => {
                let e = ApiError::bad_request(code, "That value can't be used in a template.");
                match field {
                    Some(f) => e.field(f),
                    None => e,
                }
            }
            CompanionsError::Internal(code) => internal(format!("companions: {code}")),
        }
    }
}

pub fn routes() -> Router<ApiState> {
    auth::protected(
        Router::new()
            .route("/companions", get(list_companions))
            .route("/companions/{id}", get(get_companion))
            .route(
                "/companions/{id}/install",
                get(install_offer).post(install_companion),
            )
            .route(
                "/companions/{id}/remove",
                get(removal_plan).post(remove_companion),
            )
            .route("/companions/{id}/restart", post(restart_companion))
            .route("/companions/{id}/logs", get(companion_logs))
            .route("/companions/{id}/template", post(companion_template))
            .route(
                "/companions/{id}/credentials/{field}/reveal",
                post(reveal_credential),
            )
            .route(
                "/companions/{id}/credentials/{field}/regenerate",
                post(regenerate_credential),
            ),
    )
}

type Handle = Option<Extension<Arc<dyn CompanionsApi>>>;

fn api(handle: Handle) -> ApiResult<Arc<dyn CompanionsApi>> {
    handle.map(|Extension(c)| c).ok_or_else(|| {
        ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "companions_unavailable",
            "Companions aren't available in this process.",
        )
    })
}

/// A catalogue id's shape (`[a-z0-9-]`, 1–32): anything else is unknown
/// before the binary is asked.
fn check_id(id: &str) -> Result<(), CompanionsError> {
    let ok = (1..=32).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
    if ok {
        Ok(())
    } else {
        Err(CompanionsError::Unknown)
    }
}

/// A credential field's shape; the binary knows which exist.
fn check_field(field: &str) -> Result<(), CompanionsError> {
    let ok = (1..=32).contains(&field.len())
        && field
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_');
    if ok {
        Ok(())
    } else {
        Err(CompanionsError::NoCredential)
    }
}

/// Every catalogue companion.
#[utoipa::path(
    operation_id = "listCompanions",
    get,
    path = "/companions",
    tag = "companions",
    responses(
        (status = 200, body = CompanionList),
        (status = 503, description = "`companions_unavailable`: companions are off in this process.", body = ErrorEnvelope),
    )
)]
pub(crate) async fn list_companions(handle: Handle) -> ApiResult<Json<CompanionList>> {
    let companions = api(handle)?.list().await?;
    Ok(Json(CompanionList { companions }))
}

/// One companion.
#[utoipa::path(
    operation_id = "getCompanion",
    get,
    path = "/companions/{id}",
    tag = "companions",
    params(("id" = String, Path, description = "The catalogue id, e.g. `qbittorrent`.")),
    responses(
        (status = 200, body = Companion),
        (status = 404, description = "`unknown_companion`.", body = ErrorEnvelope),
        (status = 503, description = "`companions_unavailable`: companions are off in this process.", body = ErrorEnvelope),
    )
)]
pub(crate) async fn get_companion(
    handle: Handle,
    Path(id): Path<String>,
) -> ApiResult<Json<Companion>> {
    check_id(&id)?;
    Ok(Json(api(handle)?.get(id).await?))
}

/// What installing or updating it would fetch: show this, then confirm
/// by echoing `version` and `sha256`.
#[utoipa::path(
    operation_id = "getCompanionInstallOffer",
    get,
    path = "/companions/{id}/install",
    tag = "companions",
    params(("id" = String, Path, description = "The catalogue id.")),
    responses(
        (status = 200, body = InstallOffer),
        (status = 404, description = "`unknown_companion`.", body = ErrorEnvelope),
        (status = 409, description = "Why it can't be installed here: `native_in_container`, `unsupported_platform`, `awaiting_pin`, `no_build`, `guided_only`, `already_current`, ….", body = ErrorEnvelope),
        (status = 503, description = "`companions_unavailable`: companions are off in this process.", body = ErrorEnvelope),
    )
)]
pub(crate) async fn install_offer(
    handle: Handle,
    Path(id): Path<String>,
) -> ApiResult<Json<InstallOffer>> {
    check_id(&id)?;
    Ok(Json(api(handle)?.offer(id).await?))
}

/// Installs or updates it to the pinned version you confirmed. The body
/// echoes the offer's `version` and `sha256`; anything else is refused
/// before anything is fetched.
#[utoipa::path(
    operation_id = "installCompanion",
    post,
    path = "/companions/{id}/install",
    tag = "companions",
    params(("id" = String, Path, description = "The catalogue id.")),
    request_body = CompanionInstallRequest,
    responses(
        (status = 200, body = Companion),
        (status = 422, description = "`bad_body`.", body = ErrorEnvelope),
        (status = 403, description = "`cross_origin`.", body = ErrorEnvelope),
        (status = 404, description = "`unknown_companion`.", body = ErrorEnvelope),
        (status = 409, description = "`confirmation_mismatch`, `not_wired`, `busy`, or why it can't be installed here.", body = ErrorEnvelope),
        (status = 415, description = "`unsupported_media_type`: not JSON.", body = ErrorEnvelope),
        (status = 503, description = "`companions_unavailable`: companions are off in this process.", body = ErrorEnvelope),
    )
)]
pub(crate) async fn install_companion(
    handle: Handle,
    headers: HeaderMap,
    Path(id): Path<String>,
    extract::Json(body): extract::Json<CompanionInstallRequest>,
) -> ApiResult<Json<Companion>> {
    same_origin_json(&headers)?;
    check_id(&id)?;
    let api = api(handle)?;
    // Refused here first, so a stale or typed-wrong confirmation never
    // reaches the manager; the manager compares with the pin again.
    let offer = api.offer(id.clone()).await?;
    let sha256 = body.sha256.trim().to_ascii_lowercase();
    if body.version != offer.version || sha256 != offer.sha256 {
        return Err(CompanionsError::Mismatch.into());
    }
    Ok(Json(api.install(id, body.version, sha256).await?))
}

/// Exactly what removing it deletes. Nothing is deleted.
#[utoipa::path(
    operation_id = "getCompanionRemoval",
    get,
    path = "/companions/{id}/remove",
    tag = "companions",
    params(("id" = String, Path, description = "The catalogue id.")),
    responses(
        (status = 200, body = RemovalPlan),
        (status = 404, description = "`unknown_companion`.", body = ErrorEnvelope),
        (status = 409, description = "`not_installed`, `native_in_container`.", body = ErrorEnvelope),
        (status = 503, description = "`companions_unavailable`: companions are off in this process.", body = ErrorEnvelope),
    )
)]
pub(crate) async fn removal_plan(
    handle: Handle,
    Path(id): Path<String>,
) -> ApiResult<Json<RemovalPlan>> {
    check_id(&id)?;
    Ok(Json(api(handle)?.removal(id).await?))
}

/// Removes it: stops it, deletes its credentials, then its folder. `confirm`
/// must be the companion's id.
#[utoipa::path(
    operation_id = "removeCompanion",
    post,
    path = "/companions/{id}/remove",
    tag = "companions",
    params(("id" = String, Path, description = "The catalogue id.")),
    request_body = RemoveRequest,
    responses(
        (status = 200, body = RemovalPlan),
        (status = 422, description = "`bad_body`.", body = ErrorEnvelope),
        (status = 403, description = "`cross_origin`.", body = ErrorEnvelope),
        (status = 404, description = "`unknown_companion`.", body = ErrorEnvelope),
        (status = 409, description = "`confirmation_mismatch`, `not_installed`, `busy`.", body = ErrorEnvelope),
        (status = 415, description = "`unsupported_media_type`: not JSON.", body = ErrorEnvelope),
        (status = 503, description = "`companions_unavailable`: companions are off in this process.", body = ErrorEnvelope),
    )
)]
pub(crate) async fn remove_companion(
    handle: Handle,
    headers: HeaderMap,
    Path(id): Path<String>,
    extract::Json(body): extract::Json<RemoveRequest>,
) -> ApiResult<Json<RemovalPlan>> {
    same_origin_json(&headers)?;
    check_id(&id)?;
    let api = api(handle)?;
    if body.confirm != id {
        return Err(CompanionsError::Mismatch.into());
    }
    Ok(Json(api.remove(id).await?))
}

/// Restarts it. `202`: the companion's row shows when it's back.
#[utoipa::path(
    operation_id = "restartCompanion",
    post,
    path = "/companions/{id}/restart",
    tag = "companions",
    params(("id" = String, Path, description = "The catalogue id.")),
    responses(
        (status = 202, body = CompanionAccepted),
        (status = 403, description = "`cross_origin`.", body = ErrorEnvelope),
        (status = 404, description = "`unknown_companion`.", body = ErrorEnvelope),
        (status = 409, description = "`not_installed`, `not_wired`, `busy`.", body = ErrorEnvelope),
        (status = 415, description = "`unsupported_media_type`: not JSON.", body = ErrorEnvelope),
        (status = 503, description = "`companions_unavailable`: companions are off in this process.", body = ErrorEnvelope),
    )
)]
pub(crate) async fn restart_companion(
    handle: Handle,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> ApiResult<(StatusCode, Json<CompanionAccepted>)> {
    same_origin_json(&headers)?;
    check_id(&id)?;
    api(handle)?.restart(id.clone()).await?;
    Ok((StatusCode::ACCEPTED, Json(CompanionAccepted { id })))
}

/// Its recent output, with its credentials redacted.
#[utoipa::path(
    operation_id = "getCompanionLogs",
    get,
    path = "/companions/{id}/logs",
    tag = "companions",
    params(("id" = String, Path, description = "The catalogue id.")),
    responses(
        (status = 200, body = CompanionLogs),
        (status = 404, description = "`unknown_companion`.", body = ErrorEnvelope),
        (status = 409, description = "`not_installed`, `not_wired`.", body = ErrorEnvelope),
        (status = 503, description = "`companions_unavailable`: companions are off in this process.", body = ErrorEnvelope),
    )
)]
pub(crate) async fn companion_logs(
    handle: Handle,
    Path(id): Path<String>,
) -> ApiResult<([(header::HeaderName, &'static str); 1], Json<CompanionLogs>)> {
    check_id(&id)?;
    let lines = api(handle)?.logs(id.clone()).await?;
    Ok((
        [(header::CACHE_CONTROL, "no-store")],
        Json(CompanionLogs { id, lines }),
    ))
}

/// Its compose service or Unraid template, for running it as its own
/// container. Each value is checked as `lodestar companion compose`
/// checks it; nothing is stored.
#[utoipa::path(
    operation_id = "getCompanionTemplate",
    post,
    path = "/companions/{id}/template",
    tag = "companions",
    params(("id" = String, Path, description = "The catalogue id.")),
    request_body = TemplateRequest,
    responses(
        (status = 200, body = CompanionTemplate),
        (status = 400, description = "`bad_template_input` and the rule's own code, with `field`.", body = ErrorEnvelope),
        (status = 422, description = "`bad_body`.", body = ErrorEnvelope),
        (status = 403, description = "`cross_origin`.", body = ErrorEnvelope),
        (status = 404, description = "`unknown_companion`.", body = ErrorEnvelope),
        (status = 409, description = "`no_template`, `awaiting_pin`, ….", body = ErrorEnvelope),
        (status = 415, description = "`unsupported_media_type`: not JSON.", body = ErrorEnvelope),
        (status = 503, description = "`companions_unavailable`: companions are off in this process.", body = ErrorEnvelope),
    )
)]
pub(crate) async fn companion_template(
    handle: Handle,
    headers: HeaderMap,
    Path(id): Path<String>,
    extract::Json(body): extract::Json<TemplateRequest>,
) -> ApiResult<Json<CompanionTemplate>> {
    same_origin_json(&headers)?;
    check_id(&id)?;
    Ok(Json(api(handle)?.template(id, body).await?))
}

/// Shows one stored credential, after the admin password is entered
/// again (Q5), so you can sign in to the companion's own web page. Logged
/// as `companions.credential_revealed`, without the value.
#[utoipa::path(
    operation_id = "revealCompanionCredential",
    post,
    path = "/companions/{id}/credentials/{field}/reveal",
    tag = "companions",
    params(
        ("id" = String, Path, description = "The catalogue id."),
        ("field" = String, Path, description = "`webui_password` or `api_key`."),
    ),
    request_body = RevealRequest,
    responses(
        (status = 200, body = RevealedCredential),
        (status = 401, description = "`bad_password`.", body = ErrorEnvelope),
        (status = 403, description = "`cross_origin`.", body = ErrorEnvelope),
        (status = 404, description = "`unknown_companion`, `no_credential`.", body = ErrorEnvelope),
        (status = 415, description = "`unsupported_media_type`: not JSON.", body = ErrorEnvelope),
        (status = 422, description = "`bad_body`.", body = ErrorEnvelope),
        (status = 428, description = "`confirm_required`: no password.", body = ErrorEnvelope),
        (status = 429, description = "`rate_limited`: too many password attempts.", body = ErrorEnvelope),
        (status = 503, description = "`companions_unavailable`: companions are off in this process.", body = ErrorEnvelope),
    )
)]
pub(crate) async fn reveal_credential(
    State(s): State<ApiState>,
    ClientIp(ip): ClientIp,
    handle: Handle,
    headers: HeaderMap,
    Path((id, field)): Path<(String, String)>,
    extract::Json(body): extract::Json<RevealRequest>,
) -> ApiResult<(
    [(header::HeaderName, &'static str); 1],
    Json<RevealedCredential>,
)> {
    same_origin_json(&headers)?;
    let api = api(handle)?;
    // The password first, so the shape of a guess learns nothing.
    auth::confirm_fresh_password(&s.auth, ip, body.password).await?;
    check_id(&id)?;
    check_field(&field)?;
    let r = api.reveal(id, field).await?;
    log_event!(
        COMPANIONS_CREDENTIAL_REVEALED,
        companion = r.id,
        field = r.field,
        client = Client(ip)
    );
    Ok((
        [(header::CACHE_CONTROL, "no-store")],
        Json(RevealedCredential {
            field: r.field.to_owned(),
            id: r.id.to_owned(),
            value: r.value,
        }),
    ))
}

/// Replaces one credential with a new random one, offered beside the
/// reveal (Q5). The old one stops working; it's never read or returned.
#[utoipa::path(
    operation_id = "regenerateCompanionCredential",
    post,
    path = "/companions/{id}/credentials/{field}/regenerate",
    tag = "companions",
    params(
        ("id" = String, Path, description = "The catalogue id."),
        ("field" = String, Path, description = "`webui_password` or `api_key`."),
    ),
    responses(
        (status = 202, body = CompanionAccepted),
        (status = 403, description = "`cross_origin`.", body = ErrorEnvelope),
        (status = 404, description = "`unknown_companion`, `no_credential`.", body = ErrorEnvelope),
        (status = 409, description = "`not_installed`, `not_wired`, `busy`.", body = ErrorEnvelope),
        (status = 415, description = "`unsupported_media_type`: not JSON.", body = ErrorEnvelope),
        (status = 503, description = "`companions_unavailable`: companions are off in this process.", body = ErrorEnvelope),
    )
)]
pub(crate) async fn regenerate_credential(
    handle: Handle,
    headers: HeaderMap,
    Path((id, field)): Path<(String, String)>,
) -> ApiResult<(StatusCode, Json<CompanionAccepted>)> {
    same_origin_json(&headers)?;
    check_id(&id)?;
    check_field(&field)?;
    api(handle)?.regenerate(id.clone(), field).await?;
    Ok((StatusCode::ACCEPTED, Json(CompanionAccepted { id })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refusals_map_to_stable_codes() {
        let cases = [
            (CompanionsError::Unknown, 404, "unknown_companion"),
            (CompanionsError::NoCredential, 404, "no_credential"),
            (CompanionsError::Mismatch, 409, "confirmation_mismatch"),
            (CompanionsError::Refused("not_wired"), 409, "not_wired"),
            (CompanionsError::Refused("busy"), 409, "busy"),
        ];
        for (e, status, code) in cases {
            let e = ApiError::from(e);
            assert_eq!((e.status().as_u16(), e.code()), (status, code));
        }
        let e = ApiError::from(CompanionsError::BadInput {
            field: Some("config_dir"),
            code: "bad_template_input",
        });
        assert_eq!((e.status().as_u16(), e.code()), (400, "bad_template_input"));
        assert_eq!(e.body()["error"]["field"], "config_dir");
        let e = ApiError::from(CompanionsError::BadInput {
            field: None,
            code: "interpolation",
        });
        assert!(e.body()["error"].get("field").is_none());
        let e = ApiError::from(CompanionsError::Internal("store"));
        assert_eq!(e.status().as_u16(), 500);
        assert!(!e.body().to_string().contains("store"));
    }

    #[test]
    fn ids_and_fields_are_shape_checked() {
        assert!(check_id("qbittorrent").is_ok());
        for bad in ["", "Q", "../x", "a/b", "a.b", &"a".repeat(33)] {
            assert_eq!(check_id(bad), Err(CompanionsError::Unknown), "{bad:?}");
        }
        assert!(check_field("webui_password").is_ok());
        for bad in ["", "API", "a-b", "a/b"] {
            assert_eq!(check_field(bad), Err(CompanionsError::NoCredential));
        }
    }
}
