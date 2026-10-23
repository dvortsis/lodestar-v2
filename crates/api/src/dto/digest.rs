//! `GET /digest/preview` (§10.9.7, `docs/metrics.md` §7.8): the digest as
//! it would be sent, rendered from the overview's numbers. JSON only; the
//! client shows `html` in a sandboxed `<iframe srcdoc>` and `text` in a
//! `<pre>`.

use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};

use super::insights::InsightsRange;

/// `GET /digest/preview`.
#[derive(Debug, Clone, Default, Deserialize, IntoParams)]
#[serde(default)]
#[into_params(parameter_in = Query)]
pub struct DigestQuery {
    /// `daily` (the last 24 hours) or `weekly` (the last 7 days, the
    /// default).
    pub period: Option<String>,
}

/// How much a digest covers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum DigestPeriod {
    /// The last 24 hours, against the 24 before.
    Daily,
    /// The last 7 days, against the 7 before.
    Weekly,
}

/// A digest as it would be sent now.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[schema(example = json!({
    "generated_at": 1_800_000_000,
    "html": "<!DOCTYPE html><html lang=\"en\"><head><meta charset=\"utf-8\"><title>Lodestar weekly digest: Jan 8, 2027 – Jan 15, 2027</title></head><body><h1>Lodestar weekly digest: Jan 8, 2027 – Jan 15, 2027</h1></body></html>",
    "period": "weekly",
    "range": { "bucket": "hour", "from": 1_799_395_200, "to": 1_800_000_000, "tz": "UTC" },
    "subject": "Lodestar weekly digest: Jan 8, 2027 – Jan 15, 2027",
    "text": "Lodestar weekly digest: Jan 8, 2027 – Jan 15, 2027\n"
}))]
pub struct DigestPreview {
    /// When the numbers were read (unix seconds): the overview's
    /// `updated_at`.
    pub generated_at: i64,
    /// A complete HTML document: no script, style, images or external
    /// links. Show it only in `<iframe sandbox srcdoc>` (no
    /// `allow-scripts`, no `allow-same-origin`).
    pub html: String,
    pub period: DigestPeriod,
    /// The overview range the digest read, as the server normalized it.
    pub range: InsightsRange,
    /// At most 120 characters.
    pub subject: String,
    /// Plain UTF-8, at most 4,000 bytes. Show it in a `<pre>`.
    pub text: String,
}
