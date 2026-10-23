//! The Discord sender.
//!
//! `POST`s a JSON payload to a Discord webhook,
//! `https://discord.com/api/webhooks/<id>/<token>?wait=true` (`wait=true`
//! makes Discord answer with the created message, so a success is
//! confirmed):
//!
//! ```json
//! {
//!   "allowed_mentions": {"parse": []},
//!   "embeds": [{
//!     "title": "4K nature docs: 2 new",     // ≤ 256 characters
//!     "url": "http://lodestar.lan/alerts/7", // when the caller gives a link
//!     "description": "…"                      // ≤ 4,096; the whole embed ≤ 6,000
//!   }]
//! }
//! ```
//!
//! - **The URL is a secret** (its path is the token) and is a [`Secret`]. The
//!   constructor refuses any host but Discord's own (`discord.com`,
//!   `discordapp.com`, and their `canary.` and `ptb.` hosts), any other path
//!   shape, and any query. Public addresses only: there is no
//!   `allow_private`. Redirects aren't followed.
//! - **Nothing can ping.** `allowed_mentions.parse` is always empty, so a
//!   torrent named `@everyone` or `<@&role>` is only text; markdown and
//!   mention syntax in names is escaped too, so a name can't become a masked
//!   link (`[text](url)`), a mention or formatting. A bare `https://…` in a
//!   name may still be shown as a plain link: its target is what it reads.
//! - **Private items stay local (§18.7):** only shareable items are listed,
//!   and the description says how many were withheld.
//! - **Rate limits:** a 429 carries `retry_after` (seconds, often
//!   fractional) in its JSON body and `Retry-After` in a header. The body's
//!   value is used when it is there, else the header's; [`deliver`]
//!   waits that long (capped by its policy) before trying again. The shared
//!   `check_status` only reads whole seconds from the header, so this
//!   sender refines `retry_after` itself.
//! - Limits count UTF-16 units, the most any Discord counter could be, so a
//!   message never fails for length.
//!
//! [`deliver`]: super::deliver

use std::fmt;
use std::time::Duration;

use reqwest::header::{CONTENT_TYPE, HeaderMap, RETRY_AFTER};
use reqwest::{Method, StatusCode, Url};
use serde_json::{Value, json};

use super::text::{clean_line, compose, web_link};
use super::{AttemptError, Channel, DeliveryError, Notification, Notifier, check_status};
use crate::BoxFuture;
use crate::net::{GuardedClient, NetPolicy, parse_url, read_limited, redact};
use crate::secret::Secret;

/// The timeout for one attempt.
pub const DISCORD_TIMEOUT: Duration = Duration::from_secs(10);

/// Discord's embed limits.
pub const MAX_TITLE: usize = 256;
pub const MAX_DESCRIPTION: usize = 4096;
/// Title and description together, for one embed (all embeds share 6,000).
pub const MAX_EMBED_TOTAL: usize = 6000;

/// How much of an answer is read. Discord's message objects are a few KB.
const MAX_ANSWER_BYTES: usize = 64 * 1024;

/// The longest wait a rate limit may ask for.
const MAX_RETRY_AFTER: Duration = Duration::from_secs(3600);

const HOSTS: [&str; 6] = [
    "discord.com",
    "canary.discord.com",
    "ptb.discord.com",
    "discordapp.com",
    "canary.discordapp.com",
    "ptb.discordapp.com",
];

/// A Discord channel's settings.
#[derive(Debug, Clone)]
pub struct DiscordConfig {
    /// `https://discord.com/api/webhooks/<id>/<token>`. Secret: anyone
    /// with it can post to the channel.
    pub webhook_url: Secret,
}

pub struct Discord {
    /// The webhook URL with `wait=true`.
    url: Url,
    http: GuardedClient,
}

impl fmt::Debug for Discord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Discord")
            .field("url", &redact(&self.url))
            .finish_non_exhaustive()
    }
}

impl Discord {
    /// Checks the URL and builds the sender. Anything but a Discord webhook
    /// URL fails here, when the channel is saved.
    pub fn new(config: &DiscordConfig) -> Result<Discord, DeliveryError> {
        let http = GuardedClient::new(NetPolicy::notification(false), DISCORD_TIMEOUT)
            .map_err(|e| AttemptError::from(e).error)?;
        Discord::with_client(config, http)
    }

    /// The same, over a given client (tests inject a resolver this way).
    pub fn with_client(
        config: &DiscordConfig,
        http: GuardedClient,
    ) -> Result<Discord, DeliveryError> {
        let url = parse_url(config.webhook_url.expose(), http.policy())
            .map_err(|e| AttemptError::from(e).error)?;
        let url = canonical_webhook_url(&url)?;
        Ok(Discord::from_url(url, http))
    }

    fn from_url(mut url: Url, http: GuardedClient) -> Discord {
        url.query_pairs_mut().append_pair("wait", "true");
        Discord { url, http }
    }

    /// The JSON document for `n`: one embed, private items left out and
    /// counted, and no mention allowed.
    pub fn payload(n: &Notification) -> Value {
        let title = clean_line(&n.title);
        let title = truncate_units(&escape_markdown(&title), MAX_TITLE);
        let budget = MAX_DESCRIPTION.min(MAX_EMBED_TOTAL - units(&title));
        let mut description = description(n, budget);
        let mut embed = serde_json::Map::new();
        if title.is_empty() && description.is_empty() {
            // An embed needs a title or a description.
            description = n.kind.as_str().to_owned();
        }
        if !title.is_empty() {
            embed.insert("title".into(), title.into());
        }
        if !description.is_empty() {
            embed.insert("description".into(), description.into());
        }
        if let Some(link) = n.link.as_deref().and_then(web_link) {
            embed.insert("url".into(), link.into());
        }
        json!({
            "allowed_mentions": {"parse": []},
            "embeds": [embed],
        })
    }
}

fn description(n: &Notification, budget: usize) -> String {
    compose(n, escape_markdown, budget, units, truncate_units)
}

/// Accepts only `https://<discord host>/api[/v<N>]/webhooks/<id>/<token>`
/// (a trailing slash is allowed) and returns it without the slash.
fn canonical_webhook_url(url: &Url) -> Result<Url, DeliveryError> {
    let bad = |why| Err(DeliveryError::InvalidEndpoint(why));
    if url.scheme() != "https" {
        return bad("a Discord webhook URL starts with https://");
    }
    if !url.host_str().is_some_and(|h| HOSTS.contains(&h)) {
        return bad("that isn't a Discord webhook URL");
    }
    if url.port().is_some() || !url.username().is_empty() || url.password().is_some() {
        return bad("that isn't a Discord webhook URL");
    }
    if url.query().is_some() || url.fragment().is_some() {
        return bad("the webhook URL must not have a query or fragment");
    }
    let segments: Vec<&str> = url
        .path()
        .trim_end_matches('/')
        .split('/')
        .skip(1)
        .collect();
    let (id, token) = match segments.as_slice() {
        ["api", "webhooks", id, token] => (*id, *token),
        ["api", v, "webhooks", id, token]
            if v.strip_prefix('v')
                .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit())) =>
        {
            (*id, *token)
        }
        _ => return bad("that isn't a Discord webhook URL"),
    };
    let id_ok = (15..=25).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_digit());
    let token_ok = (1..=128).contains(&token.len())
        && token
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b));
    if id_ok && token_ok {
        let mut canonical = url.clone();
        canonical.set_path(&format!("/{}", segments.join("/")));
        Ok(canonical)
    } else {
        bad("that isn't a Discord webhook URL")
    }
}

impl Notifier for Discord {
    fn channel(&self) -> Channel {
        Channel::Discord
    }

    fn attempt<'a>(&'a self, n: &'a Notification) -> BoxFuture<'a, Result<(), AttemptError>> {
        Box::pin(async move {
            let body = serde_json::to_vec(&Discord::payload(n)).map_err(|_| {
                AttemptError::fatal(DeliveryError::Failed("couldn't build the payload"))
            })?;
            let req = self
                .http
                .request(Method::POST, self.url.clone())
                .header(CONTENT_TYPE, "application/json")
                .body(body)
                .build()
                .map_err(|_| {
                    AttemptError::fatal(DeliveryError::Failed("couldn't build the request"))
                })?;
            // Discord has no reason to redirect a webhook, and the token is
            // in the path: a redirect is an error, not something to follow.
            let resp = self.http.send(req).await?;
            let status = resp.status();
            let headers = resp.headers().clone();
            let checked = check_status(status, &headers);
            let body = read_limited(resp, MAX_ANSWER_BYTES).await;
            lodestar_log::log_event!(
                crate::log_events::NOTIFY_ATTEMPT,
                channel = "discord",
                status = lodestar_log::Count(status.as_u16().into()),
                url = lodestar_log::Url::new(&redact(&self.url))
            );
            match checked {
                Err(mut e) => {
                    if status == StatusCode::TOO_MANY_REQUESTS {
                        let body = body.as_deref().unwrap_or_default();
                        e.retry_after = rate_limit_delay(body, &headers).or(e.retry_after);
                    }
                    Err(e)
                }
                Ok(()) => {
                    // `wait=true`: a message object with an `id`.
                    let confirmed = status == StatusCode::NO_CONTENT
                        || serde_json::from_slice::<Value>(&body?)
                            .ok()
                            .is_some_and(|v| v.get("id").is_some_and(Value::is_string));
                    if confirmed {
                        Ok(())
                    } else {
                        Err(AttemptError::fatal(DeliveryError::Failed(
                            "the endpoint didn't answer like Discord",
                        )))
                    }
                }
            }
        })
    }
}

/// How long a 429 asks to wait: the body's `retry_after` (seconds, possibly
/// fractional), else the `Retry-After` header.
fn rate_limit_delay(body: &[u8], headers: &HeaderMap) -> Option<Duration> {
    let from_body = serde_json::from_slice::<Value>(body)
        .ok()
        .and_then(|v| v.get("retry_after").and_then(Value::as_f64));
    let from_header = headers
        .get(RETRY_AFTER)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().parse::<f64>().ok());
    let secs = from_body.or(from_header)?;
    // Negative, NaN and absurd values are ignored or clamped, never panic.
    Duration::try_from_secs_f64(secs)
        .ok()
        .map(|d| d.min(MAX_RETRY_AFTER))
}

/// Backslash-escapes what Discord renders specially, so a name can't turn
/// into a link, a mention, a code block or a quote.
fn escape_markdown(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if matches!(
            c,
            '\\' | '*' | '_' | '~' | '`' | '|' | '>' | '<' | '[' | ']' | '(' | ')' | '#' | '@'
        ) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

fn units(s: &str) -> usize {
    s.encode_utf16().count()
}

/// At most `max` UTF-16 units, ending in `…` when anything was cut. Never
/// cuts inside a character, and never leaves a dangling escape backslash.
fn truncate_units(s: &str, max: usize) -> String {
    if units(s) <= max {
        return s.to_owned();
    }
    let mut out = String::new();
    let mut used = 0;
    for c in s.chars() {
        if used + c.len_utf16() > max.saturating_sub(1) {
            break;
        }
        used += c.len_utf16();
        out.push(c);
    }
    // An odd run of trailing backslashes would escape the ellipsis.
    let trailing = out.chars().rev().take_while(|&c| c == '\\').count();
    if trailing % 2 == 1 {
        out.pop();
    }
    out.push('…');
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

    const TOKEN: &str = "Tk3n-SECRET_value";
    const PATH: &str = "/api/webhooks/123456789012345678/Tk3n-SECRET_value";

    #[derive(Deserialize)]
    struct Fixture {
        request: FixtureRequest,
        response: FixtureResponse,
    }

    #[derive(Deserialize)]
    struct FixtureRequest {
        method: String,
        path: String,
        query: Option<String>,
    }

    #[derive(Deserialize)]
    struct FixtureResponse {
        status: u16,
        headers: Vec<(String, String)>,
        body: String,
    }

    fn fixture(name: &str) -> Fixture {
        let path = format!(
            "{}/tests/fixtures/discord/{name}.json",
            env!("CARGO_MANIFEST_DIR")
        );
        let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"));
        serde_json::from_str(&text).unwrap_or_else(|e| panic!("{path}: {e}"))
    }

    #[derive(Debug)]
    struct Received {
        method: String,
        path: String,
        query: Option<String>,
        headers: Vec<(String, String)>,
        body: Vec<u8>,
    }

    impl Received {
        fn header(&self, name: &str) -> Option<&str> {
            self.headers
                .iter()
                .find(|(k, _)| k == name)
                .map(|(_, v)| v.as_str())
        }

        fn json(&self) -> Value {
            serde_json::from_slice(&self.body).unwrap()
        }
    }

    /// Plays the named fixtures in order (then `message_ok` forever),
    /// checking each request's method, path and query against the fixture.
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
                let query = req.uri().query().map(str::to_owned);
                let headers = req
                    .headers()
                    .iter()
                    .map(|(k, v)| (k.to_string(), v.to_str().unwrap_or("?").to_owned()))
                    .collect();
                let body = to_bytes(req.into_body(), 1 << 20).await.unwrap().to_vec();
                let next = script
                    .lock()
                    .unwrap()
                    .pop()
                    .unwrap_or_else(|| fixture("message_ok"));
                assert_eq!(
                    (
                        next.request.method.as_str(),
                        next.request.path.as_str(),
                        next.request.query.as_deref()
                    ),
                    (method.as_str(), path.as_str(), query.as_deref())
                );
                log.lock().unwrap().push(Received {
                    method,
                    path,
                    query,
                    headers,
                    body,
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

    /// A sender aimed at the local server, bypassing the host check that
    /// the constructor enforces (tested separately).
    fn discord(port: u16) -> Discord {
        let r = ScriptedResolver::new(&[("discord.test", &[&["127.0.0.1"]])]);
        let http =
            GuardedClient::with_resolver(NetPolicy::loopback_for_tests(), DISCORD_TIMEOUT, r)
                .unwrap();
        let url = Url::parse(&format!("http://discord.test:{port}{PATH}")).unwrap();
        Discord::from_url(url, http)
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
    async fn posts_one_embed_with_wait_and_no_mentions() {
        let (port, seen) = replay(&["message_ok"]).await;
        let out = deliver(&discord(port), &alert(), &FAST).await;
        assert_eq!(out.result, Ok(()));
        assert_eq!(out.attempts, 1);
        let seen = seen.lock().unwrap();
        let r = &seen[0];
        assert_eq!((r.method.as_str(), r.path.as_str()), ("POST", PATH));
        assert_eq!(r.query.as_deref(), Some("wait=true"));
        assert_eq!(r.header("content-type"), Some("application/json"));
        assert_eq!(r.header("user-agent"), Some(crate::USER_AGENT));
        assert_eq!(
            r.json(),
            json!({
                "allowed_mentions": {"parse": []},
                "embeds": [{
                    "title": "4K nature docs: 2 new",
                    "url": "http://lodestar.lan/alerts/7",
                    "description": "Two new results.\n\n• Planet.Earth.III.2160p (4.37\u{a0}GiB)\n\n\
                                    1 result from a private source isn't included.",
                }],
            })
        );
        assert!(!String::from_utf8_lossy(&r.body).contains("Private.Tracker"));
    }

    #[test]
    fn nothing_in_a_name_can_mention_or_hide_a_link() {
        let mut n = Notification::new(NotificationKind::Alert, "@everyone <@&1234>", "@here");
        n.items = vec![NotificationItem {
            name: "[click](http://evil.example) <@12345> @everyone `x` **b** > q".into(),
            info_hash: "d".repeat(40),
            size_bytes: None,
            private_source: false,
        }];
        let p = Discord::payload(&n);
        assert_eq!(p["allowed_mentions"], json!({"parse": []}));
        let embed = &p["embeds"][0];
        for text in [
            embed["title"].as_str().unwrap(),
            embed["description"].as_str().unwrap(),
        ] {
            assert!(!text.contains("<@") && !text.contains("<@&"), "{text}");
            assert!(!text.contains("](") && !text.contains("[click]"), "{text}");
            // Every `@` is escaped.
            assert_eq!(
                text.matches('@').count(),
                text.matches("\\@").count(),
                "{text}"
            );
        }
        assert!(
            embed["description"]
                .as_str()
                .unwrap()
                .contains("\\[click\\]\\(http")
        );
    }

    #[test]
    fn limits_hold_for_any_size() {
        for (title, body, items) in [
            (1000, 10_000, 0),
            (300, 5000, 50),
            (256, 4096, 3),
            (10, 5800, 25),
            (0, 0, 0),
        ] {
            let mut n = Notification::new(
                NotificationKind::Digest,
                "t".repeat(title),
                "😀".repeat(body / 2),
            );
            n.items = (0..items)
                .map(|i| NotificationItem {
                    name: "ñ".repeat(150 + i),
                    info_hash: format!("{i:040x}"),
                    size_bytes: Some(i as u64),
                    private_source: false,
                })
                .collect();
            let p = Discord::payload(&n);
            let e = &p["embeds"][0];
            let t = units(e["title"].as_str().unwrap_or(""));
            let d = units(e["description"].as_str().unwrap_or(""));
            assert!(t <= MAX_TITLE, "{t}");
            assert!(d <= MAX_DESCRIPTION, "{d}");
            assert!(t + d <= MAX_EMBED_TOTAL, "{t}+{d}");
            assert!(t + d > 0, "an embed needs text");
            assert_eq!(p["embeds"].as_array().unwrap().len(), 1);
        }
    }

    #[test]
    fn the_private_count_survives_an_overflowing_description() {
        let mut n = Notification::new(NotificationKind::Alert, "t", "b".repeat(500));
        n.items = (0..23)
            .map(|i| NotificationItem {
                name: format!("{i:02} {}", "ñ".repeat(250)),
                info_hash: format!("{i:040x}"),
                size_bytes: Some(1 << 30),
                private_source: i >= 21,
            })
            .collect();
        let p = Discord::payload(&n);
        let d = p["embeds"][0]["description"].as_str().unwrap();
        assert!(units(d) <= MAX_DESCRIPTION, "{}", units(d));
        assert!(d.contains('…'), "the list part was cut");
        assert!(
            d.ends_with("\n\n2 results from private sources aren't included."),
            "{d}"
        );
    }

    #[test]
    fn truncation_is_clean() {
        assert_eq!(truncate_units("abc", 3), "abc");
        assert_eq!(truncate_units("abcd", 3), "ab…");
        // An astral character is two units and isn't split.
        assert_eq!(truncate_units("a😀😀", 4), "a😀…");
        assert_eq!(truncate_units("a😀😀", 3), "a…");
        // A cut after a lone escape backslash drops it.
        assert_eq!(truncate_units("ab\\*cd", 4), "ab…");
        assert_eq!(truncate_units("ab\\\\cd", 5), "ab\\\\…");
    }

    #[test]
    fn empty_notifications_still_make_a_valid_embed() {
        let n = Notification::new(NotificationKind::Test, " ", "");
        let p = Discord::payload(&n);
        assert_eq!(p["embeds"][0], json!({"description": "test"}));
    }

    #[test]
    fn only_discord_webhook_urls_are_accepted() {
        let id = "123456789012345678";
        let ok = |url: String| {
            Discord::with_client(
                &DiscordConfig {
                    webhook_url: Secret::new(url),
                },
                GuardedClient::new(NetPolicy::notification(false), DISCORD_TIMEOUT).unwrap(),
            )
        };
        for host in HOSTS {
            assert!(
                ok(format!("https://{host}/api/webhooks/{id}/{TOKEN}")).is_ok(),
                "{host}"
            );
        }
        assert!(ok(format!("https://discord.com/api/v10/webhooks/{id}/{TOKEN}")).is_ok());
        let slashed = ok(format!("https://discord.com/api/webhooks/{id}/{TOKEN}/")).unwrap();
        assert_eq!(slashed.url.path(), PATH, "no trailing slash is sent");
        assert_eq!(slashed.url.query(), Some("wait=true"));
        for bad in [
            format!("http://discord.com/api/webhooks/{id}/{TOKEN}"),
            format!("https://evil.example/api/webhooks/{id}/{TOKEN}"),
            format!("https://discord.com.evil.example/api/webhooks/{id}/{TOKEN}"),
            format!("https://evildiscord.com/api/webhooks/{id}/{TOKEN}"),
            format!("https://discord.com@evil.example/api/webhooks/{id}/{TOKEN}"),
            format!("https://user:pw@discord.com/api/webhooks/{id}/{TOKEN}"),
            format!("https://discord.com:8443/api/webhooks/{id}/{TOKEN}"),
            format!("https://discord.com/api/webhooks/{id}/{TOKEN}?thread_id=1"),
            format!("https://discord.com/api/webhooks/{id}/{TOKEN}#x"),
            format!("https://discord.com/api/webhooks/{id}/{TOKEN}/slack"),
            format!("https://discord.com/api/webhooks/{id}"),
            format!("https://discord.com/api/webhooks/notanid/{TOKEN}"),
            format!("https://discord.com/api/vx/webhooks/{id}/{TOKEN}"),
            format!("https://discord.com/other/webhooks/{id}/{TOKEN}"),
            "https://discord.com/".to_owned(),
            "https://127.0.0.1/api/webhooks/123456789012345678/t".to_owned(),
            "https://[::1]/api/webhooks/123456789012345678/t".to_owned(),
            "file:///etc/passwd".to_owned(),
            String::new(),
        ] {
            let err = ok(bad.clone()).unwrap_err();
            assert!(
                matches!(
                    err,
                    DeliveryError::InvalidEndpoint(_) | DeliveryError::Blocked(_)
                ),
                "{bad}: {err:?}"
            );
        }
        // No private-network escape hatch: the policy is the public one.
        assert_eq!(
            Discord::new(&DiscordConfig {
                webhook_url: Secret::new("https://127.0.0.1/api/webhooks/123456789012345678/t")
            })
            .unwrap_err(),
            DeliveryError::Blocked(AddrClass::Loopback)
        );
    }

    #[tokio::test]
    async fn rate_limits_wait_then_succeed_with_the_same_payload() {
        let (port, seen) = replay(&["rate_limited", "message_ok"]).await;
        let out = deliver(&discord(port), &alert(), &FAST).await;
        assert_eq!(out.result, Ok(()));
        assert_eq!(out.attempts, 2);
        let seen = seen.lock().unwrap();
        assert_eq!(seen[0].body, seen[1].body);
    }

    #[test]
    fn retry_after_prefers_the_body_and_survives_junk() {
        let headers = |v: &str| {
            let mut h = HeaderMap::new();
            h.insert(RETRY_AFTER, v.parse().unwrap());
            h
        };
        let body = br#"{"message":"You are being rate limited.","retry_after":0.5,"global":false}"#;
        assert_eq!(
            rate_limit_delay(body, &headers("2")),
            Some(Duration::from_millis(500))
        );
        assert_eq!(
            rate_limit_delay(b"<html>", &headers("2")),
            Some(Duration::from_secs(2))
        );
        assert_eq!(
            rate_limit_delay(b"<html>", &headers("1.5")),
            Some(Duration::from_millis(1500))
        );
        assert_eq!(rate_limit_delay(b"", &HeaderMap::new()), None);
        assert_eq!(
            rate_limit_delay(br#"{"retry_after":-3}"#, &HeaderMap::new()),
            None
        );
        assert_eq!(
            rate_limit_delay(br#"{"retry_after":"soon"}"#, &HeaderMap::new()),
            None
        );
        assert_eq!(
            rate_limit_delay(br#"{"retry_after":1e300}"#, &HeaderMap::new()),
            None,
            "too large for a Duration"
        );
        assert_eq!(
            rate_limit_delay(br#"{"retry_after":99999}"#, &HeaderMap::new()),
            Some(MAX_RETRY_AFTER)
        );
    }

    #[tokio::test]
    async fn the_429_body_sets_the_attempts_retry_after() {
        let (port, _) = replay(&["rate_limited"]).await;
        let e = discord(port).attempt(&alert()).await.unwrap_err();
        assert!(e.retry);
        assert_eq!(e.error, DeliveryError::Rejected { status: 429 });
        // The body said 0.002 s; the header's whole second is refined away.
        assert_eq!(e.retry_after, Some(Duration::from_millis(2)));
    }

    #[tokio::test]
    async fn server_errors_are_retried_and_client_errors_are_final() {
        let (port, seen) = replay(&["bad_gateway_html", "bad_gateway_html", "message_ok"]).await;
        let out = deliver(&discord(port), &alert(), &FAST).await;
        assert_eq!(out.result, Ok(()));
        assert_eq!(out.attempts, 3);
        assert_eq!(seen.lock().unwrap().len(), 3);

        for (name, status) in [
            ("unknown_webhook", 404),
            ("invalid_token", 401),
            ("invalid_body", 400),
        ] {
            let (port, seen) = replay(&[name, "message_ok"]).await;
            let out = deliver(&discord(port), &alert(), &FAST).await;
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
    async fn answers_that_arent_discords_are_not_deliveries() {
        let (port, _) = replay(&["captive_portal_html"]).await;
        let out = deliver(&discord(port), &alert(), &FAST).await;
        assert_eq!(
            out.result,
            Err(DeliveryError::Failed(
                "the endpoint didn't answer like Discord"
            ))
        );
        assert_eq!(out.attempts, 1);

        let (port, _) = replay(&["oversized_answer"]).await;
        let out = deliver(&discord(port), &alert(), &FAST).await;
        assert_eq!(
            out.result,
            Err(DeliveryError::Failed("the endpoint's answer was too large"))
        );
    }

    #[tokio::test]
    async fn redirects_are_final_and_never_followed() {
        use axum::response::Response;
        let hits = Arc::new(Mutex::new(0));
        let count = Arc::clone(&hits);
        let app = axum::Router::new().fallback(move |_: axum::extract::Request| {
            let count = Arc::clone(&count);
            async move {
                *count.lock().unwrap() += 1;
                Response::builder()
                    .status(307)
                    .header("location", "https://other.example/steal")
                    .body(axum::body::Body::empty())
                    .unwrap()
            }
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let out = deliver(&discord(port), &alert(), &FAST).await;
        assert_eq!(out.result, Err(DeliveryError::Rejected { status: 307 }));
        assert_eq!(out.attempts, 1);
        assert_eq!(*hits.lock().unwrap(), 1);
    }

    #[tokio::test]
    async fn the_url_never_appears_in_debug_errors_or_logs() {
        let logs = crate::test_support::capture_logs();
        let (port, _) = replay(&["rate_limited", "unknown_webhook"]).await;
        let sender = discord(port);
        let out = deliver(&sender, &alert(), &FAST).await;
        assert_eq!(out.result, Err(DeliveryError::Rejected { status: 404 }));
        let config = DiscordConfig {
            webhook_url: Secret::new(format!("https://discord.com{PATH}")),
        };
        let real = Discord::new(&config).unwrap();
        let boxed: Box<dyn Notifier> = Box::new(discord(port));
        let refused = Discord::new(&DiscordConfig {
            webhook_url: Secret::new(format!("https://evil.example{PATH}")),
        })
        .unwrap_err();
        let text = format!(
            "{sender:?} {config:?} {real:?} {boxed:?} {out:?} {refused:?} {refused} {} {}",
            out.result.as_ref().unwrap_err(),
            NetError::Redirect
        );
        for secret in [TOKEN, "123456789012345678"] {
            assert!(!text.contains(secret), "{secret} in {text}");
        }
        assert!(text.contains("https://discord.com/…"), "{text}");
        let logged = logs.contents();
        assert!(
            logged.contains("discord attempt"),
            "the capture works: {logged}"
        );
        for secret in [TOKEN, "123456789012345678"] {
            assert!(!logged.contains(secret), "{secret} in the logs: {logged}");
        }
    }
}
