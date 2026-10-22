//! The process's summary and controls (architecture §9, §10.9, §10.10,
//! §18.8): `GET /stats`, `GET|PATCH /crawler`, `POST /crawler/pause` and
//! `/crawler/resume`, `GET /sources`, `PATCH /sources/{id}`, and
//! `GET|POST /network/check`.
//!
//! - **One source of truth.** The crawler's pause and intensity are the
//!   settings registry's `crawler.paused` and `crawler.intensity`: these
//!   routes change them through [`crate::settings::SettingsApi::patch`],
//!   exactly as `PATCH /settings/crawler` does, then have the running
//!   pipeline apply and record them at once (the binary also follows the
//!   registry, so a change made either way takes effect and is recorded
//!   once).
//! - **Everything else** comes from the running pipeline through
//!   [`OpsApi`], which the server binary implements: this crate can't
//!   depend on it.
//! - **Security:** every route is behind a session; writes are
//!   same-origin JSON. Pause and resume need no password: they're an
//!   operational control, not a filter change.

use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::{Map, Value};
use utoipa::OpenApi;

use crate::ApiState;
use crate::auth;
pub use crate::dto::ops::{
    ConsentClass, CrawlerIntensity, CrawlerPatch, CrawlerState, CrawlerStatus, DhtStatus,
    ExternalAddress, FetchStatus, NetworkCheck, PortMapping, ReachReason, RiskLevel, Source,
    SourceHealth, SourceKind, SourceList, SourcePatch, SourcePriority, SourceState, StatsResponse,
};
use crate::error::{ApiError, ApiResult, internal};
use crate::extract::{Json as JsonBody, Path, same_origin_json};
use crate::settings::{SettingsApi, SettingsPatch};

/// This module's half of `/openapi.json`.
#[derive(OpenApi)]
#[openapi(paths(
    stats,
    crawler,
    patch_crawler,
    pause,
    resume,
    sources,
    patch_source,
    network,
    restart_network
))]
pub(crate) struct ApiDoc;

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// The crawler's runtime state, beside the registry's settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CrawlerRuntime {
    pub since: i64,
    pub dht: Option<DhtStatus>,
    pub fetch: FetchStatus,
}

/// What these routes ask of the running process.
pub trait OpsApi: Send + Sync + fmt::Debug {
    fn crawler(&self) -> CrawlerRuntime;
    /// Pauses or resumes the pipeline now and records it; a no-op when
    /// it already is.
    fn apply_paused(&self, paused: bool) -> BoxFuture<'_, Result<(), OpsError>>;
    /// Records an intensity change (the node follows the registry).
    fn note_intensity(&self, intensity: CrawlerIntensity) -> BoxFuture<'_, Result<(), OpsError>>;
    fn stats(&self) -> BoxFuture<'_, StatsResponse>;
    fn sources(&self) -> BoxFuture<'_, Result<Vec<Source>, OpsError>>;
    /// Applies `patch` (its not-yet-editable fields already refused).
    fn patch_source(&self, id: u8, patch: SourcePatch) -> BoxFuture<'_, Result<Source, OpsError>>;
    fn network(&self) -> NetworkCheck;
    fn restart_network_check(&self) -> Result<NetworkCheck, OpsError>;
}

/// Why an operation was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpsError {
    UnknownSource,
    /// Enabling the source needs consent to its class.
    ConsentRequired(ConsentClass),
    /// The network check was restarted too recently.
    RateLimited {
        retry_after_secs: u64,
    },
    /// Logged, never shown.
    Internal(String),
}

fn class_name(c: ConsentClass) -> &'static str {
    match c {
        ConsentClass::None => "none",
        ConsentClass::External => "external",
        ConsentClass::Account => "account",
        ConsentClass::Restricted => "restricted",
        ConsentClass::Overlay => "overlay",
    }
}

impl From<OpsError> for ApiError {
    fn from(e: OpsError) -> ApiError {
        match e {
            OpsError::UnknownSource => unknown_source(),
            OpsError::ConsentRequired(class) => {
                let name = class_name(class);
                ApiError::new(
                    StatusCode::CONFLICT,
                    "consent_required",
                    format!(
                        "Enabling this source needs your consent: its class is \"{name}\". \
                         Show the consent screen, then send \"consent\": \"{name}\"."
                    ),
                )
                .field("consent")
            }
            OpsError::RateLimited { retry_after_secs } => ApiError::new(
                StatusCode::TOO_MANY_REQUESTS,
                "rate_limited",
                format!("The network check was just restarted. Try again in {retry_after_secs} s."),
            ),
            OpsError::Internal(e) => internal(format!("ops: {e}")),
        }
    }
}

fn unknown_source() -> ApiError {
    ApiError::new(
        StatusCode::NOT_FOUND,
        "unknown_source",
        "There's no such source.",
    )
}

/// The routes: all behind a session.
pub fn routes() -> Router<ApiState> {
    auth::protected(
        Router::new()
            .route("/stats", get(stats))
            .route("/crawler", get(crawler).patch(patch_crawler))
            .route("/crawler/pause", post(pause))
            .route("/crawler/resume", post(resume))
            .route("/sources", get(sources))
            .route("/sources/{id}", axum::routing::patch(patch_source))
            .route("/network/check", get(network).post(restart_network)),
    )
}

fn unavailable() -> ApiError {
    ApiError::new(
        StatusCode::SERVICE_UNAVAILABLE,
        "ops_unavailable",
        "The crawler isn't running in this process.",
    )
}

fn ops(s: &ApiState) -> ApiResult<Arc<dyn OpsApi>> {
    s.ops.clone().ok_or_else(unavailable)
}

fn registry(s: &ApiState) -> ApiResult<Arc<dyn SettingsApi>> {
    s.settings.clone().ok_or_else(unavailable)
}

/// The crawler's status: the registry's pause and intensity, and the
/// pipeline's runtime state.
async fn status(s: &ApiState) -> ApiResult<CrawlerStatus> {
    let (ops, settings) = (ops(s)?, registry(s)?);
    let values = tokio::task::spawn_blocking(move || settings.section("crawler"))
        .await
        .map_err(internal)?
        .ok_or_else(|| internal("settings: no crawler section"))?;
    let field = |name: &str| {
        values
            .fields
            .get(name)
            .ok_or_else(|| internal(format!("settings: no crawler.{name}")))
    };
    let paused = field("paused")?
        .value
        .as_bool()
        .ok_or_else(|| internal("settings: crawler.paused isn't a bool"))?;
    let intensity = field("intensity")?;
    let intensity_env = intensity.env.clone();
    let intensity: CrawlerIntensity =
        serde_json::from_value(intensity.value.clone()).map_err(internal)?;
    let rt = ops.crawler();
    Ok(CrawlerStatus {
        dht: rt.dht,
        fetch: rt.fetch,
        intensity,
        intensity_env,
        paused,
        since: rt.since,
        state: if paused {
            CrawlerState::Paused
        } else {
            CrawlerState::Running
        },
    })
}

/// Saves one `crawler` field through the registry, as
/// `PATCH /settings/crawler` would.
async fn save(s: &ApiState, field: &str, value: Value) -> ApiResult<()> {
    let settings = registry(s)?;
    let mut patch = Map::new();
    patch.insert(field.to_string(), value);
    tokio::task::spawn_blocking(move || settings.patch("crawler", SettingsPatch(patch)))
        .await
        .map_err(internal)?
        .map_err(ApiError::from)?;
    Ok(())
}

/// The global bar's summary: searchable and stored torrents, harvest
/// and fetch rates, queue depth, index lag, disk, uptime. Unmeasurable
/// values are null, never 0.
#[utoipa::path(
    operation_id = "getStats",
    get,
    path = "/stats",
    tag = "crawler",
    responses((status = 200, body = StatsResponse))
)]
pub(crate) async fn stats(State(s): State<ApiState>) -> ApiResult<Json<StatsResponse>> {
    Ok(Json(ops(&s)?.stats().await))
}

/// The crawler: running or paused (and since when), its intensity, the
/// DHT node and the fetch pool.
#[utoipa::path(
    operation_id = "getCrawler",
    get,
    path = "/crawler",
    tag = "crawler",
    responses((status = 200, body = CrawlerStatus))
)]
pub(crate) async fn crawler(State(s): State<ApiState>) -> ApiResult<Json<CrawlerStatus>> {
    status(&s).await.map(Json)
}

/// Sets the crawler's intensity at once: the same setting as
/// `crawler.intensity` in `/settings/crawler`, refused the same way when
/// an environment variable sets it.
#[utoipa::path(
    operation_id = "patchCrawler",
    patch,
    path = "/crawler",
    tag = "crawler",
    request_body = CrawlerPatch,
    responses((status = 200, body = CrawlerStatus))
)]
pub(crate) async fn patch_crawler(
    State(s): State<ApiState>,
    headers: HeaderMap,
    JsonBody(body): JsonBody<CrawlerPatch>,
) -> ApiResult<Json<CrawlerStatus>> {
    same_origin_json(&headers)?;
    let ops = ops(&s)?;
    let value = serde_json::to_value(body.intensity).map_err(internal)?;
    save(&s, "intensity", value).await?;
    ops.note_intensity(body.intensity).await?;
    status(&s).await.map(Json)
}

async fn set_paused(
    s: ApiState,
    headers: HeaderMap,
    paused: bool,
) -> ApiResult<Json<CrawlerStatus>> {
    same_origin_json(&headers)?;
    let ops = ops(&s)?;
    save(&s, "paused", Value::Bool(paused)).await?;
    ops.apply_paused(paused).await?;
    status(&s).await.map(Json)
}

/// Pauses harvesting and metadata fetching. The DHT node keeps answering
/// other nodes, fetches in flight finish, and indexing goes on. The
/// pause survives a restart. Pausing a paused crawler changes nothing.
#[utoipa::path(
    operation_id = "pauseCrawler",
    post,
    path = "/crawler/pause",
    tag = "crawler",
    responses((status = 200, body = CrawlerStatus))
)]
pub(crate) async fn pause(
    State(s): State<ApiState>,
    headers: HeaderMap,
) -> ApiResult<Json<CrawlerStatus>> {
    set_paused(s, headers, true).await
}

/// Resumes harvesting and fetching where they stopped.
#[utoipa::path(
    operation_id = "resumeCrawler",
    post,
    path = "/crawler/resume",
    tag = "crawler",
    responses((status = 200, body = CrawlerStatus))
)]
pub(crate) async fn resume(
    State(s): State<ApiState>,
    headers: HeaderMap,
) -> ApiResult<Json<CrawlerStatus>> {
    set_paused(s, headers, false).await
}

/// Every source plugin of this build: kind, consent class, stored state,
/// priority and health.
#[utoipa::path(
    operation_id = "listSources",
    get,
    path = "/sources",
    tag = "crawler",
    responses((status = 200, body = SourceList))
)]
pub(crate) async fn sources(State(s): State<ApiState>) -> ApiResult<Json<SourceList>> {
    let sources = ops(&s)?.sources().await?;
    Ok(Json(SourceList { sources }))
}

/// Enables or disables a source, or sets its priority. A priority
/// applies at once; enabling or disabling at the next start. Enabling a
/// `restricted` or `overlay` source needs `"consent"` naming its class
/// (`409 consent_required` otherwise). Agents, egress, purge and the
/// schedule arrive in v0.6 (`409 not_editable`).
#[utoipa::path(
    operation_id = "patchSource",
    patch,
    path = "/sources/{id}",
    tag = "crawler",
    params(("id" = u8, Path, description = "The source's id, as `GET /sources` lists it.")),
    request_body = SourcePatch,
    responses((status = 200, body = Source))
)]
pub(crate) async fn patch_source(
    State(s): State<ApiState>,
    headers: HeaderMap,
    Path(id): Path<u8>,
    JsonBody(body): JsonBody<SourcePatch>,
) -> ApiResult<Json<Source>> {
    same_origin_json(&headers)?;
    let ops = ops(&s)?;
    if let Some(field) = body.not_yet_editable() {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "not_editable",
            "This can't be changed yet; it arrives with the sources settings in v0.6.",
        )
        .field(field));
    }
    Ok(Json(ops.patch_source(id, body).await?))
}

/// The network check: our address as other DHT nodes see it, and
/// whether unsolicited inbound queries reach the DHT port.
#[utoipa::path(
    operation_id = "getNetworkCheck",
    get,
    path = "/network/check",
    tag = "crawler",
    responses((status = 200, body = NetworkCheck))
)]
pub(crate) async fn network(State(s): State<ApiState>) -> ApiResult<Json<NetworkCheck>> {
    Ok(Json(ops(&s)?.network()))
}

/// Starts the reachability measurement again (after changing a port
/// forward, say). At most once every 30 s (`429 rate_limited`).
#[utoipa::path(
    operation_id = "restartNetworkCheck",
    post,
    path = "/network/check",
    tag = "crawler",
    responses((status = 200, body = NetworkCheck))
)]
pub(crate) async fn restart_network(
    State(s): State<ApiState>,
    headers: HeaderMap,
) -> ApiResult<Json<NetworkCheck>> {
    same_origin_json(&headers)?;
    Ok(Json(ops(&s)?.restart_network_check()?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn refusals_map_to_the_envelope() {
        let e = ApiError::from(OpsError::ConsentRequired(ConsentClass::Restricted));
        assert_eq!(
            (e.status(), e.code()),
            (StatusCode::CONFLICT, "consent_required")
        );
        assert_eq!(e.body()["error"]["field"], json!("consent"));
        assert!(
            e.body()["error"]["message"]
                .as_str()
                .unwrap()
                .contains("\"restricted\"")
        );
        let e = ApiError::from(OpsError::UnknownSource);
        assert_eq!(
            (e.status(), e.code()),
            (StatusCode::NOT_FOUND, "unknown_source")
        );
        let e = ApiError::from(OpsError::RateLimited {
            retry_after_secs: 12,
        });
        assert_eq!(e.status(), StatusCode::TOO_MANY_REQUESTS);
        assert!(
            e.body()["error"]["message"]
                .as_str()
                .unwrap()
                .contains("12 s")
        );
        let e = ApiError::from(OpsError::Internal("disk /secret".into()));
        assert_eq!(e.status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert!(!e.body().to_string().contains("secret"));
    }

    #[test]
    fn not_yet_editable_fields_are_named() {
        let p: SourcePatch = serde_json::from_value(json!({ "enabled": true })).unwrap();
        assert_eq!(p.not_yet_editable(), None);
        let p: SourcePatch =
            serde_json::from_value(json!({ "priority": "low", "schedule": "01:00-07:00" }))
                .unwrap();
        assert_eq!(p.not_yet_editable(), Some("schedule"));
        assert!(serde_json::from_value::<SourcePatch>(json!({ "weight": 3 })).is_err());
    }
}
