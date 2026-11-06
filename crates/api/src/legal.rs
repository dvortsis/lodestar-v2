//! The Legal Notice (owner decision §2.38): its version and text, built
//! into the binary, and the two routes the setup screen and the sign-in
//! gate use. Where acceptance is stored and enforced is [`crate::auth`].
//!
//! - **`NOTICE_VERSION`** is raised only when the notice changes in
//!   substance. Raising it shows the notice again to every admin at their
//!   next request; an editorial change that leaves it alone never does.
//!   It matches the "Version N" line at the top of the text (a test checks).
//! - **The text** is `docs/legal/legal-notice.md` on this branch, copied
//!   here so the crate builds without the docs folder (the Docker build
//!   copies only `crates/`). A test keeps the two the same.

use axum::extract::State;
use axum::http::HeaderMap;

use crate::ApiState;
use crate::auth::auth_error;
use crate::dto::auth::{AcceptNoticeRequest, LegalNotice, OkResponse};
use crate::error::{ApiResult, internal};
use crate::extract::{ClientIp, Json, same_origin_json};

/// The Legal Notice's version. See the module docs before changing it.
pub const NOTICE_VERSION: u32 = 4;

/// The Legal Notice, as Markdown.
pub const NOTICE: &str = include_str!("legal/notice.md");

/// The Legal Notice this build shows, for the setup screen, the sign-in
/// gate and Settings → About. Open: it's shown before there's a session.
#[utoipa::path(
    operation_id = "getLegalNotice",
    get,
    path = "/legal/notice",
    tag = "auth",
    responses((status = 200, body = LegalNotice))
)]
pub(crate) async fn get_notice(State(s): State<ApiState>) -> axum::Json<LegalNotice> {
    axum::Json(LegalNotice {
        version: s.auth.notice_version(),
        text: NOTICE.to_string(),
    })
}

/// Records that the signed-in admin accepted the Legal Notice `version`
/// (the current one; 409 `notice_required` for any other). The one
/// session route the notice gate lets through. Accepting again is a
/// no-op.
#[utoipa::path(
    operation_id = "acceptLegalNotice",
    post,
    path = "/legal/accept",
    tag = "auth",
    request_body = AcceptNoticeRequest,
    responses((status = 200, body = OkResponse))
)]
pub(crate) async fn accept_notice(
    State(s): State<ApiState>,
    ClientIp(ip): ClientIp,
    headers: HeaderMap,
    Json(body): Json<AcceptNoticeRequest>,
) -> ApiResult<axum::Json<OkResponse>> {
    same_origin_json(&headers)?;
    // Saving writes auth.json under the auth lock: off the async workers.
    let auth = s.auth.clone();
    let version = body.version;
    tokio::task::spawn_blocking(move || auth.accept_notice(version))
        .await
        .map_err(internal)?
        .map_err(|e| auth_error(e, "version"))?;
    // The audit line: who (the client address) and which version; the
    // record carries the time. Never the session or the cookie.
    crate::auth::log_events::notice_accepted(ip, version);
    Ok(axum::Json(OkResponse::OK))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The heading's "Version N" is the constant: the screen and the API
    /// never disagree about which notice was accepted.
    #[test]
    fn the_text_states_this_version() {
        let line = NOTICE
            .lines()
            .find(|l| l.starts_with("**Version "))
            .expect("a version line");
        assert!(
            line.starts_with(&format!("**Version {NOTICE_VERSION} ·")),
            "{line}"
        );
    }

    /// The built-in copy is the published one.
    #[test]
    fn the_text_is_the_published_copy() {
        let docs = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../docs/legal/legal-notice.md"
        );
        // The crate built outside the repository (Docker copies `crates/`
        // only) has no docs to compare; the repository's CI does.
        if let Ok(published) = std::fs::read_to_string(docs) {
            assert_eq!(
                NOTICE.replace("\r\n", "\n"),
                published.replace("\r\n", "\n"),
                "copy docs/legal/legal-notice.md to crates/api/src/legal/notice.md"
            );
        }
    }

    /// Only what the in-app renderer handles: no raw HTML, images, tables
    /// or numbered lists, which it would show as text.
    #[test]
    fn the_text_uses_only_what_the_screen_renders() {
        for line in NOTICE.lines() {
            let t = line.trim_start();
            assert!(!t.starts_with('<'), "raw HTML: {line}");
            assert!(!t.starts_with('|'), "a table: {line}");
            assert!(!t.contains("!["), "an image: {line}");
            assert!(
                !t.split_once(". ")
                    .is_some_and(|(n, _)| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit())),
                "a numbered list: {line}"
            );
        }
    }
}
