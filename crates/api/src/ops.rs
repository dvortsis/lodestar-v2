//! The process's summary and controls (architecture §9, §10.9, §10.10,
//! §18.8): `GET /stats`, `GET|PATCH /crawler`, `POST /crawler/pause` and
//! `/crawler/resume`, `GET /sources`, `PATCH /sources/{id}`, and
//! `GET|POST /network/check`.
//!
//! - **One source of truth.** The crawler's pause and intensity are the
//!   settings registry's `crawler.paused` and `crawler.intensity`: these
//!   routes change them through the registry exactly as
//!   `PATCH /settings/crawler` does (publishing `settings` on the
//!   `/events` hub when a save changes the section), then have the running
//!   pipeline apply and record them at once (the binary also follows the
//!   registry, so a change made either way takes effect and is recorded
//!   once).
//! - **Everything else** comes from the running pipeline through
//!   [`OpsApi`], which the server binary implements: this crate can't
//!   depend on it.
//! - **Security:** every route is behind a session; writes are
//!   same-origin JSON. Pause and resume need no password: they're an
//!   operational control, not a filter change.
//! - **Consent (§18.7):** the server refuses to enable a source until its
//!   class's condition holds (the binary checks it against the store;
//!   this crate only checks the password). Enabling a `restricted` or
//!   `overlay` source needs the admin password, every time (Q13, §20.11).
//! - **Credentials** (`PATCH /sources/{id}/credentials`) are taken in and
//!   never given back: no response, error or log line holds a value. The
//!   body is read by hand, so a malformed one is refused without quoting
//!   it.

use std::collections::BTreeMap;
use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use axum::extract::{Extension, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::{Map, Value};
use utoipa::OpenApi;

use crate::ApiState;
use crate::auth;
pub use crate::dto::ops::{
    ConsentClass, CrawlerIntensity, CrawlerPatch, CrawlerState, CrawlerStatus, CredentialsPatch,
    DhtStatus, EgressCounts, EgressLive, EgressRoute, EgressSetting, EgressState, ExternalAddress,
    FetchStatus, NetworkCheck, PortMapping, ReachReason, RiskLevel, SamplingBlocked, SocksDns,
    Source, SourceAgents, SourceEgress, SourceHealth, SourceKind, SourceList, SourceNeed,
    SourcePatch, SourcePriority, SourceSchedule, SourceState, StatsResponse,
};
use crate::error::{ApiError, ApiResult, internal};
use crate::events::EventHub;
use crate::extract::{ClientIp, Json as JsonBody, Path, same_origin_json};
use crate::settings::{SettingsApi, SettingsBatch, SettingsPatch, save_and_announce};
pub use lodestar_integrations::Secret;

/// The `/events` hub, when the server layers one.
type Hub = Option<Extension<Arc<EventHub>>>;

/// This module's half of `/openapi.json`.
#[derive(OpenApi)]
#[openapi(paths(
    stats,
    crawler,
    patch_crawler,
    pause,
    resume,
    sources,
    source,
    patch_source,
    patch_source_credentials,
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
    /// Applies the registry's `crawler` section as it is *now* (pause,
    /// and the intensity's event), after a save. It reads the section
    /// under the same lock every change takes, so concurrent saves leave
    /// the process as the registry says, whatever order they finish in.
    /// Recording an event is best effort: the change applies regardless.
    fn sync_crawler(&self) -> BoxFuture<'_, ()>;
    fn stats(&self) -> BoxFuture<'_, StatsResponse>;
    fn sources(&self) -> BoxFuture<'_, Result<Vec<Source>, OpsError>>;
    /// Applies `patch` (its not-yet-editable fields already refused, its
    /// egress and schedule already checked, its schedule canonical).
    /// `password_confirmed` says the route checked `patch.password`; the
    /// implementation refuses to enable a Restricted or Overlay source,
    /// or to change any source's egress, without it
    /// ([`OpsError::PasswordRequired`]).
    fn patch_source(
        &self,
        id: u8,
        patch: SourcePatch,
        password_confirmed: bool,
    ) -> BoxFuture<'_, Result<Source, OpsError>>;
    /// Stores (`Some`) or removes (`None`) the source's credentials,
    /// sealed. Never returns, logs or quotes a value.
    fn patch_source_credentials(
        &self,
        id: u8,
        changes: Vec<(String, Option<Secret>)>,
    ) -> BoxFuture<'_, Result<Source, OpsError>>;
    fn network(&self) -> NetworkCheck;
    fn restart_network_check(&self) -> Result<NetworkCheck, OpsError>;
}

/// Why an operation was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpsError {
    UnknownSource,
    /// Enabling the source needs an acknowledgement of its class at this
    /// version (none, a wrong one or a stale one was sent).
    ConsentRequired {
        class: ConsentClass,
        version: String,
    },
    /// Enabling this source needs the admin password.
    PasswordRequired,
    /// Enabling the source needs its credentials stored first.
    CredentialsRequired,
    /// A credential change was refused; the reason names a rule, never a
    /// value.
    BadCredential(String),
    /// A setting was refused: `code` and `field` name it, `message`
    /// says why (never quoting a secret).
    Invalid {
        code: &'static str,
        field: &'static str,
        message: String,
    },
    /// The interface or bind-address route, which isn't offered.
    RouteUnavailable,
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
            OpsError::ConsentRequired { class, version } => {
                let name = class_name(class);
                ApiError::new(
                    StatusCode::CONFLICT,
                    "consent_required",
                    format!(
                        "Enabling this source needs your consent: its class is \"{name}\". \
                         Show what it contacts and the consent text, then send \
                         \"consent\": \"{name}\" and \"consent_version\": \"{version}\"."
                    ),
                )
                .field("consent")
            }
            OpsError::PasswordRequired => auth::confirm_required(),
            OpsError::CredentialsRequired => ApiError::new(
                StatusCode::CONFLICT,
                "credentials_required",
                "This source signs in to an account. Save its credentials first, then enable it.",
            )
            .field("credentials"),
            OpsError::BadCredential(reason) => ApiError::new(
                StatusCode::BAD_REQUEST,
                "invalid_credentials",
                format!("The credentials weren't saved: {reason}."),
            )
            .field("credentials"),
            OpsError::Invalid {
                code,
                field,
                message,
            } => ApiError::bad_request(code, message).field(field),
            OpsError::RouteUnavailable => route_unavailable(),
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
            .route("/sources/{id}", get(source).patch(patch_source))
            .route(
                "/sources/{id}/credentials",
                axum::routing::patch(patch_source_credentials),
            )
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
/// `PATCH /settings/crawler` would, and publishes `settings` on the
/// `/events` hub when it changed something (a no-op pause changes
/// nothing, so it publishes nothing).
async fn save(s: &ApiState, hub: Hub, field: &str, value: Value) -> ApiResult<()> {
    let settings = registry(s)?;
    let mut patch = Map::new();
    patch.insert(field.to_string(), value);
    let batch = SettingsBatch(BTreeMap::from([(
        "crawler".to_string(),
        SettingsPatch(patch),
    )]));
    let hub = hub.map(|Extension(h)| h);
    tokio::task::spawn_blocking(move || {
        save_and_announce(settings.as_ref(), hub.as_deref(), batch)
    })
    .await
    .map_err(internal)?
    .map_err(|f| ApiError::from(f.error))?;
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
    hub: Hub,
    headers: HeaderMap,
    JsonBody(body): JsonBody<CrawlerPatch>,
) -> ApiResult<Json<CrawlerStatus>> {
    same_origin_json(&headers)?;
    let ops = ops(&s)?;
    let value = serde_json::to_value(body.intensity).map_err(internal)?;
    save(&s, hub, "intensity", value).await?;
    ops.sync_crawler().await;
    status(&s).await.map(Json)
}

async fn set_paused(
    s: ApiState,
    hub: Hub,
    headers: HeaderMap,
    paused: bool,
) -> ApiResult<Json<CrawlerStatus>> {
    same_origin_json(&headers)?;
    let ops = ops(&s)?;
    save(&s, hub, "paused", Value::Bool(paused)).await?;
    ops.sync_crawler().await;
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
    hub: Hub,
    headers: HeaderMap,
) -> ApiResult<Json<CrawlerStatus>> {
    set_paused(s, hub, headers, true).await
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
    hub: Hub,
    headers: HeaderMap,
) -> ApiResult<Json<CrawlerStatus>> {
    set_paused(s, hub, headers, false).await
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

/// One source, as `GET /sources` lists it.
#[utoipa::path(
    operation_id = "getSource",
    get,
    path = "/sources/{id}",
    tag = "crawler",
    params(("id" = u8, Path, description = "The source's id, as `GET /sources` lists it.")),
    responses((status = 200, body = Source))
)]
pub(crate) async fn source(
    State(s): State<ApiState>,
    Path(id): Path<u8>,
) -> ApiResult<Json<Source>> {
    ops(&s)?
        .sources()
        .await?
        .into_iter()
        .find(|s| s.id == id)
        .map(Json)
        .ok_or_else(unknown_source)
}

/// Most windows a source's schedule holds.
pub const MAX_WINDOWS: usize = 8;

/// A schedule window, `HH:MM-HH:MM`, as minutes since local midnight
/// (`start`, `end`); `24:00` isn't a time, so `00:00` ends a day.
pub fn parse_window(text: &str) -> Option<(u16, u16)> {
    fn minute(t: &str) -> Option<u16> {
        let (h, m) = t.split_once(':')?;
        if h.len() != 2 || m.len() != 2 || !(h.bytes().chain(m.bytes())).all(|b| b.is_ascii_digit())
        {
            return None;
        }
        let (h, m): (u16, u16) = (h.parse().ok()?, m.parse().ok()?);
        (h < 24 && m < 60).then_some(h * 60 + m)
    }
    let (a, b) = text.split_once('-')?;
    Some((minute(a)?, minute(b)?))
}

/// The text of a window [`parse_window`] reads.
pub fn window_text(start: u16, end: u16) -> String {
    format!(
        "{:02}:{:02}-{:02}:{:02}",
        start / 60,
        start % 60,
        end / 60,
        end % 60
    )
}

/// A SOCKS5 proxy's `host:port`: a DNS name, an IPv4 literal or a
/// bracketed IPv6 literal, and a port from 1. The host comes back without
/// brackets.
pub fn parse_proxy(text: &str) -> Option<(String, u16)> {
    let (host, port) = text.rsplit_once(':')?;
    if port.is_empty() || port.len() > 5 || !port.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let port: u16 = port.parse().ok().filter(|p| *p != 0)?;
    // Not the unspecified address (this machine), multicast or broadcast.
    // An IPv4-mapped address is judged as the IPv4 address it carries
    // (`[::ffff:0.0.0.0]` connects to this machine too), as
    // `lodestar_sources::egress::usable_proxy` judges it.
    let usable = |ip: std::net::IpAddr| {
        let ip = match ip {
            std::net::IpAddr::V6(v6) => v6.to_ipv4_mapped().map_or(ip, std::net::IpAddr::V4),
            std::net::IpAddr::V4(_) => ip,
        };
        !ip.is_unspecified()
            && !ip.is_multicast()
            && ip != std::net::IpAddr::V4(std::net::Ipv4Addr::BROADCAST)
    };
    if let Some(v6) = host.strip_prefix('[').and_then(|h| h.strip_suffix(']')) {
        return v6
            .parse::<std::net::Ipv6Addr>()
            .ok()
            .filter(|a| usable((*a).into()))
            .map(|a| (a.to_string(), port));
    }
    if let Ok(v4) = host.parse::<std::net::Ipv4Addr>() {
        return usable(v4.into()).then(|| (host.to_string(), port));
    }
    // A DNS name: labels of 1–63 letters, digits and hyphens (not at
    // either end), 253 bytes in all, not all digits.
    let name = host.strip_suffix('.').unwrap_or(host);
    let ok = !name.is_empty()
        && name.len() <= 253
        && name.split('.').all(|l| {
            !l.is_empty()
                && l.len() <= 63
                && !l.starts_with('-')
                && !l.ends_with('-')
                && l.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
        })
        // Not a number in disguise: an all-digit or `0x` last label is
        // an address to `getaddrinfo` (`127.1`, `0x7f000001`).
        && !name.split('.').next_back().is_some_and(|l| {
            l.bytes().all(|b| b.is_ascii_digit()) || l.to_ascii_lowercase().starts_with("0x")
        });
    ok.then(|| (name.to_ascii_lowercase(), port))
}

/// `409 route_unavailable`: the interface and bind-address routes.
fn route_unavailable() -> ApiError {
    ApiError::new(
        StatusCode::CONFLICT,
        "route_unavailable",
        "Choosing a network interface or a local address isn't offered yet: DNS lookups \
         would still leave outside it. Run Lodestar's whole container through your VPN, or \
         use the VPN's SOCKS5 proxy.",
    )
    .field("egress.route")
}

fn invalid(code: &'static str, field: &'static str, message: &'static str) -> ApiError {
    ApiError::bad_request(code, message).field(field)
}

/// Checks the settings part of `body` and puts its schedule in canonical
/// form, so the implementation stores what it was given only once it's
/// valid.
fn check_settings(body: &mut SourcePatch) -> ApiResult<()> {
    if let Some(e) = &body.egress {
        match e.route {
            EgressRoute::Interface | EgressRoute::BindAddress => {
                return Err(route_unavailable());
            }
            EgressRoute::Direct if e.proxy.is_some() || e.dns.is_some() => {
                return Err(invalid(
                    "invalid_egress",
                    if e.proxy.is_some() {
                        "egress.proxy"
                    } else {
                        "egress.dns"
                    },
                    "A direct route takes no proxy or DNS choice.",
                ));
            }
            EgressRoute::Direct => {}
            EgressRoute::Socks5 => {
                if e.proxy.as_deref().and_then(parse_proxy).is_none() {
                    return Err(invalid(
                        "invalid_egress",
                        "egress.proxy",
                        "Give the SOCKS5 proxy as host:port, such as 10.8.0.1:1080 or \
                         gluetun:1080.",
                    ));
                }
            }
        }
    }
    if let Some(windows) = &mut body.schedule {
        if windows.len() > MAX_WINDOWS {
            return Err(invalid(
                "invalid_schedule",
                "schedule",
                "A schedule holds at most 8 windows.",
            ));
        }
        for w in windows.iter_mut() {
            let Some((start, end)) = parse_window(w.trim()) else {
                return Err(invalid(
                    "invalid_schedule",
                    "schedule",
                    "Give each window as HH:MM-HH:MM, such as 01:00-07:00.",
                ));
            };
            *w = window_text(start, end);
        }
    }
    if body.agents == Some(0) {
        return Err(invalid(
            "invalid_agents",
            "agents",
            "A source needs at least one agent; disable it to stop it.",
        ));
    }
    Ok(())
}

/// Enables or disables a source, or sets its priority. A priority
/// applies at once; enabling or disabling at the next start. Enabling
/// checks the source's class (§18.7, its `needs`): an `external`,
/// `restricted` or `overlay` source needs `"consent"` naming its class and
/// `"consent_version"` (`409 consent_required` otherwise) until that
/// version is acknowledged; enabling a `restricted` or `overlay` source
/// also needs the admin `"password"`, every time (`428 confirm_required`); an
/// `account` source needs its credentials stored
/// (`409 credentials_required`). Agents, egress and the schedule apply
/// at the next start; changing egress needs the admin `"password"`
/// (`428 confirm_required`), and the `interface` and `bind_address`
/// routes are refused (`409 route_unavailable`). Purge isn't editable yet
/// (`409 not_editable`).
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
    ClientIp(ip): ClientIp,
    headers: HeaderMap,
    Path(id): Path<u8>,
    JsonBody(mut body): JsonBody<SourcePatch>,
) -> ApiResult<Json<Source>> {
    same_origin_json(&headers)?;
    let ops = ops(&s)?;
    if let Some(field) = body.not_yet_editable() {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "not_editable",
            "This can't be changed yet.",
        )
        .field(field));
    }
    check_settings(&mut body)?;
    // The password is checked before anything is read or written. A
    // route change always needs it: the route is what keeps a source's
    // traffic on the VPN. With an enable, whether the source needs it
    // (Restricted and Overlay, every time) is the implementation's to
    // decide.
    let password = body.password.take();
    let needs_check = body.egress.is_some()
        || (body.enabled == Some(true) && password.as_deref().is_some_and(|p| !p.is_empty()));
    if needs_check {
        auth::confirm_fresh_password(&s.auth, ip, password).await?;
    }
    let confirmed = needs_check;
    Ok(Json(ops.patch_source(id, body, confirmed).await?))
}

/// Stores or removes a source's credentials: each name maps to a value
/// (stored, sealed with the install's key) or `null` (removed). Values
/// are never returned: the response lists the names stored. Removing what
/// an enabled `account` source needs disables it. `400
/// invalid_credentials` names the rule a change broke, never a value.
#[utoipa::path(
    operation_id = "patchSourceCredentials",
    patch,
    path = "/sources/{id}/credentials",
    tag = "crawler",
    params(("id" = u8, Path, description = "The source's id, as `GET /sources` lists it.")),
    request_body = CredentialsPatch,
    responses((status = 200, body = Source))
)]
pub(crate) async fn patch_source_credentials(
    State(s): State<ApiState>,
    headers: HeaderMap,
    Path(id): Path<u8>,
    JsonBody(body): JsonBody<Value>,
) -> ApiResult<Json<Source>> {
    same_origin_json(&headers)?;
    let ops = ops(&s)?;
    let changes = credential_changes(body)?;
    Ok(Json(ops.patch_source_credentials(id, changes).await?))
}

/// The changes in a `CredentialsPatch` body, read by hand so a refusal
/// never quotes a value (serde's messages can).
fn credential_changes(body: Value) -> ApiResult<Vec<(String, Option<Secret>)>> {
    let shape = || {
        ApiError::bad_request(
            "invalid_credentials",
            "Send {\"credentials\": {\"<name>\": \"<value>\" or null}}.",
        )
        .field("credentials")
    };
    let Value::Object(mut top) = body else {
        return Err(shape());
    };
    let Some(Value::Object(map)) = top.remove("credentials") else {
        return Err(shape());
    };
    if !top.is_empty() {
        return Err(shape());
    }
    map.into_iter()
        .map(|(name, v)| match v {
            Value::String(v) => Ok((name, Some(Secret::new(v)))),
            Value::Null => Ok((name, None)),
            _ => Err(shape()),
        })
        .collect()
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
        let e = ApiError::from(OpsError::ConsentRequired {
            class: ConsentClass::Restricted,
            version: "1-00ff".into(),
        });
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
        assert!(e.body().to_string().contains("1-00ff"));
        let e = ApiError::from(OpsError::PasswordRequired);
        assert_eq!(
            (e.status(), e.code()),
            (StatusCode::PRECONDITION_REQUIRED, "confirm_required")
        );
        let e = ApiError::from(OpsError::CredentialsRequired);
        assert_eq!(
            (e.status(), e.code()),
            (StatusCode::CONFLICT, "credentials_required")
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
    fn a_malformed_credentials_body_is_refused_without_quoting_it() {
        for body in [
            json!("hunter2-secret"),
            json!({ "credentials": "hunter2-secret" }),
            json!({ "credentials": { "api_key": 424242 } }),
            json!({ "credentials": { "api_key": ["hunter2-secret"] } }),
            json!({ "credentials": {}, "extra": "hunter2-secret" }),
        ] {
            let e = credential_changes(body).unwrap_err();
            assert_eq!(e.code(), "invalid_credentials");
            let text = e.body().to_string();
            assert!(
                !text.contains("hunter2") && !text.contains("424242"),
                "{text}"
            );
        }
        let changes =
            credential_changes(json!({ "credentials": { "a": "x", "b": null } })).unwrap();
        assert_eq!(changes.len(), 2);
        assert_eq!(changes[0].1.as_ref().map(Secret::expose), Some("x"));
        assert!(changes[1].1.is_none());
        // Debug never prints a password or a value.
        let p: SourcePatch = serde_json::from_value(json!({ "password": "hunter2" })).unwrap();
        assert!(!format!("{p:?}").contains("hunter2"));
        let c: CredentialsPatch =
            serde_json::from_value(json!({ "credentials": { "k": "hunter2" } })).unwrap();
        assert!(!format!("{c:?}").contains("hunter2"));
    }

    #[test]
    fn not_yet_editable_fields_are_named() {
        let p: SourcePatch = serde_json::from_value(json!({ "enabled": true })).unwrap();
        assert_eq!(p.not_yet_editable(), None);
        let p: SourcePatch =
            serde_json::from_value(json!({ "priority": "low", "purge": true })).unwrap();
        assert_eq!(p.not_yet_editable(), Some("purge"));
        assert!(serde_json::from_value::<SourcePatch>(json!({ "weight": 3 })).is_err());
        // Nothing turns verification on (it waits for a per-source cap).
        assert!(
            serde_json::from_value::<SourcePatch>(json!({ "verify_unverified": true })).is_err()
        );
        // A schedule is a list of windows, not one string.
        assert!(
            serde_json::from_value::<SourcePatch>(json!({ "schedule": "01:00-07:00" })).is_err()
        );
    }

    #[test]
    fn windows_parse_and_print() {
        assert_eq!(parse_window("01:00-07:00"), Some((60, 420)));
        assert_eq!(parse_window("22:30-00:00"), Some((1350, 0)));
        assert_eq!(parse_window("23:59-00:01"), Some((1439, 1)));
        for bad in [
            "",
            "1:00-07:00",
            "24:00-01:00",
            "01:60-02:00",
            "01:00",
            "01:00-",
            "+1:00-02:00",
            "01:00-02:00-03:00",
            "０1:00-02:00",
            "01:00 - 02:00",
        ] {
            assert_eq!(parse_window(bad), None, "{bad}");
        }
        assert_eq!(window_text(1350, 0), "22:30-00:00");
        for m in [0u16, 59, 60, 1439] {
            assert_eq!(parse_window(&window_text(m, m)), Some((m, m)));
        }
    }

    #[test]
    fn proxies_parse() {
        assert_eq!(
            parse_proxy("10.8.0.1:1080"),
            Some(("10.8.0.1".into(), 1080))
        );
        assert_eq!(
            parse_proxy("[fd00::1]:1080"),
            Some(("fd00::1".into(), 1080))
        );
        assert_eq!(parse_proxy("Gluetun:1080"), Some(("gluetun".into(), 1080)));
        assert_eq!(
            parse_proxy("proxy.example.net.:443"),
            Some(("proxy.example.net".into(), 443))
        );
        for bad in [
            "",
            "10.8.0.1",
            "10.8.0.1:0",
            "10.8.0.1:65536",
            "10.8.0.1:+80",
            "fd00::1:1080",
            "[fd00::1]",
            "[10.0.0.1]:80",
            "-a:80",
            "a_b:80",
            "a b:80",
            "user@host:80",
            "host/x:80",
            "1.2.3:80",
            "127.1:80",
            "0x7f000001:80",
            "0.0.0.0:1080",
            "[::]:1080",
            "224.0.0.1:1080",
            "[ff02::1]:1080",
            "255.255.255.255:1080",
            "[::ffff:0.0.0.0]:1080",
            "[::ffff:224.0.0.1]:1080",
            "[::ffff:255.255.255.255]:1080",
            "999.1.1.1:80",
            ":80",
        ] {
            assert_eq!(parse_proxy(bad), None, "{bad}");
        }
    }

    #[test]
    fn settings_are_checked_and_canonical() {
        let patch = |v: Value| serde_json::from_value::<SourcePatch>(v).unwrap();
        let code = |v: Value| check_settings(&mut patch(v)).unwrap_err().code();
        assert_eq!(
            code(json!({ "egress": { "route": "interface" } })),
            "route_unavailable"
        );
        assert_eq!(
            code(json!({ "egress": { "route": "bind_address" } })),
            "route_unavailable"
        );
        assert_eq!(
            code(json!({ "egress": { "route": "socks5" } })),
            "invalid_egress"
        );
        assert_eq!(
            code(json!({ "egress": { "route": "direct", "dns": "local" } })),
            "invalid_egress"
        );
        assert_eq!(code(json!({ "schedule": ["1-2"] })), "invalid_schedule");
        assert_eq!(
            code(json!({ "schedule": vec!["01:00-02:00"; 9] })),
            "invalid_schedule"
        );
        assert_eq!(code(json!({ "agents": 0 })), "invalid_agents");
        // An unknown key inside egress is refused, not ignored.
        assert!(
            serde_json::from_value::<SourcePatch>(
                json!({ "egress": { "route": "direct", "via": "x" } })
            )
            .is_err()
        );
        let mut p = patch(json!({
            "schedule": [" 01:00-07:00 "],
            "egress": { "route": "socks5", "proxy": "gluetun:1080" }
        }));
        check_settings(&mut p).unwrap();
        assert_eq!(p.schedule.unwrap(), ["01:00-07:00"]);
    }
}
