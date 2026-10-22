//! The settings API (architecture §10.10, §11): the runtime settings
//! registry's sections, their effective values, and changes to them.
//!
//! The registry itself lives in the server binary (`lodestar::settings`),
//! which this crate can't depend on, so it's reached through the
//! [`SettingsApi`] trait. The registry owns layering (defaults <
//! `config.toml` < saved < environment), validation, persistence and
//! live updates; this module owns the routes, the session guard, the
//! same-origin check and the error envelope.
//!
//! Filtering state (rules, the floor, packs, subscriptions) is the
//! filters API's alone (§20.7) and never passes through here, and
//! neither do secrets (§18.10).

use std::fmt;
use std::sync::Arc;

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::routing::get;
use axum::{Json, Router};
use utoipa::OpenApi;

use crate::ApiState;
use crate::auth;
pub use crate::dto::settings::{
    SectionValues, SettingField, SettingSource, SettingValue, SettingsIndex, SettingsPatch,
    SettingsSection,
};
use crate::error::{ApiError, ApiResult, internal};
use crate::extract::{Json as JsonBody, Path, same_origin_json};

/// This module's half of `/openapi.json`.
#[derive(OpenApi)]
#[openapi(paths(index, section, patch))]
pub(crate) struct ApiDoc;

/// What the settings routes ask of the registry. Calls may block on the
/// database; the handlers make them off the async runtime.
pub trait SettingsApi: Send + Sync + fmt::Debug {
    /// Every section with its fields and schema.
    fn index(&self) -> SettingsIndex;
    /// One section's effective values; `None` for an unknown section.
    fn section(&self, section: &str) -> Option<SectionValues>;
    /// Validates every field of `patch`, saves them in one transaction,
    /// applies the live ones and returns the section's new values. A
    /// refused patch changes nothing.
    fn patch(&self, section: &str, patch: SettingsPatch) -> Result<SectionValues, SettingsError>;
}

/// Why a patch was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SettingsError {
    UnknownSection,
    /// The section has no such field.
    UnknownField(String),
    /// The field is never editable in the app.
    ReadOnly(String),
    /// An environment variable sets the field, and it wins.
    SetByEnv {
        field: String,
        var: String,
    },
    /// The value has the wrong type or is out of bounds.
    Invalid {
        field: String,
        message: String,
    },
    /// The patch needs a confirmation it didn't carry (turning on "Show
    /// adult content" needs `confirm_adult: true`, §20.9); `code` names
    /// which, so the UI can ask for it.
    NeedsConfirmation {
        field: String,
        code: &'static str,
        message: String,
    },
    /// Saving failed (logged, never shown).
    Store(String),
}

impl From<SettingsError> for ApiError {
    fn from(e: SettingsError) -> ApiError {
        match e {
            SettingsError::UnknownSection => unknown_section(),
            SettingsError::UnknownField(field) => ApiError::bad_request(
                "unknown_field",
                format!("This section has no setting {field:?}."),
            )
            .field(field),
            SettingsError::ReadOnly(field) => ApiError::new(
                StatusCode::CONFLICT,
                "read_only",
                "This setting can't be changed in the app.",
            )
            .field(field),
            SettingsError::SetByEnv { field, var } => ApiError::new(
                StatusCode::CONFLICT,
                "set_by_environment",
                format!(
                    "The environment variable {var} sets this, and it takes precedence. \
                     Change or remove {var} and restart."
                ),
            )
            .field(field),
            SettingsError::Invalid { field, message } => {
                ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, "invalid_setting", message)
                    .field(field)
            }
            SettingsError::NeedsConfirmation {
                field,
                code,
                message,
            } => ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, code, message).field(field),
            SettingsError::Store(e) => internal(format!("settings: {e}")),
        }
    }
}

fn unknown_section() -> ApiError {
    ApiError::new(
        StatusCode::NOT_FOUND,
        "unknown_section",
        "There's no such settings section.",
    )
}

/// The settings routes: all behind a session.
pub fn routes() -> Router<ApiState> {
    auth::protected(
        Router::new()
            .route("/settings", get(index))
            .route("/settings/{section}", get(section).patch(patch)),
    )
}

fn settings(s: &ApiState) -> ApiResult<Arc<dyn SettingsApi>> {
    s.settings.clone().ok_or_else(|| {
        ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "settings_unavailable",
            "Settings aren't available.",
        )
    })
}

/// Every settings section, with the fields and JSON schema its form is
/// generated from.
#[utoipa::path(
    operation_id = "listSettings",
    get,
    path = "/settings",
    tag = "settings",
    responses((status = 200, body = SettingsIndex))
)]
pub(crate) async fn index(State(s): State<ApiState>) -> ApiResult<Json<SettingsIndex>> {
    let api = settings(&s)?;
    Ok(Json(api.index()))
}

/// One section's effective values, where each comes from, and which
/// can't be changed here or wait for a restart.
#[utoipa::path(
    operation_id = "getSettings",
    get,
    path = "/settings/{section}",
    tag = "settings",
    params(("section" = String, Path, description = "The section, as `GET /settings` names it.")),
    responses((status = 200, body = SectionValues))
)]
pub(crate) async fn section(
    State(s): State<ApiState>,
    Path(name): Path<String>,
) -> ApiResult<Json<SectionValues>> {
    let api = settings(&s)?;
    tokio::task::spawn_blocking(move || api.section(&name).ok_or_else(unknown_section))
        .await
        .map_err(internal)?
        .map(Json)
}

/// Changes some of a section's settings. Every field is checked first,
/// then all are saved together; live ones apply at once, and the rest
/// are listed in `restart_required`. A field an environment variable
/// sets is refused, naming the variable.
#[utoipa::path(
    operation_id = "patchSettings",
    patch,
    path = "/settings/{section}",
    tag = "settings",
    params(("section" = String, Path, description = "The section, as `GET /settings` names it.")),
    request_body = SettingsPatch,
    responses((status = 200, body = SectionValues))
)]
pub(crate) async fn patch(
    State(s): State<ApiState>,
    headers: HeaderMap,
    Path(name): Path<String>,
    JsonBody(body): JsonBody<SettingsPatch>,
) -> ApiResult<Json<SectionValues>> {
    same_origin_json(&headers)?;
    let api = settings(&s)?;
    tokio::task::spawn_blocking(move || api.patch(&name, body))
        .await
        .map_err(internal)?
        .map(Json)
        .map_err(ApiError::from)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn refusals_map_to_the_envelope() {
        let e = ApiError::from(SettingsError::SetByEnv {
            field: "intensity".into(),
            var: "LODESTAR_INTENSITY".into(),
        });
        assert_eq!(e.status(), StatusCode::CONFLICT);
        assert_eq!(e.body()["error"]["field"], json!("intensity"));
        assert!(
            e.body()["error"]["message"]
                .as_str()
                .unwrap()
                .contains("LODESTAR_INTENSITY")
        );
        let e = ApiError::from(SettingsError::UnknownField("x".into()));
        assert_eq!(e.status(), StatusCode::BAD_REQUEST);
        assert_eq!(e.code(), "unknown_field");
        let e = ApiError::from(SettingsError::UnknownSection);
        assert_eq!(e.status(), StatusCode::NOT_FOUND);
        let e = ApiError::from(SettingsError::Invalid {
            field: "timeout_secs".into(),
            message: "Must be at most 120.".into(),
        });
        assert_eq!(e.status(), StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(e.body()["error"]["field"], json!("timeout_secs"));
        let e = ApiError::from(SettingsError::Store("disk /secret".into()));
        assert_eq!(e.status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert!(!e.body().to_string().contains("secret"));
    }
}
