//! The webhook sender (the reference channel).
//!
//! `POST`s a JSON document to the user's URL:
//!
//! ```json
//! {
//!   "schema": "lodestar.notification",
//!   "schema_version": 1,
//!   "id": "9f1c…",              // the same on every retry: deduplicate on it
//!   "kind": "alert",            // alert | digest | test
//!   "created_at": 1761134400,   // Unix seconds
//!   "title": "4K nature docs: 3 new results",
//!   "body": "…",
//!   "link": "http://lodestar.lan/alerts/7",   // or null
//!   "items": [{"name": "…", "info_hash": "…", "size_bytes": 123}],
//!   "withheld_private": 1       // items from private sources, not sent
//! }
//! ```
//!
//! Headers: `Content-Type: application/json`, `X-Lodestar-Delivery: <id>`
//! and `X-Lodestar-Schema: 1`. A 2xx answer is delivered. Only 307 and 308
//! redirects are followed, each hop checked by the SSRF guard.
//!
//! The URL is treated as a secret (Discord-style URLs carry their token in
//! the path): it's stored sealed, and only its scheme and host are ever
//! shown.

use std::fmt;
use std::time::Duration;

use reqwest::header::CONTENT_TYPE;
use reqwest::{Method, Url};
use serde_json::json;

use super::{AttemptError, Channel, DeliveryError, Notification, Notifier, check_status};
use crate::BoxFuture;
use crate::net::{GuardedClient, NetPolicy, parse_url, read_limited, redact};
use crate::secret::Secret;

/// The payload's schema version. Additive changes keep it; anything a
/// receiver could misread bumps it.
pub const SCHEMA_VERSION: u32 = 1;

/// The timeout for one attempt.
pub const WEBHOOK_TIMEOUT: Duration = Duration::from_secs(10);

/// A webhook channel's settings.
#[derive(Debug, Clone)]
pub struct WebhookConfig {
    pub url: Secret,
    /// Let this channel reach private-network addresses (a LAN service such
    /// as Home Assistant). Off by default; loopback, link-local and
    /// metadata addresses stay refused either way.
    pub allow_private: bool,
}

pub struct Webhook {
    url: Url,
    http: GuardedClient,
}

impl fmt::Debug for Webhook {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Webhook")
            .field("url", &redact(&self.url))
            .finish_non_exhaustive()
    }
}

impl Webhook {
    /// Checks the URL against the notification policy and builds the
    /// sender. A refused address fails here, when the channel is saved.
    pub fn new(config: &WebhookConfig) -> Result<Webhook, DeliveryError> {
        let http = GuardedClient::new(
            NetPolicy::notification(config.allow_private),
            WEBHOOK_TIMEOUT,
        )
        .map_err(|e| AttemptError::from(e).error)?;
        Webhook::with_client(config, http)
    }

    /// The same, over a given client (tests inject a resolver this way).
    pub fn with_client(
        config: &WebhookConfig,
        http: GuardedClient,
    ) -> Result<Webhook, DeliveryError> {
        let url = parse_url(config.url.expose(), http.policy())
            .map_err(|e| AttemptError::from(e).error)?;
        Ok(Webhook { url, http })
    }

    /// The JSON document for `n`, private items left out.
    pub fn payload(n: &Notification) -> serde_json::Value {
        let items: Vec<_> = n
            .shareable_items()
            .map(|i| {
                json!({
                    "name": i.name,
                    "info_hash": i.info_hash,
                    "size_bytes": i.size_bytes,
                })
            })
            .collect();
        json!({
            "schema": "lodestar.notification",
            "schema_version": SCHEMA_VERSION,
            "id": n.id(),
            "kind": n.kind.as_str(),
            "created_at": n.created_at,
            "title": n.title,
            "body": n.body,
            "link": n.link,
            "items": items,
            "withheld_private": n.withheld(),
        })
    }
}

impl Notifier for Webhook {
    fn channel(&self) -> Channel {
        Channel::Webhook
    }

    fn attempt<'a>(&'a self, n: &'a Notification) -> BoxFuture<'a, Result<(), AttemptError>> {
        Box::pin(async move {
            let body = serde_json::to_vec(&Webhook::payload(n)).map_err(|_| {
                AttemptError::fatal(DeliveryError::Failed("couldn't build the payload"))
            })?;
            let req = self
                .http
                .request(Method::POST, self.url.clone())
                .header(CONTENT_TYPE, "application/json")
                .header("X-Lodestar-Delivery", n.id())
                .header("X-Lodestar-Schema", SCHEMA_VERSION.to_string())
                .body(body)
                .build()
                .map_err(|_| {
                    AttemptError::fatal(DeliveryError::Failed("couldn't build the request"))
                })?;
            let resp = self.http.send_following(req).await?;
            let status = resp.status();
            let checked = check_status(status, resp.headers());
            // Drain a little of the answer so the connection can be reused;
            // its content is never used or logged.
            let _ = read_limited(resp, 64 * 1024).await;
            lodestar_log::log_event!(
                crate::log_events::NOTIFY_ATTEMPT,
                channel = "webhook",
                status = lodestar_log::Count(status.as_u16().into()),
                url = lodestar_log::Url::new(&redact(&self.url))
            );
            checked
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::tests::ScriptedResolver;
    use crate::notify::{NotificationItem, NotificationKind, RetryPolicy, deliver};
    use crate::{AddrClass, NetError};
    use std::sync::{Arc, Mutex};

    type Headers = Vec<(String, String)>;

    #[derive(Default)]
    struct Seen {
        /// Path, headers and body of each request.
        requests: Vec<(String, Headers, Vec<u8>)>,
    }

    /// A receiver that answers with the scripted statuses in turn, then 200.
    async fn receiver(statuses: Vec<(u16, Option<&'static str>)>) -> (u16, Arc<Mutex<Seen>>) {
        use axum::body::{Body, to_bytes};
        use axum::extract::Request;
        let seen = Arc::new(Mutex::new(Seen::default()));
        let log = Arc::clone(&seen);
        let script = Arc::new(Mutex::new(statuses));
        let app = axum::Router::new().fallback(move |req: Request| {
            let log = Arc::clone(&log);
            let script = Arc::clone(&script);
            async move {
                let path = req.uri().to_string();
                let headers = req
                    .headers()
                    .iter()
                    .map(|(k, v)| (k.to_string(), v.to_str().unwrap_or("").to_owned()))
                    .collect();
                let body = to_bytes(req.into_body(), 1 << 20).await.unwrap().to_vec();
                log.lock().unwrap().requests.push((path, headers, body));
                let (status, location) = {
                    let mut s = script.lock().unwrap();
                    if s.is_empty() {
                        (200, None)
                    } else {
                        s.remove(0)
                    }
                };
                let mut resp = axum::response::Response::builder().status(status);
                if let Some(location) = location {
                    resp = resp.header("location", location);
                }
                if status == 429 {
                    resp = resp.header("retry-after", "0");
                }
                resp.body(Body::from("ignored")).unwrap()
            }
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (port, seen)
    }

    fn hook(url: &str) -> Webhook {
        let r = ScriptedResolver::new(&[
            ("hooks.test", &[&["127.0.0.1"]]),
            ("metadata.test", &[&["169.254.169.254"]]),
        ]);
        let http =
            GuardedClient::with_resolver(NetPolicy::loopback_for_tests(), WEBHOOK_TIMEOUT, r)
                .unwrap();
        Webhook::with_client(
            &WebhookConfig {
                url: Secret::new(url),
                allow_private: false,
            },
            http,
        )
        .unwrap()
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
                size_bytes: Some(42),
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
    async fn delivers_the_versioned_payload_without_private_items() {
        let (port, seen) = receiver(vec![]).await;
        let hook = hook(&format!("http://hooks.test:{port}/in/SECRET-TOKEN"));
        let n = alert();
        let out = deliver(&hook, &n, &FAST).await;
        assert_eq!(out.result, Ok(()));
        assert_eq!(out.attempts, 1);
        let seen = seen.lock().unwrap();
        let (path, headers, body) = &seen.requests[0];
        assert_eq!(path, "/in/SECRET-TOKEN");
        let header = |k: &str| {
            headers
                .iter()
                .find(|(h, _)| h == k)
                .map(|(_, v)| v.as_str())
        };
        assert_eq!(header("content-type"), Some("application/json"));
        assert_eq!(header("x-lodestar-delivery"), Some(n.id()));
        assert_eq!(header("x-lodestar-schema"), Some("1"));
        assert_eq!(header("user-agent"), Some(crate::USER_AGENT));
        let v: serde_json::Value = serde_json::from_slice(body).unwrap();
        assert_eq!(v["schema"], "lodestar.notification");
        assert_eq!(v["schema_version"], 1);
        assert_eq!(v["kind"], "alert");
        assert_eq!(v["id"], n.id());
        assert_eq!(v["link"], "http://lodestar.lan/alerts/7");
        assert_eq!(v["items"].as_array().unwrap().len(), 1);
        assert_eq!(v["items"][0]["name"], "Planet.Earth.III.2160p");
        assert_eq!(v["withheld_private"], 1);
        assert!(!String::from_utf8_lossy(body).contains("Private.Tracker"));
    }

    #[tokio::test]
    async fn retries_server_errors_with_the_same_id() {
        let (port, seen) = receiver(vec![(503, None), (429, None), (500, None)]).await;
        let hook = hook(&format!("http://hooks.test:{port}/in"));
        let n = alert();
        let out = deliver(&hook, &n, &FAST).await;
        assert_eq!(out.result, Ok(()));
        assert_eq!(out.attempts, 4);
        let seen = seen.lock().unwrap();
        let ids: Vec<_> = seen
            .requests
            .iter()
            .map(|(_, h, _)| {
                h.iter()
                    .find(|(k, _)| k == "x-lodestar-delivery")
                    .unwrap()
                    .1
                    .clone()
            })
            .collect();
        assert_eq!(ids, vec![n.id().to_owned(); 4]);
    }

    #[tokio::test]
    async fn client_errors_are_final() {
        let (port, seen) = receiver(vec![(404, None)]).await;
        let hook = hook(&format!("http://hooks.test:{port}/in"));
        let out = deliver(&hook, &alert(), &FAST).await;
        assert_eq!(out.result, Err(DeliveryError::Rejected { status: 404 }));
        assert_eq!(out.attempts, 1);
        assert_eq!(seen.lock().unwrap().requests.len(), 1);
    }

    #[tokio::test]
    async fn redirects_to_refused_addresses_are_not_followed() {
        let (port, seen) = receiver(vec![
            (307, Some("http://metadata.test/latest/meta-data/")),
            (308, Some("http://192.168.1.1/admin")),
        ])
        .await;
        let hook = hook(&format!("http://hooks.test:{port}/in"));
        let out = deliver(&hook, &alert(), &FAST).await;
        assert_eq!(out.result, Err(DeliveryError::Blocked(AddrClass::Metadata)));
        assert_eq!(out.attempts, 1, "a refused address isn't retried");
        let out = deliver(&hook, &alert(), &FAST).await;
        assert_eq!(out.result, Err(DeliveryError::Blocked(AddrClass::Private)));
        assert_eq!(seen.lock().unwrap().requests.len(), 2);
    }

    #[tokio::test]
    async fn a_followed_redirect_resends_the_payload() {
        let (port, seen) = receiver(vec![(308, Some("/moved"))]).await;
        let hook = hook(&format!("http://hooks.test:{port}/in"));
        let out = deliver(&hook, &alert(), &FAST).await;
        assert_eq!(out.result, Ok(()));
        let seen = seen.lock().unwrap();
        assert_eq!(seen.requests[1].0, "/moved");
        assert_eq!(seen.requests[0].2, seen.requests[1].2);
    }

    #[test]
    fn refused_endpoints_fail_when_saved() {
        for (url, private, want) in [
            (
                "http://127.0.0.1:9/x",
                true,
                DeliveryError::Blocked(AddrClass::Loopback),
            ),
            (
                "http://[::1]/x",
                true,
                DeliveryError::Blocked(AddrClass::Loopback),
            ),
            (
                "http://169.254.169.254/",
                true,
                DeliveryError::Blocked(AddrClass::Metadata),
            ),
            (
                "http://192.168.1.5/hook",
                false,
                DeliveryError::Blocked(AddrClass::Private),
            ),
        ] {
            let got = Webhook::new(&WebhookConfig {
                url: Secret::new(url),
                allow_private: private,
            })
            .unwrap_err();
            assert_eq!(got, want, "{url}");
        }
        assert!(
            Webhook::new(&WebhookConfig {
                url: Secret::new("http://192.168.1.5/hook"),
                allow_private: true,
            })
            .is_ok()
        );
        assert!(matches!(
            Webhook::new(&WebhookConfig {
                url: Secret::new("file:///etc/passwd"),
                allow_private: true,
            }),
            Err(DeliveryError::InvalidEndpoint(_))
        ));
    }

    #[tokio::test]
    async fn a_name_resolving_to_loopback_is_refused_on_the_real_policy() {
        let r = ScriptedResolver::new(&[("hooks.test", &[&["127.0.0.1"]])]);
        let http = GuardedClient::with_resolver(NetPolicy::notification(true), WEBHOOK_TIMEOUT, r)
            .unwrap();
        let (port, seen) = receiver(vec![]).await;
        let hook = Webhook::with_client(
            &WebhookConfig {
                url: Secret::new(format!("http://hooks.test:{port}/in")),
                allow_private: true,
            },
            http,
        )
        .unwrap();
        let out = deliver(&hook, &alert(), &FAST).await;
        assert_eq!(out.result, Err(DeliveryError::Blocked(AddrClass::Loopback)));
        assert_eq!(out.attempts, 1);
        assert!(seen.lock().unwrap().requests.is_empty());
    }

    #[tokio::test]
    async fn the_url_never_appears_in_debug_errors_or_logs() {
        let logs = crate::test_support::capture_logs();
        let r = ScriptedResolver::new(&[]);
        let http = GuardedClient::with_resolver(NetPolicy::notification(false), WEBHOOK_TIMEOUT, r)
            .unwrap();
        let config = WebhookConfig {
            url: Secret::new("https://hooks.example/api/webhooks/1/TOPSECRETTOKEN"),
            allow_private: false,
        };
        let hook = Webhook::with_client(&config, http).unwrap();
        let out = deliver(&hook, &alert(), &FAST).await;
        assert_eq!(out.result, Err(DeliveryError::Unreachable));
        let text = format!(
            "{hook:?} {config:?} {out:?} {}",
            out.result.as_ref().unwrap_err()
        );
        assert!(!text.contains("TOPSECRETTOKEN"), "{text}");
        assert!(text.contains("https://hooks.example/…"), "{text}");
        let logged = logs.contents();
        assert!(
            logged.contains("notification"),
            "the capture works: {logged}"
        );
        assert!(!logged.contains("TOPSECRETTOKEN"), "{logged}");
        let _ = NetError::Resolve;
    }
}
