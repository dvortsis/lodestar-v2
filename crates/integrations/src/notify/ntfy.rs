//! The ntfy sender.
//!
//! `POST`s the message text to the topic URL (`https://ntfy.sh/<topic>`, or
//! a self-hosted server: set `allow_private` for one on the LAN):
//!
//! - the body is plain text: the notification's body, then one line per
//!   shareable item, then how many private-source items were withheld
//!   (§18.7). It is cut at 4,096 bytes (ntfy's limit; a longer message
//!   becomes an attachment) on a character boundary, ending in `…`;
//! - `Title` (RFC 2047 encoded when it isn't ASCII), `Tags` and `Priority`
//!   from the channel's settings, and `Click` when the notification links
//!   back into Lodestar;
//! - `Authorization: Bearer <token>` or basic credentials, when the topic is
//!   protected, marked sensitive. Credentials are refused over plain `http`
//!   unless the channel is allowed to reach the private network.
//!
//! ntfy has no idempotency key, so a retried delivery can show twice; none
//! is invented. The topic URL is a secret (anyone who has it can read the
//! topic): it is a [`Secret`], stored sealed, and only its scheme and host
//! are ever shown. A 2xx answer counts only when it is ntfy's JSON message
//! (`{"id": …}`), so a captive portal or a wrong URL isn't reported as
//! delivered.

use std::fmt;
use std::time::Duration;

use reqwest::header::{CONTENT_TYPE, HeaderName, HeaderValue};
use reqwest::{Method, Url};

use super::{AttemptError, Channel, DeliveryError, Notification, Notifier, check_status};
use crate::BoxFuture;
use crate::net::{GuardedClient, NetPolicy, parse_url, read_limited, redact};
use crate::secret::Secret;

/// The timeout for one attempt.
pub const NTFY_TIMEOUT: Duration = Duration::from_secs(10);

/// ntfy's message size limit in bytes (its default).
pub const MAX_MESSAGE_BYTES: usize = 4096;

/// The longest title sent, in characters.
const MAX_TITLE_CHARS: usize = 120;

/// Items listed in the message before "and N more".
const MAX_LISTED_ITEMS: usize = 20;

/// How much of an answer is read. ntfy's are a few hundred bytes.
const MAX_ANSWER_BYTES: usize = 64 * 1024;

/// How a topic is protected.
#[derive(Debug, Clone, Default)]
pub enum NtfyAuth {
    /// A public topic.
    #[default]
    None,
    /// An access token (`tk_…`).
    Bearer(Secret),
    Basic {
        username: Secret,
        password: Secret,
    },
}

/// An ntfy channel's settings.
#[derive(Debug, Clone)]
pub struct NtfyConfig {
    /// The topic URL, such as `https://ntfy.sh/my-lodestar-alerts`. Secret:
    /// it is the only thing keeping a public topic private.
    pub topic_url: Secret,
    pub auth: NtfyAuth,
    /// Let this channel reach private-network addresses (a self-hosted
    /// server on the LAN). Off by default; loopback, link-local and
    /// metadata addresses stay refused either way.
    pub allow_private: bool,
    /// 1 (min) to 5 (max); ntfy's default is 3 when unset.
    pub priority: Option<u8>,
    /// ntfy tags or emoji short codes: lowercase letters, digits, `_`, `-`.
    pub tags: Vec<String>,
}

impl NtfyConfig {
    pub fn new(topic_url: Secret) -> NtfyConfig {
        NtfyConfig {
            topic_url,
            auth: NtfyAuth::None,
            allow_private: false,
            priority: None,
            tags: Vec::new(),
        }
    }
}

pub struct Ntfy {
    url: Url,
    http: GuardedClient,
    auth: NtfyAuth,
    priority: Option<u8>,
    tags: Option<HeaderValue>,
}

impl fmt::Debug for Ntfy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Ntfy")
            .field("url", &redact(&self.url))
            .finish_non_exhaustive()
    }
}

impl Ntfy {
    /// Checks the settings and the URL against the notification policy and
    /// builds the sender. A refused address fails here, when the channel is
    /// saved.
    pub fn new(config: &NtfyConfig) -> Result<Ntfy, DeliveryError> {
        let http = GuardedClient::new(NetPolicy::notification(config.allow_private), NTFY_TIMEOUT)
            .map_err(|e| AttemptError::from(e).error)?;
        Ntfy::with_client(config, http)
    }

    /// The same, over a given client (tests inject a resolver this way).
    pub fn with_client(config: &NtfyConfig, http: GuardedClient) -> Result<Ntfy, DeliveryError> {
        let url = parse_url(config.topic_url.expose(), http.policy())
            .map_err(|e| AttemptError::from(e).error)?;
        if url.path().trim_matches('/').is_empty() {
            return Err(DeliveryError::InvalidEndpoint("the URL has no topic"));
        }
        if url.query().is_some() || url.fragment().is_some() {
            return Err(DeliveryError::InvalidEndpoint(
                "the URL must not have a query or fragment",
            ));
        }
        if !url.username().is_empty() || url.password().is_some() {
            return Err(DeliveryError::InvalidEndpoint(
                "put credentials in the login settings, not the URL",
            ));
        }
        match &config.auth {
            NtfyAuth::None => {}
            NtfyAuth::Bearer(token) => check_credential(token, "invalid access token")?,
            NtfyAuth::Basic { username, password } => {
                check_credential(username, "invalid username")?;
                check_credential(password, "invalid password")?;
                if username.expose().contains(':') {
                    return Err(DeliveryError::InvalidEndpoint("invalid username"));
                }
            }
        }
        if !matches!(config.auth, NtfyAuth::None) && url.scheme() == "http" && !config.allow_private
        {
            return Err(DeliveryError::InvalidEndpoint(
                "credentials need https unless the server is on the private network",
            ));
        }
        if config.priority.is_some_and(|p| !(1..=5).contains(&p)) {
            return Err(DeliveryError::InvalidEndpoint("priority must be 1 to 5"));
        }
        let tags = if config.tags.is_empty() {
            None
        } else {
            for tag in &config.tags {
                let ok = !tag.is_empty()
                    && tag.len() <= 40
                    && tag.bytes().all(|b| {
                        b.is_ascii_lowercase() || b.is_ascii_digit() || b"_-".contains(&b)
                    });
                if !ok {
                    return Err(DeliveryError::InvalidEndpoint("invalid tag"));
                }
            }
            HeaderValue::from_str(&config.tags.join(",")).ok()
        };
        Ok(Ntfy {
            url,
            http,
            auth: config.auth.clone(),
            priority: config.priority,
            tags,
        })
    }

    /// The message text for `n`: private items left out, their count said.
    pub fn message(n: &Notification) -> String {
        let mut text = clean_text(&n.body);
        let mut listed = 0;
        let mut more = 0;
        for item in n.shareable_items() {
            if listed == MAX_LISTED_ITEMS {
                more += 1;
                continue;
            }
            if listed == 0 && !text.is_empty() {
                text.push_str("\n\n");
            } else if listed > 0 {
                text.push('\n');
            }
            text.push_str("• ");
            text.push_str(&clean_line(&item.name));
            if let Some(size) = item.size_bytes {
                text.push_str(" (");
                text.push_str(&format_bytes(size));
                text.push(')');
            }
            listed += 1;
        }
        let mut notes = Vec::new();
        if more > 0 {
            notes.push(format!("…and {more} more"));
        }
        match n.withheld() {
            0 => {}
            1 => notes.push("1 result from a private source isn't included.".to_owned()),
            w => notes.push(format!("{w} results from private sources aren't included.")),
        }
        for note in notes {
            if !text.is_empty() {
                text.push_str("\n\n");
            }
            text.push_str(&note);
        }
        truncate_bytes(&text, MAX_MESSAGE_BYTES)
    }

    fn title_header(n: &Notification) -> Option<HeaderValue> {
        let title = clean_line(&n.title);
        let title = truncate_chars(&title, MAX_TITLE_CHARS);
        if title.is_empty() {
            return None;
        }
        HeaderValue::from_str(&encode_header_text(&title)).ok()
    }
}

fn check_credential(secret: &Secret, why: &'static str) -> Result<(), DeliveryError> {
    let v = secret.expose();
    // Visible ASCII, so it is a valid header value and can't smuggle a
    // second header.
    if v.is_empty() || !v.bytes().all(|b| (0x21..=0x7e).contains(&b)) {
        return Err(DeliveryError::InvalidEndpoint(why));
    }
    Ok(())
}

impl Notifier for Ntfy {
    fn channel(&self) -> Channel {
        Channel::Ntfy
    }

    fn attempt<'a>(&'a self, n: &'a Notification) -> BoxFuture<'a, Result<(), AttemptError>> {
        Box::pin(async move {
            let mut req = self
                .http
                .request(Method::POST, self.url.clone())
                .header(CONTENT_TYPE, "text/plain; charset=utf-8")
                .body(Ntfy::message(n));
            if let Some(title) = Ntfy::title_header(n) {
                req = req.header("Title", title);
            }
            if let Some(tags) = &self.tags {
                req = req.header("Tags", tags.clone());
            }
            if let Some(priority) = self.priority {
                req = req.header("Priority", priority.to_string());
            }
            if let Some(link) = n.link.as_deref().and_then(web_link) {
                req = req.header(HeaderName::from_static("click"), link);
            }
            // `bearer_auth` and `basic_auth` mark the header sensitive.
            req = match &self.auth {
                NtfyAuth::None => req,
                NtfyAuth::Bearer(token) => req.bearer_auth(token.expose()),
                NtfyAuth::Basic { username, password } => {
                    req.basic_auth(username.expose(), Some(password.expose()))
                }
            };
            let req = req.build().map_err(|_| {
                AttemptError::fatal(DeliveryError::Failed("couldn't build the request"))
            })?;
            let resp = self.http.send_following(req).await?;
            let status = resp.status();
            let checked = check_status(status, resp.headers());
            // Always drain a little, so the connection can be reused; only a
            // success looks at the content, and never logs it.
            let body = read_limited(resp, MAX_ANSWER_BYTES).await;
            tracing::debug!(status = status.as_u16(), url = %redact(&self.url), "ntfy attempt");
            checked?;
            let looks_like_ntfy = serde_json::from_slice::<serde_json::Value>(&body?)
                .ok()
                .is_some_and(|v| v.get("id").is_some_and(|id| id.is_string()));
            if looks_like_ntfy {
                Ok(())
            } else {
                Err(AttemptError::fatal(DeliveryError::Failed(
                    "the endpoint didn't answer like ntfy",
                )))
            }
        })
    }
}

/// `link` as an absolute `http(s)` URL, or `None`: a `Click` header opens
/// whatever it holds, so `javascript:` and the like are never passed on.
pub(crate) fn web_link(link: &str) -> Option<String> {
    let url = Url::parse(link.trim()).ok()?;
    let web = matches!(url.scheme(), "http" | "https") && url.host_str().is_some();
    web.then(|| url.to_string())
}

/// `s` on one line: control characters become spaces, and the bidirectional
/// overrides that could disguise a name are dropped.
pub(crate) fn clean_line(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if is_bidi_control(c) {
            continue;
        }
        out.push(if c.is_control() { ' ' } else { c });
    }
    out.trim().to_owned()
}

/// `s` with newlines kept and every other control character dropped.
pub(crate) fn clean_text(s: &str) -> String {
    let s = s.replace("\r\n", "\n");
    let cleaned: String = s
        .chars()
        .filter(|&c| c == '\n' || !(c.is_control() || is_bidi_control(c)))
        .collect();
    cleaned.trim().to_owned()
}

fn is_bidi_control(c: char) -> bool {
    matches!(c, '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}')
}

/// At most `max` bytes, cut at a character boundary, ending in `…` when
/// anything was cut.
pub(crate) fn truncate_bytes(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_owned();
    }
    let mut end = max.saturating_sub('…'.len_utf8());
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &s[..end])
}

/// At most `max` characters, ending in `…` when anything was cut.
pub(crate) fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_owned();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// Sizes the way `docs/metrics.md` writes them: IEC units, two digits under
/// 10, one under 100, none from 100, joined to the unit by U+00A0.
pub(crate) fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 6] = ["B", "KiB", "MiB", "GiB", "TiB", "PiB"];
    if bytes < 1024 {
        return format!("{bytes}\u{a0}B");
    }
    let mut unit = 1;
    while unit < 5 && bytes >= 1u64 << (10 * (unit + 1)) {
        unit += 1;
    }
    loop {
        let div = 1u128 << (10 * unit);
        let b = u128::from(bytes);
        let digits: u32 = if b < 10 * div {
            2
        } else if b < 100 * div {
            1
        } else {
            0
        };
        let scale = 10u128.pow(digits);
        // Round half up, in integers, so ties don't depend on float
        // representation.
        let scaled = (b * scale * 2 + div) / (div * 2);
        if scaled >= 1024 * scale && unit < 5 {
            unit += 1;
            continue;
        }
        let whole = scaled / scale;
        return if digits == 0 {
            format!("{whole}\u{a0}{}", UNITS[unit])
        } else {
            let frac = scaled % scale;
            format!(
                "{whole}.{frac:0width$}\u{a0}{}",
                UNITS[unit],
                width = digits as usize
            )
        };
    }
}

/// A header value for `s`: as it is when ASCII, else RFC 2047 encoded-words
/// (`=?UTF-8?B?…?=`), which ntfy decodes.
fn encode_header_text(s: &str) -> String {
    if s.bytes().all(|b| (0x20..=0x7e).contains(&b)) {
        return s.to_owned();
    }
    // Each word stays within 75 characters: 45 bytes of text encode to 60.
    let mut words = Vec::new();
    let mut chunk = String::new();
    for c in s.chars() {
        if chunk.len() + c.len_utf8() > 45 {
            words.push(std::mem::take(&mut chunk));
        }
        chunk.push(c);
    }
    if !chunk.is_empty() {
        words.push(chunk);
    }
    words
        .iter()
        .map(|w| format!("=?UTF-8?B?{}?=", base64(w.as_bytes())))
        .collect::<Vec<_>>()
        .join(" ")
}

fn base64(data: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let n = chunk
            .iter()
            .enumerate()
            .fold(0u32, |acc, (i, &b)| acc | u32::from(b) << (16 - 8 * i));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(char::from(ALPHABET[(n >> (18 - 6 * i) & 63) as usize]));
            } else {
                out.push('=');
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::tests::ScriptedResolver;
    use crate::notify::{NotificationItem, NotificationKind, RetryPolicy, deliver};
    use crate::{AddrClass, NetError};
    use serde::Deserialize;
    use std::sync::{Arc, Mutex};

    #[derive(Deserialize)]
    struct Fixture {
        request: FixtureRequest,
        response: FixtureResponse,
    }

    #[derive(Deserialize)]
    struct FixtureRequest {
        method: String,
    }

    #[derive(Deserialize)]
    struct FixtureResponse {
        status: u16,
        headers: Vec<(String, String)>,
        body: String,
    }

    fn fixture(name: &str) -> Fixture {
        let path = format!(
            "{}/tests/fixtures/ntfy/{name}.json",
            env!("CARGO_MANIFEST_DIR")
        );
        let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"));
        serde_json::from_str(&text).unwrap_or_else(|e| panic!("{path}: {e}"))
    }

    #[derive(Debug)]
    struct Received {
        method: String,
        path: String,
        headers: Vec<(String, String)>,
        body: String,
    }

    impl Received {
        fn header(&self, name: &str) -> Option<&str> {
            self.headers
                .iter()
                .find(|(k, _)| k == name)
                .map(|(_, v)| v.as_str())
        }
    }

    /// Plays the named fixtures in order (then 200 `publish_ok` forever) and
    /// records what arrived.
    async fn replay(names: &[&str]) -> (u16, Arc<Mutex<Vec<Received>>>) {
        use axum::body::{Body, to_bytes};
        use axum::extract::Request;
        let seen = Arc::new(Mutex::new(Vec::new()));
        let log = Arc::clone(&seen);
        let mut script: Vec<Fixture> = names.iter().map(|n| fixture(n)).collect();
        script.reverse();
        let script = Arc::new(Mutex::new(script));
        let app = axum::Router::new().fallback(move |req: Request| {
            let log = Arc::clone(&log);
            let script = Arc::clone(&script);
            async move {
                let method = req.method().to_string();
                let path = req.uri().path().to_owned();
                let headers = req
                    .headers()
                    .iter()
                    .map(|(k, v)| (k.to_string(), v.to_str().unwrap_or("?").to_owned()))
                    .collect();
                let body = to_bytes(req.into_body(), 1 << 20).await.unwrap();
                let scripted = script.lock().unwrap().pop();
                let next = match scripted {
                    Some(next) => {
                        // The topic is the user's, so only the method is fixed;
                        // tests assert the path they configured.
                        assert_eq!(next.request.method, method);
                        next
                    }
                    None => fixture("publish_ok"),
                };
                log.lock().unwrap().push(Received {
                    method,
                    path,
                    headers,
                    body: String::from_utf8_lossy(&body).into_owned(),
                });
                let mut resp = axum::response::Response::builder().status(next.response.status);
                for (k, v) in &next.response.headers {
                    resp = resp.header(k, v);
                }
                resp.body(Body::from(next.response.body)).unwrap()
            }
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (port, seen)
    }

    fn client() -> GuardedClient {
        let r = ScriptedResolver::new(&[
            ("ntfy.test", &[&["127.0.0.1"]]),
            ("metadata.test", &[&["169.254.169.254"]]),
        ]);
        GuardedClient::with_resolver(NetPolicy::loopback_for_tests(), NTFY_TIMEOUT, r).unwrap()
    }

    fn config(port: u16) -> NtfyConfig {
        NtfyConfig::new(Secret::new(format!(
            "http://ntfy.test:{port}/lodestar-alerts"
        )))
    }

    fn ntfy(config: &NtfyConfig) -> Ntfy {
        Ntfy::with_client(config, client()).unwrap()
    }

    fn alert() -> Notification {
        let mut n = Notification::new(
            NotificationKind::Alert,
            "4K nature docs: 2 new",
            "Two new results.",
        );
        n.link = Some("http://lodestar.lan/alerts/7".into());
        n.items = vec![
            NotificationItem {
                name: "Planet.Earth.III.2160p".into(),
                info_hash: "a".repeat(40),
                size_bytes: Some(4_692_251_770),
                private_source: false,
            },
            NotificationItem {
                name: "Private.Tracker.Exclusive".into(),
                info_hash: "b".repeat(40),
                size_bytes: Some(7),
                private_source: true,
            },
        ];
        n
    }

    const FAST: RetryPolicy = RetryPolicy {
        max_attempts: 4,
        base_delay: Duration::from_millis(2),
        max_delay: Duration::from_millis(20),
    };

    #[tokio::test]
    async fn publishes_the_message_with_its_headers() {
        let (port, seen) = replay(&["publish_ok"]).await;
        let mut cfg = config(port);
        cfg.priority = Some(4);
        cfg.tags = vec!["mag".into(), "new-results".into()];
        let out = deliver(&ntfy(&cfg), &alert(), &FAST).await;
        assert_eq!(out.result, Ok(()));
        assert_eq!(out.attempts, 1);
        let seen = seen.lock().unwrap();
        let r = &seen[0];
        assert_eq!(
            (r.method.as_str(), r.path.as_str()),
            ("POST", "/lodestar-alerts")
        );
        assert_eq!(r.header("title"), Some("4K nature docs: 2 new"));
        assert_eq!(r.header("priority"), Some("4"));
        assert_eq!(r.header("tags"), Some("mag,new-results"));
        assert_eq!(r.header("click"), Some("http://lodestar.lan/alerts/7"));
        assert_eq!(r.header("content-type"), Some("text/plain; charset=utf-8"));
        assert_eq!(r.header("authorization"), None);
        assert_eq!(
            r.header("x-lodestar-delivery"),
            None,
            "ntfy has no idempotency key"
        );
        assert_eq!(r.header("user-agent"), Some(crate::USER_AGENT));
        assert_eq!(
            r.body,
            "Two new results.\n\n• Planet.Earth.III.2160p (4.37\u{a0}GiB)\n\n\
             1 result from a private source isn't included."
        );
        assert!(!r.body.contains("Private.Tracker"));
    }

    #[tokio::test]
    async fn sends_a_token_or_basic_credentials() {
        let (port, seen) = replay(&[]).await;
        let mut cfg = config(port);
        cfg.allow_private = true;
        cfg.auth = NtfyAuth::Bearer(Secret::new("tk_abc123"));
        deliver(&ntfy(&cfg), &alert(), &FAST).await;
        cfg.auth = NtfyAuth::Basic {
            username: Secret::new("alice"),
            password: Secret::new("pa55"),
        };
        deliver(&ntfy(&cfg), &alert(), &FAST).await;
        let seen = seen.lock().unwrap();
        assert_eq!(seen[0].header("authorization"), Some("Bearer tk_abc123"));
        // base64("alice:pa55")
        assert_eq!(
            seen[1].header("authorization"),
            Some("Basic YWxpY2U6cGE1NQ==")
        );
    }

    #[test]
    fn credentials_are_refused_over_plain_http_off_the_lan() {
        let mut cfg = config(80);
        cfg.auth = NtfyAuth::Bearer(Secret::new("tk_abc123"));
        assert!(matches!(
            Ntfy::with_client(&cfg, client()),
            Err(DeliveryError::InvalidEndpoint(_))
        ));
        cfg.topic_url = Secret::new("https://ntfy.test/lodestar-alerts");
        assert!(Ntfy::with_client(&cfg, client()).is_ok());
    }

    #[test]
    fn settings_are_checked_when_saved() {
        let bad = |edit: &dyn Fn(&mut NtfyConfig)| {
            let mut cfg = NtfyConfig::new(Secret::new("https://ntfy.test/topic"));
            edit(&mut cfg);
            matches!(
                Ntfy::with_client(&cfg, client()),
                Err(DeliveryError::InvalidEndpoint(_))
            )
        };
        assert!(bad(&|c| c.topic_url = Secret::new("https://ntfy.test/")));
        assert!(bad(&|c| c.topic_url = Secret::new("https://ntfy.test")));
        assert!(bad(
            &|c| c.topic_url = Secret::new("https://ntfy.test/t?auth=x")
        ));
        assert!(bad(
            &|c| c.topic_url = Secret::new("https://u:p@ntfy.test/t")
        ));
        assert!(bad(&|c| c.topic_url = Secret::new("ftp://ntfy.test/t")));
        assert!(bad(&|c| c.priority = Some(0)));
        assert!(bad(&|c| c.priority = Some(6)));
        assert!(bad(&|c| c.tags = vec!["Has Space".into()]));
        assert!(bad(&|c| c.tags = vec![String::new()]));
        assert!(bad(&|c| c.auth = NtfyAuth::Bearer(Secret::new(""))));
        assert!(bad(&|c| c.auth = NtfyAuth::Bearer(Secret::new("a\r\nX: y"))));
        assert!(bad(&|c| {
            c.auth = NtfyAuth::Basic {
                username: Secret::new("a:b"),
                password: Secret::new("p"),
            }
        }));
        assert!(!bad(&|c| c.priority = Some(5)));

        for (url, private, want) in [
            ("http://127.0.0.1:9/t", true, AddrClass::Loopback),
            ("http://169.254.169.254/t", true, AddrClass::Metadata),
            ("http://192.168.1.5/t", false, AddrClass::Private),
        ] {
            let mut cfg = NtfyConfig::new(Secret::new(url));
            cfg.allow_private = private;
            assert_eq!(
                Ntfy::new(&cfg).unwrap_err(),
                DeliveryError::Blocked(want),
                "{url}"
            );
        }
        let mut cfg = NtfyConfig::new(Secret::new("http://192.168.1.5/t"));
        cfg.allow_private = true;
        assert!(Ntfy::new(&cfg).is_ok());
    }

    #[tokio::test]
    async fn rate_limits_are_retried_with_retry_after_then_succeed() {
        let (port, seen) = replay(&["rate_limited", "publish_ok"]).await;
        let out = deliver(&ntfy(&config(port)), &alert(), &FAST).await;
        assert_eq!(out.result, Ok(()));
        assert_eq!(out.attempts, 2);
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 2);
        assert_eq!(seen[0].body, seen[1].body);
    }

    #[tokio::test]
    async fn server_errors_are_retried() {
        let (port, seen) = replay(&["bad_gateway_html", "bad_gateway_html", "publish_ok"]).await;
        let out = deliver(&ntfy(&config(port)), &alert(), &FAST).await;
        assert_eq!(out.result, Ok(()));
        assert_eq!(out.attempts, 3);
        assert_eq!(seen.lock().unwrap().len(), 3);

        let (port, _) = replay(&["bad_gateway_html"; 4]).await;
        let out = deliver(&ntfy(&config(port)), &alert(), &FAST).await;
        assert_eq!(out.result, Err(DeliveryError::Rejected { status: 502 }));
        assert_eq!(out.attempts, 4);
    }

    #[tokio::test]
    async fn client_errors_are_final() {
        for (name, status) in [("unauthorized", 401), ("forbidden", 403)] {
            let (port, seen) = replay(&[name, "publish_ok"]).await;
            let out = deliver(&ntfy(&config(port)), &alert(), &FAST).await;
            assert_eq!(
                out.result,
                Err(DeliveryError::Rejected { status }),
                "{name}"
            );
            assert_eq!(out.attempts, 1);
            assert_eq!(seen.lock().unwrap().len(), 1);
        }
    }

    #[tokio::test]
    async fn an_answer_that_isnt_ntfys_is_not_a_delivery() {
        let (port, _) = replay(&["captive_portal_html"]).await;
        let out = deliver(&ntfy(&config(port)), &alert(), &FAST).await;
        assert_eq!(
            out.result,
            Err(DeliveryError::Failed(
                "the endpoint didn't answer like ntfy"
            ))
        );
        assert_eq!(out.attempts, 1, "not retried");
    }

    #[tokio::test]
    async fn an_oversized_answer_is_refused() {
        let (port, _) = replay(&["oversized_answer"]).await;
        let out = deliver(&ntfy(&config(port)), &alert(), &FAST).await;
        assert_eq!(
            out.result,
            Err(DeliveryError::Failed("the endpoint's answer was too large"))
        );
        assert_eq!(out.attempts, 1);
    }

    #[tokio::test]
    async fn redirects_to_refused_addresses_are_not_followed() {
        use axum::response::Response;
        let hits = Arc::new(Mutex::new(Vec::new()));
        let log = Arc::clone(&hits);
        let app = axum::Router::new().fallback(move |req: axum::extract::Request| {
            let log = Arc::clone(&log);
            async move {
                log.lock().unwrap().push(req.uri().to_string());
                Response::builder()
                    .status(307)
                    .header("location", "http://metadata.test/latest/meta-data/")
                    .body(axum::body::Body::empty())
                    .unwrap()
            }
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let out = deliver(&ntfy(&config(port)), &alert(), &FAST).await;
        assert_eq!(out.result, Err(DeliveryError::Blocked(AddrClass::Metadata)));
        assert_eq!(out.attempts, 1);
        assert_eq!(hits.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn credentials_are_dropped_on_a_cross_origin_redirect() {
        use axum::response::Response;
        let (target_port, seen) = replay(&[]).await;
        let location = format!("http://other.test:{target_port}/elsewhere");
        let app = axum::Router::new().fallback(move |_: axum::extract::Request| {
            let location = location.clone();
            async move {
                Response::builder()
                    .status(308)
                    .header("location", location)
                    .body(axum::body::Body::empty())
                    .unwrap()
            }
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let r = ScriptedResolver::new(&[
            ("ntfy.test", &[&["127.0.0.1"]]),
            ("other.test", &[&["127.0.0.1"]]),
        ]);
        let http =
            GuardedClient::with_resolver(NetPolicy::loopback_for_tests(), NTFY_TIMEOUT, r).unwrap();
        let mut cfg = config(port);
        cfg.allow_private = true;
        cfg.auth = NtfyAuth::Bearer(Secret::new("tk_abc123"));
        let sender = Ntfy::with_client(&cfg, http).unwrap();
        // The target answers with an unexpected path for the fixture, so
        // script nothing and just look at what arrived.
        let _ = sender.attempt(&alert()).await;
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].header("authorization"), None);
    }

    #[test]
    fn long_messages_are_cut_at_a_character_boundary_with_an_ellipsis() {
        let mut n = Notification::new(NotificationKind::Digest, "t", "é".repeat(4000));
        n.items = vec![];
        let m = Ntfy::message(&n);
        assert!(m.len() <= MAX_MESSAGE_BYTES);
        assert!(m.ends_with('…'));
        // 'é' is two bytes: the cut landed between whole characters.
        assert!(m.trim_end_matches('…').chars().all(|c| c == 'é'));
        assert_eq!(m.len(), MAX_MESSAGE_BYTES - 1, "one byte lost to alignment");

        let short = Notification::new(NotificationKind::Test, "t", "fits");
        assert_eq!(Ntfy::message(&short), "fits");
        assert_eq!(truncate_bytes("abcdef", 6), "abcdef");
        assert_eq!(truncate_bytes("abcdefg", 6), "abc…");
        assert_eq!(truncate_bytes("日本語日本語", 10), "日本…");
    }

    #[test]
    fn many_items_are_summarised_and_private_ones_counted() {
        let mut n = Notification::new(NotificationKind::Alert, "t", "");
        n.items = (0..30)
            .map(|i| NotificationItem {
                name: format!("item {i}"),
                info_hash: format!("{i:040x}"),
                size_bytes: None,
                private_source: i % 10 == 0,
            })
            .collect();
        let m = Ntfy::message(&n);
        assert_eq!(m.matches('•').count(), MAX_LISTED_ITEMS);
        assert!(m.contains("…and 7 more"), "{m}");
        assert!(m.ends_with("3 results from private sources aren't included."));
        assert!(!m.contains("item 0\n") && !m.contains("item 10\n"));
    }

    #[test]
    fn hostile_names_cant_inject_lines_or_controls() {
        let mut n = Notification::new(NotificationKind::Alert, "ti\r\ntle\u{0}", "b\u{7}ody");
        n.items = vec![NotificationItem {
            name: "evil\nname\u{202e}gpj.exe".into(),
            info_hash: "c".repeat(40),
            size_bytes: None,
            private_source: false,
        }];
        let m = Ntfy::message(&n);
        assert!(m.contains("• evil name") && m.contains("gpj.exe"));
        assert!(!m.contains('\u{202e}') && !m.contains('\u{7}'));
        let title = Ntfy::title_header(&n).unwrap();
        assert_eq!(title.to_str().unwrap(), "ti  tle");
    }

    #[test]
    fn click_links_must_be_web_urls() {
        assert_eq!(
            web_link("https://l.lan/a?b=1"),
            Some("https://l.lan/a?b=1".into())
        );
        assert_eq!(web_link("javascript:alert(1)"), None);
        assert_eq!(web_link("file:///etc/passwd"), None);
        assert_eq!(web_link("not a url"), None);
    }

    #[test]
    fn non_ascii_titles_use_rfc_2047() {
        assert_eq!(encode_header_text("plain title"), "plain title");
        assert_eq!(encode_header_text("é"), "=?UTF-8?B?w6k=?=");
        let long = encode_header_text(&"日本語".repeat(20));
        for word in long.split(' ') {
            assert!(word.len() <= 75 && word.starts_with("=?UTF-8?B?"), "{word}");
        }
        assert!(HeaderValue::from_str(&long).is_ok());
    }

    #[test]
    fn base64_matches_the_standard_vectors() {
        for (input, want) in [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("fooba", "Zm9vYmE="),
            ("foobar", "Zm9vYmFy"),
        ] {
            assert_eq!(base64(input.as_bytes()), want);
        }
    }

    #[test]
    fn sizes_follow_the_metrics_contract() {
        for (bytes, want) in [
            (0, "0\u{a0}B"),
            (512, "512\u{a0}B"),
            (1023, "1023\u{a0}B"),
            (1024, "1.00\u{a0}KiB"),
            (912 * 1024, "912\u{a0}KiB"),
            (1_048_576, "1.00\u{a0}MiB"),
            (1023 * 1024 + 900, "1.00\u{a0}MiB"),
            (1024 * 1024 - 1, "1.00\u{a0}MiB"),
            (40_027_955, "38.2\u{a0}MiB"),
            (4_692_251_770, "4.37\u{a0}GiB"),
            (u64::MAX, "16384\u{a0}PiB"),
        ] {
            assert_eq!(format_bytes(bytes), want, "{bytes}");
        }
    }

    #[tokio::test]
    async fn secrets_never_appear_in_debug_errors_or_logs() {
        let logs = crate::test_support::capture_logs();
        const TOPIC: &str = "TOPICSECRET-ab12cd";
        const TOKEN: &str = "tk_TOKENSECRET9";
        const USER: &str = "USERSECRETname";
        const PASS: &str = "PASSSECRETword";
        let secrets = [TOPIC, TOKEN, USER, PASS];

        // A full delivery against a local server, over both kinds of auth,
        // through a failure and a success.
        let (port, _) = replay(&["rate_limited", "unauthorized"]).await;
        let mut outs = Vec::new();
        let mut texts = String::new();
        for auth in [
            NtfyAuth::Bearer(Secret::new(TOKEN)),
            NtfyAuth::Basic {
                username: Secret::new(USER),
                password: Secret::new(PASS),
            },
        ] {
            let mut cfg = NtfyConfig::new(Secret::new(format!("http://ntfy.test:{port}/{TOPIC}")));
            cfg.allow_private = true;
            cfg.auth = auth;
            let sender = Ntfy::with_client(&cfg, client()).unwrap();
            let out = deliver(&sender, &alert(), &FAST).await;
            texts.push_str(&format!("{sender:?} {cfg:?} {out:?}"));
            if let Err(e) = &out.result {
                texts.push_str(&e.to_string());
            }
            let boxed: Box<dyn Notifier> = Box::new(sender);
            texts.push_str(&format!("{boxed:?}"));
            outs.push(out);
        }
        assert!(outs.iter().any(|o| o.result.is_err()));

        // And a refusal at the guard, with the secret URL in play.
        let mut cfg = NtfyConfig::new(Secret::new(format!("http://192.168.1.5/{TOPIC}")));
        cfg.auth = NtfyAuth::Bearer(Secret::new(TOKEN));
        let err = Ntfy::new(&cfg).unwrap_err();
        texts.push_str(&format!("{err:?} {err} {}", NetError::Redirect));

        let logged = logs.contents();
        assert!(
            logged.contains("ntfy attempt"),
            "the capture works: {logged}"
        );
        for s in secrets {
            assert!(!texts.contains(s), "{s} in debug or error text: {texts}");
            assert!(!logged.contains(s), "{s} in the logs: {logged}");
        }
        assert!(texts.contains("http://ntfy.test:"), "{texts}");
    }
}
