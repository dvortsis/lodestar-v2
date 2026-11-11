//! qBittorrent through its WebUI API v2 (checked against upstream
//! `release-4.6.3`, `release-5.0.3`, `release-5.1.2` and `release-5.2.0`,
//! `src/webui/api/`; sources in
//! `docs/decisions/2026-11-08-CMP-8b.md`).
//!
//! - **Sign-in:** `POST /api/v2/auth/login`, a form of `username` and
//!   `password`. Up to 5.1, `200 Ok.` with the `SID` cookie is a session
//!   and `200 Fails.` a wrong password; from 5.2, `204` with the
//!   `QBT_SID_<port>` cookie is a session and `401` a wrong password. `403`
//!   is a ban after failed sign-ins in both, never retried. Requests carry
//!   no `Origin` or `Referer`, which qBittorrent's cross-site check passes.
//!   A custom `WebUI\SessionCookieName` (up to 5.1) isn't supported: the
//!   sign-in fails as a protocol error.
//! - **Reading:** `GET /api/v2/app/preferences` and
//!   `GET /api/v2/torrents/categories`, with the session cookie.
//! - **Writing:** `POST /api/v2/app/setPreferences` with `json=` and only
//!   the listed keys; `POST /api/v2/torrents/createCategory` or
//!   `editCategory` with `category` and `savePath`.
//!
//! **Wanted state**, each a [`SettingKey`]:
//! - `webui.bind_address` (`web_ui_address`): `127.0.0.1` or `*` per
//!   [`WebUiBind`], only where it keeps the target reachable;
//! - `webui.local_auth` (`bypass_local_auth`): always `false`, so a
//!   process on the same machine must sign in too;
//! - `downloads.save_path` (`save_path`) and `downloads.categories`;
//! - `network.interface` (`current_network_interface`) and
//!   `network.bind_address` (`current_interface_address`) per the route:
//!   both empty for direct, one of them for an interface or an address. A
//!   SOCKS5 route is refused (Q8: qBittorrent follows an interface or an
//!   address);
//! - `webui.password` (`web_ui_password`), for a guided install whose
//!   temporary password still works (Q19).
//!
//! Applying writes the new password first, which it checks by signing in
//! with it, then categories, then the preferences (turning the localhost
//! bypass off is checked with a fresh sign-in and put back on failure), and
//! the WebUI address last, since qBittorrent restarts its WebUI on that
//! change.

use std::net::IpAddr;
use std::time::Duration;

use lodestar_integrations::secret::Secret;
use lodestar_sources_feeds::json::{self, Walk};

use crate::adapter::{
    AdapterError, BoxFuture, CheckedPlan, ConfigPlan, Configure, ConfigureInput, CookieName,
    GuardedHttp, HeaderName, HeaderValue, Limits, ProbeRequest, ProbeResponse, RequestPath,
    SettingKey, Target,
};
use crate::credentials::Field;
use crate::inputs::{CompanionId, Version};
use crate::log_events::CONFIG_APPLIED;

use super::{
    BIND_UNSUITED, CONTENT_TYPE, Origin, PLAN_MISMATCH, ROUTE_UNSUPPORTED, Route,
    SAVE_PATH_PLATFORM, SavePath, UNKNOWN_SETTING, WOULD_LOOSEN, WebUiBind, consent, cookie,
    credential, failed, form, header, key, plain,
};

/// Its catalogue id.
pub const ID: &str = "qbittorrent";

const LOGIN: &str = "/api/v2/auth/login";
const VERSION: &str = "/api/v2/app/version";
const PREFERENCES: &str = "/api/v2/app/preferences";
const SET_PREFERENCES: &str = "/api/v2/app/setPreferences";
const CATEGORIES: &str = "/api/v2/torrents/categories";
const CREATE_CATEGORY: &str = "/api/v2/torrents/createCategory";
const EDIT_CATEGORY: &str = "/api/v2/torrents/editCategory";

/// The session cookie: `SID` up to 5.1 (or a custom
/// `WebUI\SessionCookieName`, unsupported), `QBT_SID_<WebUI port>` from
/// 5.2. The port inside a container can differ from the target's, so any
/// digits match.
const SESSION: &[CookieName] = &[
    cookie(CookieName::new("SID")),
    cookie(CookieName::numbered("QBT_SID_")),
];
const COOKIE: HeaderName = header("Cookie");
const FORM: &str = "application/x-www-form-urlencoded";

pub const PASSWORD: SettingKey = key("webui.password");
pub const WEBUI_BIND: SettingKey = key("webui.bind_address");
pub const LOCAL_AUTH: SettingKey = key("webui.local_auth");
pub const SAVE_PATH: SettingKey = key("downloads.save_path");
pub const CATEGORY_SET: SettingKey = key("downloads.categories");
pub const INTERFACE: SettingKey = key("network.interface");
pub const INTERFACE_ADDRESS: SettingKey = key("network.bind_address");

/// Every key this adapter plans, in the order `apply` writes them.
const ORDER: [SettingKey; 7] = [
    PASSWORD,
    CATEGORY_SET,
    SAVE_PATH,
    LOCAL_AUTH,
    INTERFACE,
    INTERFACE_ADDRESS,
    WEBUI_BIND,
];

/// The refusal when qBittorrent has banned this address.
pub const BANNED: &str = "banned_after_failed_sign_ins";
/// The refusal when the new password doesn't sign in after it was set.
pub const PASSWORD_NOT_TAKEN: &str = "password_not_taken";
/// The refusal when turning the localhost bypass off would lock Lodestar
/// out. The bypass is put back first if this run turned it off.
pub const LOCAL_AUTH_LOCKOUT: &str = "local_auth_would_lock_out";
/// The refusal when the localhost bypass couldn't be put back: it's off and
/// the stored password doesn't sign in, so you have to fix the password in
/// qBittorrent (CMP-5g says so).
pub const LOCAL_AUTH_RESTORE_FAILED: &str = "local_auth_restore_failed";

/// A configuration request's bounds: preferences run to about 12 KiB, and a
/// write may take a moment on a slow NAS.
fn limits() -> Limits {
    Limits::new(Duration::from_secs(10), 256 * 1024).unwrap_or(Limits::PROBE)
}

/// The qBittorrent configuration adapter.
pub struct QBittorrent {
    id: CompanionId,
    /// CMP-11's version gate on Windows; `None` elsewhere.
    gate: Option<VersionGate>,
}

impl std::fmt::Debug for QBittorrent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("QBittorrent")
            .field("id", &self.id)
            .field("gate", &self.gate.is_some())
            .finish()
    }
}

impl QBittorrent {
    /// **The ungated adapter.** Production code gets a configurator only
    /// through [`super::configurator`], which applies the Windows version
    /// gate (CMP-11b); it's what that hands out on every platform but
    /// Windows. Only adapter tests construct this directly, so CMP-8b's
    /// own tests exercise it on every platform.
    /// `tests/windows_qbittorrent.rs` fails if `crates/lodestar/src` or
    /// `crates/api/src` names it.
    pub fn new(id: CompanionId) -> QBittorrent {
        QBittorrent { id, gate: None }
    }

    /// A Windows save path only behind the Windows gate, and a Linux one
    /// only without it, refused before anything is sent.
    fn path_kind(&self, input: &ConfigureInput<'_>) -> Result<(), AdapterError> {
        let windows = self.gate.is_some();
        match input.downloads.as_ref().map(|d| d.save_path()) {
            Some(SavePath::Host(_)) if windows => Err(AdapterError::Refused(SAVE_PATH_PLATFORM)),
            Some(SavePath::Windows(_)) if !windows => {
                Err(AdapterError::Refused(SAVE_PATH_PLATFORM))
            }
            _ => Ok(()),
        }
    }

    /// With CMP-11's version gate, checked right after each sign-in on the
    /// same session, so it costs no sign-in of its own.
    pub(crate) fn gated(id: CompanionId, gate: VersionGate) -> QBittorrent {
        QBittorrent {
            id,
            gate: Some(gate),
        }
    }
}

/// What the input asks for, worked out before anything is sent.
struct Wanted {
    webui: Option<&'static str>,
    /// `(interface, address)`, each empty for "any".
    route: Option<(String, String)>,
    /// A companion you confirmed or guided rather than one Lodestar
    /// installed: nothing that limits it may be cleared or widened.
    yours: bool,
}

fn wanted(input: &ConfigureInput<'_>, target: &Target) -> Result<Wanted, AdapterError> {
    let yours = !matches!(input.origin, Origin::Installed(_));
    let webui = match input.webui {
        None => None,
        // Only on a companion Lodestar installed. In your own container
        // with a published port, `127.0.0.1` cuts off Docker's port
        // forward for good, and `*` would widen what reaches it.
        Some(_) if yours => return Err(AdapterError::Refused(BIND_UNSUITED)),
        Some(b) if !b.suits(target) => return Err(AdapterError::Refused(BIND_UNSUITED)),
        Some(WebUiBind::Loopback) => Some("127.0.0.1"),
        Some(WebUiBind::ContainerNetwork) => Some("*"),
    };
    let route = match &input.route {
        None => None,
        Some(Route::Direct) => Some((String::new(), String::new())),
        Some(Route::Interface(name)) => Some((name.as_str().to_owned(), String::new())),
        Some(Route::BindAddress(ip)) => Some((String::new(), ip.get().to_string())),
        Some(Route::Socks5(_)) => return Err(AdapterError::Refused(ROUTE_UNSUPPORTED)),
    };
    Ok(Wanted {
        webui,
        route,
        yours,
    })
}

/// The preferences this adapter compares, from `/api/v2/app/preferences`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Prefs {
    pub web_ui_address: String,
    pub save_path: String,
    pub interface: String,
    pub interface_address: String,
    pub bypass_local_auth: bool,
}

/// Caps for the preferences object: about 250 flat keys, a few small
/// nested objects and lists.
const PREFS_LIMITS: json::Limits = json::Limits {
    max_depth: 4,
    max_values: 4096,
    max_array: 256,
    max_text: 1024,
};

/// The preferences' five keys this adapter compares, each exactly once and
/// of its type; other keys are passed over. `None` for anything else,
/// including a key with an escape (so `save_path` can't hide a second
/// one) or a kept string past the cap.
pub(crate) fn parse_preferences(body: &[u8]) -> Option<Prefs> {
    let s = std::str::from_utf8(body).ok()?;
    let mut j = json::Reader::new(s, PREFS_LIMITS);
    if j.peek() != Some(b'{') {
        return None;
    }
    let mut strings: [Option<String>; 4] = [None, None, None, None];
    let mut bypass: Option<bool> = None;
    const NAMES: [&str; 4] = [
        "web_ui_address",
        "save_path",
        "current_network_interface",
        "current_interface_address",
    ];
    j.object(|j, k| {
        if k.contains('\\') {
            return Err(json::Stop::Malformed);
        }
        if let Some(i) = NAMES.iter().position(|n| *n == k) {
            if strings[i].is_some() || j.peek() != Some(b'"') {
                return Err(json::Stop::Malformed);
            }
            strings[i] = Some(j.text()?.map_err(|_| json::Stop::Malformed)?);
        } else if k == "bypass_local_auth" {
            let value = match j.peek() {
                Some(b't') => true,
                Some(b'f') => false,
                _ => return Err(json::Stop::Malformed),
            };
            if bypass.is_some() {
                return Err(json::Stop::Malformed);
            }
            j.skip()?;
            bypass = Some(value);
        } else {
            j.skip()?;
        }
        Ok(())
    })
    .ok()?;
    j.end().ok()?;
    let [web_ui_address, save_path, interface, interface_address] = strings;
    Some(Prefs {
        web_ui_address: web_ui_address?,
        save_path: save_path?,
        interface: interface?,
        interface_address: interface_address?,
        bypass_local_auth: bypass?,
    })
}

/// Caps for the categories object: up to some hundreds of categories of a
/// few short keys each.
const CATEGORY_LIMITS: json::Limits = json::Limits {
    max_depth: 4,
    max_values: 4096,
    max_array: 64,
    max_text: 1024,
};

/// `/api/v2/torrents/categories`: each category's name and `savePath`
/// (empty when it has none). A name with an escape can't be one of
/// Lodestar's (plain ASCII) and is passed over; a name twice, a value
/// that isn't an object, or `savePath` twice or not a string is `None`.
pub(crate) fn parse_categories(body: &[u8]) -> Option<Vec<(String, String)>> {
    let s = std::str::from_utf8(body).ok()?;
    let mut j = json::Reader::new(s, CATEGORY_LIMITS);
    if j.peek() != Some(b'{') {
        return None;
    }
    let mut out: Vec<(String, String)> = Vec::new();
    j.object(|j, name| {
        if name.contains('\\') {
            j.skip()?;
            return Ok(());
        }
        if out.iter().any(|(n, _)| n == name) || j.peek() != Some(b'{') {
            return Err(json::Stop::Malformed);
        }
        let mut path: Option<String> = None;
        j.object(|j, k| {
            if k == "savePath" {
                if path.is_some() || j.peek() != Some(b'"') {
                    return Err(json::Stop::Malformed);
                }
                path = Some(j.text()?.map_err(|_| json::Stop::Malformed)?);
            } else {
                j.skip()?;
            }
            Ok(())
        })?;
        out.push((name.to_owned(), path.unwrap_or_default()));
        Ok(())
    })
    .ok()?;
    j.end().ok()?;
    Some(out)
}

/// Two paths as qBittorrent stores them, ignoring one trailing `/`.
fn same_path(a: &str, b: &str) -> bool {
    let trim = |s: &str| {
        if s.len() > 1 {
            s.strip_suffix('/').unwrap_or(s).to_owned()
        } else {
            s.to_owned()
        }
    };
    trim(a) == trim(b)
}

/// The save path qBittorrent has against the wanted one: a Windows path
/// without case and with either separator (CMP-11), else [`same_path`].
fn same_save_path(have: &str, want: &SavePath) -> bool {
    match want {
        SavePath::Host(p) => same_path(have, p.as_str()),
        SavePath::Windows(p) => p.same(have),
    }
}

/// Two bind addresses: both empty, or the same address however written.
fn same_address(have: &str, want: &str) -> bool {
    match (have.trim(), want) {
        ("", "") => true,
        (h, w) => matches!(
            (h.parse::<IpAddr>(), w.parse::<IpAddr>()),
            (Ok(a), Ok(b)) if a == b
        ),
    }
}

/// A session: the session cookie as `name=value`.
struct Session(Secret);

impl Session {
    fn header(&self) -> Result<HeaderValue, AdapterError> {
        HeaderValue::secret(self.0.clone()).map_err(|_| AdapterError::Protocol("session cookie"))
    }
}

fn path(p: &'static str) -> Result<RequestPath, AdapterError> {
    RequestPath::parse(p).map_err(|_| AdapterError::Protocol("request path"))
}

/// One sign-in with `password`.
async fn login(
    http: &dyn GuardedHttp,
    target: &Target,
    input: &ConfigureInput<'_>,
    password: &Secret,
) -> Result<Session, AdapterError> {
    let body = form(&[
        ("username", input.username.as_str()),
        ("password", password.expose()),
    ]);
    let req = ProbeRequest::new(target, path(LOGIN)?, limits())
        .post(body)
        .header(CONTENT_TYPE, plain(FORM)?)
        .session_cookie(SESSION);
    let r = http.send(req).await?;
    // Up to 5.1: `200 Ok.` or `200 Fails.`. From 5.2: `204` with no body,
    // or `401`. A ban is `403` in both.
    match (r.status(), r.body().trim_ascii()) {
        (200, b"Ok.") | (204, b"") => r
            .session_cookie()
            .cloned()
            .map(Session)
            .ok_or(AdapterError::Protocol("no session cookie")),
        (200, b"Fails.") | (401, _) => Err(AdapterError::Unauthorized),
        (403, _) => Err(AdapterError::Refused(BANNED)),
        _ => Err(AdapterError::Protocol("sign-in answer")),
    }
}

/// Signs in with the stored credential, or, for a guided install, the
/// temporary password. `temporary_first` tries the temporary one first
/// (an apply whose plan replaces it), so a run costs at most one failed
/// sign-in. True when the temporary password signed in. When the stored
/// credential is the temporary password itself, it signs in once.
async fn sign_in(
    http: &dyn GuardedHttp,
    target: &Target,
    input: &ConfigureInput<'_>,
    temporary_first: bool,
) -> Result<(Session, bool), AdapterError> {
    let stored = credential(input, Field::WebUiPassword)?;
    let Origin::Guided { temporary, .. } = &input.origin else {
        return Ok((login(http, target, input, &stored).await?, false));
    };
    // The temporary password held as the credential too (a guided plan,
    // before any password is generated): one sign-in, so a wrong
    // temporary password costs one failure, not two.
    if stored.expose() == temporary.expose() {
        return Ok((login(http, target, input, temporary).await?, true));
    }
    let order = if temporary_first {
        [(temporary, true), (&stored, false)]
    } else {
        [(&stored, false), (temporary, true)]
    };
    match login(http, target, input, order[0].0).await {
        Ok(s) => Ok((s, order[0].1)),
        Err(AdapterError::Unauthorized) => {
            Ok((login(http, target, input, order[1].0).await?, order[1].1))
        }
        Err(e) => Err(e),
    }
}

/// A request with the session cookie.
async fn call(
    http: &dyn GuardedHttp,
    target: &Target,
    session: &Session,
    p: &'static str,
    body: Option<Vec<u8>>,
) -> Result<ProbeResponse, AdapterError> {
    let mut req = ProbeRequest::new(target, path(p)?, limits()).header(COOKIE, session.header()?);
    if let Some(b) = body {
        req = req.post(b).header(CONTENT_TYPE, plain(FORM)?);
    }
    let r = http.send(req).await?;
    // Writes answer `200` with an empty body up to 5.1, `204` from 5.2.
    match r.status() {
        200 | 204 => Ok(r),
        401 | 403 => Err(AdapterError::Unauthorized),
        400 | 404 | 409 => Err(AdapterError::Refused("setting_refused")),
        _ => Err(AdapterError::Protocol("HTTP status")),
    }
}

/// A check of the signed-in qBittorrent's version, run on the adapter's
/// own session before anything is read or written (CMP-11's Windows gate).
/// It sees only Lodestar's parsed [`Version`].
pub(crate) type VersionGate = Box<dyn Fn(&Version) -> Result<(), AdapterError> + Send + Sync>;

/// Reads the version on `session` and runs `gate` on it.
async fn check_version(
    http: &dyn GuardedHttp,
    target: &Target,
    session: &Session,
    gate: &VersionGate,
) -> Result<(), AdapterError> {
    let r = call(http, target, session, VERSION, None).await?;
    let v = crate::detect::qbittorrent::parse_version(r.body())
        .ok_or(AdapterError::Protocol("version"))?;
    gate(&v)
}

async fn read_prefs(
    http: &dyn GuardedHttp,
    target: &Target,
    session: &Session,
) -> Result<Prefs, AdapterError> {
    let r = call(http, target, session, PREFERENCES, None).await?;
    parse_preferences(r.body()).ok_or(AdapterError::Protocol("preferences"))
}

async fn read_categories(
    http: &dyn GuardedHttp,
    target: &Target,
    session: &Session,
) -> Result<Vec<(String, String)>, AdapterError> {
    let r = call(http, target, session, CATEGORIES, None).await?;
    parse_categories(r.body()).ok_or(AdapterError::Protocol("categories"))
}

/// The categories that differ: `(name, save path, exists)`.
fn category_changes(
    input: &ConfigureInput<'_>,
    have: &[(String, String)],
) -> Vec<(String, String, bool)> {
    let Some(d) = &input.downloads else {
        return Vec::new();
    };
    d.categories()
        .iter()
        .filter_map(|c| {
            let want = c.save_path.as_ref().map(|p| p.as_str()).unwrap_or("");
            match have.iter().find(|(n, _)| n == c.name.as_str()) {
                Some((_, p)) if same_path(p, want) => None,
                found => Some((c.name.as_str().to_owned(), want.to_owned(), found.is_some())),
            }
        })
        .collect()
}

/// The keys whose current value differs from the wanted one.
fn differing(input: &ConfigureInput<'_>, w: &Wanted, p: &Prefs) -> Vec<SettingKey> {
    let mut out = Vec::new();
    if p.bypass_local_auth {
        out.push(LOCAL_AUTH);
    }
    if let Some(d) = &input.downloads
        && !same_save_path(&p.save_path, d.save_path())
    {
        out.push(SAVE_PATH);
    }
    if let Some((iface, addr)) = &w.route {
        if p.interface != *iface {
            out.push(INTERFACE);
        }
        if !same_address(&p.interface_address, addr) {
            out.push(INTERFACE_ADDRESS);
        }
    }
    if let Some(addr) = w.webui
        && p.web_ui_address != addr
    {
        out.push(WEBUI_BIND);
    }
    out
}

/// Whether the route would clear or replace an interface or address that
/// your own qBittorrent is already bound to (a VPN kill-switch binding).
/// Binding one where none is set only narrows it, and is allowed.
fn loosens(w: &Wanted, p: &Prefs) -> bool {
    let Some((iface, addr)) = &w.route else {
        return false;
    };
    w.yours
        && ((!p.interface.is_empty() && p.interface != *iface)
            || (!p.interface_address.trim().is_empty()
                && !same_address(&p.interface_address, addr)))
}

/// Every listed key has what it needs in the input, checked before
/// anything is sent.
fn check_plan(
    plan: &ConfigPlan,
    input: &ConfigureInput<'_>,
    w: &Wanted,
) -> Result<(), AdapterError> {
    let guided = matches!(input.origin, Origin::Guided { .. });
    for k in plan.changes() {
        let ok = if *k == PASSWORD {
            guided
        } else if *k == CATEGORY_SET || *k == SAVE_PATH {
            input.downloads.is_some()
        } else if *k == LOCAL_AUTH {
            true
        } else if *k == INTERFACE || *k == INTERFACE_ADDRESS {
            w.route.is_some()
        } else if *k == WEBUI_BIND {
            w.webui.is_some()
        } else {
            return Err(AdapterError::Refused(UNKNOWN_SETTING));
        };
        if !ok {
            return Err(AdapterError::Refused(PLAN_MISMATCH));
        }
    }
    Ok(())
}

/// Whether qBittorrent skips sign-in for Lodestar's address (a subnet
/// whitelist, or loopback with `bypass_local_auth`): the version endpoint
/// answers `200` without a session. Then any password signs in, so a
/// guided install can't tell the temporary password from the new one.
async fn auth_bypassed(http: &dyn GuardedHttp, target: &Target) -> Result<bool, AdapterError> {
    let r = http
        .send(ProbeRequest::new(target, path(VERSION)?, limits()))
        .await?;
    Ok(r.status() == 200)
}

/// One `setPreferences` with this JSON object.
async fn set_preferences(
    http: &dyn GuardedHttp,
    target: &Target,
    session: &Session,
    json: serde_json::Map<String, serde_json::Value>,
) -> Result<(), AdapterError> {
    let text = serde_json::Value::Object(json).to_string();
    call(
        http,
        target,
        session,
        SET_PREFERENCES,
        Some(form(&[("json", &text)])),
    )
    .await
    .map(|_| ())
}

fn one(name: &str, value: serde_json::Value) -> serde_json::Map<String, serde_json::Value> {
    let mut m = serde_json::Map::new();
    m.insert(name.to_owned(), value);
    m
}

fn applied(k: &SettingKey) {
    lodestar_log::log_event!(CONFIG_APPLIED, companion = ID, setting = k.as_str());
}

impl QBittorrent {
    async fn plan_inner(
        &self,
        http: &dyn GuardedHttp,
        target: &Target,
        input: &ConfigureInput<'_>,
    ) -> Result<ConfigPlan, AdapterError> {
        consent(input, target)?;
        self.path_kind(input)?;
        let w = wanted(input, target)?;
        let guided = matches!(input.origin, Origin::Guided { .. });
        let bypassed = guided && auth_bypassed(http, target).await?;
        let (session, temporary) = sign_in(http, target, input, false).await?;
        if let Some(g) = &self.gate {
            check_version(http, target, &session, g).await?;
        }
        let prefs = read_prefs(http, target, &session).await?;
        if loosens(&w, &prefs) {
            return Err(AdapterError::Refused(WOULD_LOOSEN));
        }
        let mut keys = differing(input, &w, &prefs);
        if input.downloads.is_some() {
            let have = read_categories(http, target, &session).await?;
            if !category_changes(input, &have).is_empty() {
                keys.push(CATEGORY_SET);
            }
        }
        // Under an auth bypass the temporary password can't be seen to
        // work or not, so a guided install always writes the new one.
        if temporary || bypassed {
            keys.push(PASSWORD);
        }
        let changes = ORDER.into_iter().filter(|k| keys.contains(k)).collect();
        Ok(ConfigPlan::new(self.id.clone(), changes))
    }

    async fn apply_inner(
        &self,
        http: &dyn GuardedHttp,
        target: &Target,
        input: &ConfigureInput<'_>,
        plan: &ConfigPlan,
    ) -> Result<(), AdapterError> {
        consent(input, target)?;
        self.path_kind(input).map_err(|e| failed(ID, "apply", e))?;
        let w = wanted(input, target).map_err(|e| failed(ID, "apply", e))?;
        check_plan(plan, input, &w).map_err(|e| failed(ID, "apply", e))?;
        let listed = |k: &SettingKey| plan.changes().contains(k);
        let bypassed = listed(&PASSWORD)
            && auth_bypassed(http, target)
                .await
                .map_err(|e| failed(ID, "apply", e))?;
        let (session, _) = sign_in(http, target, input, listed(&PASSWORD))
            .await
            .map_err(|e| failed(ID, "apply", e))?;
        if let Some(g) = &self.gate {
            check_version(http, target, &session, g)
                .await
                .map_err(|e| failed(ID, "version", e))?;
        }
        let stored = credential(input, Field::WebUiPassword).map_err(|e| failed(ID, "apply", e))?;

        // The password first: qBittorrent printed the temporary one to its
        // log, so no later failure may leave it valid.
        if listed(&PASSWORD) {
            let json = one("web_ui_password", stored.expose().to_owned().into());
            set_preferences(http, target, &session, json)
                .await
                .map_err(|e| failed(ID, PASSWORD.as_str(), e))?;
            // The new password has to sign in before this counts as done.
            // Under an auth bypass any password does, so the check can't be
            // made there (decisions file).
            if !bypassed {
                login(http, target, input, &stored).await.map_err(|e| {
                    let e = match e {
                        AdapterError::Unauthorized => AdapterError::Refused(PASSWORD_NOT_TAKEN),
                        e => e,
                    };
                    failed(ID, PASSWORD.as_str(), e)
                })?;
            }
            applied(&PASSWORD);
        }

        if listed(&CATEGORY_SET) {
            let have = read_categories(http, target, &session)
                .await
                .map_err(|e| failed(ID, CATEGORY_SET.as_str(), e))?;
            let changes = category_changes(input, &have);
            for (name, save_path, exists) in &changes {
                let endpoint = if *exists {
                    EDIT_CATEGORY
                } else {
                    CREATE_CATEGORY
                };
                let body = form(&[("category", name), ("savePath", save_path)]);
                call(http, target, &session, endpoint, Some(body))
                    .await
                    .map_err(|e| failed(ID, CATEGORY_SET.as_str(), e))?;
            }
            if !changes.is_empty() {
                applied(&CATEGORY_SET);
            }
        }

        // Read once before the batch: for the loosening check on your own
        // companion, and to know whether this run is the one turning the
        // localhost bypass off (and so the one to put it back).
        let before = if (w.yours && (listed(&INTERFACE) || listed(&INTERFACE_ADDRESS)))
            || listed(&LOCAL_AUTH)
        {
            Some(
                read_prefs(http, target, &session)
                    .await
                    .map_err(|e| failed(ID, "apply", e))?,
            )
        } else {
            None
        };
        if before.as_ref().is_some_and(|p| loosens(&w, p)) {
            return Err(failed(
                ID,
                INTERFACE.as_str(),
                AdapterError::Refused(WOULD_LOOSEN),
            ));
        }
        let mut prefs = serde_json::Map::new();
        let mut batch = Vec::new();
        for k in [SAVE_PATH, LOCAL_AUTH, INTERFACE, INTERFACE_ADDRESS] {
            if !listed(&k) {
                continue;
            }
            // `check_plan` made sure each listed key has its input.
            let (name, value): (&str, serde_json::Value) = if k == SAVE_PATH {
                let d = input
                    .downloads
                    .as_ref()
                    .ok_or(AdapterError::Refused(PLAN_MISMATCH))?;
                ("save_path", d.save_path().as_str().to_owned().into())
            } else if k == LOCAL_AUTH {
                ("bypass_local_auth", false.into())
            } else {
                let (iface, addr) = w
                    .route
                    .as_ref()
                    .ok_or(AdapterError::Refused(PLAN_MISMATCH))?;
                if k == INTERFACE {
                    ("current_network_interface", iface.clone().into())
                } else {
                    ("current_interface_address", addr.clone().into())
                }
            };
            prefs.insert(name.to_owned(), value);
            batch.push(k);
        }
        if !batch.is_empty() {
            set_preferences(http, target, &session, prefs)
                .await
                .map_err(|e| failed(ID, batch[0].as_str(), e))?;
            // Turning the localhost bypass off must not lock Lodestar out:
            // the stored password has to sign in afresh, or the bypass goes
            // back on through the session still open.
            // Any failure counts, not only a refused password: after
            // failed sign-ins qBittorrent bans the address (`403`), and a
            // transport error proves nothing either.
            let fresh = if listed(&LOCAL_AUTH) {
                login(http, target, input, &stored).await.err()
            } else {
                None
            };
            if let Some(e) = fresh {
                // The fresh sign-in's own failure, then the refusal.
                failed(ID, "webui.local_auth.sign_in", e);
                let turned_off_here = before.as_ref().is_some_and(|p| p.bypass_local_auth);
                let code = if !turned_off_here {
                    LOCAL_AUTH_LOCKOUT
                } else {
                    let restore = one("bypass_local_auth", true.into());
                    match set_preferences(http, target, &session, restore).await {
                        Ok(()) => LOCAL_AUTH_LOCKOUT,
                        Err(_) => LOCAL_AUTH_RESTORE_FAILED,
                    }
                };
                return Err(failed(ID, LOCAL_AUTH.as_str(), AdapterError::Refused(code)));
            }
            batch.iter().for_each(applied);
        }

        if listed(&WEBUI_BIND) {
            let addr = w.webui.ok_or(AdapterError::Refused(PLAN_MISMATCH))?;
            set_preferences(http, target, &session, one("web_ui_address", addr.into()))
                .await
                .map_err(|e| failed(ID, WEBUI_BIND.as_str(), e))?;
            applied(&WEBUI_BIND);
        }
        Ok(())
    }
}

impl Configure for QBittorrent {
    fn companion(&self) -> &CompanionId {
        &self.id
    }

    fn plan<'a>(
        &'a self,
        http: &'a dyn GuardedHttp,
        target: &'a Target,
        input: &'a ConfigureInput<'a>,
    ) -> BoxFuture<'a, Result<ConfigPlan, AdapterError>> {
        Box::pin(async move {
            self.plan_inner(http, target, input)
                .await
                .map_err(|e| failed(ID, "plan", e))
        })
    }

    fn apply_plan<'a>(
        &'a self,
        http: &'a dyn GuardedHttp,
        target: &'a Target,
        input: &'a ConfigureInput<'a>,
        plan: CheckedPlan<'a>,
    ) -> BoxFuture<'a, Result<(), AdapterError>> {
        Box::pin(async move {
            // Consent first, so even an empty plan is refused for a target
            // nobody confirmed.
            consent(input, target).map_err(|e| failed(ID, "apply", e))?;
            if plan.plan().changes().is_empty() {
                return Ok(());
            }
            self.apply_inner(http, target, input, plan.plan()).await
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    const PREFS: &str = r#"{"save_path":"/downloads/","web_ui_address":"*","current_network_interface":"","current_interface_address":"","bypass_local_auth":false,"scan_dirs":{},"x":[1,2,{"y":null}]}"#;

    #[test]
    fn preferences() {
        let p = parse_preferences(PREFS.as_bytes()).unwrap();
        assert_eq!(p.save_path, "/downloads/");
        assert_eq!(p.web_ui_address, "*");
        assert!(!p.bypass_local_auth);
        for bad in [
            "",
            "[]",
            "{}",
            &PREFS.replace("\"bypass_local_auth\":false", "\"bypass_local_auth\":0"),
            &PREFS.replace("\"save_path\":\"/downloads/\"", "\"save_path\":1"),
            &PREFS.replace("{\"save_path\"", "{\"save_path\":\"/a\",\"save_path\""),
            &PREFS.replace(
                "{\"save_path\"",
                "{\"save\\u005fpath\":\"/a\",\"save_path\"",
            ),
            &PREFS.replace(
                "\"bypass_local_auth\":false",
                "\"bypass_local_auth\":false,\"bypass_local_auth\":true",
            ),
            &format!("{PREFS} x"),
            &PREFS.replace("/downloads/", &"a".repeat(2000)),
        ] {
            assert_eq!(parse_preferences(bad.as_bytes()), None, "{bad}");
        }
        assert_eq!(parse_preferences(b"{\"save_path\":\"\xff\"}"), None);
    }

    #[test]
    fn categories() {
        let c = parse_categories(
            br#"{"lodestar":{"name":"lodestar","savePath":"/downloads/lodestar"},"tv":{"name":"tv"},"we\u0069rd":{}}"#,
        )
        .unwrap();
        assert_eq!(
            c,
            vec![
                ("lodestar".into(), "/downloads/lodestar".into()),
                ("tv".into(), String::new())
            ]
        );
        assert_eq!(parse_categories(b"{}").unwrap(), vec![]);
        for bad in [
            &br#"{"a":{},"a":{}}"#[..],
            br#"{"a":1}"#,
            br#"{"a":{"savePath":1}}"#,
            br#"{"a":{"savePath":"x","savePath":"y"}}"#,
            b"[]",
            b"",
        ] {
            assert_eq!(parse_categories(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn comparisons() {
        assert!(same_path("/downloads/", "/downloads"));
        assert!(same_path("/", "/"));
        assert!(!same_path("/a", "/b"));
        assert!(same_address("", ""));
        assert!(same_address("::1", "0:0:0:0:0:0:0:1"));
        assert!(!same_address("10.0.0.1", ""));
        assert!(!same_address("junk", "10.0.0.1"));
    }

    #[test]
    fn keys_are_well_formed_and_ordered() {
        assert_eq!(ORDER.len(), 7);
        assert_eq!(ORDER.last(), Some(&WEBUI_BIND));
    }

    proptest! {
        #[test]
        fn parsers_are_total(body in proptest::collection::vec(any::<u8>(), 0..256)) {
            let _ = parse_preferences(&body);
            let _ = parse_categories(&body);
        }

        #[test]
        fn preferences_round_trip(path in "/[a-z0-9 ._-]{0,40}", iface in "[a-z0-9]{0,8}", bypass: bool) {
            let body = serde_json::json!({
                "save_path": path, "web_ui_address": "127.0.0.1",
                "current_network_interface": iface, "current_interface_address": "",
                "bypass_local_auth": bypass, "other": [1, {"a": "b"}],
            }).to_string();
            let p = parse_preferences(body.as_bytes()).unwrap();
            prop_assert_eq!(p.save_path, path);
            prop_assert_eq!(p.interface, iface);
            prop_assert_eq!(p.bypass_local_auth, bypass);
        }

        #[test]
        fn categories_round_trip(names in proptest::collection::btree_set("[a-z]{1,8}", 0..8)) {
            let mut obj = serde_json::Map::new();
            for n in &names {
                obj.insert(n.clone(), serde_json::json!({"name": n, "savePath": format!("/d/{n}")}));
            }
            let body = serde_json::Value::Object(obj).to_string();
            let c = parse_categories(body.as_bytes()).unwrap();
            prop_assert_eq!(c.len(), names.len());
            for (n, p) in c {
                prop_assert_eq!(p, format!("/d/{n}"));
            }
        }
    }
}
