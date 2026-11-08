//! qBittorrent through its WebUI API v2 (checked against upstream
//! `release-4.6.3` and `release-5.0.3`, `src/webui/api/`; sources in
//! `docs/decisions/2026-11-08-CMP-8b.md`).
//!
//! - **Sign-in:** `POST /api/v2/auth/login`, a form of `username` and
//!   `password`. `200 Ok.` with the `SID` cookie is a session; `200 Fails.`
//!   is a wrong password; `403` is a ban after failed sign-ins, never
//!   retried. Requests carry no `Origin` or `Referer`, which qBittorrent's
//!   cross-site check passes. A custom `WebUI\SessionCookieName` isn't
//!   supported: the sign-in fails as a protocol error.
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
//! Applying writes categories first, then the preferences, then the new
//! password, which it checks by signing in with it, and the WebUI address
//! last, since qBittorrent restarts its WebUI on that change.

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
use crate::inputs::CompanionId;
use crate::log_events::CONFIG_APPLIED;

use super::{
    BIND_UNSUITED, CONTENT_TYPE, Origin, ROUTE_UNSUPPORTED, Route, WebUiBind, consent, cookie,
    credential, failed, form, header, key, plain,
};

/// Its catalogue id.
pub const ID: &str = "qbittorrent";

const LOGIN: &str = "/api/v2/auth/login";
const PREFERENCES: &str = "/api/v2/app/preferences";
const SET_PREFERENCES: &str = "/api/v2/app/setPreferences";
const CATEGORIES: &str = "/api/v2/torrents/categories";
const CREATE_CATEGORY: &str = "/api/v2/torrents/createCategory";
const EDIT_CATEGORY: &str = "/api/v2/torrents/editCategory";

const SID: CookieName = cookie("SID");
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
    CATEGORY_SET,
    SAVE_PATH,
    LOCAL_AUTH,
    INTERFACE,
    INTERFACE_ADDRESS,
    PASSWORD,
    WEBUI_BIND,
];

/// The refusal when qBittorrent has banned this address.
pub const BANNED: &str = "qBittorrent has banned this address after failed sign-ins";
/// The refusal when the new password doesn't sign in after it was set.
pub const PASSWORD_NOT_TAKEN: &str = "the new WebUI password didn't take";

/// A configuration request's bounds: preferences run to about 12 KiB, and a
/// write may take a moment on a slow NAS.
fn limits() -> Limits {
    Limits::new(Duration::from_secs(10), 256 * 1024).unwrap_or(Limits::PROBE)
}

/// The qBittorrent configuration adapter.
#[derive(Debug)]
pub struct QBittorrent {
    id: CompanionId,
}

impl QBittorrent {
    pub(crate) fn new(id: CompanionId) -> QBittorrent {
        QBittorrent { id }
    }
}

/// What the input asks for, worked out before anything is sent.
struct Wanted {
    webui: Option<&'static str>,
    /// `(interface, address)`, each empty for "any".
    route: Option<(String, String)>,
}

fn wanted(input: &ConfigureInput<'_>, target: &Target) -> Result<Wanted, AdapterError> {
    let webui = match input.webui {
        None => None,
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
    Ok(Wanted { webui, route })
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

/// A session: the `SID` cookie's value.
struct Session(Secret);

impl Session {
    fn header(&self) -> Result<HeaderValue, AdapterError> {
        HeaderValue::secret(Secret::new(format!("SID={}", self.0.expose())))
            .map_err(|_| AdapterError::Protocol("session cookie"))
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
        .session_cookie(SID);
    let r = http.send(req).await?;
    match (r.status(), r.body().trim_ascii()) {
        (200, b"Ok.") => r
            .session_cookie()
            .cloned()
            .map(Session)
            .ok_or(AdapterError::Protocol("no session cookie")),
        (200, b"Fails.") => Err(AdapterError::Unauthorized),
        (403, _) => Err(AdapterError::Refused(BANNED)),
        _ => Err(AdapterError::Protocol("sign-in answer")),
    }
}

/// Signs in with the stored credential, or, for a guided install, the
/// temporary password. `temporary_first` tries the temporary one first
/// (an apply whose plan replaces it), so a run costs at most one failed
/// sign-in. True when the temporary password signed in.
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
    match r.status() {
        200 => Ok(r),
        403 => Err(AdapterError::Unauthorized),
        400 | 409 => Err(AdapterError::Refused("qBittorrent refused the setting")),
        _ => Err(AdapterError::Protocol("HTTP status")),
    }
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
        && !same_path(&p.save_path, d.save_path().as_str())
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
        let w = wanted(input, target)?;
        let (session, temporary) = sign_in(http, target, input, false).await?;
        let prefs = read_prefs(http, target, &session).await?;
        let mut keys = differing(input, &w, &prefs);
        if input.downloads.is_some() {
            let have = read_categories(http, target, &session).await?;
            if !category_changes(input, &have).is_empty() {
                keys.push(CATEGORY_SET);
            }
        }
        if temporary {
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
        let w = wanted(input, target)?;
        let listed = |k: &SettingKey| plan.changes().contains(k);
        if plan.changes().iter().any(|k| !ORDER.contains(k)) {
            return Err(AdapterError::Refused("a setting this adapter doesn't know"));
        }
        let guided = matches!(input.origin, Origin::Guided { .. });
        if listed(&PASSWORD) && !guided {
            return Err(AdapterError::Refused(
                "only a guided install replaces the password",
            ));
        }
        let (session, _) = sign_in(http, target, input, listed(&PASSWORD)).await?;

        if listed(&CATEGORY_SET) {
            let have = read_categories(http, target, &session).await?;
            for (name, save_path, exists) in category_changes(input, &have) {
                let endpoint = if exists {
                    EDIT_CATEGORY
                } else {
                    CREATE_CATEGORY
                };
                let body = form(&[("category", &name), ("savePath", &save_path)]);
                call(http, target, &session, endpoint, Some(body))
                    .await
                    .map_err(|e| failed(ID, CATEGORY_SET.as_str(), e))?;
            }
            applied(&CATEGORY_SET);
        }

        let mut prefs = serde_json::Map::new();
        let mut batch = Vec::new();
        for k in [SAVE_PATH, LOCAL_AUTH, INTERFACE, INTERFACE_ADDRESS] {
            if !listed(&k) {
                continue;
            }
            let (name, value) = match (&k, &input.downloads, &w.route) {
                (k, Some(d), _) if *k == SAVE_PATH => {
                    ("save_path", d.save_path().as_str().to_owned().into())
                }
                (k, _, _) if *k == LOCAL_AUTH => ("bypass_local_auth", false.into()),
                (k, _, Some((iface, _))) if *k == INTERFACE => {
                    ("current_network_interface", iface.clone().into())
                }
                (k, _, Some((_, addr))) if *k == INTERFACE_ADDRESS => {
                    ("current_interface_address", addr.clone().into())
                }
                _ => return Err(AdapterError::Refused("the plan doesn't match the input")),
            };
            prefs.insert(name.to_owned(), value);
            batch.push(k);
        }
        if !batch.is_empty() {
            set_preferences(http, target, &session, prefs)
                .await
                .map_err(|e| failed(ID, batch[0].as_str(), e))?;
            batch.iter().for_each(applied);
        }

        if listed(&PASSWORD) {
            let next = credential(input, Field::WebUiPassword)?;
            let mut json = serde_json::Map::new();
            json.insert(
                "web_ui_password".to_owned(),
                next.expose().to_owned().into(),
            );
            set_preferences(http, target, &session, json)
                .await
                .map_err(|e| failed(ID, PASSWORD.as_str(), e))?;
            // The temporary password must not be left in place: the new one
            // has to sign in before this counts as done.
            login(http, target, input, &next).await.map_err(|e| {
                let e = match e {
                    AdapterError::Unauthorized => AdapterError::Refused(PASSWORD_NOT_TAKEN),
                    e => e,
                };
                failed(ID, PASSWORD.as_str(), e)
            })?;
            applied(&PASSWORD);
        }

        if listed(&WEBUI_BIND) {
            let addr = w
                .webui
                .ok_or(AdapterError::Refused("the plan doesn't match the input"))?;
            let mut json = serde_json::Map::new();
            json.insert("web_ui_address".to_owned(), addr.into());
            set_preferences(http, target, &session, json)
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
