//! Prowlarr through its API v1 (checked against upstream `develop`,
//! `src/Prowlarr.Api.V1/Config/HostConfigController.cs` and
//! `HostConfigResource.cs`; sources in `docs/decisions/2026-11-08-CMP-8b.md`).
//!
//! - **Key:** every request carries `X-Api-Key`, a sensitive header.
//!   `401` is a refused key.
//! - **Reading:** `GET /api/v1/config/host`, the host settings as one flat
//!   JSON object with `id` 1.
//! - **Writing:** `PUT /api/v1/config/host/1` with the whole object back:
//!   Prowlarr saves every property it's sent, so a partial object would
//!   reset the rest. The object goes back to the same Prowlarr it came
//!   from, with only the listed proxy fields changed; nothing of it is
//!   kept, logged or shown.
//!
//! **Wanted state** (Q8: Prowlarr follows a SOCKS5 proxy and nothing
//! else): for a direct route, `proxy.enabled` (`proxyEnabled`) off; for a
//! SOCKS5 route, `proxy.enabled` on, `proxy.type` (`proxyType`) `socks5`,
//! `proxy.hostname` (`proxyHostname`) and `proxy.port` (`proxyPort`) the
//! route's, and `proxy.bypass_filter` (`proxyBypassFilter`) empty, so no
//! indexer host is left on the direct route. An interface or bind-address route is refused
//! ([`ROUTE_UNSUPPORTED`](super::ROUTE_UNSUPPORTED)). With no route
//! nothing is planned and nothing sent. When the host or port changes, the
//! proxy's username and password are set empty, so the old proxy's
//! credentials never go to the new one.
//!
//! **Your own Prowlarr** (confirmed or guided): a proxy it already uses is
//! never turned off or moved
//! ([`WOULD_LOOSEN`](super::WOULD_LOOSEN)); turning one on where none is
//! set is allowed.

use std::time::Duration;

use lodestar_sources_feeds::json::{self, Walk};

use crate::adapter::{
    AdapterError, BoxFuture, CheckedPlan, ConfigPlan, Configure, ConfigureInput, GuardedHttp,
    HeaderName, HeaderValue, Limits, ProbeRequest, ProbeResponse, RequestPath, SettingKey, Target,
};
use crate::credentials::Field;
use crate::inputs::CompanionId;
use crate::log_events::CONFIG_APPLIED;

use super::{
    CONTENT_TYPE, Origin, PLAN_MISMATCH, ROUTE_UNSUPPORTED, Route, UNKNOWN_SETTING, WOULD_LOOSEN,
    consent, credential, failed, header, key, plain,
};

/// The refusal when Prowlarr's validation refused its host settings.
pub const HOST_SETTINGS_REFUSED: &str = "host_settings_refused";

/// A companion you confirmed or guided rather than one Lodestar installed.
fn yours(input: &ConfigureInput<'_>) -> bool {
    !matches!(input.origin, Origin::Installed(_))
}

/// Its catalogue id.
pub const ID: &str = "prowlarr";

const HOST: &str = "/api/v1/config/host";
const HOST_PUT: &str = "/api/v1/config/host/1";
const API_KEY: HeaderName = header("X-Api-Key");

pub const PROXY_ENABLED: SettingKey = key("proxy.enabled");
pub const PROXY_TYPE: SettingKey = key("proxy.type");
pub const PROXY_HOSTNAME: SettingKey = key("proxy.hostname");
pub const PROXY_PORT: SettingKey = key("proxy.port");
pub const PROXY_BYPASS: SettingKey = key("proxy.bypass_filter");

const ORDER: [SettingKey; 5] = [
    PROXY_ENABLED,
    PROXY_TYPE,
    PROXY_HOSTNAME,
    PROXY_PORT,
    PROXY_BYPASS,
];

fn limits() -> Limits {
    Limits::new(Duration::from_secs(10), 64 * 1024).unwrap_or(Limits::PROBE)
}

/// The Prowlarr configuration adapter.
#[derive(Debug)]
pub struct Prowlarr {
    id: CompanionId,
}

impl Prowlarr {
    pub(crate) fn new(id: CompanionId) -> Prowlarr {
        Prowlarr { id }
    }
}

/// The proxy fields of the host settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Proxy {
    pub enabled: bool,
    pub kind: String,
    pub hostname: String,
    pub port: u32,
    /// `proxyBypassFilter`, empty for `null`.
    pub bypass: String,
}

/// What the route asks for: `None` for a direct route (proxy off), the
/// host and port for SOCKS5.
fn wanted(route: &Route) -> Result<Option<(String, u32)>, AdapterError> {
    match route {
        Route::Direct => Ok(None),
        Route::Socks5(p) => Ok(Some((p.get().ip().to_string(), u32::from(p.get().port())))),
        Route::Interface(_) | Route::BindAddress(_) => {
            Err(AdapterError::Refused(ROUTE_UNSUPPORTED))
        }
    }
}

/// Caps for the host settings: one flat object of about 40 scalar keys.
/// Depth 1 refuses any nested object or list, so the round trip in
/// `apply` has nothing to reinterpret.
const HOST_LIMITS: json::Limits = json::Limits {
    max_depth: 1,
    max_values: 512,
    max_array: 64,
    max_text: 512,
};

/// The host settings' `id` (which must be 1) and proxy fields, each once
/// and of its type (`proxyHostname` may be `null`). `None` for anything
/// else: a key with an escape, a port outside `0..=65535`, a nested object
/// or list, or a number anywhere that isn't a whole number in `i64`. The
/// last two keep `apply`'s `serde_json` round trip exact: every other
/// field goes back as Prowlarr sent it.
pub(crate) fn parse_host(body: &[u8]) -> Option<Proxy> {
    let s = std::str::from_utf8(body).ok()?;
    let mut j = json::Reader::new(s, HOST_LIMITS);
    if j.peek() != Some(b'{') {
        return None;
    }
    let (mut id, mut enabled, mut kind, mut hostname, mut port, mut bypass) =
        (None, None, None, None, None, None);
    j.object(|j, k| {
        if k.contains('\\') {
            return Err(json::Stop::Malformed);
        }
        let bad = json::Stop::Malformed;
        match k {
            "id" | "proxyPort" => {
                let slot = if k == "id" { &mut id } else { &mut port };
                if slot.is_some() || !matches!(j.peek(), Some(b'0'..=b'9')) {
                    return Err(bad);
                }
                let n = j.num()?.parse::<u32>().map_err(|_| bad)?;
                *slot = Some(n);
            }
            "proxyEnabled" => {
                let v = match j.peek() {
                    Some(b't') => true,
                    Some(b'f') => false,
                    _ => return Err(bad),
                };
                if enabled.is_some() {
                    return Err(bad);
                }
                j.skip()?;
                enabled = Some(v);
            }
            "proxyType" | "proxyHostname" | "proxyBypassFilter" => {
                let nullable = k != "proxyType";
                let slot = match k {
                    "proxyType" => &mut kind,
                    "proxyHostname" => &mut hostname,
                    _ => &mut bypass,
                };
                if slot.is_some() {
                    return Err(bad);
                }
                *slot = Some(match j.peek() {
                    Some(b'"') => j.text()?.map_err(|_| bad)?,
                    Some(b'n') if nullable => {
                        j.skip()?;
                        String::new()
                    }
                    _ => return Err(bad),
                });
            }
            _ => {
                if matches!(j.peek(), Some(b'-' | b'0'..=b'9')) {
                    j.num()?.parse::<i64>().map_err(|_| bad)?;
                } else {
                    j.skip()?;
                }
            }
        }
        Ok(())
    })
    .ok()?;
    j.end().ok()?;
    if id? != 1 || port? > 65535 {
        return None;
    }
    Some(Proxy {
        enabled: enabled?,
        kind: kind?,
        hostname: hostname?,
        port: port?,
        bypass: bypass?,
    })
}

/// The keys whose current value differs from the route's.
fn differing(want: &Option<(String, u32)>, have: &Proxy) -> Vec<SettingKey> {
    let mut out = Vec::new();
    match want {
        None => {
            if have.enabled {
                out.push(PROXY_ENABLED);
            }
        }
        Some((host, port)) => {
            if !have.enabled {
                out.push(PROXY_ENABLED);
            }
            if !have.kind.eq_ignore_ascii_case("socks5") {
                out.push(PROXY_TYPE);
            }
            if have.hostname != *host {
                out.push(PROXY_HOSTNAME);
            }
            if have.port != *port {
                out.push(PROXY_PORT);
            }
            // A bypass filter would send the hosts it names on the direct
            // route, which Q8 forbids.
            if !have.bypass.is_empty() {
                out.push(PROXY_BYPASS);
            }
        }
    }
    out
}

/// Whether the route would turn off or move a proxy your own Prowlarr is
/// already using. Turning one on where none is set only narrows it.
fn loosens(yours: bool, want: &Option<(String, u32)>, have: &Proxy) -> bool {
    yours
        && have.enabled
        && match want {
            None => true,
            Some((host, port)) => {
                !have.kind.eq_ignore_ascii_case("socks5")
                    || have.hostname != *host
                    || have.port != *port
            }
        }
}

/// Every listed key has what the route supplies, checked before anything
/// is sent.
fn check_plan(plan: &ConfigPlan, want: &Option<(String, u32)>) -> Result<(), AdapterError> {
    for k in plan.changes() {
        if !ORDER.contains(k) {
            return Err(AdapterError::Refused(UNKNOWN_SETTING));
        }
        if *k != PROXY_ENABLED && want.is_none() {
            return Err(AdapterError::Refused(PLAN_MISMATCH));
        }
    }
    Ok(())
}

fn path(p: &'static str) -> Result<RequestPath, AdapterError> {
    RequestPath::parse(p).map_err(|_| AdapterError::Protocol("request path"))
}

fn key_header(input: &ConfigureInput<'_>) -> Result<HeaderValue, AdapterError> {
    HeaderValue::secret(credential(input, Field::ApiKey)?)
        .map_err(|_| AdapterError::Protocol("API key"))
}

/// The host settings: the whole answer and its parsed proxy fields.
async fn read_host(
    http: &dyn GuardedHttp,
    target: &Target,
    input: &ConfigureInput<'_>,
) -> Result<(ProbeResponse, Proxy), AdapterError> {
    let req = ProbeRequest::new(target, path(HOST)?, limits()).header(API_KEY, key_header(input)?);
    let r = http.send(req).await?;
    match r.status() {
        200 => {}
        401 => return Err(AdapterError::Unauthorized),
        _ => return Err(AdapterError::Protocol("HTTP status")),
    }
    let proxy = parse_host(r.body()).ok_or(AdapterError::Protocol("host settings"))?;
    Ok((r, proxy))
}

impl Prowlarr {
    async fn plan_inner(
        &self,
        http: &dyn GuardedHttp,
        target: &Target,
        input: &ConfigureInput<'_>,
    ) -> Result<ConfigPlan, AdapterError> {
        consent(input, target)?;
        let Some(route) = &input.route else {
            return Ok(ConfigPlan::new(self.id.clone(), Vec::new()));
        };
        let want = wanted(route)?;
        let (_, have) = read_host(http, target, input).await?;
        if loosens(yours(input), &want, &have) {
            return Err(AdapterError::Refused(WOULD_LOOSEN));
        }
        Ok(ConfigPlan::new(self.id.clone(), differing(&want, &have)))
    }

    async fn apply_inner(
        &self,
        http: &dyn GuardedHttp,
        target: &Target,
        input: &ConfigureInput<'_>,
        plan: &ConfigPlan,
    ) -> Result<(), AdapterError> {
        consent(input, target)?;
        let route = input
            .route
            .as_ref()
            .ok_or(AdapterError::Refused(PLAN_MISMATCH))?;
        let want = wanted(route)?;
        check_plan(plan, &want)?;
        let (answer, have) = read_host(http, target, input).await?;
        if loosens(yours(input), &want, &have) {
            return Err(failed(ID, "proxy", AdapterError::Refused(WOULD_LOOSEN)));
        }
        // Already checked by `parse_host` with the bounded reader: one flat
        // object of strings, booleans, nulls and whole numbers, so every
        // field `serde_json` reads it into goes back unchanged. Prowlarr's
        // own `apiKey`, password hash, bind address and port are among
        // them; `SaveHostConfig` skips `ApiKey`, and leaves the password
        // alone when the hash it gets equals the stored one.
        let mut host: serde_json::Map<String, serde_json::Value> =
            serde_json::from_slice(answer.body())
                .map_err(|_| AdapterError::Protocol("host settings"))?;
        for k in plan.changes() {
            let (name, value): (&str, serde_json::Value) = match (k, &want) {
                (k, w) if *k == PROXY_ENABLED => ("proxyEnabled", w.is_some().into()),
                (k, Some(_)) if *k == PROXY_TYPE => ("proxyType", "socks5".into()),
                (k, Some((h, _))) if *k == PROXY_HOSTNAME => ("proxyHostname", h.clone().into()),
                (k, Some((_, p))) if *k == PROXY_PORT => ("proxyPort", (*p).into()),
                (k, Some(_)) if *k == PROXY_BYPASS => ("proxyBypassFilter", "".into()),
                _ => return Err(AdapterError::Refused(PLAN_MISMATCH)),
            };
            host.insert(name.to_owned(), value);
        }
        // A new proxy host or port never gets the old proxy's credentials.
        // Empty strings, not null: `ConfigService.SaveConfigDictionary`
        // skips nulls, which would keep the old values; both default to
        // `string.Empty` upstream.
        if plan.changes().contains(&PROXY_HOSTNAME) || plan.changes().contains(&PROXY_PORT) {
            host.insert("proxyUsername".to_owned(), "".into());
            host.insert("proxyPassword".to_owned(), "".into());
        }
        let body = serde_json::Value::Object(host).to_string().into_bytes();
        let req = ProbeRequest::new(target, path(HOST_PUT)?, limits())
            .put(body)
            .header(API_KEY, key_header(input)?)
            .header(CONTENT_TYPE, plain("application/json")?);
        let step = plan.changes().first().map_or("proxy", SettingKey::as_str);
        let r = http.send(req).await.map_err(|e| failed(ID, step, e))?;
        match r.status() {
            200 | 202 => {}
            401 => return Err(failed(ID, step, AdapterError::Unauthorized)),
            400 => {
                return Err(failed(
                    ID,
                    step,
                    AdapterError::Refused(HOST_SETTINGS_REFUSED),
                ));
            }
            _ => return Err(failed(ID, step, AdapterError::Protocol("HTTP status"))),
        }
        for k in plan.changes() {
            lodestar_log::log_event!(CONFIG_APPLIED, companion = ID, setting = k.as_str());
        }
        Ok(())
    }
}

impl Configure for Prowlarr {
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

    const HOST_JSON: &str = r#"{"bindAddress":"*","port":9696,"apiKey":"k","proxyEnabled":false,"proxyType":"http","proxyHostname":null,"proxyPort":8080,"proxyUsername":null,"proxyBypassFilter":null,"id":1}"#;

    #[test]
    fn host_settings() {
        let p = parse_host(HOST_JSON.as_bytes()).unwrap();
        assert_eq!(
            p,
            Proxy {
                enabled: false,
                kind: "http".into(),
                hostname: String::new(),
                port: 8080,
                bypass: String::new(),
            }
        );
        for bad in [
            "",
            "{}",
            "[]",
            &HOST_JSON.replace("\"id\":1", "\"id\":2"),
            &HOST_JSON.replace("\"proxyPort\":8080", "\"proxyPort\":70000"),
            &HOST_JSON.replace("\"proxyPort\":8080", "\"proxyPort\":-1"),
            &HOST_JSON.replace("\"proxyPort\":8080", "\"proxyPort\":1.5"),
            &HOST_JSON.replace("\"proxyEnabled\":false", "\"proxyEnabled\":\"no\""),
            &HOST_JSON.replace("\"proxyType\":\"http\"", "\"proxyType\":null"),
            &HOST_JSON.replace("{", "{\"proxyEnabled\":true,"),
            &HOST_JSON.replace("{", "{\"proxy\\u0045nabled\":true,"),
            &format!("{HOST_JSON},"),
            &HOST_JSON.replace("\"port\":9696", "\"port\":9696.5"),
            &HOST_JSON.replace("\"port\":9696", "\"port\":99999999999999999999"),
            &HOST_JSON.replace("\"bindAddress\":\"*\"", "\"bindAddress\":[\"*\"]"),
            &HOST_JSON.replace("\"bindAddress\":\"*\"", "\"bindAddress\":{}"),
        ] {
            assert_eq!(parse_host(bad.as_bytes()), None, "{bad}");
        }
    }

    #[test]
    fn differences() {
        let off = Proxy {
            enabled: false,
            kind: "http".into(),
            hostname: String::new(),
            port: 8080,
            bypass: String::new(),
        };
        assert!(differing(&None, &off).is_empty());
        let want = Some(("172.17.0.2".to_owned(), 1080));
        assert_eq!(differing(&want, &off), ORDER[..4].to_vec());
        let on = Proxy {
            enabled: true,
            kind: "Socks5".into(),
            hostname: "172.17.0.2".into(),
            port: 1080,
            bypass: String::new(),
        };
        assert!(differing(&want, &on).is_empty());
        assert_eq!(differing(&None, &on), vec![PROXY_ENABLED]);
        let leaky = Proxy {
            bypass: "*.org".into(),
            ..on
        };
        assert_eq!(differing(&want, &leaky), vec![PROXY_BYPASS]);
        // Off, a filter doesn't matter.
        assert!(
            differing(
                &None,
                &Proxy {
                    enabled: false,
                    ..leaky
                }
            )
            .is_empty()
        );
    }

    proptest! {
        #[test]
        fn parser_is_total(body in proptest::collection::vec(any::<u8>(), 0..256)) {
            let _ = parse_host(&body);
        }

        #[test]
        fn parser_round_trips(enabled: bool, port in 0u32..=65535, host in "[a-z0-9.]{0,20}") {
            let body = serde_json::json!({
                "id": 1, "proxyEnabled": enabled, "proxyType": "socks5",
                "proxyHostname": host, "proxyPort": port, "apiKey": "x", "port": 9696,
                "proxyBypassFilter": null,
            }).to_string();
            let p = parse_host(body.as_bytes()).unwrap();
            prop_assert_eq!(p, Proxy { enabled, kind: "socks5".into(), hostname: host, port, bypass: String::new() });
        }
    }
}
