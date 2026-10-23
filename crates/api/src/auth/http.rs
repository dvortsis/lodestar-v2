//! The auth endpoints (`/auth`, `/setup`, `/login`, `/logout` open;
//! `/password`, `/auth/proxy` and `/sessions/revoke-others` behind a
//! session) and the session guard every protected route goes through.

use std::net::IpAddr;
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::extract::{Request, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use lodestar_store::Store;
use utoipa::OpenApi;

use super::proxy::Proxy;
use super::{
    Auth, AuthError, MIN_PASSWORD_CHARS, ProxyConfig, SetProxyError, confirm_fresh_password,
};
use crate::ApiState;
use crate::dto::auth::{
    AuthStatus, ConfirmRequest, LoginRequest, OkResponse, PasswordRequest, ProxySettings,
    ProxyUpdate, SetupRequest,
};
use crate::error::{ApiError, ApiResult, internal, wrong_method};
use crate::extract::{ClientIp, Json, peer_ip, same_origin_json};

/// This module's half of `/openapi.json`.
#[derive(OpenApi)]
#[openapi(paths(
    auth_status,
    setup,
    login,
    logout,
    change_password,
    get_proxy,
    put_proxy,
    revoke_other_sessions
))]
pub(crate) struct ApiDoc;

pub fn routes() -> Router<ApiState> {
    let open = Router::new()
        .route("/auth", get(auth_status))
        .route("/setup", post(setup))
        .route("/login", post(login))
        .route("/logout", post(logout));
    let session = protected(
        Router::new()
            .route("/password", post(change_password))
            .route("/auth/proxy", get(get_proxy).put(put_proxy))
            .route("/sessions/revoke-others", post(revoke_other_sessions)),
    );
    open.merge(session)
}

/// What the guards check against; [`crate::router`] puts it in every
/// request's extensions: the [`Auth`], and the user-data store API keys
/// are checked in.
#[derive(Clone, Debug)]
pub(crate) struct SessionAuth {
    pub auth: Arc<Auth>,
    pub user_data: Option<Arc<Mutex<Store>>>,
}

impl SessionAuth {
    pub(crate) fn of(state: &ApiState) -> SessionAuth {
        SessionAuth {
            auth: state.auth.clone(),
            user_data: state.user_data.clone(),
        }
    }
}

/// Serves `routes` outside [`crate::router`] with what the guards check
/// against, as `crate::router` does for its own (tests and tools).
pub fn mount(routes: Router<ApiState>, state: ApiState) -> Router {
    routes
        .layer(axum::Extension(SessionAuth::of(&state)))
        .with_state(state)
}

/// Puts every route in `routes` behind a session: a module's protected
/// half goes through this. Without [`SessionAuth`] in the request (a
/// router not built by [`crate::router`]) it fails closed.
///
/// The wrong method on a protected path is a 405 only with a session:
/// the guard wraps the 405 fallback too, so a caller without one gets a
/// 401 and learns nothing about which methods the route takes.
pub fn protected(routes: Router<ApiState>) -> Router<ApiState> {
    routes
        .method_not_allowed_fallback(wrong_method)
        .route_layer(middleware::from_fn(guard))
}

async fn guard(req: Request, next: Next) -> Response {
    let Some(SessionAuth { auth, .. }) = req.extensions().get::<SessionAuth>().cloned() else {
        return internal("a protected route was mounted without the session guard's state")
            .into_response();
    };
    match check_session(&auth, req.headers(), peer_ip(req.extensions())) {
        Ok(()) => next.run(req).await,
        Err(e) => e.into_response(),
    }
}

/// Middleware for routes outside [`crate::router`]: a valid session, or
/// 401.
pub async fn require_session(State(s): State<ApiState>, req: Request, next: Next) -> Response {
    match check_session(&s.auth, req.headers(), peer_ip(req.extensions())) {
        Ok(()) => next.run(req).await,
        Err(e) => e.into_response(),
    }
}

pub(super) fn check_session(auth: &Auth, headers: &HeaderMap, peer: IpAddr) -> ApiResult<()> {
    if logged_in(auth, headers, peer) {
        Ok(())
    } else {
        Err(login_required(auth))
    }
}

pub(super) fn login_required(auth: &Auth) -> ApiError {
    ApiError::new(StatusCode::UNAUTHORIZED, "login_required", "Log in first.")
        .setup_required(auth.needs_setup())
}

/// A session cookie, or the trusted proxy's user header (`peer` is the
/// TCP peer).
pub(crate) fn logged_in(auth: &Auth, headers: &HeaderMap, peer: IpAddr) -> bool {
    headers
        .get(header::COOKIE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|c| auth.verify(c))
        || auth.proxy_signs_in(peer, headers)
}

/// `password` names the request field a password error is about.
pub(crate) fn auth_error(e: AuthError, password: &'static str) -> ApiError {
    match e {
        AuthError::SetupDone => ApiError::new(
            StatusCode::CONFLICT,
            "setup_done",
            "This instance is already set up.",
        ),
        AuthError::BadToken => ApiError::new(
            StatusCode::UNAUTHORIZED,
            "bad_token",
            "That setup token is wrong.",
        )
        .field("token"),
        AuthError::WeakPassword => ApiError::bad_request(
            "weak_password",
            format!("The password needs at least {MIN_PASSWORD_CHARS} characters."),
        )
        .field(password),
        AuthError::BadPassword => ApiError::new(
            StatusCode::UNAUTHORIZED,
            "bad_password",
            "That password is wrong.",
        )
        .field(password),
        AuthError::RateLimited => ApiError::new(
            StatusCode::TOO_MANY_REQUESTS,
            "rate_limited",
            "Too many attempts; wait a minute and try again.",
        ),
        AuthError::NotSetUp => ApiError::new(
            StatusCode::CONFLICT,
            "not_set_up",
            "This instance isn't set up yet.",
        ),
        AuthError::Storage(_) => internal(e),
    }
}

/// `{ "ok": true }` with a `Set-Cookie`.
fn with_cookie(cookie: String) -> Response {
    ([(header::SET_COOKIE, cookie)], axum::Json(OkResponse::OK)).into_response()
}

/// Whether the instance is set up and the caller logged in.
#[utoipa::path(
    operation_id = "getAuthStatus",
    get,
    path = "/auth",
    tag = "auth",
    responses((status = 200, body = AuthStatus))
)]
pub(crate) async fn auth_status(
    State(s): State<ApiState>,
    headers: HeaderMap,
    req: axum::http::Extensions,
) -> axum::Json<AuthStatus> {
    axum::Json(AuthStatus {
        logged_in: logged_in(&s.auth, &headers, peer_ip(&req)),
        setup_required: s.auth.needs_setup(),
    })
}

/// Claims a fresh instance with the setup token from the logs and a new
/// password; logs in.
#[utoipa::path(
    operation_id = "setup",
    post,
    path = "/setup",
    tag = "auth",
    request_body = SetupRequest,
    responses((
        status = 200,
        body = OkResponse,
        description = "Set up; the session cookie is set.",
        headers(("Set-Cookie" = String, description = "The session cookie."))
    ))
)]
pub(crate) async fn setup(
    State(s): State<ApiState>,
    ClientIp(ip): ClientIp,
    headers: HeaderMap,
    Json(body): Json<SetupRequest>,
) -> ApiResult<Response> {
    same_origin_json(&headers)?;
    let auth = s.auth.clone();
    // Argon2 is deliberately slow; keep it off the async workers.
    let cookie = tokio::task::spawn_blocking(move || auth.setup(ip, &body.token, &body.password))
        .await
        .map_err(internal)?
        .map_err(|e| auth_error(e, "password"))?;
    Ok(with_cookie(cookie))
}

/// Logs in; rate-limited per address.
#[utoipa::path(
    operation_id = "login",
    post,
    path = "/login",
    tag = "auth",
    request_body = LoginRequest,
    responses((
        status = 200,
        body = OkResponse,
        description = "Logged in; the session cookie is set.",
        headers(("Set-Cookie" = String, description = "The session cookie."))
    ))
)]
pub(crate) async fn login(
    State(s): State<ApiState>,
    ClientIp(ip): ClientIp,
    headers: HeaderMap,
    Json(body): Json<LoginRequest>,
) -> ApiResult<Response> {
    same_origin_json(&headers)?;
    let auth = s.auth.clone();
    let cookie = tokio::task::spawn_blocking(move || auth.login(ip, &body.password))
        .await
        .map_err(internal)?
        .map_err(|e| auth_error(e, "password"))?;
    Ok(with_cookie(cookie))
}

/// Clears the session cookie. The body is ignored but must be JSON.
#[utoipa::path(
    operation_id = "logout",
    post,
    path = "/logout",
    tag = "auth",
    request_body(content = Object, description = "Any JSON, such as `{}`."),
    responses((
        status = 200,
        body = OkResponse,
        headers(("Set-Cookie" = String, description = "Clears the session cookie."))
    ))
)]
pub(crate) async fn logout(State(s): State<ApiState>, headers: HeaderMap) -> ApiResult<Response> {
    same_origin_json(&headers)?;
    Ok(with_cookie(s.auth.logout_cookie()))
}

/// Changes the password and ends every other session.
#[utoipa::path(
    operation_id = "changePassword",
    post,
    path = "/password",
    tag = "auth",
    request_body = PasswordRequest,
    responses((
        status = 200,
        body = OkResponse,
        description = "Changed; a new session cookie is set.",
        headers(("Set-Cookie" = String, description = "The new session cookie."))
    ))
)]
pub(crate) async fn change_password(
    State(s): State<ApiState>,
    ClientIp(ip): ClientIp,
    headers: HeaderMap,
    Json(body): Json<PasswordRequest>,
) -> ApiResult<Response> {
    same_origin_json(&headers)?;
    let auth = s.auth.clone();
    let cookie =
        tokio::task::spawn_blocking(move || auth.change_password(ip, &body.old, &body.new))
            .await
            .map_err(internal)?
            .map_err(|e| match e {
                // The old password is the one that's wrong; the new one
                // is the one that's weak.
                AuthError::BadPassword => auth_error(e, "old"),
                e => auth_error(e, "new"),
            })?;
    Ok(with_cookie(cookie))
}

fn proxy_settings(auth: &Auth) -> ProxySettings {
    let c = auth.proxy();
    ProxySettings {
        allowed_users: c.allowed_users,
        enabled: c.enabled,
        forced_off: auth.proxy_forced_off(),
        header: c.header,
        trusted: c.trusted,
    }
}

/// The trusted reverse-proxy header settings (§12). `forced_off` says
/// `LODESTAR_PROXY_AUTH=off` turned them off for this run.
#[utoipa::path(
    operation_id = "getProxyAuth",
    get,
    path = "/auth/proxy",
    tag = "auth",
    responses((status = 200, body = ProxySettings))
)]
pub(crate) async fn get_proxy(State(s): State<ApiState>) -> axum::Json<ProxySettings> {
    axum::Json(proxy_settings(&s.auth))
}

/// Changes the trusted reverse-proxy header settings. Needs the password
/// (428 `confirm_required` without it).
#[utoipa::path(
    operation_id = "putProxyAuth",
    put,
    path = "/auth/proxy",
    tag = "auth",
    request_body = ProxyUpdate,
    responses((status = 200, body = ProxySettings))
)]
pub(crate) async fn put_proxy(
    State(s): State<ApiState>,
    ClientIp(ip): ClientIp,
    headers: HeaderMap,
    Json(body): Json<ProxyUpdate>,
) -> ApiResult<axum::Json<ProxySettings>> {
    same_origin_json(&headers)?;
    let ProxyUpdate {
        allowed_users,
        enabled,
        header,
        password,
        trusted,
    } = body;
    let config = ProxyConfig {
        enabled,
        trusted,
        header,
        allowed_users,
    };
    // A bad value is reported before the password is asked for.
    Proxy::compile(config.clone()).map_err(bad_proxy)?;
    confirm_fresh_password(&s.auth, ip, password).await?;
    // Saving writes auth.json under the auth lock: off the async workers.
    let auth = s.auth.clone();
    let saved = tokio::task::spawn_blocking(move || auth.set_proxy(config))
        .await
        .map_err(internal)?
        .map_err(|e| match e {
            SetProxyError::Invalid(e) => bad_proxy(e),
            SetProxyError::Storage(e) => internal(e),
        })?;
    // How many ranges are trusted, never who may sign in.
    lodestar_log::log_event!(
        super::log_events::AUTH_PROXY_CHANGED,
        client = lodestar_log::Client(ip),
        enabled = saved.enabled,
        trusted = super::log_events::count(saved.trusted.len())
    );
    Ok(axum::Json(proxy_settings(&s.auth)))
}

fn bad_proxy(e: super::ProxyError) -> ApiError {
    ApiError::bad_request("bad_proxy_settings", e.message).field(e.field)
}

/// Signs out every other browser: rotates the session key and sets a new
/// cookie for this one. Needs the password (428 `confirm_required`
/// without it).
#[utoipa::path(
    operation_id = "revokeOtherSessions",
    post,
    path = "/sessions/revoke-others",
    tag = "auth",
    request_body = ConfirmRequest,
    responses((
        status = 200,
        body = OkResponse,
        description = "Every other session ended; a new session cookie is set.",
        headers(("Set-Cookie" = String, description = "The new session cookie."))
    ))
)]
pub(crate) async fn revoke_other_sessions(
    State(s): State<ApiState>,
    ClientIp(ip): ClientIp,
    headers: HeaderMap,
    Json(body): Json<ConfirmRequest>,
) -> ApiResult<Response> {
    same_origin_json(&headers)?;
    confirm_fresh_password(&s.auth, ip, body.password).await?;
    let auth = s.auth.clone();
    let cookie = tokio::task::spawn_blocking(move || auth.rotate_sessions())
        .await
        .map_err(internal)?
        .map_err(|e| auth_error(e, "password"))?;
    lodestar_log::log_event!(
        super::log_events::AUTH_SESSIONS_REVOKED,
        client = lodestar_log::Client(ip)
    );
    Ok(with_cookie(cookie))
}
