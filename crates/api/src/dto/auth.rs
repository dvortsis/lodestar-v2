//! `/auth`, `/setup`, `/login`, `/logout` and `/password` (§12).

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
        ];
        for s in shown {
            assert!(!s.contains(secret), "{s}");
            assert!(s.contains(REDACTED), "{s}");
        }
    }
}
