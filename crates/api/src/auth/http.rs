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
use serde::Deserialize;
use serde_json::{Value, json};

use super::{Auth, AuthError, MIN_PASSWORD_CHARS};
use crate::ApiState;
use crate::error::{ApiError, ApiResult, internal};
use crate::extract::{ClientIp, Json, same_origin_json};

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
pub fn protected(routes: Router<ApiState>) -> Router<ApiState> {
    routes.route_layer(middleware::from_fn(guard))
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
                .with("setup_required", auth.needs_setup()),
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
fn auth_error(e: AuthError, password: &'static str) -> ApiError {
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

fn with_cookie(cookie: String, body: Value) -> Response {
    ([(header::SET_COOKIE, cookie)], axum::Json(body)).into_response()
}

async fn auth_status(State(s): State<ApiState>, headers: HeaderMap) -> axum::Json<Value> {
    axum::Json(json!({
        "setup_required": s.auth.needs_setup(),
        "logged_in": logged_in(&s.auth, &headers),
    }))
}

#[derive(Deserialize)]
struct SetupBody {
    token: String,
    password: String,
}

async fn setup(
    State(s): State<ApiState>,
    ClientIp(ip): ClientIp,
    headers: HeaderMap,
    Json(body): Json<SetupBody>,
) -> ApiResult<Response> {
    same_origin_json(&headers)?;
    let auth = s.auth.clone();
    // Argon2 is deliberately slow; keep it off the async workers.
    let cookie = tokio::task::spawn_blocking(move || auth.setup(ip, &body.token, &body.password))
        .await
        .map_err(internal)?
        .map_err(|e| auth_error(e, "password"))?;
    tracing::info!("setup complete; the setup token is spent");
    Ok(with_cookie(cookie, json!({ "ok": true })))
}

#[derive(Deserialize)]
struct LoginBody {
    password: String,
}

async fn login(
    State(s): State<ApiState>,
    ClientIp(ip): ClientIp,
    headers: HeaderMap,
    Json(body): Json<LoginBody>,
) -> ApiResult<Response> {
    same_origin_json(&headers)?;
    let auth = s.auth.clone();
    let cookie = tokio::task::spawn_blocking(move || auth.login(ip, &body.password))
        .await
        .map_err(internal)?
        .map_err(|e| auth_error(e, "password"))?;
    Ok(with_cookie(cookie, json!({ "ok": true })))
}

async fn logout(State(s): State<ApiState>, headers: HeaderMap) -> ApiResult<Response> {
    same_origin_json(&headers)?;
    Ok(with_cookie(s.auth.logout_cookie(), json!({ "ok": true })))
}

#[derive(Deserialize)]
struct PasswordBody {
    old: String,
    new: String,
}

async fn change_password(
    State(s): State<ApiState>,
    ClientIp(ip): ClientIp,
    headers: HeaderMap,
    Json(body): Json<PasswordBody>,
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
    Ok(with_cookie(cookie, json!({ "ok": true })))
}
