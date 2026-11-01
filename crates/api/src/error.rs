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
use serde_json::Value;

use crate::dto::error::{ErrorBody, ErrorEnvelope};
use crate::dto::filters::Assessment;

/// An API error: a status plus the envelope's fields.
#[derive(Debug)]
pub struct ApiError {
    status: StatusCode,
    code: &'static str,
    message: Cow<'static, str>,
    field: Option<Cow<'static, str>>,
    span: Option<(usize, usize)>,
    /// A stable code for the cause, when `code` has several.
    reason: Option<&'static str>,
    /// Top-level keys beside `error` (keep these rare).
    setup_required: Option<bool>,
    assessment: Option<Box<Assessment>>,
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
            reason: None,
            setup_required: None,
            assessment: None,
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

    /// The cause as a stable snake_case code (`error.reason`), for a
    /// `code` with several; the message names it too.
    pub fn reason(mut self, reason: &'static str) -> ApiError {
        self.reason = Some(reason);
        self
    }

    /// The 401's `setup_required` beside `error`: whether to send the
    /// client to first-run setup rather than the login form.
    pub fn setup_required(mut self, required: bool) -> ApiError {
        self.setup_required = Some(required);
        self
    }

    /// The 428's `assessment` beside `error`: the warning the client
    /// shows before asking for the password.
    pub fn assessment(mut self, assessment: Assessment) -> ApiError {
        self.assessment = Some(Box::new(assessment));
        self
    }

    pub fn status(&self) -> StatusCode {
        self.status
    }

    pub fn code(&self) -> &'static str {
        self.code
    }

    /// The envelope's `error` object.
    pub fn error_body(&self) -> ErrorBody {
        ErrorBody {
            code: self.code.to_string(),
            end: self.span.map(|(_, end)| end),
            field: self.field.as_deref().map(str::to_string),
            message: self.message.to_string(),
            reason: self.reason.map(str::to_string),
            start: self.span.map(|(start, _)| start),
        }
    }

    /// The response body: the envelope plus any top-level keys.
    pub fn envelope(&self) -> ErrorEnvelope {
        ErrorEnvelope {
            assessment: self.assessment.as_deref().cloned(),
            error: self.error_body(),
            setup_required: self.setup_required,
        }
    }

    /// [`ApiError::envelope`] as JSON.
    pub fn body(&self) -> Value {
        serde_json::to_value(self.envelope()).expect("the envelope serializes")
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
        (self.status, Json(self.envelope())).into_response()
    }
}

/// Logs `e` and returns a 500 that reveals nothing about it.
pub fn internal<E: std::fmt::Display>(e: E) -> ApiError {
    // Its type only: an error's message can name a torrent or a path.
    lodestar_log::log_event!(
        crate::log_events::API_INTERNAL_ERROR,
        error = lodestar_log::Error::new(&e)
    );
    ApiError::new(
        StatusCode::INTERNAL_SERVER_ERROR,
        "internal",
        "Something went wrong on the server.",
    )
}

/// The 501 an endpoint returns while it's a typed stub (see `README.md`):
/// its route and contract are served before its handler is written.
pub fn not_implemented() -> ApiError {
    ApiError::new(
        StatusCode::NOT_IMPLEMENTED,
        "not_implemented",
        "This endpoint isn't implemented yet.",
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
    use serde_json::json;

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
            .setup_required(true);
        assert_eq!(
            e.body(),
            json!({
                "error": { "code": "query_syntax", "message": "Nope.", "field": "q", "start": 2, "end": 5 },
                "setup_required": true,
            })
        );
        // Keys in the order a `Value` body always had them.
        let e = ApiError::new(
            StatusCode::PRECONDITION_REQUIRED,
            "confirm_required",
            "Confirm.",
        )
        .field("password")
        .assessment(Assessment::default())
        .setup_required(false);
        let text = serde_json::to_string(&e.envelope()).unwrap();
        assert_eq!(text, e.body().to_string());
    }

    #[test]
    fn stubs_are_501_in_the_envelope() {
        let e = not_implemented();
        assert_eq!(e.status(), StatusCode::NOT_IMPLEMENTED);
        assert_eq!(e.body()["error"]["code"], json!("not_implemented"));
    }

    #[test]
    fn internal_errors_hide_the_cause() {
        let e = internal("disk on fire at /secret/path");
        assert_eq!(e.status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert!(!e.body().to_string().contains("secret"));
    }
}
