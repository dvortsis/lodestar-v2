//! `/workers` (architecture §19.6, §19.7, §10.10.7): Settings → Workers'
//! list and actions, admin only.
//!
//! - `GET /workers`: every joined worker and its state.
//! - `POST /workers/tokens`: a one-time `lsj1` join token (15 minutes,
//!   single use, bound to the name), returned **once** with ready-to-copy
//!   commands. Only its hash is kept, so no route can read it back.
//! - `POST /workers/{id}/drain`, `/resume`, `/pause`.
//! - `POST /workers/{id}/revoke`: a Danger zone action (§10.10.7). The
//!   certificate is refused at once, the leases go back, and the worker's
//!   connection is closed within one heartbeat.
//! - `PATCH /workers/{id}`: budget, intensity, pool cap and DHT pin,
//!   each range-checked here.
//!
//! WK-13 adds `quarantine`, `release` and `purge` beside `revoke`: one
//! route each in [`routes`], one method each on [`WorkersApi`].
//!
//! **The binary answers** through [`WorkersApi`], an `Extension` the
//! server layers over the router (as `/system` does). Without one, this
//! process has no cluster: the list is empty, as on a single machine, and
//! every action is `404 unknown_worker` (a token is `409 workers_off`).
//!
//! **Security:** every route is behind a session (API keys and feed
//! tokens are refused), writes are same-origin JSON, and the token
//! appears in the create response alone: never in a log line, an error or
//! another response. Nothing here carries a peer address (§18.7) or
//! certificate material.

use std::sync::Arc;

use axum::extract::{Extension, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::routing::{get, patch, post};
use axum::{Json, Router};
use utoipa::OpenApi;

use crate::dto::error::ErrorEnvelope;
use crate::dto::workers::{
    CreateWorkerToken, JoinCommands, Worker, WorkerBudget, WorkerList, WorkerPatch, WorkerRevoked,
    WorkerTokenCreated,
};
use crate::error::{ApiError, ApiResult, internal};
use crate::extract::{self, ClientIp, Path, same_origin_json};
use crate::ops::BoxFuture;
use crate::{ApiState, auth};

/// This module's half of `/openapi.json`.
#[derive(OpenApi)]
#[openapi(
    paths(
        list_workers,
        create_token,
        drain_worker,
        resume_worker,
        pause_worker,
        revoke_worker,
        update_worker
    ),
    tags((name = "workers", description = "Remote workers: join, drain, pause, revoke and their budgets (§19.6, §19.7)."))
)]
pub(crate) struct ApiDoc;

/// CPU share, ‰.
pub const CPU_PERMILLE: std::ops::RangeInclusive<u16> = 1..=1000;
/// Memory cap, MB (0 = no cap). 1 TiB.
pub const RAM_MB: std::ops::RangeInclusive<u32> = 0..=1_048_576;
/// Upload and download caps, kB/s (0 = no cap). 10 GB/s.
pub const KBPS: std::ops::RangeInclusive<u32> = 0..=10_000_000;
/// Fetch-pool cap, keys (`lodestar_cluster::assign::MAX_POOL_CAP`).
pub const POOL_CAP: std::ops::RangeInclusive<u32> = 1..=1_000_000;

/// The image the commands run (the one `packaging/docker` builds).
const IMAGE: &str = "ghcr.io/dvortsis/lodestar-v2:latest";
/// Written in place of a host the request didn't give in a usable form.
const HOST_PLACEHOLDER: &str = "<this-server>";

/// A new join token as the binary hands it over: the API adds the
/// commands. Its text is overwritten when dropped (best effort: the
/// response body is a copy).
pub struct NewToken {
    pub token: String,
    pub name: String,
    /// Unix seconds.
    pub expires_at: i64,
    /// The cluster port workers dial.
    pub port: u16,
}

impl std::fmt::Debug for NewToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NewToken")
            .field("name", &self.name)
            .field("expires_at", &self.expires_at)
            .finish_non_exhaustive()
    }
}

impl Drop for NewToken {
    fn drop(&mut self) {
        let mut bytes = std::mem::take(&mut self.token).into_bytes();
        bytes.fill(0);
        std::hint::black_box(&bytes);
    }
}

/// A worker name: one DNS label of lowercase letters, digits and hyphens,
/// 1–63 characters, not starting or ending with a hyphen
/// (`lodestar_cluster::valid_worker_name`, which the coordinator checks
/// again).
fn valid_name(name: &str) -> bool {
    (1..=63).contains(&name.len())
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && !name.starts_with('-')
        && !name.ends_with('-')
}

/// Why a workers action was refused. No variant carries a token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkersError {
    /// "Allow workers" is off: there's no cluster port to join.
    Off,
    /// No worker has this id.
    UnknownWorker,
    /// A drain or pause needs a connected worker.
    NotConnected,
    /// The name isn't a valid worker name.
    BadName,
    /// A worker with an unexpired certificate or open token has the name.
    NameInUse,
    /// Too many unredeemed tokens (64).
    TooManyTokens,
    /// The coordinator keeps settings for at most 4,096 workers.
    PrefsFull,
    /// Something failed on the server; the text is for the log only and
    /// never holds a secret.
    Failed(String),
}

impl From<WorkersError> for ApiError {
    fn from(e: WorkersError) -> ApiError {
        let conflict = |code, msg| ApiError::new(StatusCode::CONFLICT, code, msg);
        match e {
            WorkersError::Off => conflict(
                "workers_off",
                "Turn on Allow workers first: there's no cluster port to join.",
            ),
            WorkersError::UnknownWorker => ApiError::new(
                StatusCode::NOT_FOUND,
                "unknown_worker",
                "No worker has this id.",
            ),
            WorkersError::NotConnected => conflict(
                "worker_offline",
                "The worker isn't connected. To stop it for good, revoke it.",
            ),
            WorkersError::BadName => ApiError::bad_request(
                "bad_name",
                "Use 1–63 lowercase letters, digits and hyphens, not starting or ending with a hyphen.",
            )
            .field("name"),
            WorkersError::NameInUse => conflict(
                "name_in_use",
                "Another worker or open join token has this name.",
            )
            .field("name"),
            WorkersError::TooManyTokens => conflict(
                "too_many_tokens",
                "Too many join tokens are waiting to be used. Wait for some to expire (15 minutes).",
            ),
            WorkersError::PrefsFull => conflict(
                "too_many_workers",
                "Lodestar keeps settings for at most 4,096 workers. Revoke workers you no longer use.",
            ),
            WorkersError::Failed(why) => internal(why),
        }
    }
}

/// What the binary does for `/workers`. Ids are checked by the binary
/// (unknown → [`WorkersError::UnknownWorker`]); ranges are checked here
/// before it's called.
pub trait WorkersApi: Send + Sync + 'static {
    fn list(&self) -> BoxFuture<'_, Result<WorkerList, WorkersError>>;
    /// A join token for a worker called `name` (already validated).
    fn create_token(&self, name: String) -> BoxFuture<'_, Result<NewToken, WorkersError>>;
    fn drain(&self, id: u32) -> BoxFuture<'_, Result<Worker, WorkersError>>;
    /// Ends a drain or a pause.
    fn resume(&self, id: u32) -> BoxFuture<'_, Result<Worker, WorkersError>>;
    fn pause(&self, id: u32) -> BoxFuture<'_, Result<Worker, WorkersError>>;
    fn revoke(&self, id: u32) -> BoxFuture<'_, Result<WorkerRevoked, WorkersError>>;
    fn update(&self, id: u32, patch: WorkerPatch) -> BoxFuture<'_, Result<Worker, WorkersError>>;
    // WK-13: quarantine, release and purge go here.
}

pub fn routes() -> Router<ApiState> {
    auth::protected(
        Router::new()
            .route("/workers", get(list_workers))
            .route("/workers/tokens", post(create_token))
            .route("/workers/{id}", patch(update_worker))
            .route("/workers/{id}/drain", post(drain_worker))
            .route("/workers/{id}/resume", post(resume_worker))
            .route("/workers/{id}/pause", post(pause_worker))
            .route("/workers/{id}/revoke", post(revoke_worker)),
    )
}

type Handle = Option<Extension<Arc<dyn WorkersApi>>>;

fn api(handle: Handle) -> Result<Arc<dyn WorkersApi>, WorkersError> {
    handle
        .map(|Extension(w)| w)
        .ok_or(WorkersError::UnknownWorker)
}

/// Worker 0 is the built-in one: the coordinator itself, not listed and
/// not actionable.
fn worker_id(id: u32) -> Result<u32, WorkersError> {
    if id == 0 {
        Err(WorkersError::UnknownWorker)
    } else {
        Ok(id)
    }
}

/// Every joined worker. Empty, with `allowed: false`, on a single
/// machine.
#[utoipa::path(
    operation_id = "listWorkers",
    get,
    path = "/workers",
    tag = "workers",
    responses((status = 200, body = WorkerList))
)]
pub(crate) async fn list_workers(handle: Handle) -> ApiResult<Json<WorkerList>> {
    let Some(Extension(api)) = handle else {
        return Ok(Json(WorkerList {
            allowed: false,
            workers: Vec::new(),
        }));
    };
    Ok(Json(api.list().await?))
}

/// Creates a one-time join token for a new worker, valid 15 minutes and
/// for this name only, with ready-to-copy commands. **It's shown this
/// once:** only its hash is kept, so it can't be read back.
#[utoipa::path(
    operation_id = "createWorkerToken",
    post,
    path = "/workers/tokens",
    tag = "workers",
    request_body = CreateWorkerToken,
    responses(
        (status = 201, body = WorkerTokenCreated),
        (status = 400, description = "`bad_name`.", body = ErrorEnvelope),
        (status = 401, description = "`bad_password`.", body = ErrorEnvelope),
        (status = 403, description = "`cross_origin`.", body = ErrorEnvelope),
        (status = 409, description = "`workers_off`, `name_in_use` or `too_many_tokens`.", body = ErrorEnvelope),
        (status = 415, description = "`unsupported_media_type`: not JSON.", body = ErrorEnvelope),
        (status = 428, description = "`confirm_required`: no password.", body = ErrorEnvelope),
        (status = 429, description = "`rate_limited`: too many password attempts.", body = ErrorEnvelope),
    )
)]
pub(crate) async fn create_token(
    State(s): State<ApiState>,
    ClientIp(ip): ClientIp,
    handle: Handle,
    headers: HeaderMap,
    extract::Json(body): extract::Json<CreateWorkerToken>,
) -> ApiResult<(
    StatusCode,
    [(header::HeaderName, &'static str); 1],
    Json<WorkerTokenCreated>,
)> {
    same_origin_json(&headers)?;
    let api = handle.map(|Extension(w)| w).ok_or(WorkersError::Off)?;
    if !valid_name(&body.name) {
        return Err(WorkersError::BadName.into());
    }
    // A join token admits a machine to the cluster: confirmed with the
    // password, as an API key is (§10.10.7 "Security").
    auth::confirm_fresh_password(&s.auth, ip, body.password).await?;
    let new = api.create_token(body.name).await?;
    let host = request_host(&headers);
    let address = format!("https://{host}:{}", new.port);
    let created = WorkerTokenCreated {
        commands: join_commands(&address, &new.token, &new.name),
        address,
        expires_at: new.expires_at,
        name: new.name.clone(),
        token: new.token.clone(),
    };
    // Never cached by the browser or a proxy.
    Ok((
        StatusCode::CREATED,
        [(header::CACHE_CONTROL, "no-store")],
        Json(created),
    ))
}

/// The host the admin reached this server by, without its port, if it's
/// a plain DNS name or IP literal (so nothing odd lands in a shell
/// command). Behind a reverse proxy that's the proxy's name: the commands
/// say to change it if workers reach this machine another way.
fn request_host(headers: &HeaderMap) -> String {
    let Some(host) = headers.get(header::HOST).and_then(|v| v.to_str().ok()) else {
        return HOST_PLACEHOLDER.into();
    };
    let name = if let Some(rest) = host.strip_prefix('[') {
        // [v6]:port
        match rest.split_once(']') {
            Some((v6, _)) if v6.parse::<std::net::Ipv6Addr>().is_ok() => return format!("[{v6}]"),
            _ => return HOST_PLACEHOLDER.into(),
        }
    } else {
        host.rsplit_once(':').map_or(host, |(h, _)| h)
    };
    let plain = !name.is_empty()
        && name.len() <= 253
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-')
        && !name.starts_with(['-', '.']);
    if plain {
        name.to_ascii_lowercase()
    } else {
        HOST_PLACEHOLDER.into()
    }
}

/// §19.6's ready-to-copy commands. The token goes in an environment
/// variable or standard input, never a command-line flag (WK-2: argv is
/// visible to every local user).
fn join_commands(address: &str, token: &str, name: &str) -> JoinCommands {
    JoinCommands {
        cli: format!("printf '%s\\n' '{token}' | lodestar worker --join {address} --token-file -"),
        cli_read: format!(
            "read -rs -p 'Join token: ' LODESTAR_JOIN_TOKEN && echo && \
             printf '%s\\n' \"$LODESTAR_JOIN_TOKEN\" | lodestar worker --join {address} --token-file - ; \
             unset LODESTAR_JOIN_TOKEN"
        ),
        // The DHT port as the Unraid worker template maps it: 6882 on the
        // host, beside a main Lodestar on 6881.
        docker_run: format!(
            "docker run -d --name lodestar-{name} --restart unless-stopped \
             -p 6882:6881/udp \
             -e LODESTAR_JOIN={address} -e LODESTAR_TOKEN={token} \
             -v lodestar-{name}:/data {IMAGE} worker"
        ),
        compose: format!(
            "services:\n  lodestar-{name}:\n    image: {IMAGE}\n    command: worker\n    \
             restart: unless-stopped\n    ports:\n      - \"6882:6881/udp\"\n    \
             environment:\n      LODESTAR_JOIN: \"{address}\"\n      \
             # Used once to join; remove it after the worker has joined.\n      \
             LODESTAR_TOKEN: \"{token}\"\n    volumes:\n      - lodestar-{name}:/data\n\
             volumes:\n  lodestar-{name}:\n"
        ),
        unraid: format!(
            "In the Lodestar Worker template, set Join address to {address} and Join token to \
             the token above. After it joins, clear the Join token field."
        ),
        windows: format!(
            "In the Lodestar installer, choose \"Join an existing Lodestar\", then enter \
             {address} and paste the token above."
        ),
    }
}

/// Asks a worker to finish the leases it holds, then stop (§19.6). It
/// stays draining across reconnects until resumed.
#[utoipa::path(
    operation_id = "drainWorker",
    post,
    path = "/workers/{id}/drain",
    tag = "workers",
    params(("id" = u32, Path, description = "The worker's id.")),
    responses(
        (status = 200, body = Worker),
        (status = 404, description = "`unknown_worker`.", body = ErrorEnvelope),
        (status = 403, description = "`cross_origin`.", body = ErrorEnvelope),
        (status = 409, description = "`worker_offline` or `workers_off`.", body = ErrorEnvelope),
        (status = 415, description = "`unsupported_media_type`: not JSON.", body = ErrorEnvelope),
    )
)]
pub(crate) async fn drain_worker(
    handle: Handle,
    headers: HeaderMap,
    Path(id): Path<u32>,
) -> ApiResult<Json<Worker>> {
    same_origin_json(&headers)?;
    Ok(Json(api(handle)?.drain(worker_id(id)?).await?))
}

/// Ends a drain or a pause: the worker takes work again.
#[utoipa::path(
    operation_id = "resumeWorker",
    post,
    path = "/workers/{id}/resume",
    tag = "workers",
    params(("id" = u32, Path, description = "The worker's id.")),
    responses(
        (status = 200, body = Worker),
        (status = 404, description = "`unknown_worker`.", body = ErrorEnvelope),
        (status = 403, description = "`cross_origin`.", body = ErrorEnvelope),
        (status = 409, description = "`workers_off`.", body = ErrorEnvelope),
        (status = 415, description = "`unsupported_media_type`: not JSON.", body = ErrorEnvelope),
    )
)]
pub(crate) async fn resume_worker(
    handle: Handle,
    headers: HeaderMap,
    Path(id): Path<u32>,
) -> ApiResult<Json<Worker>> {
    same_origin_json(&headers)?;
    Ok(Json(api(handle)?.resume(worker_id(id)?).await?))
}

/// Pauses a worker: it stays connected and keeps the leases it holds, but
/// is given no new work until resumed.
#[utoipa::path(
    operation_id = "pauseWorker",
    post,
    path = "/workers/{id}/pause",
    tag = "workers",
    params(("id" = u32, Path, description = "The worker's id.")),
    responses(
        (status = 200, body = Worker),
        (status = 404, description = "`unknown_worker`.", body = ErrorEnvelope),
        (status = 403, description = "`cross_origin`.", body = ErrorEnvelope),
        (status = 409, description = "`workers_off`.", body = ErrorEnvelope),
        (status = 415, description = "`unsupported_media_type`: not JSON.", body = ErrorEnvelope),
    )
)]
pub(crate) async fn pause_worker(
    handle: Handle,
    headers: HeaderMap,
    Path(id): Path<u32>,
) -> ApiResult<Json<Worker>> {
    same_origin_json(&headers)?;
    Ok(Json(api(handle)?.pause(worker_id(id)?).await?))
}

/// **Danger zone** (§10.10.7): revokes a worker. Its certificate is
/// refused from now on, its leases go back to other workers, and its
/// connection closes within one heartbeat. It can't reconnect: to use
/// the machine again, join it with a new token.
#[utoipa::path(
    operation_id = "revokeWorker",
    post,
    path = "/workers/{id}/revoke",
    tag = "workers",
    params(("id" = u32, Path, description = "The worker's id.")),
    responses(
        (status = 200, body = WorkerRevoked),
        (status = 404, description = "`unknown_worker`.", body = ErrorEnvelope),
        (status = 403, description = "`cross_origin`.", body = ErrorEnvelope),
        (status = 409, description = "`workers_off`: turn workers on to revoke one.", body = ErrorEnvelope),
        (status = 415, description = "`unsupported_media_type`: not JSON.", body = ErrorEnvelope),
    )
)]
pub(crate) async fn revoke_worker(
    handle: Handle,
    headers: HeaderMap,
    Path(id): Path<u32>,
) -> ApiResult<Json<WorkerRevoked>> {
    same_origin_json(&headers)?;
    Ok(Json(api(handle)?.revoke(worker_id(id)?).await?))
}

/// Changes a worker's budget, intensity, pool cap or DHT pin. Saved, so
/// it's kept across reconnects and coordinator restarts, and planned at
/// once; a worker applies it once the worker runtime sends assignments
/// (WK-10), not before.
#[utoipa::path(
    operation_id = "updateWorker",
    patch,
    path = "/workers/{id}",
    tag = "workers",
    params(("id" = u32, Path, description = "The worker's id.")),
    request_body = WorkerPatch,
    responses(
        (status = 200, body = Worker),
        (status = 422, description = "`invalid_value` (with the field) or `bad_body`.", body = ErrorEnvelope),
        (status = 404, description = "`unknown_worker`.", body = ErrorEnvelope),
        (status = 403, description = "`cross_origin`.", body = ErrorEnvelope),
        (status = 409, description = "`too_many_workers` or `workers_off`.", body = ErrorEnvelope),
        (status = 415, description = "`unsupported_media_type`: not JSON.", body = ErrorEnvelope),
    )
)]
pub(crate) async fn update_worker(
    handle: Handle,
    headers: HeaderMap,
    Path(id): Path<u32>,
    extract::Json(patch): extract::Json<WorkerPatch>,
) -> ApiResult<Json<Worker>> {
    same_origin_json(&headers)?;
    check_patch(&patch)?;
    Ok(Json(api(handle)?.update(worker_id(id)?, patch).await?))
}

/// 422, as a body that doesn't fit the endpoint is (`bad_body`).
fn out_of_range(field: &'static str, low: u64, high: u64) -> ApiError {
    ApiError::new(
        StatusCode::UNPROCESSABLE_ENTITY,
        "invalid_value",
        format!("{field} must be between {low} and {high}."),
    )
    .field(field)
}

/// Every range a `PATCH` can set.
fn check_patch(p: &WorkerPatch) -> ApiResult<()> {
    if let Some(WorkerBudget {
        cpu_permille,
        down_kbps,
        ram_mb,
        up_kbps,
    }) = p.budget
    {
        let r = |range: &std::ops::RangeInclusive<u32>, v: u32, field| {
            if range.contains(&v) {
                Ok(())
            } else {
                Err(out_of_range(
                    field,
                    u64::from(*range.start()),
                    u64::from(*range.end()),
                ))
            }
        };
        if !CPU_PERMILLE.contains(&cpu_permille) {
            return Err(out_of_range(
                "budget.cpu_permille",
                u64::from(*CPU_PERMILLE.start()),
                u64::from(*CPU_PERMILLE.end()),
            ));
        }
        r(&RAM_MB, ram_mb, "budget.ram_mb")?;
        r(&KBPS, up_kbps, "budget.up_kbps")?;
        r(&KBPS, down_kbps, "budget.down_kbps")?;
    }
    if let Some(cap) = p.pool_cap
        && !POOL_CAP.contains(&cap)
    {
        return Err(out_of_range(
            "pool_cap",
            u64::from(*POOL_CAP.start()),
            u64::from(*POOL_CAP.end()),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use axum::http::HeaderValue;

    use super::*;

    fn host(h: &str) -> String {
        let mut m = HeaderMap::new();
        m.insert(header::HOST, HeaderValue::from_str(h).unwrap());
        request_host(&m)
    }

    #[test]
    fn the_host_is_kept_only_when_plain() {
        assert_eq!(host("nas.local:8190"), "nas.local");
        assert_eq!(host("192.168.1.5:8190"), "192.168.1.5");
        assert_eq!(host("NAS"), "nas");
        assert_eq!(host("[fd00::5]:8190"), "[fd00::5]");
        for odd in ["a;rm -rf", "$(id)", "[nope]:1", "-x", "a b", "'q'"] {
            assert_eq!(host(odd), HOST_PLACEHOLDER, "{odd}");
        }
        assert_eq!(request_host(&HeaderMap::new()), HOST_PLACEHOLDER);
    }

    #[test]
    fn commands_carry_the_token_outside_argv_flags() {
        let c = join_commands("https://nas:8091", "lsj1abc", "w1");
        assert!(c.cli.contains("--token-file -") && c.cli.contains("lsj1abc"));
        assert!(!c.cli.contains("--token "));
        assert!(c.docker_run.contains("LODESTAR_TOKEN=lsj1abc"));
        assert!(c.docker_run.contains("LODESTAR_JOIN=https://nas:8091"));
        assert!(c.compose.contains("LODESTAR_TOKEN: \"lsj1abc\""));
        assert!(!c.unraid.contains("lsj1") && !c.windows.contains("lsj1"));
    }

    #[test]
    fn patch_ranges() {
        let budget = |cpu, ram, up, down| WorkerPatch {
            budget: Some(WorkerBudget {
                cpu_permille: cpu,
                ram_mb: ram,
                up_kbps: up,
                down_kbps: down,
            }),
            ..WorkerPatch::default()
        };
        assert!(check_patch(&budget(1, 0, 0, 0)).is_ok());
        assert!(check_patch(&budget(1000, 1_048_576, 10_000_000, 10_000_000)).is_ok());
        for (p, field) in [
            (budget(0, 0, 0, 0), "budget.cpu_permille"),
            (budget(1001, 0, 0, 0), "budget.cpu_permille"),
            (budget(1, 1_048_577, 0, 0), "budget.ram_mb"),
            (budget(1, 0, 10_000_001, 0), "budget.up_kbps"),
            (budget(1, 0, 0, 10_000_001), "budget.down_kbps"),
            (
                WorkerPatch {
                    pool_cap: Some(0),
                    ..WorkerPatch::default()
                },
                "pool_cap",
            ),
            (
                WorkerPatch {
                    pool_cap: Some(1_000_001),
                    ..WorkerPatch::default()
                },
                "pool_cap",
            ),
        ] {
            let e = check_patch(&p).unwrap_err();
            assert_eq!(
                (e.status(), e.code(), e.error_body().field.as_deref()),
                (
                    StatusCode::UNPROCESSABLE_ENTITY,
                    "invalid_value",
                    Some(field)
                )
            );
        }
    }

    #[test]
    fn no_error_mentions_a_token() {
        for e in [
            WorkersError::Off,
            WorkersError::UnknownWorker,
            WorkersError::NotConnected,
            WorkersError::BadName,
            WorkersError::NameInUse,
            WorkersError::TooManyTokens,
            WorkersError::PrefsFull,
        ] {
            let body = ApiError::from(e).body().to_string();
            assert!(!body.contains("lsj1"), "{body}");
        }
        assert_eq!(
            ApiError::from(WorkersError::PrefsFull).status(),
            StatusCode::CONFLICT
        );
    }
}
