//! The one error shape every `/api/v1` endpoint returns (§9):
//!
//! ```json
//! { "error": { "code": "unknown_sort", "message": "…", "field": "sort" } }
//! ```
//!
//! `code` is a stable snake_case identifier for clients to branch on;
//! `message` is a sentence for people. `field` names the offending
//! parameter, and `start`/`end` give the byte span of a query syntax
//! error so the UI can highlight it.

use std::borrow::Cow;

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::{Map, Value, json};

/// An API error: a status plus the envelope's fields.
#[derive(Debug)]
pub struct ApiError {
    status: StatusCode,
    code: &'static str,
    message: Cow<'static, str>,
    field: Option<Cow<'static, str>>,
    span: Option<(usize, usize)>,
    /// Top-level keys beside `error` (the 401's `setup_required`).
    extra: Map<String, Value>,
}

pub type ApiResult<T> = Result<T, ApiError>;

impl ApiError {
    pub fn new(
        status: StatusCode,
        code: &'static str,
        message: impl Into<Cow<'static, str>>,
    ) -> ApiError {
        debug_assert!(
            !code.is_empty()
                && code
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_'),
            "error codes are snake_case: {code:?}"
        );
        ApiError {
            status,
            code,
            message: message.into(),
            field: None,
            span: None,
            extra: Map::new(),
        }
    }

    pub fn bad_request(code: &'static str, message: impl Into<Cow<'static, str>>) -> ApiError {
        ApiError::new(StatusCode::BAD_REQUEST, code, message)
    }

    /// The request parameter or body field at fault.
    pub fn field(mut self, name: impl Into<Cow<'static, str>>) -> ApiError {
        self.field = Some(name.into());
        self
    }

    /// The byte span of the fault within `field`.
    pub fn span(mut self, start: usize, end: usize) -> ApiError {
        self.span = Some((start, end));
        self
    }

    /// A top-level key beside `error`, for context a client needs to
    /// recover (keep these rare).
    pub fn with(mut self, key: &str, value: impl Into<Value>) -> ApiError {
        self.extra.insert(key.to_string(), value.into());
        self
    }

    pub fn status(&self) -> StatusCode {
        self.status
    }

    pub fn code(&self) -> &'static str {
        self.code
    }

    /// The response body: the envelope plus any top-level keys.
    pub fn body(&self) -> Value {
        let mut error = Map::new();
        error.insert("code".into(), self.code.into());
        error.insert("message".into(), json!(self.message));
        if let Some(field) = &self.field {
            error.insert("field".into(), json!(field));
        }
        if let Some((start, end)) = self.span {
            error.insert("start".into(), start.into());
            error.insert("end".into(), end.into());
        }
        let mut body = self.extra.clone();
        body.insert("error".into(), error.into());
        body.into()
    }
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} {}: {}",
            self.status.as_u16(),
            self.code,
            self.message
        )
    }
}

impl std::error::Error for ApiError {}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.status, Json(self.body())).into_response()
    }
}

/// Logs `e` and returns a 500 that reveals nothing about it.
pub fn internal(e: impl std::fmt::Display) -> ApiError {
    tracing::error!("api: {e}");
    ApiError::new(
        StatusCode::INTERNAL_SERVER_ERROR,
        "internal",
        "Something went wrong on the server.",
    )
}

/// The `/api/v1` fallback: an unknown path.
pub async fn no_route() -> ApiError {
    ApiError::new(
        StatusCode::NOT_FOUND,
        "no_route",
        "There's no such endpoint.",
    )
}

/// The `/api/v1` fallback for a known path with the wrong method.
pub async fn wrong_method() -> ApiError {
    ApiError::new(
        StatusCode::METHOD_NOT_ALLOWED,
        "method_not_allowed",
        "That endpoint doesn't take this method.",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_envelope_has_only_the_fields_set() {
        let e = ApiError::bad_request("bad_thing", "That's bad.");
        assert_eq!(
            e.body(),
            json!({ "error": { "code": "bad_thing", "message": "That's bad." } })
        );
        let e = ApiError::bad_request("query_syntax", "Nope.")
            .field("q")
            .span(2, 5)
            .with("hint", true);
        assert_eq!(
            e.body(),
            json!({
                "error": { "code": "query_syntax", "message": "Nope.", "field": "q", "start": 2, "end": 5 },
                "hint": true,
            })
        );
    }

    #[test]
    fn internal_errors_hide_the_cause() {
        let e = internal("disk on fire at /secret/path");
        assert_eq!(e.status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert!(!e.body().to_string().contains("secret"));
    }
}
