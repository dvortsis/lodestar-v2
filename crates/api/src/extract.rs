//! Extractors shared by every module: `Json`, `Query` and `Path` that
//! reject in the error envelope (house-style messages, the bad field's
//! serde path in `field`, axum's status codes), the client IP, and the
//! CSRF check for writes (see the crate docs).

use std::net::{IpAddr, SocketAddr};

use axum::body::Bytes;
use axum::extract::rejection::PathRejection;
use axum::extract::{ConnectInfo, FromRequest, FromRequestParts, Request};
use axum::http::request::Parts;
use axum::http::{Extensions, HeaderMap, StatusCode, header};
use serde::de::DeserializeOwned;

use crate::auth::SessionAuth;
use crate::error::{ApiError, ApiResult};

/// A JSON body; a bad one is an [`ApiError`] whose `field` is the path
/// to the bad value (`ast.field`, `tags[2]`) when there is one.
#[derive(Debug, Clone, Copy, Default)]
pub struct Json<T>(pub T);

impl<S: Send + Sync, T: DeserializeOwned> FromRequest<S> for Json<T> {
    type Rejection = ApiError;

    async fn from_request(req: Request, state: &S) -> Result<Self, ApiError> {
        if !json_content_type(req.headers()) {
            return Err(not_json());
        }
        // Within axum's default body limit, as `axum::Json` is.
        let bytes = Bytes::from_request(req, state).await.map_err(|e| {
            if e.status() == StatusCode::PAYLOAD_TOO_LARGE {
                ApiError::new(
                    e.status(),
                    "body_too_large",
                    "The request body is too large.",
                )
            } else {
                ApiError::new(e.status(), "bad_body", "The request body couldn't be read.")
            }
        })?;
        let mut de = serde_json::Deserializer::from_slice(&bytes);
        let value = serde_path_to_error::deserialize(&mut de)
            .map_err(|e| (e.path().to_string(), e.into_inner()))
            // Trailing characters after the value, as `axum::Json` rejects.
            .and_then(|v| de.end().map(|()| v).map_err(|e| (".".to_string(), e)))
            .map_err(|(path, e)| {
                if e.classify() != serde_json::error::Category::Data {
                    // 400, as `axum::Json`'s syntax rejection.
                    return ApiError::bad_request("bad_json", "The request body isn't valid JSON.");
                }
                // 422, as `axum::Json`'s data rejection.
                let err = ApiError::new(
                    StatusCode::UNPROCESSABLE_ENTITY,
                    "bad_body",
                    "The request body isn't valid JSON for this endpoint.",
                );
                match bad_field(&path, &e.to_string()) {
                    Some(field) => err.field(field),
                    None => err,
                }
            })?;
        Ok(Json(value))
    }
}

/// The field a deserialization error is about: its path, or for a
/// missing field (whose path is the parent) the parent plus its name.
fn bad_field(path: &str, message: &str) -> Option<String> {
    let missing = message
        .strip_prefix("missing field `")
        .and_then(|m| m.split_once('`'))
        .map(|(name, _)| name);
    match (path, missing) {
        (".", Some(name)) => Some(name.to_string()),
        (".", None) => None,
        (path, Some(name)) => Some(format!("{path}.{name}")),
        (path, None) => Some(path.to_string()),
    }
}

/// `application/json` or `application/*+json`, as `axum::Json` accepts.
fn json_content_type(headers: &HeaderMap) -> bool {
    let Some(ct) = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
    else {
        return false;
    };
    let mime = ct
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    mime == "application/json"
        || mime
            .strip_prefix("application/")
            .is_some_and(|sub| sub.ends_with("+json"))
}

/// Query parameters; bad ones are an [`ApiError`] naming the parameter.
#[derive(Debug, Clone, Copy, Default)]
pub struct Query<T>(pub T);

impl<S: Send + Sync, T: DeserializeOwned> FromRequestParts<S> for Query<T> {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, _: &S) -> Result<Self, ApiError> {
        let query = parts.uri.query().unwrap_or("");
        let de = serde_urlencoded::Deserializer::new(form_urlencoded::parse(query.as_bytes()));
        serde_path_to_error::deserialize(de)
            .map(Query)
            .map_err(|e| {
                let path = e.path().to_string();
                let err = ApiError::bad_request(
                    "bad_parameter",
                    "A query parameter has the wrong type or value.",
                );
                match bad_field(&path, &e.into_inner().to_string()) {
                    Some(field) => err.field(field),
                    None => err,
                }
            })
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
            .map_err(|e: PathRejection| {
                ApiError::new(e.status(), "bad_path", "The request path isn't valid.")
            })
    }
}

/// The client's address, for per-client login and key limits: the TCP
/// peer (from `ConnectInfo`), or, when it's a trusted proxy and proxy
/// auth is on, the rightmost `X-Forwarded-For` address that isn't one
/// (see [`crate::auth::proxy`]). Outside [`crate::router`] it's the peer.
#[derive(Debug, Clone, Copy)]
pub struct ClientIp(pub IpAddr);

impl<S: Send + Sync> FromRequestParts<S> for ClientIp {
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(parts: &mut Parts, _: &S) -> Result<Self, Self::Rejection> {
        let peer = peer_ip(&parts.extensions);
        let ip = match parts.extensions.get::<SessionAuth>() {
            Some(s) => s.auth.client_ip(peer, &parts.headers),
            None => peer,
        };
        Ok(ClientIp(ip))
    }
}

/// The TCP peer's address (`0.0.0.0` without `ConnectInfo`).
pub(crate) fn peer_ip(extensions: &Extensions) -> IpAddr {
    extensions
        .get::<ConnectInfo<SocketAddr>>()
        .map_or(IpAddr::from([0, 0, 0, 0]), |c| c.0.ip())
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
