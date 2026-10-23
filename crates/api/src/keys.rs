//! API keys and feed tokens (§12): making their secrets, and `/keys`.
//!
//! **The format:** `lsk_<prefix>_<secret>` for an API key and
//! `lsf_<prefix>_<secret>` for a feed token, 56 characters:
//! - `<prefix>`: 8 random characters of `A-Z a-z 0-9`. With the tag
//!   (`lsk_Q7f2bX9a`) it's the public prefix the store finds the row by
//!   and the list shows. It's shown, so it doesn't count towards the
//!   secret's strength.
//! - `<secret>`: 32 bytes (256 bits) from the operating system's RNG, as
//!   43 characters of unpadded base64url.
//!
//! The store gets the public prefix and the SHA-256 of the whole string,
//! never the string (see `lodestar_store::api_keys`). The secret is in
//! the create or regenerate response once, and nowhere else: not in the
//! list, not in a log. "Reveal" can't exist, so the Security screen
//! offers Regenerate instead.
//!
//! **Endpoints** (session only, same-origin JSON): `GET /keys`,
//! `POST /keys` and `POST /keys/{id}/regenerate` (both need the fresh
//! password, §20.11), `POST /keys/{id}/revoke` and `DELETE /keys/{id}`
//! (neither does: they only take access away).
//!
//! **Presenting one** is up to the routes that opt in, through
//! [`crate::auth::key_or_session`] and [`crate::auth::feed_token`].

use std::sync::{Arc, Mutex, MutexGuard};

use axum::Router;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use lodestar_store::{ApiKey, Error as StoreError, KeyKind as StoreKind, NewApiKey, Store};
use rand::TryRng;
use sha2::{Digest, Sha256};
use utoipa::OpenApi;

use crate::ApiState;
use crate::auth::{confirm_fresh_password, protected};
use crate::dto::auth::{ConfirmRequest, OkResponse};
use crate::dto::keys::{
    CreateKeyRequest, CreatedKey, KeyInfo, KeyKind, KeyList, KeyScope, KeyStatus,
};
use crate::error::{ApiError, ApiResult, internal};
use crate::extract::{ClientIp, Json, Path, same_origin_json};

/// The scope bits stored with a key (`api_keys.scopes`). A route opts in
/// to one ([`crate::auth::key_or_session`]); a key reaches it only with
/// that bit set. Bits are never reused for something else.
pub mod scope {
    /// The JSON search API, read-only (`/search`, `/parse`, `/facets`,
    /// torrent detail), for integrations.
    pub const SEARCH: u32 = 1 << 0;
    /// The Torznab endpoint, for Prowlarr, Sonarr, Radarr and the rest.
    pub const TORZNAB: u32 = 1 << 1;
    /// Every RSS feed. A feed token always has it, and only it.
    pub const FEEDS: u32 = 1 << 2;
    /// Every bit defined.
    pub const ALL: u32 = SEARCH | TORZNAB | FEEDS;
}

pub const API_KEY_TAG: &str = "lsk_";
pub const FEED_TOKEN_TAG: &str = "lsf_";
/// Random characters in the public prefix, after the tag.
const PREFIX_CHARS: usize = 8;
const SECRET_BYTES: usize = 32;
/// `SECRET_BYTES` as unpadded base64url.
const SECRET_CHARS: usize = 43;
/// The public prefix: the tag and the random characters.
const PREFIX_LEN: usize = 4 + PREFIX_CHARS;
/// A whole key or token.
pub const SECRET_LEN: usize = PREFIX_LEN + 1 + SECRET_CHARS;
/// At most this many keys and tokens, revoked ones included.
pub const MAX_KEYS: usize = 500;
const MAX_NAME_CHARS: usize = 200;

const BASE62: &[u8; 62] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
const BASE64URL: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

/// A whole secret. `Debug` redacts it; read it with [`Secret::expose`].
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(crate::dto::REDACTED)
    }
}

/// A new secret, its public prefix and its hash. `Debug` shows only the
/// prefix.
pub struct Minted {
    pub secret: Secret,
    pub prefix: String,
    pub hash: [u8; 32],
}

/// A presented secret, split. `Debug` shows only the kind and prefix.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Presented<'a> {
    pub kind: StoreKind,
    pub prefix: &'a str,
    pub hash: [u8; 32],
}

/// Nothing derived from a secret beyond its public prefix reaches a log
/// line (§12.1), not even the hash.
impl std::fmt::Debug for Minted {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Minted")
            .field("secret", &self.secret)
            .field("prefix", &self.prefix)
            .field("hash", &crate::dto::REDACTED)
            .finish()
    }
}

impl std::fmt::Debug for Presented<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Presented")
            .field("kind", &self.kind)
            .field("prefix", &self.prefix)
            .field("hash", &crate::dto::REDACTED)
            .finish()
    }
}

fn tag(kind: StoreKind) -> &'static str {
    match kind {
        StoreKind::ApiKey => API_KEY_TAG,
        StoreKind::FeedToken => FEED_TOKEN_TAG,
    }
}

/// Makes a new secret from the operating system's RNG.
pub fn mint(kind: StoreKind) -> std::io::Result<Minted> {
    let mut rng = rand::rngs::SysRng;
    let mut prefix = String::with_capacity(PREFIX_LEN);
    prefix.push_str(tag(kind));
    // Rejection sampling, so every character is equally likely.
    while prefix.len() < PREFIX_LEN {
        let mut b = [0u8; 16];
        rng.try_fill_bytes(&mut b).map_err(std::io::Error::other)?;
        for x in b {
            if usize::from(x) < 248 && prefix.len() < PREFIX_LEN {
                prefix.push(char::from(BASE62[usize::from(x) % 62]));
            }
        }
    }
    let mut raw = [0u8; SECRET_BYTES];
    rng.try_fill_bytes(&mut raw)
        .map_err(std::io::Error::other)?;
    let secret = format!("{prefix}_{}", base64url(&raw));
    debug_assert_eq!(secret.len(), SECRET_LEN);
    let hash = sha256(&secret);
    Ok(Minted {
        secret: Secret(secret),
        prefix,
        hash,
    })
}

/// Splits a presented secret, or `None` if it isn't one in the format.
pub fn parse(presented: &str) -> Option<Presented<'_>> {
    if presented.len() != SECRET_LEN {
        return None;
    }
    let kind = if presented.starts_with(API_KEY_TAG) {
        StoreKind::ApiKey
    } else if presented.starts_with(FEED_TOKEN_TAG) {
        StoreKind::FeedToken
    } else {
        return None;
    };
    let b = presented.as_bytes();
    let random = &b[4..PREFIX_LEN];
    let secret = &b[PREFIX_LEN + 1..];
    if b[PREFIX_LEN] != b'_'
        || !random.iter().all(u8::is_ascii_alphanumeric)
        || !secret.iter().all(|c| BASE64URL.contains(c))
    {
        return None;
    }
    Some(Presented {
        kind,
        prefix: &presented[..PREFIX_LEN],
        hash: sha256(presented),
    })
}

fn sha256(s: &str) -> [u8; 32] {
    Sha256::digest(s.as_bytes()).into()
}

fn base64url(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = chunk
            .iter()
            .enumerate()
            .fold(0u32, |acc, (i, &b)| acc | (u32::from(b) << (16 - 8 * i)));
        for i in 0..=chunk.len() {
            out.push(char::from(BASE64URL[((n >> (18 - 6 * i)) & 63) as usize]));
        }
    }
    out
}

pub(crate) fn scopes_of(bits: u32) -> Vec<KeyScope> {
    [
        (scope::SEARCH, KeyScope::Search),
        (scope::TORZNAB, KeyScope::Torznab),
        (scope::FEEDS, KeyScope::Feeds),
    ]
    .into_iter()
    .filter(|(b, _)| bits & b != 0)
    .map(|(_, s)| s)
    .collect()
}

fn bits_of(scopes: &[KeyScope]) -> u32 {
    scopes.iter().fold(0, |acc, s| {
        acc | match s {
            KeyScope::Search => scope::SEARCH,
            KeyScope::Torznab => scope::TORZNAB,
            KeyScope::Feeds => scope::FEEDS,
        }
    })
}

fn info(k: &ApiKey, now: i64) -> KeyInfo {
    KeyInfo {
        created_at: k.created_at,
        expires_at: k.expires_at,
        id: k.id,
        kind: match k.kind {
            StoreKind::ApiKey => KeyKind::ApiKey,
            StoreKind::FeedToken => KeyKind::FeedToken,
        },
        last_used_at: k.last_used_at,
        name: k.name.clone(),
        prefix: k.prefix.clone(),
        revoked_at: k.revoked_at,
        saved_search_id: k.saved_search_id,
        scopes: scopes_of(k.scopes),
        status: status(k, now),
    }
}

fn status(k: &ApiKey, now: i64) -> KeyStatus {
    if k.revoked_at.is_some() {
        KeyStatus::Revoked
    } else if k.expires_at.is_some_and(|e| e <= now) {
        KeyStatus::Expired
    } else {
        KeyStatus::Active
    }
}

/// This module's half of `/openapi.json`.
#[derive(OpenApi)]
#[openapi(paths(list_keys, create_key, regenerate_key, revoke_key, delete_key))]
pub(crate) struct ApiDoc;

pub fn routes() -> Router<ApiState> {
    protected(
        Router::new()
            .route("/keys", get(list_keys).post(create_key))
            .route("/keys/{id}", axum::routing::delete(delete_key))
            .route("/keys/{id}/regenerate", post(regenerate_key))
            .route("/keys/{id}/revoke", post(revoke_key)),
    )
}

fn user_data(s: &ApiState) -> ApiResult<Arc<Mutex<Store>>> {
    s.user_data.clone().ok_or_else(|| {
        ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "keys_unavailable",
            "API keys aren't available.",
        )
    })
}

pub(crate) fn lock(store: &Mutex<Store>) -> ApiResult<MutexGuard<'_, Store>> {
    store
        .lock()
        .map_err(|_| internal("the user-data store's lock is poisoned"))
}

/// Runs `f` with the user-data store on the blocking pool.
async fn with_store<T: Send + 'static>(
    s: &ApiState,
    f: impl FnOnce(&mut Store) -> ApiResult<T> + Send + 'static,
) -> ApiResult<T> {
    let store = user_data(s)?;
    tokio::task::spawn_blocking(move || f(&mut *lock(&store)?))
        .await
        .map_err(internal)?
}

fn not_found() -> ApiError {
    ApiError::new(
        StatusCode::NOT_FOUND,
        "key_not_found",
        "There's no such key or token.",
    )
}

fn store_error(e: StoreError) -> ApiError {
    match e {
        StoreError::NotFound("saved search") => ApiError::new(
            StatusCode::NOT_FOUND,
            "saved_search_not_found",
            "There's no such saved search.",
        )
        .field("saved_search_id"),
        e => internal(e),
    }
}

/// What a new key will be, checked before the password is asked for.
struct Spec {
    kind: StoreKind,
    name: String,
    scopes: u32,
    saved_search_id: Option<i64>,
    expires_at: Option<i64>,
}

fn check(body: &CreateKeyRequest, now: i64) -> ApiResult<Spec> {
    let name = body.name.trim();
    let chars = name.chars().count();
    if chars == 0 || chars > MAX_NAME_CHARS || name.chars().any(char::is_control) {
        return Err(
            ApiError::bad_request("bad_name", "Give it a name of 1 to 200 characters.")
                .field("name"),
        );
    }
    if body.expires_at.is_some_and(|e| e <= now) {
        return Err(
            ApiError::bad_request("bad_expiry", "The expiry must be in the future.")
                .field("expires_at"),
        );
    }
    let (kind, scopes) = match body.kind {
        KeyKind::ApiKey => {
            if body.saved_search_id.is_some() {
                return Err(ApiError::bad_request(
                    "bad_key",
                    "Only a feed token belongs to a saved search.",
                )
                .field("saved_search_id"));
            }
            let bits = bits_of(&body.scopes);
            if bits == 0 {
                return Err(ApiError::bad_request(
                    "bad_scopes",
                    "Choose at least one thing the key may reach.",
                )
                .field("scopes"));
            }
            (StoreKind::ApiKey, bits)
        }
        KeyKind::FeedToken => {
            if !body.scopes.is_empty() {
                return Err(ApiError::bad_request(
                    "bad_scopes",
                    "A feed token reads feeds only; it takes no scopes.",
                )
                .field("scopes"));
            }
            (StoreKind::FeedToken, scope::FEEDS)
        }
    };
    Ok(Spec {
        kind,
        name: name.to_string(),
        scopes,
        saved_search_id: body.saved_search_id,
        expires_at: body.expires_at,
    })
}

/// Stores a new key for `spec`, minting again on a prefix collision.
fn insert(store: &mut Store, spec: &Spec, now: i64) -> ApiResult<(ApiKey, Secret)> {
    for _ in 0..4 {
        let m = mint(spec.kind).map_err(internal)?;
        let new = NewApiKey {
            kind: spec.kind,
            name: &spec.name,
            prefix: &m.prefix,
            hash: m.hash,
            scopes: spec.scopes,
            saved_search_id: spec.saved_search_id,
            expires_at: spec.expires_at,
        };
        match store.create_api_key(&new, now) {
            Ok(k) => return Ok((k, m.secret)),
            Err(StoreError::Invalid("prefix in use")) => continue,
            Err(e) => return Err(store_error(e)),
        }
    }
    Err(internal("four prefix collisions in a row"))
}

fn created(k: &ApiKey, secret: Secret, now: i64) -> CreatedKey {
    CreatedKey {
        key: info(k, now),
        secret: secret.0,
    }
}

/// Every API key and feed token, oldest first: never a secret.
#[utoipa::path(
    operation_id = "listKeys",
    get,
    path = "/keys",
    tag = "auth",
    responses((status = 200, body = KeyList))
)]
pub(crate) async fn list_keys(State(s): State<ApiState>) -> ApiResult<axum::Json<KeyList>> {
    let now = crate::unix_now();
    let keys = with_store(&s, move |st| st.api_keys().map_err(store_error)).await?;
    Ok(axum::Json(KeyList {
        keys: keys.iter().map(|k| info(k, now)).collect(),
    }))
}

/// Makes an API key or feed token. Needs the password (428
/// `confirm_required` without it). The secret is in this response only.
#[utoipa::path(
    operation_id = "createKey",
    post,
    path = "/keys",
    tag = "auth",
    request_body = CreateKeyRequest,
    responses((status = 201, body = CreatedKey, description = "Made; `secret` is shown this once."))
)]
pub(crate) async fn create_key(
    State(s): State<ApiState>,
    ClientIp(ip): ClientIp,
    headers: HeaderMap,
    Json(body): Json<CreateKeyRequest>,
) -> ApiResult<Response> {
    same_origin_json(&headers)?;
    let spec = check(&body, crate::unix_now())?;
    user_data(&s)?;
    confirm_fresh_password(&s.auth, ip, body.password).await?;
    let out = with_store(&s, move |st| {
        if st.api_keys().map_err(store_error)?.len() >= MAX_KEYS {
            return Err(ApiError::new(
                StatusCode::CONFLICT,
                "too_many_keys",
                "There are 500 keys and tokens already; delete some first.",
            ));
        }
        let now = crate::unix_now();
        let (k, secret) = insert(st, &spec, now)?;
        Ok(created(&k, secret, now))
    })
    .await?;
    Ok((StatusCode::CREATED, NO_STORE, axum::Json(out)).into_response())
}

/// The secret is in the body: no cache may keep it.
const NO_STORE: [(axum::http::HeaderName, &str); 1] =
    [(axum::http::header::CACHE_CONTROL, "no-store")];

/// Replaces a key's secret: the old one stops working at once. The key
/// keeps its name, kind, scopes, feed and expiry, under a new id and
/// prefix. Needs the password (428 `confirm_required` without it).
#[utoipa::path(
    operation_id = "regenerateKey",
    post,
    path = "/keys/{id}/regenerate",
    tag = "auth",
    params(("id" = i64, Path, description = "The key's id.")),
    request_body = ConfirmRequest,
    responses((status = 200, body = CreatedKey, description = "Replaced; `secret` is shown this once."))
)]
pub(crate) async fn regenerate_key(
    State(s): State<ApiState>,
    ClientIp(ip): ClientIp,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Json(body): Json<ConfirmRequest>,
) -> ApiResult<Response> {
    same_origin_json(&headers)?;
    // Unknown or retired keys are answered before the password is asked.
    with_store(&s, move |st| replaceable(st, id).map(drop)).await?;
    confirm_fresh_password(&s.auth, ip, body.password).await?;
    let out = with_store(&s, move |st| {
        // Checked again under this lock: the key may have been revoked,
        // deleted or regenerated while the password was checked.
        let old = replaceable(st, id)?;
        let spec = Spec {
            kind: old.kind,
            name: old.name.clone(),
            scopes: old.scopes,
            saved_search_id: old.saved_search_id,
            expires_at: old.expires_at,
        };
        let now = crate::unix_now();
        let (k, secret) = insert(st, &spec, now)?;
        // The new key exists before the old goes, so a failure here
        // leaves both working rather than neither.
        if !st.delete_api_key(old.id).map_err(store_error)? {
            return Err(internal("a key vanished while it was regenerated"));
        }
        Ok(created(&k, secret, now))
    })
    .await?;
    Ok((NO_STORE, axum::Json(out)).into_response())
}

/// The key `id`, if it can be regenerated: it exists and is active.
fn replaceable(st: &Store, id: i64) -> ApiResult<ApiKey> {
    let k = st.api_key(id).map_err(store_error)?.ok_or_else(not_found)?;
    if status(&k, crate::unix_now()) != KeyStatus::Active {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "key_inactive",
            "This key is revoked or expired; make a new one instead.",
        ));
    }
    Ok(k)
}

/// Revokes a key or token: it stops working and stays listed as revoked.
/// Revoking one already revoked changes nothing.
#[utoipa::path(
    operation_id = "revokeKey",
    post,
    path = "/keys/{id}/revoke",
    tag = "auth",
    params(("id" = i64, Path, description = "The key's id.")),
    request_body(content = Object, description = "Any JSON, such as `{}`."),
    responses((status = 200, body = KeyInfo))
)]
pub(crate) async fn revoke_key(
    State(s): State<ApiState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> ApiResult<axum::Json<KeyInfo>> {
    same_origin_json(&headers)?;
    let now = crate::unix_now();
    let k = with_store(&s, move |st| {
        st.revoke_api_key(id, now).map_err(store_error)?;
        st.api_key(id).map_err(store_error)?.ok_or_else(not_found)
    })
    .await?;
    Ok(axum::Json(info(&k, now)))
}

/// Deletes a key or token and its hash.
#[utoipa::path(
    operation_id = "deleteKey",
    delete,
    path = "/keys/{id}",
    tag = "auth",
    params(("id" = i64, Path, description = "The key's id.")),
    request_body(content = Object, description = "Any JSON, such as `{}`."),
    responses((status = 200, body = OkResponse))
)]
pub(crate) async fn delete_key(
    State(s): State<ApiState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> ApiResult<axum::Json<OkResponse>> {
    same_origin_json(&headers)?;
    let deleted = with_store(&s, move |st| st.delete_api_key(id).map_err(store_error)).await?;
    if !deleted {
        return Err(not_found());
    }
    Ok(axum::Json(OkResponse::OK))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dto::REDACTED;

    #[test]
    fn minted_secrets_parse_back() {
        for kind in [StoreKind::ApiKey, StoreKind::FeedToken] {
            let m = mint(kind).unwrap();
            let s = m.secret.expose();
            assert_eq!(s.len(), SECRET_LEN);
            assert!(s.starts_with(tag(kind)));
            assert_eq!(&s[..PREFIX_LEN], m.prefix);
            let p = parse(s).unwrap();
            assert_eq!(
                (p.kind, p.prefix, p.hash),
                (kind, m.prefix.as_str(), m.hash)
            );
            // The store's prefix rules.
            assert!((8..=32).contains(&m.prefix.len()));
            assert!(
                m.prefix
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_')
            );
        }
    }

    #[test]
    fn secrets_differ_and_hash_the_whole_string() {
        let a = mint(StoreKind::ApiKey).unwrap();
        let b = mint(StoreKind::ApiKey).unwrap();
        assert_ne!(a.secret, b.secret);
        assert_ne!(a.prefix, b.prefix);
        let mut s = a.secret.expose().to_string();
        let last = s.pop().unwrap();
        s.push(if last == 'A' { 'B' } else { 'A' });
        let p = parse(&s).unwrap();
        assert_eq!(p.prefix, a.prefix);
        assert_ne!(p.hash, a.hash, "the secret part is hashed too");
    }

    #[test]
    fn malformed_secrets_dont_parse() {
        let good = mint(StoreKind::ApiKey).unwrap().secret.expose().to_string();
        let bad = [
            String::new(),
            good[..SECRET_LEN - 1].to_string(),
            format!("{good}A"),
            good.replacen("lsk_", "lsx_", 1),
            format!("{}-{}", &good[..PREFIX_LEN], &good[PREFIX_LEN + 1..]),
            format!("{}é{}", &good[..PREFIX_LEN - 2], &good[PREFIX_LEN..]),
            format!("{}!", &good[..SECRET_LEN - 1]),
            format!("{}_{}", &good[..PREFIX_LEN - 1], &good[PREFIX_LEN..]),
        ];
        for b in bad {
            assert!(parse(&b).is_none(), "{b:?}");
        }
    }

    #[test]
    fn base64url_matches_the_standard() {
        assert_eq!(base64url(b""), "");
        assert_eq!(base64url(b"f"), "Zg");
        assert_eq!(base64url(b"fo"), "Zm8");
        assert_eq!(base64url(b"foo"), "Zm9v");
        assert_eq!(base64url(b"foobar"), "Zm9vYmFy");
        assert_eq!(base64url(&[0xfb, 0xff]), "-_8");
        assert_eq!(base64url(&[0u8; 32]).len(), SECRET_CHARS);
    }

    #[test]
    fn scopes_round_trip() {
        for bits in 0..=scope::ALL {
            assert_eq!(bits_of(&scopes_of(bits)), bits);
        }
        assert_eq!(
            scopes_of(1 << 20),
            Vec::<KeyScope>::new(),
            "unknown bits aren't shown"
        );
    }

    #[test]
    fn debug_redacts_the_secret() {
        let m = mint(StoreKind::ApiKey).unwrap();
        let p = parse(m.secret.expose()).unwrap();
        for shown in [format!("{m:?}"), format!("{p:?}")] {
            assert!(!shown.contains(&m.secret.expose()[PREFIX_LEN..]), "{shown}");
            assert!(
                shown.contains(&m.prefix) && shown.contains(REDACTED),
                "{shown}"
            );
        }
    }

    proptest::proptest! {
        #[test]
        fn parse_never_panics(s in ".{0,80}") {
            let _ = parse(&s);
        }
    }
}
