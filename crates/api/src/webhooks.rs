//! Sonarr and Radarr webhooks (plan `2026-11-01-v06-sources.md` §5 Q9,
//! architecture §12, §18.2 rank 17, §18.7): an inbound route each app
//! calls on a grab or an import, and the routes that make its token.
//!
//! **Inbound:** `POST /webhooks/{integration}`, `sonarr` or `radarr`. No
//! session: the integration's token, as the password of HTTP Basic auth
//! (the apps' Username and Password fields; the username is ignored) or
//! as `Authorization: Bearer`. Never in the query string, which proxies
//! and access logs keep.
//! - **One answer for every failure to authenticate:** no token, a
//!   malformed one, a wrong one, none made yet, or an unknown integration
//!   are all `401 login_required`, the same body as a missing session.
//!   Only a token in the right format is checked, and a wrong one spends a
//!   failed check from the client's per-IP bucket that API keys use
//!   (`429` once it's empty) and logs `auth.key_refused` (kind
//!   `webhook_token`, the reason and the client only).
//! - **The token:** `lsw_` and 43 characters of base64url (32 bytes from
//!   the operating system's RNG). The store keeps its SHA-256 only, and
//!   the presented token's hash is compared with it in constant time
//!   ([`ct_eq`]), against a fixed hash when none is stored, so the answer
//!   takes as long either way. The hashes are cached in [`Webhooks`] and
//!   kept current by the token routes, so a call does no I/O before it
//!   authenticates; a store that can't be read is `503`, never "no token".
//! - **Then, in order:** the body is refused past [`MAX_BODY`] bytes
//!   (`413`, from `Content-Length` before reading, and while reading); the
//!   integration's own bucket of [`RATE_BURST`] calls refilling at
//!   [`RATE_PER_SEC`] a second; and the body goes to the [`WebhookSink`],
//!   which parses it with the source plugin's bounded parser and hands a
//!   find to the pipeline without waiting. A body that isn't an event is
//!   `400 malformed_webhook` and counted.
//! - **A real event that's dropped still answers `200`** (`dropped:
//!   true`): over the rate, the source off, or the pipeline full. Sonarr
//!   and Radarr count every other answer as a failure and back the
//!   connection off for up to hours. Only the app's Test call is answered
//!   `503 source_off` while the source is off, so the Test button shows it.
//! - **Nothing from the request is logged:** not the token, not a header,
//!   not the body. `api.request` logs the route template and status only.
//!
//! **Tokens** (session only, same-origin JSON): `GET /webhooks` lists the
//! integrations, whether each has a token and its counts since start;
//! `POST /webhooks/{integration}/token` makes a new one (needs the fresh
//! password, §20.11, as making an API key does), which replaces the old at
//! once and is in that response only; `DELETE
//! /webhooks/{integration}/token` removes it (no password: it only takes
//! access away).
//!
//! **Wiring:** the host adds a [`Webhooks`] to the router as an
//! `Extension` ([`Webhooks::layer`]), over a sink that delivers to the
//! `arr_webhooks` plugin's inbox. Without one, an authenticated call is
//! `503 webhooks_unavailable` and nothing is read.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Extension, Router};
use rand::TryRng;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use utoipa::{OpenApi, ToSchema};

use crate::ApiState;
use crate::auth::{confirm_fresh_password, protected};
use crate::dto::auth::{ConfirmRequest, OkResponse};
use crate::error::{ApiError, ApiResult, internal};
use crate::extract::{ClientIp, Json, Path, same_origin_json};

/// The largest body read: a real Grab or Download body is 2–40 KB. The
/// plugin's parser has the same cap.
pub const MAX_BODY: usize = 256 * 1024;
/// Calls an integration may make at once.
pub const RATE_BURST: f64 = 120.0;
/// Calls an integration regains a second.
pub const RATE_PER_SEC: f64 = 1.0;
/// The tag every webhook token starts with.
pub const TOKEN_TAG: &str = "lsw_";
const TOKEN_BYTES: usize = 32;
/// The tag and `TOKEN_BYTES` as unpadded base64url.
pub const TOKEN_LEN: usize = 4 + 43;
/// A header value longer than this holds no token (room for a long Basic
/// username).
const MAX_HEADER: usize = 1024;

const BASE64URL: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
const BASE64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// An app that can call in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum Integration {
    Sonarr,
    Radarr,
}

impl Integration {
    pub const ALL: [Integration; 2] = [Integration::Sonarr, Integration::Radarr];

    pub const fn name(self) -> &'static str {
        match self {
            Integration::Sonarr => "sonarr",
            Integration::Radarr => "radarr",
        }
    }

    pub fn from_name(name: &str) -> Option<Integration> {
        Integration::ALL.into_iter().find(|i| i.name() == name)
    }

    const fn index(self) -> usize {
        match self {
            Integration::Sonarr => 0,
            Integration::Radarr => 1,
        }
    }

    /// The `settings` key its token's hash is kept under.
    fn key(self) -> &'static str {
        match self {
            Integration::Sonarr => "webhooks.sonarr.token_sha256",
            Integration::Radarr => "webhooks.radarr.token_sha256",
        }
    }
}

/// What became of a body the sink was given.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Delivered {
    /// A key went to the pipeline.
    Found,
    /// A valid event with nothing to find (a Test, another event type, a
    /// Usenet download).
    Ignored,
    /// Not an event within the parser's caps.
    Malformed,
    /// The pipeline is full: dropped.
    Busy,
    /// The source is turned off: dropped. `test`: it was the app's Test
    /// call, answered with an error so the Test button shows the problem.
    /// A real event is answered `200` (dropped), so the app doesn't count
    /// a failure and back its notifications off for hours.
    Off { test: bool },
}

/// Where authenticated bodies go: the host's adapter over the
/// `arr_webhooks` plugin's inbox. Must not block.
pub trait WebhookSink: Send + Sync + 'static {
    fn deliver(&self, integration: Integration, body: &[u8]) -> Delivered;
}

/// The inbound side's state: the sink, each integration's call bucket and
/// counts. Shared by every request through an `Extension`.
pub struct Webhooks {
    sink: Arc<dyn WebhookSink>,
    buckets: Mutex<[Bucket; 2]>,
    counts: [Counts; 2],
    /// Each integration's stored token hash, read from the store once and
    /// kept current by the token routes, so a call does no I/O before it
    /// authenticates. `None` until first read.
    hashes: RwLock<Option<Hashes>>,
    /// Bumped by every token change, so a first read that raced one isn't
    /// cached.
    version: AtomicU64,
}

type Hashes = [Option<[u8; 32]>; 2];

impl std::fmt::Debug for Webhooks {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Webhooks").finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, Copy)]
struct Bucket {
    tokens: f64,
    at: Instant,
}

#[derive(Debug, Default)]
struct Counts {
    found: AtomicU64,
    ignored: AtomicU64,
    malformed: AtomicU64,
    refused: AtomicU64,
}

impl Webhooks {
    pub fn new(sink: Arc<dyn WebhookSink>) -> Arc<Webhooks> {
        let full = Bucket {
            tokens: RATE_BURST,
            at: Instant::now(),
        };
        Arc::new(Webhooks {
            sink,
            buckets: Mutex::new([full; 2]),
            counts: Default::default(),
            hashes: RwLock::new(None),
            version: AtomicU64::new(0),
        })
    }

    /// The layer the host adds to [`crate::router`]'s router.
    pub fn layer(self: &Arc<Self>) -> Extension<Arc<Webhooks>> {
        Extension(self.clone())
    }

    /// Takes one call from `i`'s bucket, or says how long until one is
    /// back.
    fn take(&self, i: Integration) -> Result<(), Duration> {
        let mut buckets = self.buckets.lock().unwrap_or_else(|e| e.into_inner());
        let b = &mut buckets[i.index()];
        let now = Instant::now();
        let since = now.saturating_duration_since(b.at).as_secs_f64();
        b.tokens = (b.tokens + since * RATE_PER_SEC).min(RATE_BURST);
        b.at = now;
        if b.tokens >= 1.0 {
            b.tokens -= 1.0;
            Ok(())
        } else {
            Err(Duration::from_secs_f64((1.0 - b.tokens) / RATE_PER_SEC))
        }
    }

    /// The stored hashes: the cached copy, or (once) the store's.
    async fn hashes(&self, s: &ApiState) -> Result<Hashes, ()> {
        if let Some(h) = *self.hashes.read().unwrap_or_else(|e| e.into_inner()) {
            return Ok(h);
        }
        let before = self.version.load(Ordering::SeqCst);
        let h = read_hashes(s).await?;
        let mut slot = self.hashes.write().unwrap_or_else(|e| e.into_inner());
        if let Some(cached) = *slot {
            return Ok(cached);
        }
        // A token changed while the store was read: answer this call with
        // what was read, and read again next time.
        if self.version.load(Ordering::SeqCst) == before {
            *slot = Some(h);
        }
        Ok(h)
    }

    /// A token route changed `i`'s hash (call it after the store write):
    /// the cache follows. Left alone before the first read, which then
    /// reads the new value.
    fn set_hash(&self, i: Integration, hash: Option<[u8; 32]>) {
        let mut slot = self.hashes.write().unwrap_or_else(|e| e.into_inner());
        self.version.fetch_add(1, Ordering::SeqCst);
        if let Some(h) = slot.as_mut() {
            h[i.index()] = hash;
        }
    }

    fn count(&self, i: Integration, d: Delivered) {
        let c = &self.counts[i.index()];
        let n = match d {
            Delivered::Found => &c.found,
            Delivered::Ignored => &c.ignored,
            Delivered::Malformed => &c.malformed,
            Delivered::Busy | Delivered::Off { .. } => &c.refused,
        };
        n.fetch_add(1, Ordering::Relaxed);
    }
}

/// `GET /webhooks`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct WebhookList {
    pub integrations: Vec<WebhookInfo>,
}

/// One integration. The counts are since start, zero when webhooks
/// aren't wired.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct WebhookInfo {
    pub integration: Integration,
    /// The path the app calls, under `/api/v1`.
    pub path: String,
    /// A token is made. It can't be shown again; make a new one instead.
    pub token_set: bool,
    /// Calls that found a key.
    pub found: u64,
    /// Valid calls with nothing to find (Test, other events).
    pub ignored: u64,
    /// Bodies that weren't an event, dropped.
    pub malformed: u64,
    /// Calls dropped: over the rate, the source off, or the pipeline full.
    pub refused: u64,
}

/// `POST /webhooks/{integration}/token`: the new token, this once.
#[derive(Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct WebhookToken {
    pub integration: Integration,
    /// Paste it into the app's webhook connection as the Password.
    pub token: String,
}

impl std::fmt::Debug for WebhookToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WebhookToken")
            .field("integration", &self.integration)
            .field("token", &crate::dto::REDACTED)
            .finish()
    }
}

/// An accepted call.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct WebhookReceipt {
    /// The event named a torrent, which went to the pipeline.
    pub found: bool,
    /// The call was dropped: over the integration's rate, the source
    /// turned off, or the pipeline full. Counted in `refused`.
    pub dropped: bool,
}

/// This module's half of `/openapi.json`.
#[derive(OpenApi)]
#[openapi(
    paths(receive, list_webhooks, create_token, delete_token),
    components(schemas(Integration, WebhookList, WebhookInfo, WebhookToken, WebhookReceipt))
)]
pub(crate) struct ApiDoc;

pub fn routes() -> Router<ApiState> {
    Router::new()
        .route("/webhooks/{integration}", post(receive))
        .merge(protected(
            Router::new().route("/webhooks", get(list_webhooks)).route(
                "/webhooks/{integration}/token",
                post(create_token).delete(delete_token),
            ),
        ))
}

// ---- tokens ----------------------------------------------------------

/// Makes a token from the operating system's RNG, with its hash.
fn mint() -> std::io::Result<(String, [u8; 32])> {
    let mut raw = [0u8; TOKEN_BYTES];
    rand::rngs::SysRng
        .try_fill_bytes(&mut raw)
        .map_err(std::io::Error::other)?;
    let mut token = String::with_capacity(TOKEN_LEN);
    token.push_str(TOKEN_TAG);
    for chunk in raw.chunks(3) {
        let n = chunk
            .iter()
            .enumerate()
            .fold(0u32, |acc, (i, &b)| acc | (u32::from(b) << (16 - 8 * i)));
        for i in 0..=chunk.len() {
            token.push(char::from(BASE64URL[((n >> (18 - 6 * i)) & 63) as usize]));
        }
    }
    debug_assert_eq!(token.len(), TOKEN_LEN);
    let hash = sha256(&token);
    Ok((token, hash))
}

fn sha256(s: &str) -> [u8; 32] {
    Sha256::digest(s.as_bytes()).into()
}

/// Whether `s` has a token's shape. Shape only: says nothing of whether
/// it's anyone's.
fn well_formed(s: &str) -> bool {
    s.len() == TOKEN_LEN
        && s.starts_with(TOKEN_TAG)
        && s.as_bytes()[TOKEN_TAG.len()..]
            .iter()
            .all(|c| BASE64URL.contains(c))
}

/// Equal, in time that depends on the length only.
#[inline(never)]
fn ct_eq(a: &[u8; 32], b: &[u8; 32]) -> bool {
    #[cfg(test)]
    tests::CT_CALLS.with(|n| n.set(n.get() + 1));
    let diff = a
        .iter()
        .zip(b)
        .fold(0u8, |acc, (x, y)| acc | std::hint::black_box(x ^ y));
    std::hint::black_box(diff) == 0
}

/// Compared against when no token is stored, so that path costs the same.
/// No token hashes to it: it isn't a SHA-256 output anyone can aim for.
const NO_TOKEN: [u8; 32] = [0xa5; 32];

/// Whether `presented` (already well-formed) is the stored token.
fn matches(stored: Option<&[u8; 32]>, presented: &str) -> bool {
    let hash = sha256(presented);
    let ok = ct_eq(stored.unwrap_or(&NO_TOKEN), &hash);
    ok && stored.is_some()
}

/// The token a request presents: HTTP Basic auth's password, or a Bearer
/// token. `None` when there's none, more than one, or any `Authorization`
/// value that can't hold one.
fn presented(headers: &HeaderMap) -> Option<String> {
    let mut values = headers.get_all(header::AUTHORIZATION).iter();
    let value = values.next()?;
    if values.next().is_some() || value.len() > MAX_HEADER {
        return None;
    }
    let (scheme, rest) = value.to_str().ok()?.trim().split_once(' ')?;
    let rest = rest.trim();
    if scheme.eq_ignore_ascii_case("bearer") {
        Some(rest.to_owned())
    } else if scheme.eq_ignore_ascii_case("basic") {
        basic_password(rest)
    } else {
        None
    }
}

/// The password in a Basic credential: standard base64 of `user:pass`,
/// padded or not.
fn basic_password(b64: &str) -> Option<String> {
    let b = b64.trim_end_matches('=').as_bytes();
    if b64.len() - b.len() > 2 || b.len() % 4 == 1 {
        return None;
    }
    let mut out = Vec::with_capacity(b.len() * 3 / 4);
    let (mut acc, mut bits) = (0u32, 0u32);
    for &c in b {
        let v = BASE64.iter().position(|&x| x == c)? as u32;
        acc = (acc << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    let text = String::from_utf8(out).ok()?;
    let (_, password) = text.split_once(':')?;
    Some(password.to_owned())
}

/// Stored hashes are 64 hex digits; anything else is no token.
fn decode_hash(hex: &str) -> Option<[u8; 32]> {
    if hex.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for (o, pair) in out.iter_mut().zip(hex.as_bytes().chunks(2)) {
        let s = std::str::from_utf8(pair).ok()?;
        *o = u8::from_str_radix(s, 16).ok()?;
    }
    Some(out)
}

fn encode_hash(h: &[u8; 32]) -> String {
    h.iter().map(|b| format!("{b:02x}")).collect()
}

/// Both integrations' stored hashes. `Err` when the store can't be read
/// (missing, locked out, an SQLite error): never taken for "no token".
async fn read_hashes(s: &ApiState) -> Result<Hashes, ()> {
    let store = s.user_data.clone().ok_or(())?;
    tokio::task::spawn_blocking(move || {
        let st = store.lock().map_err(drop)?;
        let mut out = [None; 2];
        for i in Integration::ALL {
            out[i.index()] = st
                .setting_text(i.key())
                .map_err(drop)?
                .as_deref()
                .and_then(decode_hash);
        }
        Ok(out)
    })
    .await
    .map_err(drop)?
}

// ---- inbound -----------------------------------------------------------

/// The one answer to a call that didn't authenticate: a missing
/// session's.
fn unauthorized(auth: &crate::Auth) -> Response {
    ApiError::new(StatusCode::UNAUTHORIZED, "login_required", "Log in first.")
        .setup_required(auth.needs_setup())
        .into_response()
}

fn unavailable() -> Response {
    ApiError::new(
        StatusCode::SERVICE_UNAVAILABLE,
        "webhooks_unavailable",
        "Webhooks aren't available.",
    )
    .into_response()
}

fn failures_limited(wait: Duration) -> Response {
    too_many(
        "rate_limited",
        "Too many failed attempts. Try again later.",
        wait,
    )
}

/// A refused token: logged by why (never the token, a header or its
/// hash), one failed check spent, and the same answer whatever failed.
/// The attempt that empties the address's bucket logs the limit once, as
/// the API-key guard does.
fn failed(auth: &crate::Auth, ip: std::net::IpAddr, reason: &'static str) -> Response {
    use crate::auth::log_events::{AUTH_KEY_REFUSED, AUTH_RATE_LIMITED};
    lodestar_log::log_event!(
        AUTH_KEY_REFUSED,
        client = lodestar_log::Client(ip),
        kind = "webhook_token",
        reason = reason
    );
    if let Err(wait) = auth.key_failures.take(ip) {
        return failures_limited(wait);
    }
    if auth.key_failures.check(ip).is_err() {
        lodestar_log::log_event!(
            AUTH_RATE_LIMITED,
            client = lodestar_log::Client(ip),
            limit = "key_checks"
        );
    }
    unauthorized(auth)
}

/// A dropped real event: `200`, so the app doesn't count it a failure.
fn dropped() -> Response {
    axum::Json(WebhookReceipt {
        found: false,
        dropped: true,
    })
    .into_response()
}

fn too_many(code: &'static str, message: &'static str, wait: Duration) -> Response {
    let mut resp = ApiError::new(StatusCode::TOO_MANY_REQUESTS, code, message).into_response();
    let secs = wait.as_secs() + u64::from(wait.subsec_nanos() > 0);
    if let Ok(v) = HeaderValue::from_str(&secs.max(1).to_string()) {
        resp.headers_mut().insert(header::RETRY_AFTER, v);
    }
    resp
}

fn too_large() -> Response {
    ApiError::new(
        StatusCode::PAYLOAD_TOO_LARGE,
        "webhook_too_large",
        format!("The webhook body is larger than {} KiB.", MAX_BODY / 1024),
    )
    .into_response()
}

/// Receives a Sonarr or Radarr webhook call. Authenticated by the
/// integration's token (Basic auth's password, or Bearer), not a session;
/// every failure to authenticate is the same `401 login_required`.
#[utoipa::path(
    operation_id = "receiveWebhook",
    post,
    path = "/webhooks/{integration}",
    tag = "crawler",
    params(("integration" = Integration, Path, description = "`sonarr` or `radarr`.")),
    request_body(content = Object, content_type = "application/json", description = "The app's webhook event, at most 256 KiB."),
    responses(
        (status = 200, body = WebhookReceipt, description = "Taken: a Grab or Download found a key, or the event had nothing to find; or a real event dropped (`dropped`: over the integration's rate, the source off, the pipeline full). Dropped events still answer 200 so the app doesn't back its notifications off."),
        (status = 400, body = crate::dto::error::ErrorEnvelope, description = "`malformed_webhook`: not an event, dropped and counted; or `webhook_unreadable`: the connection failed while the body was read."),
        (status = 401, body = crate::dto::error::ErrorEnvelope, description = "`login_required`: no valid token, however it failed."),
        (status = 413, body = crate::dto::error::ErrorEnvelope, description = "`webhook_too_large`."),
        (status = 429, body = crate::dto::error::ErrorEnvelope, description = "`rate_limited`: too many failed tokens from this address, with `Retry-After`."),
        (status = 503, body = crate::dto::error::ErrorEnvelope, description = "`webhooks_unavailable` (not wired, or the store can't be read), or `source_off` for the app's Test call while the source is off."),
    )
)]
pub(crate) async fn receive(
    State(s): State<ApiState>,
    ClientIp(ip): ClientIp,
    Path(name): Path<String>,
    hub: Option<Extension<Arc<Webhooks>>>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    let Some(token) = presented(&headers).filter(|t| well_formed(t)) else {
        // Not a guess at a token: nothing spent, nothing read.
        return unauthorized(&s.auth);
    };
    // A well-formed guess spends from the address's failed-check bucket,
    // which API keys share; one already empty is answered before any
    // work.
    if let Err(wait) = s.auth.key_failures.check(ip) {
        return failures_limited(wait);
    }
    let Some(integration) = Integration::from_name(&name) else {
        return failed(&s.auth, ip, "unknown_integration");
    };
    // The hub's cached hashes: no I/O before authenticating. Unwired (no
    // hub), the store is read each time. A store that can't be read is
    // the server's fault, not the caller's: 503, nothing spent.
    let hashes = match &hub {
        Some(Extension(h)) => h.hashes(&s).await,
        None => read_hashes(&s).await,
    };
    let Ok(hashes) = hashes else {
        return unavailable();
    };
    let stored = hashes[integration.index()];
    if !matches(stored.as_ref(), &token) {
        let reason = if stored.is_some() { "wrong" } else { "unset" };
        return failed(&s.auth, ip, reason);
    }
    let Some(Extension(hub)) = hub else {
        return unavailable();
    };
    let declared = headers
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok());
    if declared.is_some_and(|n| n > MAX_BODY as u64) {
        hub.count(integration, Delivered::Busy);
        return too_large();
    }
    if hub.take(integration).is_err() {
        hub.count(integration, Delivered::Busy);
        return dropped();
    }
    let bytes = match axum::body::to_bytes(body, MAX_BODY).await {
        Ok(b) => b,
        // With a declared length the transport enforces it, so a failure
        // is the connection's; without one it's the cap.
        Err(_) if declared.is_some() => {
            hub.count(integration, Delivered::Busy);
            return ApiError::bad_request(
                "webhook_unreadable",
                "The webhook body couldn't be read.",
            )
            .into_response();
        }
        Err(_) => {
            hub.count(integration, Delivered::Busy);
            return too_large();
        }
    };
    let delivered = hub.sink.deliver(integration, &bytes);
    hub.count(integration, delivered);
    match delivered {
        Delivered::Found | Delivered::Ignored => axum::Json(WebhookReceipt {
            found: delivered == Delivered::Found,
            dropped: false,
        })
        .into_response(),
        Delivered::Malformed => ApiError::bad_request(
            "malformed_webhook",
            "That isn't a Sonarr or Radarr webhook event.",
        )
        .into_response(),
        Delivered::Off { test: true } => ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "source_off",
            "The Sonarr and Radarr webhooks source is turned off in Settings → Sources.",
        )
        .into_response(),
        Delivered::Off { test: false } | Delivered::Busy => dropped(),
    }
}

// ---- token routes --------------------------------------------------------

fn integration(name: &str) -> ApiResult<Integration> {
    Integration::from_name(name).ok_or_else(|| {
        ApiError::new(
            StatusCode::NOT_FOUND,
            "webhook_not_found",
            "There's no such integration; use sonarr or radarr.",
        )
    })
}

async fn with_store<T: Send + 'static>(
    s: &ApiState,
    f: impl FnOnce(&mut lodestar_store::Store) -> ApiResult<T> + Send + 'static,
) -> ApiResult<T> {
    let store = s.user_data.clone().ok_or_else(|| {
        ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "webhooks_unavailable",
            "Webhooks aren't available.",
        )
    })?;
    tokio::task::spawn_blocking(move || f(&mut *crate::keys::lock(&store)?))
        .await
        .map_err(internal)?
}

/// Lists the integrations: whether each has a token, and its counts.
#[utoipa::path(
    operation_id = "listWebhooks",
    get,
    path = "/webhooks",
    tag = "crawler",
    responses((status = 200, body = WebhookList, description = "Both integrations."))
)]
pub(crate) async fn list_webhooks(
    State(s): State<ApiState>,
    hub: Option<Extension<Arc<Webhooks>>>,
) -> ApiResult<axum::Json<WebhookList>> {
    let set = with_store(&s, |st| {
        Integration::ALL
            .iter()
            .map(|i| {
                st.setting_text(i.key())
                    .map(|h| h.as_deref().and_then(decode_hash).is_some())
                    .map_err(internal)
            })
            .collect::<ApiResult<Vec<bool>>>()
    })
    .await?;
    let integrations = Integration::ALL
        .into_iter()
        .zip(set)
        .map(|(i, token_set)| {
            let n = |f: fn(&Counts) -> &AtomicU64| {
                hub.as_ref()
                    .map_or(0, |h| f(&h.counts[i.index()]).load(Ordering::Relaxed))
            };
            WebhookInfo {
                integration: i,
                path: format!("{}/webhooks/{}", crate::openapi::PREFIX, i.name()),
                token_set,
                found: n(|c| &c.found),
                ignored: n(|c| &c.ignored),
                malformed: n(|c| &c.malformed),
                refused: n(|c| &c.refused),
            }
        })
        .collect();
    Ok(axum::Json(WebhookList { integrations }))
}

/// Makes the integration's token, replacing any old one at once. Needs
/// the password (428 `confirm_required` without it). The token is in this
/// response only.
#[utoipa::path(
    operation_id = "createWebhookToken",
    post,
    path = "/webhooks/{integration}/token",
    tag = "crawler",
    params(("integration" = Integration, Path, description = "`sonarr` or `radarr`.")),
    request_body = ConfirmRequest,
    responses((status = 200, body = WebhookToken, description = "Made; `token` is shown this once."))
)]
pub(crate) async fn create_token(
    State(s): State<ApiState>,
    hub: Option<Extension<Arc<Webhooks>>>,
    ClientIp(ip): ClientIp,
    headers: HeaderMap,
    Path(name): Path<String>,
    Json(body): Json<ConfirmRequest>,
) -> ApiResult<Response> {
    same_origin_json(&headers)?;
    let i = integration(&name)?;
    with_store(&s, |_| Ok(())).await?;
    confirm_fresh_password(&s.auth, ip, body.password).await?;
    let (token, hash) = mint().map_err(internal)?;
    let hex = encode_hash(&hash);
    with_store(&s, move |st| {
        st.set_settings_text(&[(i.key(), &hex)]).map_err(internal)
    })
    .await?;
    if let Some(Extension(h)) = &hub {
        h.set_hash(i, Some(hash));
    }
    let out = WebhookToken {
        integration: i,
        token,
    };
    Ok(([(header::CACHE_CONTROL, "no-store")], axum::Json(out)).into_response())
}

/// Removes the integration's token: its calls are refused from now on.
#[utoipa::path(
    operation_id = "deleteWebhookToken",
    delete,
    path = "/webhooks/{integration}/token",
    tag = "crawler",
    params(("integration" = Integration, Path, description = "`sonarr` or `radarr`.")),
    responses((status = 200, body = OkResponse, description = "Removed, or there was none."))
)]
pub(crate) async fn delete_token(
    State(s): State<ApiState>,
    hub: Option<Extension<Arc<Webhooks>>>,
    headers: HeaderMap,
    Path(name): Path<String>,
) -> ApiResult<axum::Json<OkResponse>> {
    same_origin_json(&headers)?;
    let i = integration(&name)?;
    with_store(&s, move |st| {
        st.update_settings_text(&[], &[i.key()]).map_err(internal)
    })
    .await?;
    if let Some(Extension(h)) = &hub {
        h.set_hash(i, None);
    }
    Ok(axum::Json(OkResponse { ok: true }))
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use proptest::prelude::*;

    use super::*;

    thread_local! {
        /// Calls to [`ct_eq`] on this thread.
        pub(super) static CT_CALLS: Cell<usize> = const { Cell::new(0) };
    }

    fn calls() -> usize {
        CT_CALLS.with(Cell::get)
    }

    #[test]
    fn tokens_are_well_formed_and_hash_to_what_is_stored() {
        let (a, ha) = mint().unwrap();
        let (b, _) = mint().unwrap();
        assert!(well_formed(&a) && well_formed(&b) && a != b);
        assert_eq!(sha256(&a), ha);
        assert_eq!(decode_hash(&encode_hash(&ha)), Some(ha));
        assert!(!well_formed(&a[1..]));
        assert!(!well_formed(&format!("lsk_{}", &a[4..])));
        assert!(!well_formed(&format!("{}!", &a[..TOKEN_LEN - 1])));
    }

    /// The compare goes through `ct_eq` once on every path, a token
    /// stored or not, right or wrong: not a timing test, a test that the
    /// constant-time compare is the one used.
    #[test]
    fn every_check_uses_the_constant_time_compare_once() {
        let (token, hash) = mint().unwrap();
        let (other, _) = mint().unwrap();
        for (stored, presented, want) in [
            (Some(&hash), token.as_str(), true),
            (Some(&hash), other.as_str(), false),
            (None, token.as_str(), false),
        ] {
            let before = calls();
            assert_eq!(matches(stored, presented), want);
            assert_eq!(calls() - before, 1);
        }
        // A token whose hash is the fixed stand-in can't be made, but even
        // then an unset token never matches.
        assert!(!matches(None, &token));
    }

    #[test]
    fn ct_eq_compares_every_byte() {
        let a = [7u8; 32];
        for i in 0..32 {
            let mut b = a;
            b[i] ^= 1;
            assert!(!ct_eq(&a, &b));
        }
        assert!(ct_eq(&a, &a));
    }

    fn auth(v: &str) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert(header::AUTHORIZATION, HeaderValue::from_str(v).unwrap());
        h
    }

    #[test]
    fn the_token_comes_as_basic_password_or_bearer() {
        let (t, _) = mint().unwrap();
        let basic = |s: &str| {
            let mut out = String::new();
            for chunk in s.as_bytes().chunks(3) {
                let n = chunk
                    .iter()
                    .enumerate()
                    .fold(0u32, |acc, (i, &b)| acc | (u32::from(b) << (16 - 8 * i)));
                for i in 0..4 {
                    if i <= chunk.len() {
                        out.push(char::from(BASE64[((n >> (18 - 6 * i)) & 63) as usize]));
                    } else {
                        out.push('=');
                    }
                }
            }
            out
        };
        assert_eq!(presented(&auth(&format!("Bearer {t}"))), Some(t.clone()));
        assert_eq!(
            presented(&auth(&format!("Basic {}", basic(&format!("sonarr:{t}"))))),
            Some(t.clone())
        );
        assert_eq!(
            presented(&auth(&format!("basic {}", basic(&format!(":{t}"))))),
            Some(t.clone())
        );
        // A username alone, another scheme, no header, two headers.
        assert_eq!(
            presented(&auth(&format!("Basic {}", basic("sonarr")))),
            None
        );
        assert_eq!(presented(&auth(&format!("Token {t}"))), None);
        assert_eq!(presented(&HeaderMap::new()), None);
        let mut two = auth(&format!("Bearer {t}"));
        two.append(
            header::AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {t}")).unwrap(),
        );
        assert_eq!(presented(&two), None);
        assert_eq!(
            presented(&auth(&format!("Bearer {}", "a".repeat(MAX_HEADER)))),
            None
        );
        assert_eq!(basic_password("a===="), None);
        assert_eq!(basic_password("a"), None);
        assert_eq!(basic_password("!!!!"), None);
    }

    #[test]
    fn the_rate_bucket_empties_and_says_how_long() {
        struct Nothing;
        impl WebhookSink for Nothing {
            fn deliver(&self, _: Integration, _: &[u8]) -> Delivered {
                Delivered::Ignored
            }
        }
        let w = Webhooks::new(Arc::new(Nothing));
        for _ in 0..RATE_BURST as usize {
            w.take(Integration::Sonarr).unwrap();
        }
        let wait = w.take(Integration::Sonarr).unwrap_err();
        assert!(wait > Duration::ZERO && wait <= Duration::from_secs(1));
        // Each integration has its own.
        w.take(Integration::Radarr).unwrap();
    }

    proptest! {
        #[test]
        fn header_parsing_never_panics(s in ".{0,300}") {
            if let Ok(v) = HeaderValue::from_str(&s) {
                let mut h = HeaderMap::new();
                h.insert(header::AUTHORIZATION, v);
                let _ = presented(&h);
            }
            let _ = basic_password(&s);
            let _ = decode_hash(&s);
        }

        #[test]
        fn basic_decoding_round_trips(user in "[a-z]{0,8}", pass in "[ -~]{0,64}") {
            let raw = format!("{user}:{pass}");
            let mut b64 = String::new();
            for chunk in raw.as_bytes().chunks(3) {
                let n = chunk
                    .iter()
                    .enumerate()
                    .fold(0u32, |acc, (i, &b)| acc | (u32::from(b) << (16 - 8 * i)));
                for i in 0..=chunk.len() {
                    b64.push(char::from(BASE64[((n >> (18 - 6 * i)) & 63) as usize]));
                }
            }
            prop_assert_eq!(basic_password(&b64), Some(pass));
        }
    }
}
