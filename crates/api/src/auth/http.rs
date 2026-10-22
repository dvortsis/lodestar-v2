//! The auth endpoints (`/auth`, `/setup`, `/login`, `/logout` open;
//! `/password` behind a session) and the session guard every protected
//! route goes through.

use std::sync::Arc;

use axum::Router;
use axum::extract::{Request, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use utoipa::OpenApi;

use super::{Auth, AuthError, MIN_PASSWORD_CHARS};
use crate::ApiState;
use crate::dto::auth::{AuthStatus, LoginRequest, OkResponse, PasswordRequest, SetupRequest};
use crate::error::{ApiError, ApiResult, internal, wrong_method};
use crate::extract::{ClientIp, Json, same_origin_json};

/// This module's half of `/openapi.json`.
#[derive(OpenApi)]
#[openapi(paths(auth_status, setup, login, logout, change_password))]
pub(crate) struct ApiDoc;

pub fn routes() -> Router<ApiState> {
    let open = Router::new()
        .route("/auth", get(auth_status))
        .route("/setup", post(setup))
        .route("/login", post(login))
        .route("/logout", post(logout));
    let session = protected(Router::new().route("/password", post(change_password)));
    open.merge(session)
}

/// The [`Auth`] the session guard checks against; [`crate::router`] puts
/// it in every request's extensions.
#[derive(Clone, Debug)]
pub(crate) struct SessionAuth(pub Arc<Auth>);

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
    let Some(SessionAuth(auth)) = req.extensions().get::<SessionAuth>().cloned() else {
        return internal("a protected route was mounted without the session guard's state")
            .into_response();
    };
    match check_session(&auth, req.headers()) {
        Ok(()) => next.run(req).await,
        Err(e) => e.into_response(),
    }
}

/// Middleware for routes outside [`crate::router`]: a valid session, or
/// 401.
pub async fn require_session(State(s): State<ApiState>, req: Request, next: Next) -> Response {
    match check_session(&s.auth, req.headers()) {
        Ok(()) => next.run(req).await,
        Err(e) => e.into_response(),
    }
}

fn check_session(auth: &Auth, headers: &HeaderMap) -> ApiResult<()> {
    if logged_in(auth, headers) {
        Ok(())
    } else {
        Err(
            ApiError::new(StatusCode::UNAUTHORIZED, "login_required", "Log in first.")
                .setup_required(auth.needs_setup()),
        )
    }
}

fn logged_in(auth: &Auth, headers: &HeaderMap) -> bool {
    headers
        .get(header::COOKIE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|c| auth.verify(c))
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
) -> axum::Json<AuthStatus> {
    axum::Json(AuthStatus {
        logged_in: logged_in(&s.auth, &headers),
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
    tracing::info!("setup complete; the setup token is spent");
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
