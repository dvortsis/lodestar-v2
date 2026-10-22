//! Extractors shared by every module: axum's `Json`, `Query` and `Path`
//! with their rejections in the error envelope, the client IP, and the
//! CSRF check for writes (see the crate docs).

use std::net::{IpAddr, SocketAddr};

use axum::extract::rejection::{JsonRejection, PathRejection, QueryRejection};
use axum::extract::{ConnectInfo, FromRequest, FromRequestParts, Request};
use axum::http::request::Parts;
use axum::http::{HeaderMap, StatusCode, header};
use serde::de::DeserializeOwned;

use crate::error::{ApiError, ApiResult};

/// A JSON body; a bad one is an [`ApiError`].
#[derive(Debug, Clone, Copy, Default)]
pub struct Json<T>(pub T);

impl<S: Send + Sync, T: DeserializeOwned> FromRequest<S> for Json<T> {
    type Rejection = ApiError;

    async fn from_request(req: Request, state: &S) -> Result<Self, ApiError> {
        match axum::Json::<T>::from_request(req, state).await {
            Ok(axum::Json(v)) => Ok(Json(v)),
            Err(JsonRejection::MissingJsonContentType(_)) => Err(not_json()),
            Err(e) => Err(ApiError::new(e.status(), "bad_body", e.body_text())),
        }
    }
}

/// Query parameters; bad ones are an [`ApiError`].
#[derive(Debug, Clone, Copy, Default)]
pub struct Query<T>(pub T);

impl<S: Send + Sync, T: DeserializeOwned> FromRequestParts<S> for Query<T> {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, ApiError> {
        axum::extract::Query::<T>::from_request_parts(parts, state)
            .await
            .map(|axum::extract::Query(v)| Query(v))
            .map_err(|e: QueryRejection| ApiError::new(e.status(), "bad_parameter", e.body_text()))
    }
}

/// Path parameters; bad ones are an [`ApiError`].
#[derive(Debug, Clone, Copy, Default)]
pub struct Path<T>(pub T);

impl<S: Send + Sync, T: DeserializeOwned + Send> FromRequestParts<S> for Path<T> {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, ApiError> {
        axum::extract::Path::<T>::from_request_parts(parts, state)
            .await
            .map(|axum::extract::Path(v)| Path(v))
            .map_err(|e: PathRejection| ApiError::new(e.status(), "bad_path", e.body_text()))
    }
}

/// The peer address (from `ConnectInfo`), for per-IP login limits.
/// Behind a reverse proxy this is the proxy; trusted-proxy headers come
/// with proxy auth (§12).
#[derive(Debug, Clone, Copy)]
pub struct ClientIp(pub IpAddr);

impl<S: Send + Sync> FromRequestParts<S> for ClientIp {
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(parts: &mut Parts, _: &S) -> Result<Self, Self::Rejection> {
        let ip = parts
            .extensions
            .get::<ConnectInfo<SocketAddr>>()
            .map_or(IpAddr::from([0, 0, 0, 0]), |c| c.0.ip());
        Ok(ClientIp(ip))
    }
}

fn not_json() -> ApiError {
    ApiError::new(
        StatusCode::UNSUPPORTED_MEDIA_TYPE,
        "unsupported_media_type",
        "Send the request body as application/json.",
    )
}

/// Rejects cross-site writes (see the crate docs).
pub fn same_origin_json(headers: &HeaderMap) -> ApiResult<()> {
    let json = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|ct| {
            ct.split(';')
                .next()
                .is_some_and(|t| t.trim().eq_ignore_ascii_case("application/json"))
        });
    if !json {
        return Err(not_json());
    }
    if let Some(origin) = headers.get(header::ORIGIN).and_then(|v| v.to_str().ok()) {
        let host = headers
            .get(header::HOST)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        let origin_host = origin.split_once("://").map_or(origin, |(_, h)| h);
        if origin_host != host {
            return Err(ApiError::new(
                StatusCode::FORBIDDEN,
                "cross_origin",
                "Cross-origin requests aren't allowed.",
            ));
        }
    }
    Ok(())
}
