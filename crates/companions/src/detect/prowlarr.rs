//! Prowlarr, found without its API key.
//!
//! - **`GET /ping`** is anonymous (`Prowlarr.Http/Ping/PingController.cs`,
//!   `[AllowAnonymous]`): `200` and `{"status": "OK"}` when its database
//!   answers, `500` and `{"status": "Error"}` when it doesn't. Sonarr,
//!   Radarr and the other *arr programs answer `/ping` the same way, so it
//!   can't tell them apart.
//! - **`GET /login`** is the login page (`frontend/src/login.html`, served
//!   by `LoginHtmlMapper` whatever the authentication setting). Its title,
//!   `<title>Login - Prowlarr</title>`, names the program.
//!
//! Detection needs both. **Never `/initialize.json`:** it's behind the UI
//! policy and carries the API key, which detection must not read. Neither
//! endpoint reports a version without the key, so [`Detected::version`] is
//! `None`. Health is `/ping`.

use crate::adapter::{
    AdapterError, BoxFuture, Detect, Detected, GuardedHttp, Health, HealthStatus, Target,
};
use crate::inputs::CompanionId;

use super::{get, health_of, not_found_or, status_field};

/// Its catalogue id.
pub const ID: &str = "prowlarr";

pub(crate) const PING_PATH: &str = "/ping";
const LOGIN_PATH: &str = "/login";
const LOGIN_MARKER: &str = "<title>Login - Prowlarr</title>";

/// What `/ping` said, as a closed value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ping {
    /// `{"status": "OK"}`.
    Ok,
    /// `{"status": "Error"}`: running, but its database didn't answer.
    Error,
}

/// `/ping`'s body, or `None` for anything but the two answers above.
pub fn parse_ping(body: &[u8]) -> Option<Ping> {
    match status_field(body)?.as_str() {
        "OK" => Some(Ping::Ok),
        "Error" => Some(Ping::Error),
        _ => None,
    }
}

/// Whether a page is Prowlarr's login page.
pub fn is_login_page(body: &[u8]) -> bool {
    std::str::from_utf8(body).is_ok_and(|s| s.contains(LOGIN_MARKER))
}

/// The Prowlarr adapter.
#[derive(Debug)]
pub struct Prowlarr {
    id: CompanionId,
}

impl Prowlarr {
    pub(crate) fn new(id: CompanionId) -> Prowlarr {
        Prowlarr { id }
    }
}

impl Detect for Prowlarr {
    fn companion(&self) -> &CompanionId {
        &self.id
    }

    fn detect<'a>(
        &'a self,
        http: &'a dyn GuardedHttp,
        target: &'a Target,
    ) -> BoxFuture<'a, Result<Option<Detected>, AdapterError>> {
        Box::pin(async move {
            let ping = match get(http, target, PING_PATH).await {
                Ok(r) => r,
                Err(e) => return not_found_or(e, target),
            };
            if !matches!(ping.status(), 200 | 500) || parse_ping(ping.body()).is_none() {
                return Ok(None);
            }
            let page = match get(http, target, LOGIN_PATH).await {
                Ok(r) => r,
                Err(e) => return not_found_or(e, target),
            };
            if page.status() == 200 && is_login_page(page.body()) {
                Ok(Some(Detected {
                    target: target.clone(),
                    version: None,
                }))
            } else {
                Ok(None)
            }
        })
    }
}

impl Health for Prowlarr {
    fn companion(&self) -> &CompanionId {
        &self.id
    }

    fn check<'a>(
        &'a self,
        http: &'a dyn GuardedHttp,
        target: &'a Target,
    ) -> BoxFuture<'a, HealthStatus> {
        Box::pin(async move {
            match get(http, target, PING_PATH).await {
                Ok(r) if r.status() == 200 && parse_ping(r.body()) == Some(Ping::Ok) => {
                    HealthStatus::Healthy
                }
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
    fn ping_answers() {
        assert_eq!(parse_ping(b"{\n  \"status\": \"OK\"\n}"), Some(Ping::Ok));
        assert_eq!(parse_ping(br#"{"status":"Error"}"#), Some(Ping::Error));
        assert_eq!(
            parse_ping(br#"{"x":[1,{"y":2}],"status":"OK"}"#),
            Some(Ping::Ok)
        );
        for bad in [
            &b""[..],
            b"OK",
            b"{}",
            b"[]",
            br#"{"status":"ok"}"#,
            br#"{"Status":"OK"}"#,
            br#"{"status":1}"#,
            br#"{"status":"OK"} x"#,
            br#"{"status":"OK","status":"OK"}"#,
            br#"{"st\u0061tus":"Error","status":"OK"}"#,
            br#"{"status":"OK""#,
            br#"{"status":"OK",}"#,
            b"{\"status\":\"\xff\"}",
            b"{\"status\":\"O\x01K\"}",
        ] {
            assert_eq!(parse_ping(bad), None, "{:?}", String::from_utf8_lossy(bad));
        }
        let long = format!(r#"{{"status":"{}"}}"#, "A".repeat(10_000));
        assert_eq!(parse_ping(long.as_bytes()), None);
        let deep = format!(
            r#"{{"x":{}1{},"status":"OK"}}"#,
            "[".repeat(100),
            "]".repeat(100)
        );
        assert_eq!(parse_ping(deep.as_bytes()), None);
    }

    #[test]
    fn login_page() {
        assert!(is_login_page(b"<head><title>Login - Prowlarr</title>"));
        assert!(!is_login_page(b"<title>Login - Sonarr</title>"));
        assert!(!is_login_page(b"\xff<title>Login - Prowlarr</title>"));
    }

    proptest! {
        #[test]
        fn ping_parser_is_total(body in proptest::collection::vec(any::<u8>(), 0..128)) {
            let _ = parse_ping(&body);
            let _ = is_login_page(&body);
        }

        #[test]
        fn ping_parser_ignores_other_keys(key in "[a-r]{1,8}", n in 0u32..1000) {
            let body = format!(r#"{{"{key}":{n},"status":"OK"}}"#);
            prop_assert_eq!(parse_ping(body.as_bytes()), Some(Ping::Ok));
        }
    }
}
