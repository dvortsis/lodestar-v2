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
//!   Changing the password rotates the key, which ends every session.
//! - **Login attempts** are rate-limited per IP; Argon2 makes each one
//!   slow anyway.

use std::collections::HashMap;
use std::io;
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, RwLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use argon2::{Algorithm, Argon2, Params, Version};
use hmac::{Hmac, KeyInit, Mac};
use rand::RngExt;
use serde::{Deserialize, Serialize};
use sha2::Sha256;

pub const COOKIE: &str = "lodestar_session";
pub const MIN_PASSWORD_CHARS: usize = 10;
const MAX_PASSWORD_BYTES: usize = 1024;
const SESSION_TTL: Duration = Duration::from_secs(30 * 86_400);
/// Login attempts per IP: a burst of 5, then one every 12 s.
const BURST: f64 = 5.0;
const REFILL_PER_SEC: f64 = 1.0 / 12.0;
const LIMITER_MAX_IPS: usize = 10_000;

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
}

#[derive(Debug)]
struct State {
    password: Option<String>,
    key: [u8; 32],
}

#[derive(Debug)]
pub struct Auth {
    path: PathBuf,
    state: RwLock<State>,
    setup_token: Mutex<Option<String>>,
    limiter: Mutex<HashMap<IpAddr, (f64, Instant)>>,
    /// Login attempts regained per second ([`REFILL_PER_SEC`]; tests set
    /// 0 so slow hashing on a busy runner can't refill the bucket).
    refill_per_sec: f64,
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
        Ok(Auth {
            path: path.to_path_buf(),
            state: RwLock::new(State {
                password: file.password,
                key,
            }),
            setup_token: Mutex::new(token),
            limiter: Mutex::default(),
            refill_per_sec: REFILL_PER_SEC,
            secure_cookies,
        })
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
    }

    /// Checks the password; returns a session cookie header value.
    pub fn login(&self, ip: IpAddr, password: &str) -> Result<String, AuthError> {
        self.allow(ip)?;
        let state = self.state.read().expect("auth lock");
        let Some(stored) = &state.password else {
            return Err(AuthError::NotSetUp);
        };
        if password.len() > MAX_PASSWORD_BYTES || !verify_password(stored, password) {
            return Err(AuthError::BadPassword);
        }
        Ok(self.cookie(&state.key))
    }

    /// Sets a new password and ends every other session.
    pub fn change_password(&self, ip: IpAddr, old: &str, new: &str) -> Result<String, AuthError> {
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
        let mut limiter = self.limiter.lock().expect("limiter lock");
        let now = Instant::now();
        if limiter.len() >= LIMITER_MAX_IPS {
            // Forget IPs that have fully recovered.
            limiter.retain(|_, (tokens, at)| {
                *tokens + now.duration_since(*at).as_secs_f64() * self.refill_per_sec < BURST
            });
        }
        let (tokens, at) = limiter.entry(ip).or_insert((BURST, now));
        *tokens =
            (*tokens + now.duration_since(*at).as_secs_f64() * self.refill_per_sec).min(BURST);
        *at = now;
        if *tokens < 1.0 {
            return Err(AuthError::RateLimited);
        }
        *tokens -= 1.0;
        Ok(())
    }

    fn save(&self, state: &State) -> Result<(), AuthError> {
        let file = AuthFile {
            password: state.password.clone(),
            session_key: hex(&state.key),
        };
        write_private(&self.path, &file)
            .map_err(|e| AuthError::Storage(format!("saving auth: {e}")))
    }
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
        auth.refill_per_sec = 0.0;
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
