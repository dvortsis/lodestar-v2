//! The typed API contract (architecture §9): every request and response
//! body of `/api/v1`, one file per domain, each with a utoipa schema for
//! `/openapi.json` and the web client's generated types.
//!
//! **Field names and nullability** follow `docs/metrics.md` §7, the UI
//! field contract. A field that is "nullable" there is always present and
//! may be `null`: an `Option` with `#[schema(required = true)]`. A field
//! that may be absent is `skip_serializing_if = "Option::is_none"` with
//! `#[schema(nullable = false)]`.
//!
//! **Field order is alphabetical** in every response struct. The handlers
//! used to build `serde_json::Value`s, whose keys serialize sorted, and
//! the golden tests (`tests/golden.rs`) hold the typed responses to those
//! bytes. Keep new fields in order too; the tests catch a slip.

pub mod auth;
pub mod compare;
pub mod discover;
pub mod error;
pub mod events;
pub mod facets;
pub mod filters;
pub mod search;
pub mod settings;
pub mod suggest;
pub mod torrents;

/// What a secret (a password, the setup token) shows as in `Debug`
/// output, so one never reaches a log.
pub(crate) const REDACTED: &str = "<redacted>";

/// Serializes through [`serde_json::Value`], so a type whose own field
/// order isn't alphabetical (the query AST) keeps the sorted keys it had
/// inside a `json!` body.
pub(crate) fn sorted<T: serde::Serialize, S: serde::Serializer>(
    value: &T,
    s: S,
) -> Result<S::Ok, S::Error> {
    use serde::Serialize as _;
    serde_json::to_value(value)
        .map_err(serde::ser::Error::custom)?
        .serialize(s)
}

/// [`sorted`] for an `Option` field (skipped when `None`).
pub(crate) fn sorted_some<T: serde::Serialize, S: serde::Serializer>(
    value: &Option<T>,
    s: S,
) -> Result<S::Ok, S::Error> {
    sorted(value, s)
}
