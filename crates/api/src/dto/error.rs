//! The error envelope every `/api/v1` failure returns (see
//! [`crate::error`]).

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use super::filters::Assessment;

/// `{ "error": { … } }`, plus the rare top-level key a client needs to
/// recover.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct ErrorEnvelope {
    /// On a 428 from `POST /filters/apply`: the warning to show before
    /// asking for the password.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        serialize_with = "super::sorted_some"
    )]
    #[schema(nullable = false)]
    pub assessment: Option<Assessment>,
    pub error: ErrorBody,
    /// On a 401: whether the instance still needs its first-run setup.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub setup_required: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ErrorBody {
    /// A stable snake_case identifier to branch on.
    #[schema(example = "unknown_sort")]
    pub code: String,
    /// The end of the fault's byte span within `field` (query syntax
    /// errors).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub end: Option<usize>,
    /// The request parameter or body field at fault (`sort`, `ast.field`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub field: Option<String>,
    /// A sentence for people.
    pub message: String,
    /// The start of the fault's byte span within `field`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub start: Option<usize>,
}
