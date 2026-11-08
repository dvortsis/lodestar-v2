//! qBittorrent's WebUI (API v2), found without a credential.
//!
//! - **`GET /api/v2/app/version`.** With a session, or with the WebUI's
//!   authentication bypassed for this address, it answers `200` and
//!   `v<version>` (`v4.6.3`, `v5.0.3`). Without one it answers `403` and
//!   `Forbidden` (recorded from 4.6.3, `crates/integrations/tests/fixtures/
//!   qbittorrent/forbidden_no_cookie.json`).
//! - **`GET /`** without a session serves the login page,
//!   `src/webui/www/public/index.html` upstream. Its title and headings
//!   are translated, so the marker is two things every release from 4.6.3
//!   through 5.0.3 and `master` carries untranslated: the form
//!   `id="loginform"` and the logo `images/qbittorrent-tray.svg`.
//!
//! So: a version answer is qBittorrent with its version; a `403` is
//! qBittorrent only if the login page is there too. Detection never logs
//! in. Health is the version probe: either answer is a live WebUI.

use crate::adapter::{
    AdapterError, BoxFuture, Detect, Detected, GuardedHttp, Health, HealthStatus, Target,
};
use crate::inputs::{CompanionId, Version};

use super::{get, health_of, not_found_or};

/// Its catalogue id.
pub const ID: &str = "qbittorrent";

const VERSION_PATH: &str = "/api/v2/app/version";
const LOGIN_PATH: &str = "/";
/// The two markers of the login page, both required.
const LOGIN_MARKERS: [&str; 2] = ["id=\"loginform\"", "images/qbittorrent-tray.svg"];

/// The qBittorrent adapter.
#[derive(Debug)]
pub struct QBittorrent {
    id: CompanionId,
}

impl QBittorrent {
    pub(crate) fn new(id: CompanionId) -> QBittorrent {
        QBittorrent { id }
    }
}

/// The version endpoint's answer: `v` and a [`Version`], nothing else (one
/// trailing newline is tolerated). Anything else, including a version
/// outside the strict grammar, is `None`.
pub fn parse_version(body: &[u8]) -> Option<Version> {
    let s = std::str::from_utf8(body).ok()?;
    let s = s.strip_suffix('\n').unwrap_or(s);
    Version::parse(s.strip_prefix('v')?).ok()
}

/// Whether a page is qBittorrent's login page.
pub fn is_login_page(body: &[u8]) -> bool {
    std::str::from_utf8(body).is_ok_and(|s| LOGIN_MARKERS.iter().all(|m| s.contains(m)))
}

/// `403 Forbidden`, as the version endpoint answers without a session.
fn forbidden(status: u16, body: &[u8]) -> bool {
    status == 403 && body == b"Forbidden"
}

impl Detect for QBittorrent {
    fn companion(&self) -> &CompanionId {
        &self.id
    }

    fn detect<'a>(
        &'a self,
        http: &'a dyn GuardedHttp,
        target: &'a Target,
    ) -> BoxFuture<'a, Result<Option<Detected>, AdapterError>> {
        Box::pin(async move {
            let r = match get(http, target, VERSION_PATH).await {
                Ok(r) => r,
                Err(e) => return not_found_or(e, target),
            };
            let found = |version| {
                Ok(Some(Detected {
                    target: target.clone(),
                    version,
                }))
            };
            match r.status() {
                200 => match parse_version(r.body()) {
                    Some(v) => found(Some(v)),
                    None => Ok(None),
                },
                403 => {
                    let page = match get(http, target, LOGIN_PATH).await {
                        Ok(p) => p,
                        Err(e) => return not_found_or(e, target),
                    };
                    if page.status() == 200 && is_login_page(page.body()) {
                        found(None)
                    } else {
                        Ok(None)
                    }
                }
                _ => Ok(None),
            }
        })
    }
}

impl Health for QBittorrent {
    fn companion(&self) -> &CompanionId {
        &self.id
    }

    fn check<'a>(
        &'a self,
        http: &'a dyn GuardedHttp,
        target: &'a Target,
    ) -> BoxFuture<'a, HealthStatus> {
        Box::pin(async move {
            match get(http, target, VERSION_PATH).await {
                Ok(r) if r.status() == 200 && parse_version(r.body()).is_some() => {
                    HealthStatus::Healthy
                }
                Ok(r) if forbidden(r.status(), r.body()) => HealthStatus::Healthy,
                Ok(_) => HealthStatus::Unhealthy,
                Err(e) => health_of(e),
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn versions() {
        assert_eq!(parse_version(b"v4.6.3").unwrap().as_str(), "4.6.3");
        assert_eq!(parse_version(b"v5.0.3\n").unwrap().as_str(), "5.0.3");
        for bad in [
            &b""[..],
            b"4.6.3",
            b"v",
            b"v5.0.3beta1",
            b"v5.0.3\n\n",
            b" v5.0.3",
            b"v5.0.3\r\n",
            b"v05.0",
            b"v1.2.3.4.5",
            b"v\xff",
            b"v5.0.3\x1b[31m",
            b"Forbidden",
        ] {
            assert!(parse_version(bad).is_none(), "{bad:?}");
        }
        assert!(parse_version(format!("v{}", "9".repeat(10_000)).as_bytes()).is_none());
    }

    #[test]
    fn login_page_needs_both_markers() {
        assert!(is_login_page(
            br#"<form id="loginform"><img src="images/qbittorrent-tray.svg">"#
        ));
        assert!(!is_login_page(br#"<form id="loginform">"#));
        assert!(!is_login_page(b"<title>qBittorrent WebUI</title>"));
        assert!(!is_login_page(
            b"\xff id=\"loginform\" images/qbittorrent-tray.svg"
        ));
        assert!(forbidden(403, b"Forbidden"));
        assert!(!forbidden(403, b"Forbidden\n"));
        assert!(!forbidden(401, b"Forbidden"));
    }

    proptest! {
        /// Any body: no panic, and a version only in the strict form.
        #[test]
        fn version_parser_is_total(body in proptest::collection::vec(any::<u8>(), 0..64)) {
            if let Some(v) = parse_version(&body) {
                prop_assert!(v.as_str().len() <= 39);
                prop_assert!(v.as_str().bytes().all(|b| b.is_ascii_digit() || b == b'.'));
            }
            let _ = is_login_page(&body);
        }

        /// Every well-formed version round-trips.
        #[test]
        fn versions_round_trip(parts in proptest::collection::vec(0u32..1_000_000, 1..=4)) {
            let s = parts.iter().map(u32::to_string).collect::<Vec<_>>().join(".");
            let body = format!("v{s}");
            prop_assert_eq!(parse_version(body.as_bytes()).map(|v| v.as_str().to_owned()), Some(s));
        }
    }
}
