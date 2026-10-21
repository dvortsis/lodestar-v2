//! The HTTP API (architecture §9, §12): `/api/v1`, JSON, behind a
//! session cookie. M2 serves auth, query parsing, search and torrent
//! detail; facets, suggest, SSE and the rest arrive with their features.
//!
//! CSRF: the session cookie is `SameSite=Strict`, every request that
//! changes state must be `application/json` (which a cross-site form
//! can't send without a CORS preflight we never grant), and a present
//! `Origin` must match `Host`.

pub mod auth;
mod torrents;

use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use axum::extract::{ConnectInfo, Query, Request, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use lodestar_search::query::Node;
use lodestar_search::{SearchIndex, Sort, parse};
use lodestar_store::Store;
use serde::Deserialize;
use serde_json::{Value, json};

pub use auth::Auth;

/// Results per page, at most.
pub const MAX_LIMIT: usize = 200;
/// How deep offset paging may go; past this, narrow the query (stable
/// cursors arrive with searcher leases, §8).
pub const MAX_WINDOW: usize = 10_000;

#[derive(Clone, Debug)]
pub struct ApiState {
    pub auth: Arc<Auth>,
    /// `None` when search is turned off.
    pub index: Option<SearchIndex>,
    /// A read-only connection for detail and enrichment.
    pub reader: Arc<Mutex<Store>>,
}

/// An API error: a status and a JSON body.
#[derive(Debug)]
pub struct ApiError {
    status: StatusCode,
    body: Value,
}

impl ApiError {
    pub fn new(status: StatusCode, message: impl Into<String>) -> ApiError {
        ApiError {
            status,
            body: json!({ "error": message.into() }),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.status, Json(self.body)).into_response()
    }
}

type ApiResult<T> = Result<T, ApiError>;

fn internal(e: impl std::fmt::Display) -> ApiError {
    tracing::error!("api: {e}");
    ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "internal error")
}

/// The `/api/v1` routes: auth endpoints are open, everything else needs a
/// session. The server must be run with `into_make_service_with_connect_info`.
pub fn router(state: ApiState) -> Router {
    let protected = Router::new()
        .route("/parse", get(parse_query))
        .route("/search", get(search_get).post(search_post))
        .route("/torrents/{hash}", get(torrents::detail))
        .route("/torrents/{hash}/files", get(torrents::files))
        .route("/password", post(change_password))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            require_session,
        ));
    Router::new()
        .route("/auth", get(auth_status))
        .route("/setup", post(setup))
        .route("/login", post(login))
        .route("/logout", post(logout))
        .merge(protected)
        .with_state(state)
}

/// Middleware: a valid session, or 401.
pub async fn require_session(State(s): State<ApiState>, req: Request, next: Next) -> Response {
    let ok = req
        .headers()
        .get(header::COOKIE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|c| s.auth.verify(c));
    if ok {
        next.run(req).await
    } else {
        (
            StatusCode::UNAUTHORIZED,
            Json(json!({ "error": "login required", "setup_required": s.auth.needs_setup() })),
        )
            .into_response()
    }
}

/// Rejects cross-site writes (see the module docs).
fn same_origin_json(headers: &HeaderMap) -> ApiResult<()> {
    let json = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|ct| {
            ct.split(';')
                .next()
                .is_some_and(|t| t.trim().eq_ignore_ascii_case("application/json"))
        });
    if !json {
        return Err(ApiError::new(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "send application/json",
        ));
    }
    if let Some(origin) = headers.get(header::ORIGIN).and_then(|v| v.to_str().ok()) {
        let host = headers
            .get(header::HOST)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        let origin_host = origin.split_once("://").map_or(origin, |(_, h)| h);
        if origin_host != host {
            return Err(ApiError::new(StatusCode::FORBIDDEN, "cross-origin request"));
        }
    }
    Ok(())
}

/// The peer address (from `ConnectInfo`), for per-IP login limits.
/// Behind a reverse proxy this is the proxy; trusted-proxy headers come
/// with proxy auth (§12).
#[derive(Debug, Clone, Copy)]
pub struct ClientIp(pub IpAddr);

impl<S: Send + Sync> axum::extract::FromRequestParts<S> for ClientIp {
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(
        parts: &mut axum::http::request::Parts,
        _: &S,
    ) -> Result<Self, Self::Rejection> {
        let ip = parts
            .extensions
            .get::<ConnectInfo<SocketAddr>>()
            .map_or(IpAddr::from([0, 0, 0, 0]), |c| c.0.ip());
        Ok(ClientIp(ip))
    }
}

fn auth_error(e: auth::AuthError) -> ApiError {
    use auth::AuthError::*;
    let status = match e {
        SetupDone => StatusCode::CONFLICT,
        BadToken | BadPassword => StatusCode::UNAUTHORIZED,
        WeakPassword => StatusCode::BAD_REQUEST,
        RateLimited => StatusCode::TOO_MANY_REQUESTS,
        NotSetUp => StatusCode::CONFLICT,
        Storage(_) => return internal(e),
    };
    ApiError::new(status, e.to_string())
}

fn with_cookie(cookie: String, body: Value) -> Response {
    ([(header::SET_COOKIE, cookie)], Json(body)).into_response()
}

async fn auth_status(State(s): State<ApiState>, headers: HeaderMap) -> Json<Value> {
    let logged_in = headers
        .get(header::COOKIE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|c| s.auth.verify(c));
    Json(json!({ "setup_required": s.auth.needs_setup(), "logged_in": logged_in }))
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
        .map_err(auth_error)?;
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
        .map_err(auth_error)?;
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
            .map_err(auth_error)?;
    Ok(with_cookie(cookie, json!({ "ok": true })))
}

#[derive(Deserialize)]
struct QParam {
    #[serde(default)]
    q: String,
}

fn parse_error(e: lodestar_search::ParseError) -> ApiError {
    ApiError {
        status: StatusCode::BAD_REQUEST,
        body: json!({ "error": e.message, "start": e.start, "end": e.end }),
    }
}

/// `GET /parse?q=` → the AST and its canonical text, or the error with
/// its span (the UI highlights it).
async fn parse_query(Query(p): Query<QParam>) -> ApiResult<Json<Value>> {
    let ast = parse(&p.q).map_err(parse_error)?;
    Ok(Json(json!({ "ast": ast, "canonical": ast.to_query() })))
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct SearchParams {
    q: Option<String>,
    ast: Option<Node>,
    sort: Option<String>,
    limit: Option<usize>,
    offset: Option<usize>,
}

fn sort_param(s: Option<&str>) -> ApiResult<Sort> {
    Ok(match s.unwrap_or("relevance") {
        "relevance" => Sort::Relevance,
        "discovered" | "-discovered" => Sort::Discovered { desc: true },
        "+discovered" => Sort::Discovered { desc: false },
        "size" | "-size" => Sort::Size { desc: true },
        "+size" => Sort::Size { desc: false },
        "files" | "-files" => Sort::Files { desc: true },
        "+files" => Sort::Files { desc: false },
        other => {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                format!(
                    "unknown sort {other:?} (relevance, discovered, size, files; +/- for order)"
                ),
            ));
        }
    })
}

async fn search_get(
    State(s): State<ApiState>,
    Query(p): Query<SearchParams>,
) -> ApiResult<Json<Value>> {
    if p.ast.is_some() {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "send an AST with POST",
        ));
    }
    run_search(s, p).await
}

async fn search_post(
    State(s): State<ApiState>,
    headers: HeaderMap,
    Json(p): Json<SearchParams>,
) -> ApiResult<Json<Value>> {
    same_origin_json(&headers)?;
    run_search(s, p).await
}

async fn run_search(s: ApiState, p: SearchParams) -> ApiResult<Json<Value>> {
    let Some(index) = s.index.clone() else {
        return Err(ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "search is turned off",
        ));
    };
    let ast = match (p.ast, p.q) {
        (Some(ast), None) => {
            // An AST from a client must be one the parser could produce.
            let text = ast.to_query();
            match parse(&text) {
                Ok(again) if again == ast => ast,
                _ => return Err(ApiError::new(StatusCode::BAD_REQUEST, "invalid query AST")),
            }
        }
        (None, q) => parse(q.as_deref().unwrap_or("")).map_err(parse_error)?,
        (Some(_), Some(_)) => {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "send q or ast, not both",
            ));
        }
    };
    let sort = sort_param(p.sort.as_deref())?;
    let limit = p.limit.unwrap_or(50).clamp(1, MAX_LIMIT);
    let offset = p.offset.unwrap_or(0);
    if offset + limit > MAX_WINDOW {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            format!("results past {MAX_WINDOW} aren't paged; narrow the query"),
        ));
    }
    let reader = s.reader.clone();
    tokio::task::spawn_blocking(move || {
        let started = Instant::now();
        let query = index.query(&ast, unix_now());
        let results = index
            .search_page(&query, sort, limit, offset)
            .map_err(internal)?;
        let store = reader.lock().expect("reader lock");
        let mut hits = Vec::with_capacity(results.hits.len());
        for h in &results.hits {
            // A torrent deleted since indexing just drops out of the page.
            if let Some(row) = store.torrent(h.id as i64).map_err(internal)? {
                let mut v = torrents::summary(&row);
                v["score"] = json!(h.score);
                hits.push(v);
            }
        }
        Ok(Json(json!({
            "total": results.total,
            "offset": offset,
            "limit": limit,
            "took_ms": started.elapsed().as_secs_f64() * 1000.0,
            "canonical": ast.to_query(),
            "hits": hits,
        })))
    })
    .await
    .map_err(internal)?
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}
