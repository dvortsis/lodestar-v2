//! Authentication (architecture §12).
//!
//! - **First run:** with no password set, a one-time setup token is
//!   printed to the logs. Whoever can read the logs (the owner) claims
//!   the instance by entering it with a new password, so nobody else on
//!   the LAN can claim a fresh headless install.
//! - **Passwords** are Argon2id (19 MiB, 2 passes, OWASP's baseline) with
//!   a random salt, stored in `auth.json` in the data directory (0600).
//! - **Sessions** are stateless signed cookies: an expiry and a nonce,
//!   HMAC-SHA256'd with a random 256-bit key from the same file.
//!   Changing the password, or signing out other sessions, rotates the
//!   key, which ends every session.
//! - **Login attempts** are rate-limited per IP; Argon2 makes each one
//!   slow anyway.
//! - **Fresh-password confirmation** ([`require_fresh_password`], §20.11):
//!   one check every confirming route calls. Asked every time, never
//!   remembered, rate-limited like a login.
//! - **Trusted reverse-proxy header auth** ([`ProxyConfig`]), off by
//!   default; see [`proxy`].
//! - **API keys and feed tokens** are checked by [`key_or_session`] and
//!   [`feed_token`] on the routes that opt in (see [`crate::keys`]).

mod guard;
mod http;
mod limit;
pub mod log_events;
pub mod proxy;

pub use guard::{Access, feed_token, key_or_session};
pub(crate) use http::logged_in;
pub(crate) use http::{ApiDoc, SessionAuth, auth_error};
pub use http::{mount, protected, require_session, routes};
pub use proxy::{PROXY_AUTH_ENV, ProxyConfig, ProxyError};

use std::io;
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use argon2::{Algorithm, Argon2, Params, Version};
use axum::http::{HeaderMap, StatusCode};
use hmac::{Hmac, KeyInit, Mac};
use rand::RngExt;
use serde::{Deserialize, Serialize};
use sha2::Sha256;

use crate::error::{ApiError, ApiResult, internal};
use limit::Buckets;
use proxy::Proxy;

pub const COOKIE: &str = "lodestar_session";
pub const MIN_PASSWORD_CHARS: usize = 10;
const MAX_PASSWORD_BYTES: usize = 1024;
const SESSION_TTL: Duration = Duration::from_secs(30 * 86_400);
/// Login attempts per IP: a burst of 5, then one every 12 s.
const BURST: f64 = 5.0;
const REFILL_PER_SEC: f64 = 1.0 / 12.0;
const LIMITER_MAX_IPS: usize = 10_000;
/// Failed key checks per IP: a burst of 10, then one every 6 s. A
/// 256-bit secret can't be guessed; this only keeps a misbehaving client
/// from making the server do lookups for nothing.
pub const KEY_FAIL_BURST: f64 = 10.0;
pub const KEY_FAIL_REFILL_PER_SEC: f64 = 1.0 / 6.0;
/// Requests per key: a burst of 300, then 5 a second (18,000 an hour),
/// well above what *arr polling and searches send.
pub const KEY_BURST: f64 = 300.0;
pub const KEY_REFILL_PER_SEC: f64 = 5.0;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthError {
    SetupDone,
    BadToken,
    WeakPassword,
    BadPassword,
    RateLimited,
    NotSetUp,
    Storage(String),
}

impl std::fmt::Display for AuthError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            AuthError::SetupDone => "already set up",
            AuthError::BadToken => "wrong setup token",
            AuthError::WeakPassword => "the password needs at least 10 characters",
            AuthError::BadPassword => "wrong password",
            AuthError::RateLimited => "too many attempts; wait a minute",
            AuthError::NotSetUp => "not set up yet",
            AuthError::Storage(e) => e,
        })
    }
}

impl std::error::Error for AuthError {}

#[derive(Debug, Serialize, Deserialize)]
struct AuthFile {
    /// `argon2id$v=19$m=…,t=…,p=…$<salt hex>$<hash hex>`.
    password: Option<String>,
    /// Hex, 32 bytes.
    session_key: String,
    /// Absent until first changed, so older files load unchanged.
    #[serde(default, skip_serializing_if = "ProxyConfig::is_default")]
    proxy: ProxyConfig,
}

#[derive(Debug)]
struct State {
    password: Option<String>,
    key: [u8; 32],
    proxy: Proxy,
}

#[derive(Debug)]
pub struct Auth {
    path: PathBuf,
    state: RwLock<State>,
    setup_token: Mutex<Option<String>>,
    /// Login, setup, password and fresh-password attempts, per IP.
    login: Buckets<IpAddr>,
    /// Failed API key and feed token checks, per IP.
    pub(crate) key_failures: Buckets<IpAddr>,
    /// Requests per key id.
    pub(crate) key_requests: Buckets<i64>,
    /// [`PROXY_AUTH_ENV`] is set: proxy auth is off for this run.
    proxy_forced_off: AtomicBool,
    secure_cookies: bool,
}

impl Auth {
    /// Loads (or creates) `path`. Without a password, a setup token is
    /// generated: see [`Auth::setup_token`].
    pub fn load(path: &Path, secure_cookies: bool) -> io::Result<Auth> {
        let file = match std::fs::read_to_string(path) {
            Ok(text) => serde_json::from_str::<AuthFile>(&text).map_err(|e| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("{}: {e}", path.display()),
                )
            })?,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                let f = AuthFile {
                    password: None,
                    session_key: hex(&random::<32>()),
                    proxy: ProxyConfig::default(),
                };
                write_private(path, &f)?;
                f
            }
            Err(e) => return Err(e),
        };
        let key = unhex32(&file.session_key).ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidData, "auth.json: bad session key")
        })?;
        let token = file.password.is_none().then(|| {
            // Grouped for reading out of a log: XXXX-XXXX-XXXX-XXXX.
            let raw = hex(&random::<8>()).to_uppercase();
            raw.as_bytes()
                .chunks(4)
                .map(|c| std::str::from_utf8(c).expect("hex"))
                .collect::<Vec<_>>()
                .join("-")
        });
        // A hand-edited section that doesn't check out loads as off: the
        // safe side, and the server still starts.
        let proxy = Proxy::compile(file.proxy).unwrap_or_else(|e| {
            lodestar_log::log_event!(log_events::AUTH_PROXY_SETTINGS_INVALID, field = e.field);
            Proxy::default()
        });
        Ok(Auth {
            path: path.to_path_buf(),
            state: RwLock::new(State {
                password: file.password,
                key,
                proxy,
            }),
            setup_token: Mutex::new(token),
            login: Buckets::new(BURST, REFILL_PER_SEC, LIMITER_MAX_IPS),
            key_failures: Buckets::new(KEY_FAIL_BURST, KEY_FAIL_REFILL_PER_SEC, LIMITER_MAX_IPS),
            key_requests: Buckets::new(KEY_BURST, KEY_REFILL_PER_SEC, crate::keys::MAX_KEYS),
            proxy_forced_off: AtomicBool::new(false),
            secure_cookies,
        })
    }

    /// Stops every limiter refilling, so tests that empty a bucket don't
    /// depend on how fast the machine runs them. Tests only.
    #[doc(hidden)]
    pub fn without_refill(mut self) -> Auth {
        self.login.refill = 0.0;
        self.key_failures.refill = 0.0;
        self.key_requests.refill = 0.0;
        self
    }

    /// Applies [`PROXY_AUTH_ENV`]'s value: `off` (any case) turns proxy
    /// auth off for this run without touching the saved settings. Called
    /// where [`Auth::load`] runs. Returns whether it applied.
    pub fn apply_proxy_override(&self, value: Option<&str>) -> bool {
        let Some(v) = value.map(str::trim).filter(|v| !v.is_empty()) else {
            return false;
        };
        let off = v.eq_ignore_ascii_case("off");
        if off {
            self.proxy_forced_off.store(true, Ordering::Relaxed);
        } else {
            // Someone locked out typed `false` or `0`: say why nothing
            // changed rather than leave them guessing.
            lodestar_log::log_event!(log_events::AUTH_PROXY_OVERRIDE_IGNORED);
        }
        off
    }

    /// Whether [`PROXY_AUTH_ENV`] turned proxy auth off for this run.
    pub fn proxy_forced_off(&self) -> bool {
        self.proxy_forced_off.load(Ordering::Relaxed)
    }

    /// The saved trusted-proxy settings (what the screen shows; see
    /// [`Auth::proxy_forced_off`] for whether they apply).
    pub fn proxy(&self) -> ProxyConfig {
        self.state.read().expect("auth lock").proxy.config.clone()
    }

    /// Checks, saves and applies new trusted-proxy settings, returning
    /// them in canonical form. It asks for no password: the route does
    /// ([`require_fresh_password`]), and a keep-or-revert timer restoring
    /// the previous value calls this directly.
    pub fn set_proxy(&self, config: ProxyConfig) -> Result<ProxyConfig, SetProxyError> {
        let proxy = Proxy::compile(config).map_err(SetProxyError::Invalid)?;
        let wide = proxy.config.enabled && proxy.has_wide_range();
        let mut state = self.state.write().expect("auth lock");
        let previous = std::mem::replace(&mut state.proxy, proxy);
        if let Err(e) = self.save(&state) {
            state.proxy = previous;
            return Err(SetProxyError::Storage(e));
        }
        let saved = state.proxy.config.clone();
        drop(state);
        // Once it applies, and outside the lock.
        if wide {
            lodestar_log::log_event!(log_events::AUTH_PROXY_WIDE_RANGE);
        }
        Ok(saved)
    }

    /// Whether the trusted-proxy user header signs this request in (see
    /// [`proxy`]): `peer` is the TCP peer, not [`crate::ClientIp`].
    pub fn proxy_signs_in(&self, peer: IpAddr, headers: &HeaderMap) -> bool {
        if self.proxy_forced_off() {
            return false;
        }
        let state = self.state.read().expect("auth lock");
        state.password.is_some() && state.proxy.signs_in(peer, headers)
    }

    /// The client's address for per-client limits: the TCP peer, or
    /// behind a trusted proxy the forwarded client (see [`proxy`]).
    pub fn client_ip(&self, peer: IpAddr, headers: &HeaderMap) -> IpAddr {
        if self.proxy_forced_off() {
            return proxy::canonical(peer);
        }
        self.state
            .read()
            .expect("auth lock")
            .proxy
            .client_ip(peer, headers)
    }

    /// The one-time token to print at startup, while setup is pending.
    pub fn setup_token(&self) -> Option<String> {
        self.setup_token.lock().expect("token lock").clone()
    }

    pub fn needs_setup(&self) -> bool {
        self.state.read().expect("auth lock").password.is_none()
    }

    /// Claims the instance; returns a session cookie header value.
    pub fn setup(&self, ip: IpAddr, token: &str, password: &str) -> Result<String, AuthError> {
        let result = (|| -> Result<String, AuthError> {
            self.allow(ip)?;
            let mut pending = self.setup_token.lock().expect("token lock");
            let Some(expected) = pending.as_deref() else {
                return Err(AuthError::SetupDone);
            };
            let normalized = token.trim().to_uppercase();
            if !constant_time_eq(normalized.as_bytes(), expected.as_bytes()) {
                return Err(AuthError::BadToken);
            }
            check_strength(password)?;
            let encoded = hash_password(password);
            let mut state = self.state.write().expect("auth lock");
            state.key = random::<32>();
            state.password = Some(encoded);
            self.save(&state)?;
            *pending = None;
            Ok(self.cookie(&state.key))
        })();
        log_events::checked(ip, log_events::Check::Setup, &result);
        result
    }

    /// Checks the password; returns a session cookie header value.
    pub fn login(&self, ip: IpAddr, password: &str) -> Result<String, AuthError> {
        let result = (|| -> Result<String, AuthError> {
            self.allow(ip)?;
            let state = self.state.read().expect("auth lock");
            let Some(stored) = &state.password else {
                return Err(AuthError::NotSetUp);
            };
            if password.len() > MAX_PASSWORD_BYTES || !verify_password(stored, password) {
                return Err(AuthError::BadPassword);
            }
            Ok(self.cookie(&state.key))
        })();
        log_events::checked(ip, log_events::Check::SignIn, &result);
        result
    }

    /// Checks the password again for one confirmed change (§20.11): asked
    /// every time, never remembered, rate-limited like a login. Routes
    /// call [`require_fresh_password`], which also answers a missing one.
    pub fn confirm(&self, ip: IpAddr, password: &str) -> Result<(), AuthError> {
        let result = (|| -> Result<(), AuthError> {
            self.allow(ip)?;
            let state = self.state.read().expect("auth lock");
            let Some(stored) = &state.password else {
                return Err(AuthError::NotSetUp);
            };
            if password.len() > MAX_PASSWORD_BYTES || !verify_password(stored, password) {
                return Err(AuthError::BadPassword);
            }
            Ok(())
        })();
        log_events::checked(ip, log_events::Check::Confirm, &result);
        result
    }

    /// Sets a new password and ends every other session.
    pub fn change_password(&self, ip: IpAddr, old: &str, new: &str) -> Result<String, AuthError> {
        let result = (|| -> Result<String, AuthError> {
            self.allow(ip)?;
            let mut state = self.state.write().expect("auth lock");
            let ok = state
                .password
                .as_deref()
                .is_some_and(|p| old.len() <= MAX_PASSWORD_BYTES && verify_password(p, old));
            if !ok {
                return Err(AuthError::BadPassword);
            }
            check_strength(new)?;
            state.password = Some(hash_password(new));
            state.key = random::<32>();
            self.save(&state)?;
            Ok(self.cookie(&state.key))
        })();
        log_events::checked(ip, log_events::Check::PasswordChange, &result);
        result
    }

    /// Ends every session but the caller's: rotates the session key and
    /// returns a new cookie for this browser. Confirm the password first
    /// ([`require_fresh_password`]).
    pub fn rotate_sessions(&self) -> Result<String, AuthError> {
        let mut state = self.state.write().expect("auth lock");
        if state.password.is_none() {
            return Err(AuthError::NotSetUp);
        }
        let previous = state.key;
        state.key = random::<32>();
        if let Err(e) = self.save(&state) {
            state.key = previous;
            return Err(e);
        }
        Ok(self.cookie(&state.key))
    }

    /// Whether a `Cookie` header carries a valid, unexpired session.
    pub fn verify(&self, cookie_header: &str) -> bool {
        let Some(value) = cookie_value(cookie_header, COOKIE) else {
            return false;
        };
        let state = self.state.read().expect("auth lock");
        if state.password.is_none() {
            return false;
        }
        let mut it = value.split('.');
        let (Some(expiry), Some(nonce), Some(mac), None) =
            (it.next(), it.next(), it.next(), it.next())
        else {
            return false;
        };
        let Ok(expiry_secs) = expiry.parse::<u64>() else {
            return false;
        };
        if expiry_secs <= now() {
            return false;
        }
        let expected = sign(&state.key, expiry, nonce);
        constant_time_eq(expected.as_bytes(), mac.as_bytes())
    }

    /// A `Set-Cookie` value that clears the session.
    pub fn logout_cookie(&self) -> String {
        format!(
            "{COOKIE}=; Path=/; HttpOnly; SameSite=Strict; Max-Age=0{}",
            self.secure()
        )
    }

    fn cookie(&self, key: &[u8; 32]) -> String {
        let expiry = (now() + SESSION_TTL.as_secs()).to_string();
        let nonce = hex(&random::<16>());
        let mac = sign(key, &expiry, &nonce);
        format!(
            "{COOKIE}={expiry}.{nonce}.{mac}; Path=/; HttpOnly; SameSite=Strict; Max-Age={}{}",
            SESSION_TTL.as_secs(),
            self.secure()
        )
    }

    fn secure(&self) -> &'static str {
        if self.secure_cookies { "; Secure" } else { "" }
    }

    fn allow(&self, ip: IpAddr) -> Result<(), AuthError> {
        self.login.take(ip).map_err(|_| AuthError::RateLimited)
    }

    fn save(&self, state: &State) -> Result<(), AuthError> {
        let file = AuthFile {
            password: state.password.clone(),
            session_key: hex(&state.key),
            proxy: state.proxy.config.clone(),
        };
        write_private(&self.path, &file)
            .map_err(|e| AuthError::Storage(format!("saving auth: {e}")))
    }
}

/// Why [`Auth::set_proxy`] refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SetProxyError {
    Invalid(ProxyError),
    Storage(AuthError),
}

/// The fresh-password check (§20.11) every confirming route calls:
/// changes that weaken filtering, and the Security screen's password,
/// auth-mode, proxy, key and sign-out-others changes (§10.10.7).
///
/// - no password (`None` or empty): `428 confirm_required`, `field:
///   "password"`, and no attempt is spent;
/// - a wrong one: `401 bad_password`, `field: "password"`;
/// - too many attempts from `ip`: `429 rate_limited` (the login bucket);
/// - not set up: `409 not_set_up`.
///
/// Nothing is remembered between calls, and the password is never
/// logged. It runs Argon2: call it on the blocking pool, or use
/// [`confirm_fresh_password`] from async code. Proxy-header auth doesn't
/// skip it.
pub fn require_fresh_password(auth: &Auth, ip: IpAddr, password: Option<&str>) -> ApiResult<()> {
    match password {
        None | Some("") => Err(confirm_required()),
        Some(p) => auth.confirm(ip, p).map_err(|e| auth_error(e, "password")),
    }
}

/// [`require_fresh_password`] on the blocking pool.
pub async fn confirm_fresh_password(
    auth: &Arc<Auth>,
    ip: IpAddr,
    password: Option<String>,
) -> ApiResult<()> {
    // Answer a missing password without a trip to the pool.
    if password.as_deref().is_none_or(str::is_empty) {
        return Err(confirm_required());
    }
    let auth = auth.clone();
    tokio::task::spawn_blocking(move || require_fresh_password(&auth, ip, password.as_deref()))
        .await
        .map_err(internal)?
}

/// The 428 a confirming route answers until the password comes along.
pub fn confirm_required() -> ApiError {
    ApiError::new(
        StatusCode::PRECONDITION_REQUIRED,
        "confirm_required",
        "Confirm this change with your password.",
    )
    .field("password")
}

fn check_strength(password: &str) -> Result<(), AuthError> {
    if password.chars().count() < MIN_PASSWORD_CHARS || password.len() > MAX_PASSWORD_BYTES {
        return Err(AuthError::WeakPassword);
    }
    Ok(())
}

fn argon2(m: u32, t: u32, p: u32) -> Option<Argon2<'static>> {
    Some(Argon2::new(
        Algorithm::Argon2id,
        Version::V0x13,
        Params::new(m, t, p, Some(32)).ok()?,
    ))
}

fn hash_password(password: &str) -> String {
    let (m, t, p) = (
        Params::DEFAULT_M_COST,
        Params::DEFAULT_T_COST,
        Params::DEFAULT_P_COST,
    );
    let salt = random::<16>();
    let mut out = [0u8; 32];
    argon2(m, t, p)
        .expect("default parameters are valid")
        .hash_password_into(password.as_bytes(), &salt, &mut out)
        .expect("valid lengths");
    format!(
        "argon2id$v=19$m={m},t={t},p={p}${}${}",
        hex(&salt),
        hex(&out)
    )
}

fn verify_password(encoded: &str, password: &str) -> bool {
    let parts: Vec<&str> = encoded.split('$').collect();
    let [alg, "v=19", params, salt, hash] = parts.as_slice() else {
        return false;
    };
    if *alg != "argon2id" {
        return false;
    }
    let mut m = None;
    let mut t = None;
    let mut p = None;
    for kv in params.split(',') {
        match kv.split_once('=') {
            Some(("m", v)) => m = v.parse().ok(),
            Some(("t", v)) => t = v.parse().ok(),
            Some(("p", v)) => p = v.parse().ok(),
            _ => return false,
        }
    }
    let (Some(m), Some(t), Some(p), Some(salt), Some(hash)) = (m, t, p, unhex(salt), unhex(hash))
    else {
        return false;
    };
    let Some(a) = argon2(m, t, p) else {
        return false;
    };
    let mut out = vec![0u8; hash.len().clamp(16, 64)];
    if hash.len() != out.len()
        || a.hash_password_into(password.as_bytes(), &salt, &mut out)
            .is_err()
    {
        return false;
    }
    constant_time_eq(&out, &hash)
}

fn sign(key: &[u8; 32], expiry: &str, nonce: &str) -> String {
    let mut mac = <Hmac<Sha256> as KeyInit>::new_from_slice(key).expect("any key length");
    mac.update(b"lodestar-session-v1|");
    mac.update(expiry.as_bytes());
    mac.update(b"|");
    mac.update(nonce.as_bytes());
    hex(&mac.finalize().into_bytes())
}

/// The value of cookie `name` in a `Cookie` header.
pub fn cookie_value<'a>(header: &'a str, name: &str) -> Option<&'a str> {
    header.split(';').find_map(|kv| {
        let (k, v) = kv.trim().split_once('=')?;
        (k == name).then_some(v)
    })
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

fn random<const N: usize>() -> [u8; N] {
    rand::rng().random()
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn unhex(s: &str) -> Option<Vec<u8>> {
    if s.len() % 2 != 0 || s.len() > 256 {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok())
        .collect()
}

fn unhex32(s: &str) -> Option<[u8; 32]> {
    unhex(s)?.try_into().ok()
}

/// Writes `file` readable by the owner only, atomically.
fn write_private(path: &Path, file: &AuthFile) -> io::Result<()> {
    let tmp = path.with_extension("json.tmp");
    let text = serde_json::to_string_pretty(file).map_err(io::Error::other)?;
    {
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        use std::io::Write;
        let mut f = opts.open(&tmp)?;
        f.write_all(text.as_bytes())?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    const IP: IpAddr = IpAddr::V4(std::net::Ipv4Addr::LOCALHOST);

    fn fresh() -> (tempfile::TempDir, Auth) {
        let dir = tempfile::tempdir().unwrap();
        let auth = Auth::load(&dir.path().join("auth.json"), false).unwrap();
        (dir, auth)
    }

    fn header(set_cookie: &str) -> String {
        set_cookie.split(';').next().unwrap().to_string()
    }

    #[test]
    fn setup_then_login_and_sessions() {
        let (dir, auth) = fresh();
        assert!(auth.needs_setup());
        let token = auth.setup_token().unwrap();
        assert_eq!(token.len(), 19);
        assert_eq!(
            auth.setup(IP, "WRONG", "long enough pw"),
            Err(AuthError::BadToken)
        );
        assert_eq!(
            auth.setup(IP, &token, "short"),
            Err(AuthError::WeakPassword)
        );
        let cookie = auth
            .setup(IP, &token.to_lowercase(), "correct horse battery")
            .unwrap();
        assert!(cookie.contains("HttpOnly") && cookie.contains("SameSite=Strict"));
        assert!(auth.verify(&header(&cookie)));
        assert!(!auth.needs_setup() && auth.setup_token().is_none());
        assert_eq!(
            auth.setup(IP, &token, "another password"),
            Err(AuthError::SetupDone)
        );

        // Survives a restart; the session key does too.
        let auth = Auth::load(&dir.path().join("auth.json"), true).unwrap();
        assert!(auth.setup_token().is_none());
        assert!(auth.verify(&header(&cookie)));
        assert_eq!(
            auth.login(IP, "nope nope nope"),
            Err(AuthError::BadPassword)
        );
        let again = auth.login(IP, "correct horse battery").unwrap();
        assert!(again.ends_with("; Secure"));
        assert!(auth.verify(&format!("other=1; {}", header(&again))));
    }

    #[test]
    fn forged_expired_and_rotated_sessions_fail() {
        let (_dir, auth) = fresh();
        let token = auth.setup_token().unwrap();
        let cookie = header(&auth.setup(IP, &token, "correct horse battery").unwrap());
        let value = cookie_value(&cookie, COOKIE).unwrap();
        let (expiry, rest) = value.split_once('.').unwrap();
        let later = format!("{COOKIE}={}.{rest}", expiry.parse::<u64>().unwrap() + 1);
        assert!(!auth.verify(&later), "changed expiry breaks the MAC");
        let past = format!("{COOKIE}=1.{}", rest);
        assert!(!auth.verify(&past));
        assert!(!auth.verify("lodestar_session=garbage"));
        assert!(!auth.verify(""));
        let new = auth
            .change_password(IP, "correct horse battery", "a different password")
            .unwrap();
        assert!(!auth.verify(&cookie), "rotated key ends old sessions");
        assert!(auth.verify(&header(&new)));
    }

    #[test]
    fn login_attempts_are_rate_limited() {
        let (_dir, mut auth) = fresh();
        auth.login.refill = 0.0;
        let token = auth.setup_token().unwrap();
        auth.setup(IP, &token, "correct horse battery").unwrap();
        // Setup used one attempt; four more fail on the password...
        for _ in 0..4 {
            assert_eq!(
                auth.login(IP, "bad bad bad bad"),
                Err(AuthError::BadPassword)
            );
        }
        // ...then the bucket is empty, even for the right password.
        assert_eq!(
            auth.login(IP, "correct horse battery"),
            Err(AuthError::RateLimited)
        );
        let other: IpAddr = "10.0.0.9".parse().unwrap();
        assert!(auth.login(other, "correct horse battery").is_ok());
    }

    #[test]
    fn stored_hashes_verify_and_tampering_fails() {
        let h = hash_password("correct horse battery");
        assert!(verify_password(&h, "correct horse battery"));
        assert!(!verify_password(&h, "correct horse batterY"));
        assert!(!verify_password(
            &h.replace("argon2id", "argon2i"),
            "correct horse battery"
        ));
        assert!(!verify_password("garbage", "x"));
        assert!(!verify_password("argon2id$v=19$m=8,t=1,p=1$00$00", "x"));
    }

    #[test]
    fn older_files_load_and_a_bad_proxy_section_loads_as_off() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("auth.json");
        let key = "ab".repeat(32);
        std::fs::write(
            &path,
            format!(r#"{{"password":null,"session_key":"{key}"}}"#),
        )
        .unwrap();
        let auth = Auth::load(&path, false).unwrap();
        assert_eq!(auth.proxy(), ProxyConfig::default());
        // Saving without a proxy change keeps the file as it was shaped.
        let token = auth.setup_token().unwrap();
        auth.setup(IP, &token, "correct horse battery").unwrap();
        assert!(!std::fs::read_to_string(&path).unwrap().contains("proxy"));

        let text = std::fs::read_to_string(&path).unwrap();
        let mut v: serde_json::Value = serde_json::from_str(&text).unwrap();
        v["proxy"] = serde_json::json!({ "enabled": true, "trusted": ["0.0.0.0/0"] });
        std::fs::write(&path, v.to_string()).unwrap();
        let auth = Auth::load(&path, false).unwrap();
        assert!(!auth.proxy().enabled, "refused on load: off");
        assert!(auth.login(IP, "correct horse battery").is_ok());
    }

    /// The proxy settings' warnings: a section that loads as off, an
    /// override that isn't `off`, and a wide range once it's saved. Each
    /// event fires only here, so its counter says it was logged.
    #[test]
    fn proxy_warnings_are_logged() {
        use log_events::{
            AUTH_PROXY_OVERRIDE_IGNORED, AUTH_PROXY_SETTINGS_INVALID, AUTH_PROXY_WIDE_RANGE,
        };
        let count = |e: &lodestar_log::Event| e.count();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("auth.json");
        let key = "ab".repeat(32);
        std::fs::write(
            &path,
            format!(
                r#"{{"password":null,"session_key":"{key}","proxy":{{"enabled":true,"trusted":["0.0.0.0/0"]}}}}"#
            ),
        )
        .unwrap();
        let before = count(&AUTH_PROXY_SETTINGS_INVALID::EVENT);
        let auth = Auth::load(&path, false).unwrap();
        assert!(count(&AUTH_PROXY_SETTINGS_INVALID::EVENT) > before);

        let before = count(&AUTH_PROXY_OVERRIDE_IGNORED::EVENT);
        assert!(!auth.apply_proxy_override(Some("false")));
        assert!(count(&AUTH_PROXY_OVERRIDE_IGNORED::EVENT) > before);
        let before = count(&AUTH_PROXY_OVERRIDE_IGNORED::EVENT);
        assert!(auth.apply_proxy_override(Some("OFF")));
        assert_eq!(count(&AUTH_PROXY_OVERRIDE_IGNORED::EVENT), before);

        let before = count(&AUTH_PROXY_WIDE_RANGE::EVENT);
        auth.set_proxy(ProxyConfig {
            enabled: true,
            trusted: vec!["10.0.0.0/8".into()],
            ..ProxyConfig::default()
        })
        .unwrap();
        assert!(count(&AUTH_PROXY_WIDE_RANGE::EVENT) > before);
    }

    #[test]
    fn set_proxy_saves_and_refuses_bad_values() {
        let (dir, auth) = fresh();
        let bad = ProxyConfig {
            enabled: true,
            ..ProxyConfig::default()
        };
        assert!(matches!(
            auth.set_proxy(bad),
            Err(SetProxyError::Invalid(_))
        ));
        let saved = auth
            .set_proxy(ProxyConfig {
                enabled: true,
                trusted: vec!["10.0.0.7/8".into()],
                ..ProxyConfig::default()
            })
            .unwrap();
        assert_eq!(saved.trusted, ["10.0.0.0/8"]);
        let again = Auth::load(&dir.path().join("auth.json"), false).unwrap();
        assert_eq!(again.proxy(), saved);
    }

    #[test]
    fn the_fresh_password_check() {
        let (_dir, mut auth) = fresh();
        auth.login.refill = 0.0;
        let token = auth.setup_token().unwrap();
        auth.setup(IP, &token, "correct horse battery").unwrap();
        for missing in [None, Some("")] {
            let e = require_fresh_password(&auth, IP, missing).unwrap_err();
            assert_eq!(
                (e.status(), e.code()),
                (StatusCode::PRECONDITION_REQUIRED, "confirm_required")
            );
            assert_eq!(e.error_body().field.as_deref(), Some("password"));
        }
        assert!(require_fresh_password(&auth, IP, Some("correct horse battery")).is_ok());
        let e = require_fresh_password(&auth, IP, Some("wrong wrong wrong")).unwrap_err();
        assert_eq!(
            (e.status(), e.code()),
            (StatusCode::UNAUTHORIZED, "bad_password")
        );
        // Setup, one right and one wrong: two attempts left. Missing ones
        // spent nothing.
        for _ in 0..2 {
            require_fresh_password(&auth, IP, Some("wrong wrong wrong")).unwrap_err();
        }
        let e = require_fresh_password(&auth, IP, Some("correct horse battery")).unwrap_err();
        assert_eq!(
            (e.status(), e.code()),
            (StatusCode::TOO_MANY_REQUESTS, "rate_limited")
        );
    }

    #[test]
    fn rotating_sessions_ends_the_old_ones() {
        let (_dir, auth) = fresh();
        assert_eq!(auth.rotate_sessions(), Err(AuthError::NotSetUp));
        let token = auth.setup_token().unwrap();
        let old = header(&auth.setup(IP, &token, "correct horse battery").unwrap());
        let new = header(&auth.rotate_sessions().unwrap());
        assert!(!auth.verify(&old) && auth.verify(&new));
    }

    #[cfg(unix)]
    #[test]
    fn the_file_is_private() {
        use std::os::unix::fs::PermissionsExt;
        let (dir, _auth) = fresh();
        let mode = std::fs::metadata(dir.path().join("auth.json"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }
}
