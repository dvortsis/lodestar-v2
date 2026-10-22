//! `/auth`, `/setup`, `/login`, `/logout` and `/password` (§12).

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

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
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct SetupRequest {
    /// The one-time token printed to the logs (`XXXX-XXXX-XXXX-XXXX`, any
    /// case).
    pub token: String,
    /// At least 10 characters.
    pub password: String,
}

/// `POST /login`.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct LoginRequest {
    pub password: String,
}

/// `POST /password`: ends every other session.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct PasswordRequest {
    pub old: String,
    /// At least 10 characters.
    pub new: String,
}
