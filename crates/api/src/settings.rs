//! The settings API (architecture §10.10, §11): the runtime settings
//! registry's sections, their effective values, and changes to them.
//!
//! The registry itself lives in the server binary (`lodestar::settings`),
//! which this crate can't depend on, so it's reached through the
//! [`SettingsApi`] trait. The registry owns layering (defaults <
//! `config.toml` < saved < environment), validation, revisions,
//! persistence, live updates, export and import; this module owns the
//! routes, the session guard, the same-origin check, the error envelope,
//! the `settings` events on the `/events` hub and the connection test.
//!
//! - `PATCH /settings/{section}` and `PATCH /settings` (several sections)
//!   save all or nothing; `POST /settings/preview` runs the same checks
//!   and saves nothing.
//! - Every save that changes a section publishes `settings` (`{section,
//!   revision}`, no values) on the `/events` hub.
//! - `POST /settings/test` runs a download client's connection test on
//!   unsaved values, through the integrations SSRF guard.
//! - `GET /settings/export` and `POST /settings/import` move saved
//!   settings between installs as TOML.
//!
//! Filtering state (rules, the floor, packs, subscriptions) is the
//! filters API's alone (§20.7) and never passes through here. Secret
//! fields (`writeOnly`) are never returned, exported or echoed in an
//! error.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::{Extension, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use lodestar_integrations::clients::BasicAuth;
use lodestar_integrations::{AuthFailure, ClientConfig, ClientError, Secret};
use utoipa::OpenApi;

use crate::ApiState;
use crate::auth;
use crate::dto::error::ErrorEnvelope;
pub use crate::dto::settings::{
    DownloadClientDraft, DownloadClientKind, ExportQuery, FieldPreview, ImportChange, ImportQuery,
    ImportReport, ImportSkip, PreviewEstimate, PreviewWarning, SectionPreview, SectionValues,
    SettingField, SettingSource, SettingValue, SettingsBatch, SettingsBatchValues,
    SettingsErrorBody, SettingsErrorEnvelope, SettingsImport, SettingsIndex, SettingsPatch,
    SettingsPreview, SettingsSection, SettingsTest, SettingsTestFailure, SettingsTestResult,
};
use crate::error::{ApiError, ApiResult, internal};
use crate::events::EventHub;
use crate::extract::{Json as JsonBody, Path, Query, same_origin_json};

/// This module's half of `/openapi.json`.
#[derive(OpenApi)]
#[openapi(
    paths(index, section, patch, patch_many, preview, test, export, import),
    components(schemas(SettingsErrorEnvelope))
)]
pub(crate) struct ApiDoc;

/// The patch key, in any section, that names the revision an edit
/// started from. It's never a field.
pub const IF_REVISION: &str = "if_revision";

/// The largest file `POST /settings/import` reads.
pub const MAX_IMPORT: usize = 64 * 1024;

/// What the settings routes ask of the registry. Calls may block on the
/// database; the handlers make them off the async runtime.
pub trait SettingsApi: Send + Sync + fmt::Debug {
    /// Every section with its fields and schema.
    fn index(&self) -> SettingsIndex;
    /// One section's effective values; `None` for an unknown section.
    fn section(&self, section: &str) -> Option<SectionValues>;
    /// Checks every section's patch (revision, fields, confirmations),
    /// then saves them all in one transaction under one lock, applies the
    /// live fields and publishes each changed section's `watch` channel. A
    /// refused batch changes nothing.
    fn save(&self, batch: SettingsBatch) -> Result<Saved, SettingsFailure>;
    /// [`SettingsApi::save`]'s checks, exactly, and what the save would do;
    /// saves nothing.
    fn preview(&self, batch: SettingsBatch) -> Result<SettingsPreview, SettingsFailure>;
    /// The saved settings as a TOML file: no secrets, nothing an
    /// environment variable sets, `preferences` only when asked.
    fn export(&self, preferences: bool) -> String;
    /// Reads an exported file and reports the diff; unless `dry_run`,
    /// saves it as one batch (all or nothing, through
    /// [`SettingsApi::save`]'s checks).
    fn import(&self, import: SettingsImport, dry_run: bool) -> Result<Imported, SettingsFailure>;

    /// One section's patch: [`SettingsApi::save`] with it alone.
    fn patch(&self, section: &str, patch: SettingsPatch) -> Result<SectionValues, SettingsError> {
        let batch = SettingsBatch(BTreeMap::from([(section.to_string(), patch)]));
        let mut saved = self.save(batch).map_err(|f| f.error)?;
        saved.sections.pop().ok_or(SettingsError::UnknownSection)
    }
}

/// What a save did.
#[derive(Debug, Clone, PartialEq)]
pub struct Saved {
    /// Each patched section's values now, in display order.
    pub sections: Vec<SectionValues>,
    /// The sections it changed, with their new revisions.
    pub changed: Vec<(String, u64)>,
}

/// What an import did.
#[derive(Debug, Clone, PartialEq)]
pub struct Imported {
    pub report: ImportReport,
    /// The sections it changed, with their new revisions (none on a dry
    /// run).
    pub changed: Vec<(String, u64)>,
}

/// Why a batch was refused, and in which section.
#[derive(Debug, Clone, PartialEq)]
pub struct SettingsFailure {
    pub section: Option<String>,
    pub error: SettingsError,
}

impl SettingsFailure {
    pub fn new(section: impl Into<String>, error: SettingsError) -> SettingsFailure {
        SettingsFailure {
            section: Some(section.into()),
            error,
        }
    }
}

/// Why a patch was refused.
#[derive(Debug, Clone, PartialEq)]
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
    /// `if_revision` is older than the section: it changed since the edit
    /// began. Holds every stale section's values now.
    Stale(Vec<SectionValues>),
    /// The import file isn't one (not TOML, another format).
    InvalidImport(String),
    /// The import file is over [`MAX_IMPORT`] bytes.
    ImportTooLarge,
    /// Saving failed (logged, never shown).
    Store(String),
}

impl SettingsError {
    fn parts(&self) -> (StatusCode, &'static str, String, Option<String>) {
        let conflict = StatusCode::CONFLICT;
        let unprocessable = StatusCode::UNPROCESSABLE_ENTITY;
        match self {
            SettingsError::UnknownSection => (
                StatusCode::NOT_FOUND,
                "unknown_section",
                "There's no such settings section.".into(),
                None,
            ),
            SettingsError::UnknownField(field) => (
                StatusCode::BAD_REQUEST,
                "unknown_field",
                format!("This section has no setting {field:?}."),
                Some(field.clone()),
            ),
            SettingsError::ReadOnly(field) => (
                conflict,
                "read_only",
                "This setting can't be changed in the app.".into(),
                Some(field.clone()),
            ),
            SettingsError::SetByEnv { field, var } => (
                conflict,
                "set_by_environment",
                format!(
                    "The environment variable {var} sets this, and it takes precedence. \
                     Change or remove {var} and restart."
                ),
                Some(field.clone()),
            ),
            SettingsError::Invalid { field, message } => (
                unprocessable,
                "invalid_setting",
                message.clone(),
                Some(field.clone()),
            ),
            SettingsError::NeedsConfirmation {
                field,
                code,
                message,
            } => (unprocessable, code, message.clone(), Some(field.clone())),
            SettingsError::Stale(_) => (
                conflict,
                "stale_revision",
                "These settings were changed elsewhere since you started editing. \
                 Review the current values and save again."
                    .into(),
                None,
            ),
            SettingsError::InvalidImport(message) => {
                (unprocessable, "invalid_import", message.clone(), None)
            }
            SettingsError::ImportTooLarge => (
                StatusCode::PAYLOAD_TOO_LARGE,
                "import_too_large",
                "The file is larger than 64 KiB; it isn't a settings export.".into(),
                Some("toml".into()),
            ),
            SettingsError::Store(_) => unreachable!("handled apart"),
        }
    }
}

/// For other routes that save through the registry (`/crawler`): the
/// API's envelope, without the settings envelope's `section` or
/// `current`.
impl From<SettingsError> for ApiError {
    fn from(e: SettingsError) -> ApiError {
        if let SettingsError::Store(e) = &e {
            return internal(format!("settings: {e}"));
        }
        let (status, code, message, field) = e.parts();
        let err = ApiError::new(status, code, message);
        match field {
            Some(field) => err.field(field),
            None => err,
        }
    }
}

impl From<SettingsError> for SettingsFailure {
    fn from(error: SettingsError) -> SettingsFailure {
        SettingsFailure {
            section: None,
            error,
        }
    }
}

impl IntoResponse for SettingsFailure {
    fn into_response(self) -> Response {
        if let SettingsError::Store(e) = &self.error {
            return internal(format!("settings: {e}")).into_response();
        }
        let (status, code, message, field) = self.error.parts();
        let current = match self.error {
            SettingsError::Stale(sections) => Some(
                sections
                    .into_iter()
                    .map(|s| (s.section.clone(), s))
                    .collect(),
            ),
            _ => None,
        };
        let body = SettingsErrorEnvelope {
            current,
            error: SettingsErrorBody {
                code: code.to_string(),
                field,
                message,
                section: self.section,
            },
        };
        (status, Json(body)).into_response()
    }
}

/// The settings routes: all behind a session.
pub fn routes() -> Router<ApiState> {
    auth::protected(
        Router::new()
            .route("/settings", get(index).patch(patch_many))
            .route("/settings/preview", post(preview))
            .route("/settings/test", post(test))
            .route("/settings/export", get(export))
            .route("/settings/import", post(import))
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

/// Runs `f` off the async runtime: registry calls may wait on the
/// database.
async fn blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> ApiResult<T> {
    tokio::task::spawn_blocking(f).await.map_err(internal)
}

/// Tells `/events` subscribers which sections a save changed. It runs
/// inside the blocking save, right after the commit, so a client that
/// goes away mid-save can't lose the event: the blocking task finishes
/// either way.
fn announce(hub: Option<&EventHub>, changed: &[(String, u64)]) {
    if let Some(hub) = hub {
        for (section, revision) in changed {
            hub.settings_saved(section, *revision);
        }
    }
}

/// [`SettingsApi::save`], then [`announce`], in one blocking step.
pub(crate) fn save_and_announce(
    api: &dyn SettingsApi,
    hub: Option<&EventHub>,
    batch: SettingsBatch,
) -> Result<Saved, SettingsFailure> {
    let saved = api.save(batch)?;
    announce(hub, &saved.changed);
    Ok(saved)
}

/// Why a settings route refused: the API's envelope (a session, a body,
/// the same-origin check), or the settings envelope with its section.
#[derive(Debug)]
pub(crate) enum Refused {
    Api(ApiError),
    Settings(Box<SettingsFailure>),
}

impl From<ApiError> for Refused {
    fn from(e: ApiError) -> Refused {
        Refused::Api(e)
    }
}

impl From<SettingsFailure> for Refused {
    fn from(e: SettingsFailure) -> Refused {
        Refused::Settings(Box::new(e))
    }
}

impl From<SettingsError> for Refused {
    fn from(e: SettingsError) -> Refused {
        SettingsFailure::from(e).into()
    }
}

impl IntoResponse for Refused {
    fn into_response(self) -> Response {
        match self {
            Refused::Api(e) => e.into_response(),
            Refused::Settings(e) => e.into_response(),
        }
    }
}

/// A settings response: a value, or the API's or the settings envelope.
type SettingsResult<T> = Result<T, Refused>;

/// Every settings section, with the fields and JSON schema its form is
/// generated from.
///
/// Each property of a section's schema carries hints for the form (the
/// server checks everything regardless): `x-group` (the group's id; its
/// heading is the message `settings.group.<id>`), `x-order`, `x-advanced`,
/// `x-preview` (`POST /settings/preview` has estimates or warnings for
/// it) and, where they apply, `x-unit` (`seconds`, `mebibytes`,
/// `connections`), `x-widget` (`radio`, `slider`, `theme`, `shortcuts`,
/// `language`, `time-zone`), `x-auto` (the value that means "automatic"),
/// `x-keywords` (a message key of search synonyms), `x-danger`
/// (`disruptive`, `privacy`, `destructive`, `security`, §10.10.7),
/// `x-config` (the `config.toml` key), `x-env` (the `LODESTAR_*`
/// variable), `format` (`socket-addr`, `log-filter`) and `writeOnly`
/// (a secret, never sent back).
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

/// One section's effective values, where each comes from, which can't be
/// changed here or wait for a restart, and its revision.
#[utoipa::path(
    operation_id = "getSettings",
    get,
    path = "/settings/{section}",
    tag = "settings",
    params(("section" = String, Path, description = "The section, as `GET /settings` names it.")),
    responses(
        (status = 200, body = SectionValues),
        (status = 404, body = SettingsErrorEnvelope, description = "`unknown_section`."),
    )
)]
pub(crate) async fn section(
    State(s): State<ApiState>,
    Path(name): Path<String>,
) -> SettingsResult<Json<SectionValues>> {
    let api = settings(&s)?;
    let values = blocking(move || api.section(&name)).await?;
    Ok(Json(values.ok_or(SettingsError::UnknownSection)?))
}

/// Changes some of a section's settings. Every field is checked first,
/// then all are saved together; live ones apply at once, and the rest
/// are listed in `restart_required`. A field an environment variable
/// sets is refused, naming the variable. With `if_revision`, a section
/// changed since is refused with its current values.
#[utoipa::path(
    operation_id = "patchSettings",
    patch,
    path = "/settings/{section}",
    tag = "settings",
    params(("section" = String, Path, description = "The section, as `GET /settings` names it.")),
    request_body = SettingsPatch,
    responses(
        (status = 200, body = SectionValues),
        (status = 400, body = SettingsErrorEnvelope, description = "`unknown_field`."),
        (status = 404, body = SettingsErrorEnvelope, description = "`unknown_section`."),
        (status = 409, body = SettingsErrorEnvelope, description = "`read_only`, `set_by_environment`, or `stale_revision` with the section's `current` values."),
        (status = 422, body = SettingsErrorEnvelope, description = "`invalid_setting`; `adult_confirmation_required`: turning on an adult setting without `\"confirm_adult\": true`. Nothing is saved."),
    )
)]
pub(crate) async fn patch(
    State(s): State<ApiState>,
    hub: Option<Extension<Arc<EventHub>>>,
    headers: HeaderMap,
    Path(name): Path<String>,
    JsonBody(body): JsonBody<SettingsPatch>,
) -> SettingsResult<Json<SectionValues>> {
    same_origin_json(&headers)?;
    let api = settings(&s)?;
    let batch = SettingsBatch(BTreeMap::from([(name, body)]));
    let hub = hub.map(|Extension(h)| h);
    let mut saved =
        blocking(move || save_and_announce(api.as_ref(), hub.as_deref(), batch)).await??;
    let values = saved
        .sections
        .pop()
        .ok_or_else(|| internal("settings: a saved section is missing from the result"))?;
    Ok(Json(values))
}

/// Changes several sections at once (`{section: {field: value}}`), as a
/// screen showing fields from several sections saves them: every
/// section's patch is checked as `PATCH /settings/{section}` checks it,
/// then all are saved in one transaction. One refused field saves
/// nothing; the error names its `section` and `field`.
#[utoipa::path(
    operation_id = "patchSettingsBatch",
    patch,
    path = "/settings",
    tag = "settings",
    request_body = SettingsBatch,
    responses(
        (status = 200, body = SettingsBatchValues),
        (status = 400, body = SettingsErrorEnvelope, description = "`unknown_field`."),
        (status = 404, body = SettingsErrorEnvelope, description = "`unknown_section`."),
        (status = 409, body = SettingsErrorEnvelope, description = "`read_only`, `set_by_environment`, or `stale_revision` with each stale section's `current` values."),
        (status = 422, body = SettingsErrorEnvelope, description = "`invalid_setting`, `adult_confirmation_required`. Nothing is saved."),
    )
)]
pub(crate) async fn patch_many(
    State(s): State<ApiState>,
    hub: Option<Extension<Arc<EventHub>>>,
    headers: HeaderMap,
    JsonBody(body): JsonBody<SettingsBatch>,
) -> SettingsResult<Json<SettingsBatchValues>> {
    same_origin_json(&headers)?;
    let api = settings(&s)?;
    let hub = hub.map(|Extension(h)| h);
    let saved = blocking(move || save_and_announce(api.as_ref(), hub.as_deref(), body)).await??;
    Ok(Json(SettingsBatchValues {
        sections: saved
            .sections
            .into_iter()
            .map(|v| (v.section.clone(), v))
            .collect(),
    }))
}

/// What `PATCH /settings` with the same body would do, without saving:
/// the same checks and refusals, then for each patched field whether it
/// applies now or after a restart, warnings, and estimates where the
/// section can make them (fetch sockets, the new address, memory).
#[utoipa::path(
    operation_id = "previewSettings",
    post,
    path = "/settings/preview",
    tag = "settings",
    request_body = SettingsBatch,
    responses(
        (status = 200, body = SettingsPreview),
        (status = 400, body = SettingsErrorEnvelope, description = "As `PATCH /settings`."),
        (status = 404, body = SettingsErrorEnvelope, description = "As `PATCH /settings`."),
        (status = 409, body = SettingsErrorEnvelope, description = "As `PATCH /settings`."),
        (status = 422, body = SettingsErrorEnvelope, description = "As `PATCH /settings`."),
    )
)]
pub(crate) async fn preview(
    State(s): State<ApiState>,
    headers: HeaderMap,
    JsonBody(body): JsonBody<SettingsBatch>,
) -> SettingsResult<Json<SettingsPreview>> {
    same_origin_json(&headers)?;
    let api = settings(&s)?;
    Ok(Json(blocking(move || api.preview(body)).await??))
}

/// The longest a connection test may take, its requests included.
const TEST_TIMEOUT: Duration = Duration::from_secs(30);

/// The longest URL, user name and password a test takes, in characters.
const MAX_TEST_FIELD: usize = 2048;

/// Connection tests running at once. Each can hold sockets for
/// [`TEST_TIMEOUT`]; past this, a test is refused with `429 tests_busy`.
pub const MAX_TESTS: usize = 4;

static TESTS: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(MAX_TESTS);

/// Tests a download client with the values on screen, before they're
/// saved: logs in and asks its version. It goes through the integrations
/// SSRF guard (download clients may be on this machine or the LAN, never
/// on link-local, metadata or reserved addresses). Nothing is saved, and
/// the result never repeats the URL or a credential.
#[utoipa::path(
    operation_id = "testSettings",
    post,
    path = "/settings/test",
    tag = "settings",
    request_body = SettingsTest,
    responses(
        (status = 200, body = SettingsTestResult, description = "The test ran; `ok` says whether it passed."),
        (status = 403, body = ErrorEnvelope, description = "`cross_origin`."),
        (status = 415, body = ErrorEnvelope, description = "`unsupported_media_type`: not `application/json`."),
        (status = 422, body = ErrorEnvelope, description = "`bad_body`: not a test this endpoint runs."),
        (status = 429, body = ErrorEnvelope, description = "`tests_busy`: four tests are running already; try again shortly."),
    )
)]
pub(crate) async fn test(
    headers: HeaderMap,
    JsonBody(body): JsonBody<SettingsTest>,
) -> ApiResult<Json<SettingsTestResult>> {
    same_origin_json(&headers)?;
    let SettingsTest::DownloadClient(draft) = body;
    let too_long = [
        Some(&draft.url),
        draft.username.as_ref(),
        draft.password.as_ref(),
    ]
    .into_iter()
    .flatten()
    .any(|v| v.chars().count() > MAX_TEST_FIELD);
    if too_long {
        return Ok(Json(failed(
            "invalid",
            format!(
                "The address, user name and password must each be at most {MAX_TEST_FIELD} characters."
            ),
        )));
    }
    // Never waits: a full house is answered at once.
    let _permit = TESTS.try_acquire().map_err(|_| {
        ApiError::new(
            StatusCode::TOO_MANY_REQUESTS,
            "tests_busy",
            "Other connection tests are still running; try again in a few seconds.",
        )
    })?;
    let run = async {
        let client = lodestar_integrations::clients::connect(client_config(draft))?;
        client.test().await
    };
    let result = match tokio::time::timeout(TEST_TIMEOUT, run).await {
        Ok(Ok(info)) => SettingsTestResult {
            api_version: info.api_version,
            error: None,
            ok: true,
            version: Some(info.version),
        },
        Ok(Err(e)) => test_failure(&e),
        Err(_) => test_failure(&ClientError::Timeout),
    };
    Ok(Json(result))
}

/// The integrations crate's configuration for a draft.
fn client_config(d: DownloadClientDraft) -> ClientConfig {
    let password = d.password.map(Secret::new);
    let auth = (d.username.is_some() || password.is_some()).then(|| BasicAuth {
        username: d.username.clone().unwrap_or_default(),
        password: password.clone().unwrap_or_else(|| Secret::new("")),
    });
    match d.client {
        DownloadClientKind::Qbittorrent => ClientConfig::QBittorrent {
            url: d.url,
            username: d.username.unwrap_or_default(),
            password: password.unwrap_or_else(|| Secret::new("")),
        },
        DownloadClientKind::Transmission => ClientConfig::Transmission { url: d.url, auth },
        DownloadClientKind::Deluge => ClientConfig::Deluge {
            url: d.url,
            password: password.unwrap_or_else(|| Secret::new("")),
        },
        DownloadClientKind::Rtorrent => ClientConfig::RTorrent { url: d.url, auth },
        DownloadClientKind::Aria2 => ClientConfig::Aria2 {
            url: d.url,
            secret: password,
        },
    }
}

fn failed(code: &str, message: String) -> SettingsTestResult {
    SettingsTestResult {
        api_version: None,
        error: Some(SettingsTestFailure {
            code: code.to_string(),
            message,
        }),
        ok: false,
        version: None,
    }
}

/// A failed test, in words. Nothing the server answered is repeated:
/// the address may be any LAN or loopback service, and the test must not
/// become a way to read it. A refusal shows at most its HTTP status. The
/// other variants' text is the adapter's own (`&'static str` reasons) and
/// never carries a URL or credential (`crates/integrations/README.md`,
/// rule 3).
fn test_failure(e: &ClientError) -> SettingsTestResult {
    let code = match e {
        ClientError::Auth(AuthFailure::BadCredentials) => "bad_credentials",
        ClientError::Auth(AuthFailure::Banned) => "banned",
        ClientError::Auth(AuthFailure::OriginRefused) => "origin_refused",
        ClientError::Auth(AuthFailure::SessionRefused) => "session_refused",
        ClientError::Auth(AuthFailure::AddressRefused) => "address_refused",
        ClientError::Unreachable => "unreachable",
        ClientError::Rejected(_) => "rejected",
        ClientError::Unsupported(_) => "unsupported",
        ClientError::Timeout => "timeout",
        ClientError::Blocked(_) => "blocked",
        ClientError::InvalidRequest(_) => "invalid",
        ClientError::Protocol(_) | ClientError::Partial { .. } => "protocol",
    };
    let message = match e {
        ClientError::Rejected(why) => match http_status(why) {
            Some(status) => {
                format!("The address answered HTTP {status}: it isn't this client's API.")
            }
            None => "The client refused the request.".to_string(),
        },
        ClientError::Partial { .. } => {
            "The client gave an answer Lodestar doesn't understand.".to_string()
        }
        _ => {
            let text = e.to_string();
            let mut chars = text.chars();
            match chars.next() {
                Some(first) => format!("{}{}.", first.to_uppercase(), chars.as_str()),
                None => "The test failed.".to_string(),
            }
        }
    };
    failed(code, message)
}

/// The status of an adapter's "HTTP 404: <body>" refusal, the body
/// dropped.
fn http_status(why: &str) -> Option<u16> {
    let digits: String = why
        .strip_prefix("HTTP ")?
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    digits.parse().ok().filter(|s| (100..1000).contains(s))
}

/// The saved settings as a TOML file to keep or move to another install,
/// laid out by section (`[crawler]`), each key with its `config.toml`
/// key and environment variable in a comment. It holds only values saved
/// in the app: never a secret, a value an environment variable sets, or
/// filter, floor, pack or subscription state (§20 has its own export).
/// `preferences` is included only when asked.
#[utoipa::path(
    operation_id = "exportSettings",
    get,
    path = "/settings/export",
    tag = "settings",
    params(ExportQuery),
    responses((
        status = 200,
        description = "The file, as an attachment (`lodestar-settings.toml`).",
        content_type = "application/toml",
        body = String,
    ))
)]
pub(crate) async fn export(
    State(s): State<ApiState>,
    Query(q): Query<ExportQuery>,
) -> ApiResult<Response> {
    let api = settings(&s)?;
    let text = blocking(move || api.export(q.preferences.unwrap_or(false))).await?;
    Ok((
        [
            (header::CONTENT_TYPE, "application/toml; charset=utf-8"),
            (
                header::CONTENT_DISPOSITION,
                "attachment; filename=\"lodestar-settings.toml\"",
            ),
            (header::CACHE_CONTROL, "no-store"),
        ],
        text,
    )
        .into_response())
}

/// Reads a file from `GET /settings/export` and reports what it would
/// change. Keys that aren't settings, read-only ones, secrets and ones an
/// environment variable sets are listed in `skipped` and left alone.
/// Unless `dry_run=true`, the changes are then saved as one batch with
/// `PATCH /settings`' checks: all or nothing.
#[utoipa::path(
    operation_id = "importSettings",
    post,
    path = "/settings/import",
    tag = "settings",
    params(ImportQuery),
    request_body = SettingsImport,
    responses(
        (status = 200, body = ImportReport),
        (status = 409, body = SettingsErrorEnvelope, description = "`stale_revision`: a section changed since the dry run's `revisions`."),
        (status = 413, body = SettingsErrorEnvelope, description = "`import_too_large`: over 64 KiB."),
        (status = 422, body = SettingsErrorEnvelope, description = "`invalid_import` (not an export file), or a value `PATCH /settings` refuses (`invalid_setting`, `adult_confirmation_required`). Nothing is saved."),
    )
)]
pub(crate) async fn import(
    State(s): State<ApiState>,
    hub: Option<Extension<Arc<EventHub>>>,
    headers: HeaderMap,
    Query(q): Query<ImportQuery>,
    JsonBody(body): JsonBody<SettingsImport>,
) -> SettingsResult<Json<ImportReport>> {
    same_origin_json(&headers)?;
    let api = settings(&s)?;
    if body.toml.len() > MAX_IMPORT {
        return Err(SettingsError::ImportTooLarge.into());
    }
    let dry_run = q.dry_run.unwrap_or(false);
    let hub = hub.map(|Extension(h)| h);
    let imported = blocking(move || {
        let imported = api.import(body, dry_run)?;
        announce(hub.as_deref(), &imported.changed);
        Ok::<_, SettingsFailure>(imported)
    })
    .await??;
    Ok(Json(imported.report))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    async fn body(r: Response) -> (StatusCode, Value) {
        let status = r.status();
        let bytes = axum::body::to_bytes(r.into_body(), 1 << 20).await.unwrap();
        (status, serde_json::from_slice(&bytes).unwrap())
    }

    #[tokio::test]
    async fn refusals_map_to_the_envelope() {
        let (status, v) = body(
            SettingsFailure::new(
                "network",
                SettingsError::SetByEnv {
                    field: "dht_port".into(),
                    var: "LODESTAR_DHT_PORT".into(),
                },
            )
            .into_response(),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(v["error"]["field"], json!("dht_port"));
        assert_eq!(v["error"]["section"], json!("network"));
        assert!(
            v["error"]["message"]
                .as_str()
                .unwrap()
                .contains("LODESTAR_DHT_PORT")
        );
        let (status, v) = body(
            SettingsFailure::new("crawler", SettingsError::UnknownField("x".into()))
                .into_response(),
        )
        .await;
        assert_eq!(
            (status, &v["error"]["code"]),
            (StatusCode::BAD_REQUEST, &json!("unknown_field"))
        );
        let (status, v) =
            body(SettingsFailure::from(SettingsError::UnknownSection).into_response()).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert!(v["error"].get("section").is_none());
        let (status, v) = body(
            SettingsFailure::new(
                "crawler",
                SettingsError::Invalid {
                    field: "timeout_secs".into(),
                    message: "Must be at most 120.".into(),
                },
            )
            .into_response(),
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(v["error"]["field"], json!("timeout_secs"));
        assert!(v.get("current").is_none());
        let (status, v) = body(
            SettingsFailure::from(SettingsError::Store("disk /secret".into())).into_response(),
        )
        .await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert!(!v.to_string().contains("secret"));
    }

    #[test]
    fn other_routes_get_the_plain_envelope() {
        let e = ApiError::from(SettingsError::SetByEnv {
            field: "intensity".into(),
            var: "LODESTAR_INTENSITY".into(),
        });
        assert_eq!(
            (e.status(), e.code()),
            (StatusCode::CONFLICT, "set_by_environment")
        );
        assert_eq!(e.body()["error"]["field"], serde_json::json!("intensity"));
        let e = ApiError::from(SettingsError::Store("disk /secret".into()));
        assert_eq!(e.status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert!(!e.body().to_string().contains("secret"));
    }

    #[tokio::test]
    async fn a_stale_revision_carries_the_current_values() {
        let current = SectionValues {
            fields: BTreeMap::new(),
            restart_required: vec![],
            revision: 7,
            section: "crawler".into(),
        };
        let (status, v) = body(
            SettingsFailure::new("crawler", SettingsError::Stale(vec![current])).into_response(),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(v["error"]["code"], json!("stale_revision"));
        assert_eq!(v["current"]["crawler"]["revision"], json!(7));
    }

    #[test]
    fn drafts_map_onto_client_configs_and_never_print_secrets() {
        let draft = DownloadClientDraft {
            client: DownloadClientKind::Transmission,
            password: Some("hunter2".into()),
            url: "http://user:pw@host:9091/".into(),
            username: None,
        };
        let shown = format!("{draft:?}");
        assert!(
            !shown.contains("hunter2") && !shown.contains("pw@"),
            "{shown}"
        );
        match client_config(draft) {
            ClientConfig::Transmission { auth: Some(a), .. } => {
                assert_eq!((a.username.as_str(), a.password.expose()), ("", "hunter2"));
            }
            other => panic!("{other:?}"),
        }
        let none = DownloadClientDraft {
            client: DownloadClientKind::Rtorrent,
            password: None,
            url: "http://host/RPC2".into(),
            username: None,
        };
        assert!(matches!(
            client_config(none),
            ClientConfig::RTorrent { auth: None, .. }
        ));
        let aria = DownloadClientDraft {
            client: DownloadClientKind::Aria2,
            password: Some("s".into()),
            url: "http://host:6800/jsonrpc".into(),
            username: None,
        };
        assert!(matches!(
            client_config(aria),
            ClientConfig::Aria2 {
                secret: Some(_),
                ..
            }
        ));
    }

    #[test]
    fn failures_read_as_sentences_with_a_code() {
        let r = test_failure(&ClientError::Auth(AuthFailure::BadCredentials));
        let e = r.error.unwrap();
        assert_eq!(e.code, "bad_credentials");
        assert_eq!(e.message, "The client refused the user name or password.");
        assert!(!r.ok && r.version.is_none());
        assert_eq!(
            test_failure(&ClientError::Timeout).error.unwrap().code,
            "timeout"
        );
        // A refusal never repeats what the server said.
        for (why, want) in [
            (
                "HTTP 404: internal-marker",
                "The address answered HTTP 404: it isn't this client's API.",
            ),
            ("internal-marker", "The client refused the request."),
            ("HTTP x: internal-marker", "The client refused the request."),
        ] {
            let e = test_failure(&ClientError::Rejected(why.into()))
                .error
                .unwrap();
            assert_eq!((e.code.as_str(), e.message.as_str()), ("rejected", want));
        }
        let partial = ClientError::Partial {
            sent: 1,
            cause: Box::new(ClientError::Rejected("internal-marker".into())),
        };
        assert!(
            !test_failure(&partial)
                .error
                .unwrap()
                .message
                .contains("marker")
        );
    }
}
