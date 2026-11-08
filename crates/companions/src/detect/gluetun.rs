//! gluetun's control server (port 8000), found without a credential.
//!
//! - **`GET /v1/vpn/status`** answers `200` and `{"status":"running"}` (or
//!   `starting`, `stopping`, `stopped`, `crashed`, `completed`: the VPN
//!   loop's states, `internal/models`) when a role lets the request in.
//!   Since v3.39.1 every route needs one unless you configured `auth =
//!   "none"`, so without a credential it answers `401` and
//!   `Unauthorized\n` (Go's `http.Error`, from
//!   `internal/server/middlewares/auth`).
//! - **A path outside its routes** answers `404` and `Not Found\n` from the
//!   same middleware, before any role is checked. `GET /` is one.
//!
//! So: a status answer is gluetun; a `401` with that exact body is gluetun
//! only if `/` gives that exact `404` too.
//!
//! **Never the public-IP route** (`/v1/publicip/ip`): your public address
//! is personal data, and the status route says what health needs. Health is
//! `running` or not; a `401` is [`HealthStatus::Unhealthy`], because a VPN
//! that can't be seen to be up must not be taken as up (it carries the
//! kill-switch's traffic, §18.10). The control server reports no version.

use crate::adapter::{
    AdapterError, BoxFuture, Detect, Detected, GuardedHttp, Health, HealthStatus, Target,
};
use crate::inputs::CompanionId;

use super::{get, health_of, not_found_or, status_field};

/// Its catalogue id.
pub const ID: &str = "gluetun";

const STATUS_PATH: &str = "/v1/vpn/status";
const NOT_A_ROUTE: &str = "/";
const UNAUTHORIZED: &[u8] = b"Unauthorized\n";
const NOT_FOUND: &[u8] = b"Not Found\n";

/// The VPN's state, as a closed value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Vpn {
    /// `running`.
    Up,
    /// `starting`, `stopping`, `stopped`, `crashed` or `completed`.
    Down,
}

/// `/v1/vpn/status`'s body, or `None` for anything but a known state.
pub fn parse_status(body: &[u8]) -> Option<Vpn> {
    match status_field(body)?.as_str() {
        "running" => Some(Vpn::Up),
        "starting" | "stopping" | "stopped" | "crashed" | "completed" => Some(Vpn::Down),
        _ => None,
    }
}

/// The gluetun adapter.
#[derive(Debug)]
pub struct Gluetun {
    id: CompanionId,
}

impl Gluetun {
    pub(crate) fn new(id: CompanionId) -> Gluetun {
        Gluetun { id }
    }
}

impl Detect for Gluetun {
    fn companion(&self) -> &CompanionId {
        &self.id
    }

    fn detect<'a>(
        &'a self,
        http: &'a dyn GuardedHttp,
        target: &'a Target,
    ) -> BoxFuture<'a, Result<Option<Detected>, AdapterError>> {
        Box::pin(async move {
            let found = Ok(Some(Detected {
                target: target.clone(),
                version: None,
            }));
            let r = match get(http, target, STATUS_PATH).await {
                Ok(r) => r,
                Err(e) => return not_found_or(e),
            };
            match r.status() {
                200 if parse_status(r.body()).is_some() => found,
                401 if r.body() == UNAUTHORIZED => match get(http, target, NOT_A_ROUTE).await {
                    Ok(p) if p.status() == 404 && p.body() == NOT_FOUND => found,
                    Ok(_) => Ok(None),
                    Err(e) => not_found_or(e),
                },
                _ => Ok(None),
            }
        })
    }
}

impl Health for Gluetun {
    fn companion(&self) -> &CompanionId {
        &self.id
    }

    fn check<'a>(
        &'a self,
        http: &'a dyn GuardedHttp,
        target: &'a Target,
    ) -> BoxFuture<'a, HealthStatus> {
        Box::pin(async move {
            match get(http, target, STATUS_PATH).await {
                Ok(r) if r.status() == 200 && parse_status(r.body()) == Some(Vpn::Up) => {
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
    fn states() {
        assert_eq!(parse_status(b"{\"status\":\"running\"}\n"), Some(Vpn::Up));
        for down in ["starting", "stopping", "stopped", "crashed", "completed"] {
            let body = format!("{{\"status\":\"{down}\"}}\n");
            assert_eq!(parse_status(body.as_bytes()), Some(Vpn::Down), "{down}");
        }
        for bad in [
            &b""[..],
            b"running",
            b"{\"status\":\"Running\"}",
            b"{\"status\":\"up\"}",
            b"{\"public_ip\":\"203.0.113.9\"}",
            b"{\"status\":\"running\"}{}",
            b"Unauthorized\n",
        ] {
            assert_eq!(
                parse_status(bad),
                None,
                "{:?}",
                String::from_utf8_lossy(bad)
            );
        }
    }

    proptest! {
        #[test]
        fn status_parser_is_total(body in proptest::collection::vec(any::<u8>(), 0..128)) {
            let _ = parse_status(&body);
        }

        #[test]
        fn only_running_is_up(s in "[a-z]{1,12}") {
            let body = format!(r#"{{"status":"{s}"}}"#);
            prop_assert_eq!(parse_status(body.as_bytes()) == Some(Vpn::Up), s == "running");
        }
    }
}
