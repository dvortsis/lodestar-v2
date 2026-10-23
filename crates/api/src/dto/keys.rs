//! `/keys`: API keys and feed tokens (§12).
//!
//! A key's secret appears once, in [`CreatedKey`], when it's made or
//! regenerated. Only its SHA-256 is kept, so it can never be shown again.

use std::fmt;

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use super::REDACTED;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum KeyKind {
    /// Torznab and integrations; its scopes say what it reaches.
    ApiKey,
    /// Read-only RSS: one saved search's feed, or every feed.
    FeedToken,
}

/// What a key may reach (see `keys::scope`). A route opts in to one.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, ToSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum KeyScope {
    /// The search API, read-only.
    Search,
    /// The Torznab endpoint for *arr apps.
    Torznab,
    /// Every RSS feed.
    Feeds,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum KeyStatus {
    Active,
    Expired,
    Revoked,
}

/// A key or token as listed: never its secret or hash.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[schema(example = json!({
    "created_at": 1_700_000_000,
    "expires_at": null,
    "id": 3,
    "kind": "api_key",
    "last_used_at": 1_700_000_600,
    "name": "Prowlarr",
    "prefix": "lsk_Q7f2bX9a",
    "revoked_at": null,
    "saved_search_id": null,
    "scopes": ["torznab"],
    "status": "active"
}))]
pub struct KeyInfo {
    /// Unix seconds.
    pub created_at: i64,
    /// Unix seconds; null: never expires.
    #[schema(required = true)]
    pub expires_at: Option<i64>,
    pub id: i64,
    pub kind: KeyKind,
    /// Unix seconds, written at most once a minute; null: never used.
    #[schema(required = true)]
    pub last_used_at: Option<i64>,
    pub name: String,
    /// The secret's public start, to tell keys apart.
    pub prefix: String,
    /// Unix seconds; null: not revoked.
    #[schema(required = true)]
    pub revoked_at: Option<i64>,
    /// A feed token's saved search; null: every feed (or an API key).
    #[schema(required = true)]
    pub saved_search_id: Option<i64>,
    pub scopes: Vec<KeyScope>,
    pub status: KeyStatus,
}

/// `GET /keys`, oldest first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct KeyList {
    pub keys: Vec<KeyInfo>,
}

/// `POST /keys`. `Debug` redacts the password.
#[derive(Clone, Serialize, Deserialize, ToSchema)]
pub struct CreateKeyRequest {
    /// Unix seconds, in the future; absent: never expires.
    #[serde(default)]
    #[schema(nullable = false)]
    pub expires_at: Option<i64>,
    pub kind: KeyKind,
    /// 1–200 characters.
    pub name: String,
    /// The admin password (428 `confirm_required` without it).
    #[serde(default)]
    #[schema(nullable = false)]
    pub password: Option<String>,
    /// A feed token's saved search; absent: every feed. API keys take
    /// none.
    #[serde(default)]
    #[schema(nullable = false)]
    pub saved_search_id: Option<i64>,
    /// An API key's scopes, at least one. A feed token takes none (it
    /// reads feeds only).
    #[serde(default)]
    pub scopes: Vec<KeyScope>,
}

/// `POST /keys` and `POST /keys/{id}/regenerate`: the key and, this once,
/// its secret. `Debug` redacts the secret.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct CreatedKey {
    pub key: KeyInfo,
    /// The whole secret (`lsk_…` or `lsf_…`). Shown only now: Lodestar
    /// keeps its hash, not the secret.
    pub secret: String,
}

impl fmt::Debug for CreateKeyRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CreateKeyRequest")
            .field("expires_at", &self.expires_at)
            .field("kind", &self.kind)
            .field("name", &self.name)
            .field("password", &REDACTED)
            .field("saved_search_id", &self.saved_search_id)
            .field("scopes", &self.scopes)
            .finish()
    }
}

impl fmt::Debug for CreatedKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CreatedKey")
            .field("key", &self.key)
            .field("secret", &REDACTED)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_never_shows_the_secrets() {
        let secret = "lsk_Q7f2bX9a_hunter2hunter2";
        let key = KeyInfo {
            created_at: 0,
            expires_at: None,
            id: 1,
            kind: KeyKind::ApiKey,
            last_used_at: None,
            name: "n".into(),
            prefix: "lsk_Q7f2bX9a".into(),
            revoked_at: None,
            saved_search_id: None,
            scopes: vec![KeyScope::Torznab],
            status: KeyStatus::Active,
        };
        let shown = [
            format!(
                "{:?}",
                CreatedKey {
                    key,
                    secret: secret.into()
                }
            ),
            format!(
                "{:?}",
                CreateKeyRequest {
                    expires_at: None,
                    kind: KeyKind::ApiKey,
                    name: "n".into(),
                    password: Some(secret.into()),
                    saved_search_id: None,
                    scopes: vec![],
                }
            ),
        ];
        for s in shown {
            assert!(!s.contains("hunter2"), "{s}");
            assert!(s.contains(REDACTED), "{s}");
        }
    }

    #[test]
    fn wire_names() {
        assert_eq!(
            serde_json::to_string(&KeyKind::FeedToken).unwrap(),
            "\"feed_token\""
        );
        assert_eq!(
            serde_json::to_string(&KeyScope::Torznab).unwrap(),
            "\"torznab\""
        );
        assert_eq!(
            serde_json::to_string(&KeyStatus::Revoked).unwrap(),
            "\"revoked\""
        );
    }
}
