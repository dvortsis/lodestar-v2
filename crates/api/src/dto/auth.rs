//! `/auth`, `/setup`, `/login`, `/logout`, `/password`, `/auth/proxy` and
//! `/sessions/revoke-others` (§12).

use std::fmt;

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use super::REDACTED;

/// `GET /auth`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct AuthStatus {
    pub logged_in: bool,
    /// No password yet: claim the instance with the setup token from the
    /// logs.
    pub setup_required: bool,
}

/// The body of a write that has nothing else to say.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct OkResponse {
    /// Always `true`.
    pub ok: bool,
}

impl OkResponse {
    pub const OK: OkResponse = OkResponse { ok: true };
}

/// `POST /setup`: claims the instance.
/// `Debug` redacts the secrets.
#[derive(Clone, Serialize, Deserialize, ToSchema)]
pub struct SetupRequest {
    /// The one-time token printed to the logs (`XXXX-XXXX-XXXX-XXXX`, any
    /// case).
    pub token: String,
    /// At least 10 characters.
    pub password: String,
}

/// `POST /login`.
/// `Debug` redacts the secrets.
#[derive(Clone, Serialize, Deserialize, ToSchema)]
pub struct LoginRequest {
    pub password: String,
}

/// `POST /password`: ends every other session.
/// `Debug` redacts the secrets.
#[derive(Clone, Serialize, Deserialize, ToSchema)]
pub struct PasswordRequest {
    pub old: String,
    /// At least 10 characters.
    pub new: String,
}

/// `POST /sessions/revoke-others`, and any other change that only needs
/// the password (§20.11). `Debug` redacts it.
#[derive(Clone, Default, Serialize, Deserialize, ToSchema)]
pub struct ConfirmRequest {
    /// The admin password, every time. Without it the answer is 428
    /// `confirm_required`.
    #[serde(default)]
    #[schema(nullable = false)]
    pub password: Option<String>,
}

/// `GET` and `PUT /auth/proxy`: trusted reverse-proxy header auth.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[schema(example = json!({
    "allowed_users": [],
    "enabled": true,
    "forced_off": false,
    "header": "Remote-User",
    "trusted": ["172.17.0.1/32"]
}))]
pub struct ProxySettings {
    /// Empty: any user the proxy names.
    pub allowed_users: Vec<String>,
    pub enabled: bool,
    /// `LODESTAR_PROXY_AUTH=off` turned proxy auth off for this run; the
    /// saved settings shown here don't apply until it's removed.
    pub forced_off: bool,
    /// The header the proxy names the signed-in user in.
    pub header: String,
    /// The proxies whose header is believed, as canonical CIDRs.
    pub trusted: Vec<String>,
}

/// `PUT /auth/proxy`. `Debug` redacts the password.
#[derive(Clone, Serialize, Deserialize, ToSchema)]
pub struct ProxyUpdate {
    /// At most 64 user names; empty allows any.
    #[serde(default)]
    pub allowed_users: Vec<String>,
    pub enabled: bool,
    /// Defaults to `Remote-User`.
    #[serde(default = "default_header")]
    pub header: String,
    /// The admin password (428 `confirm_required` without it).
    #[serde(default)]
    #[schema(nullable = false)]
    pub password: Option<String>,
    /// IP addresses or CIDR ranges, at most 64. Required to turn it on.
    #[serde(default)]
    pub trusted: Vec<String>,
}

fn default_header() -> String {
    crate::auth::proxy::DEFAULT_USER_HEADER.to_string()
}

impl fmt::Debug for ConfirmRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ConfirmRequest")
            .field("password", &REDACTED)
            .finish()
    }
}

impl fmt::Debug for ProxyUpdate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ProxyUpdate")
            .field("allowed_users", &self.allowed_users.len())
            .field("enabled", &self.enabled)
            .field("header", &self.header)
            .field("password", &REDACTED)
            .field("trusted", &self.trusted)
            .finish()
    }
}

impl fmt::Debug for SetupRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SetupRequest")
            .field("token", &REDACTED)
            .field("password", &REDACTED)
            .finish()
    }
}

impl fmt::Debug for LoginRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LoginRequest")
            .field("password", &REDACTED)
            .finish()
    }
}

impl fmt::Debug for PasswordRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PasswordRequest")
            .field("old", &REDACTED)
            .field("new", &REDACTED)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_never_shows_the_secrets() {
        let secret = "hunter2-correct-horse";
        let shown = [
            format!(
                "{:?}",
                SetupRequest {
                    token: secret.into(),
                    password: secret.into(),
                }
            ),
            format!(
                "{:?}",
                LoginRequest {
                    password: secret.into(),
                }
            ),
            format!(
                "{:?}",
                PasswordRequest {
                    old: secret.into(),
                    new: secret.into(),
                }
            ),
            format!(
                "{:?}",
                ConfirmRequest {
                    password: Some(secret.into()),
                }
            ),
            format!(
                "{:?}",
                ProxyUpdate {
                    allowed_users: vec![secret.into()],
                    enabled: true,
                    header: "Remote-User".into(),
                    password: Some(secret.into()),
                    trusted: vec![],
                }
            ),
        ];
        for s in shown {
            assert!(!s.contains(secret), "{s}");
            assert!(s.contains(REDACTED), "{s}");
        }
    }
}
