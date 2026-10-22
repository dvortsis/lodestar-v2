//! Notifications (§8 alerts, §10.9.7 digest): webhook, ntfy, Discord,
//! email, RSS.
//!
//! A channel implements [`Notifier::attempt`]: one try, classified as done,
//! retryable or final. The shared [`deliver`] loop owns retries, backoff and
//! the [`DeliveryOutcome`] that feeds the Serve metrics, so every channel
//! behaves the same. [`webhook`] is the reference sender.
//!
//! **Private results stay local (§18.7).** Items found only through private
//! sources are never sent to a channel: [`Notification::shareable_items`]
//! leaves them out and senders say only how many were withheld.

pub mod discord;
pub mod email;
pub mod ntfy;
pub mod rss;
pub(crate) mod text;
pub mod webhook;

use std::fmt;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use reqwest::StatusCode;
use reqwest::header::{HeaderMap, RETRY_AFTER};

use crate::BoxFuture;
use crate::net::{AddrClass, NetError};

/// A notification channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Channel {
    Webhook,
    Ntfy,
    Discord,
    Email,
    Rss,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotificationKind {
    /// A saved search found new results (§8, §10.8).
    Alert,
    /// The daily or weekly Insights digest (§10.9.7).
    Digest,
    /// "Send a test notification" in Settings.
    Test,
}

impl NotificationKind {
    pub fn as_str(self) -> &'static str {
        match self {
            NotificationKind::Alert => "alert",
            NotificationKind::Digest => "digest",
            NotificationKind::Test => "test",
        }
    }
}

/// One torrent in a notification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotificationItem {
    pub name: String,
    /// Lowercase hex.
    pub info_hash: String,
    pub size_bytes: Option<u64>,
    /// Found only through a private source (§18.7): never sent.
    pub private_source: bool,
}

/// What to send. Its `id` stays the same across retries, so receivers can
/// drop duplicates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notification {
    id: String,
    pub kind: NotificationKind,
    pub title: String,
    /// Plain text.
    pub body: String,
    /// A link back into Lodestar (the alert's results, or Insights).
    pub link: Option<String>,
    pub items: Vec<NotificationItem>,
    /// Unix seconds.
    pub created_at: u64,
}

impl Notification {
    pub fn new(kind: NotificationKind, title: impl Into<String>, body: impl Into<String>) -> Self {
        let mut id = [0u8; 16];
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default();
        if getrandom::fill(&mut id).is_err() {
            // No randomness: the time still makes the ID unique enough to
            // deduplicate retries.
            id = now.as_nanos().to_le_bytes();
        }
        Notification {
            id: id.iter().map(|b| format!("{b:02x}")).collect(),
            kind,
            title: title.into(),
            body: body.into(),
            link: None,
            items: Vec::new(),
            created_at: now.as_secs(),
        }
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    /// The items a channel may send: everything not from a private source.
    pub fn shareable_items(&self) -> impl Iterator<Item = &NotificationItem> {
        self.items.iter().filter(|i| !i.private_source)
    }

    /// How many items were held back as private.
    pub fn withheld(&self) -> usize {
        self.items.iter().filter(|i| i.private_source).count()
    }
}

/// Why a delivery failed. No variant carries the endpoint URL, which may
/// hold a token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeliveryError {
    /// The endpoint is an address class this channel may not reach.
    Blocked(AddrClass),
    /// The endpoint URL is unusable.
    InvalidEndpoint(&'static str),
    /// The endpoint answered with a non-success status.
    Rejected {
        status: u16,
    },
    /// A mail server refused with this SMTP reply code (4xx retryable, 5xx
    /// final).
    SmtpRejected {
        code: u16,
    },
    Unreachable,
    Timeout,
    /// Anything else (an unsafe redirect, a payload that couldn't be built).
    Failed(&'static str),
}

impl fmt::Display for DeliveryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DeliveryError::Blocked(class) => write!(f, "{}", NetError::Blocked(*class)),
            DeliveryError::InvalidEndpoint(why) => write!(f, "invalid endpoint: {why}"),
            DeliveryError::Rejected { status } => {
                write!(f, "the endpoint answered HTTP {status}")
            }
            DeliveryError::SmtpRejected { code } => {
                write!(f, "the mail server answered {code}")
            }
            DeliveryError::Unreachable => f.write_str("couldn't reach the endpoint"),
            DeliveryError::Timeout => f.write_str("the endpoint didn't answer in time"),
            DeliveryError::Failed(why) => f.write_str(why),
        }
    }
}

impl std::error::Error for DeliveryError {}

/// The result of one attempt that didn't succeed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttemptError {
    pub error: DeliveryError,
    /// Whether trying again could help.
    pub retry: bool,
    /// The endpoint's `Retry-After`, when it gave one.
    pub retry_after: Option<Duration>,
}

impl AttemptError {
    pub fn fatal(error: DeliveryError) -> AttemptError {
        AttemptError {
            error,
            retry: false,
            retry_after: None,
        }
    }

    pub fn transient(error: DeliveryError) -> AttemptError {
        AttemptError {
            error,
            retry: true,
            retry_after: None,
        }
    }
}

impl From<NetError> for AttemptError {
    fn from(e: NetError) -> AttemptError {
        match e {
            NetError::Blocked(class) => AttemptError::fatal(DeliveryError::Blocked(class)),
            NetError::InvalidUrl(why) => AttemptError::fatal(DeliveryError::InvalidEndpoint(why)),
            NetError::Redirect => AttemptError::fatal(DeliveryError::Failed(
                "the endpoint redirected too often or unsafely",
            )),
            NetError::TooLarge => {
                AttemptError::fatal(DeliveryError::Failed("the endpoint's answer was too large"))
            }
            NetError::Timeout => AttemptError::transient(DeliveryError::Timeout),
            NetError::Resolve | NetError::Connect | NetError::Http => {
                AttemptError::transient(DeliveryError::Unreachable)
            }
        }
    }
}

/// Classifies an HTTP answer: 2xx is delivered; 408, 429 and 5xx are worth
/// retrying (honouring `Retry-After` in seconds); anything else is final.
pub fn check_status(status: StatusCode, headers: &HeaderMap) -> Result<(), AttemptError> {
    if status.is_success() {
        return Ok(());
    }
    let error = DeliveryError::Rejected {
        status: status.as_u16(),
    };
    let retry = status == StatusCode::REQUEST_TIMEOUT
        || status == StatusCode::TOO_MANY_REQUESTS
        || status.is_server_error();
    let retry_after = headers
        .get(RETRY_AFTER)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().parse::<u64>().ok())
        .map(Duration::from_secs);
    Err(AttemptError {
        error,
        retry,
        retry_after,
    })
}

/// A notification channel.
pub trait Notifier: Send + Sync + fmt::Debug {
    fn channel(&self) -> Channel;

    /// One attempt to deliver `n`. Retries belong to [`deliver`].
    fn attempt<'a>(&'a self, n: &'a Notification) -> BoxFuture<'a, Result<(), AttemptError>>;
}

/// How [`deliver`] retries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetryPolicy {
    /// Attempts in total, the first included (at least 1).
    pub max_attempts: u32,
    /// The wait before the second attempt; it doubles each time.
    pub base_delay: Duration,
    /// No wait is longer, `Retry-After` included.
    pub max_delay: Duration,
}

impl Default for RetryPolicy {
    /// Four attempts over about half a minute: 2 s, 4 s, 8 s (each with
    /// jitter), capped at a minute for `Retry-After`.
    fn default() -> RetryPolicy {
        RetryPolicy {
            max_attempts: 4,
            base_delay: Duration::from_secs(2),
            max_delay: Duration::from_secs(60),
        }
    }
}

impl RetryPolicy {
    /// The wait after attempt `attempt` (1-based) failed: exponential, with
    /// "equal jitter" (half fixed, half random) so many alerts failing at
    /// once don't retry in lockstep, and never less than `Retry-After`.
    pub fn delay(&self, attempt: u32, retry_after: Option<Duration>) -> Duration {
        let exp = self
            .base_delay
            .saturating_mul(1u32 << attempt.saturating_sub(1).min(16))
            .min(self.max_delay);
        let mut r = [0u8; 4];
        let fraction = match getrandom::fill(&mut r) {
            Ok(()) => f64::from(u32::from_le_bytes(r)) / f64::from(u32::MAX),
            Err(_) => 0.5,
        };
        let jittered = exp / 2 + exp.mul_f64(fraction / 2.0);
        match retry_after {
            Some(after) => jittered.max(after.min(self.max_delay)),
            None => jittered,
        }
    }
}

/// What happened to one notification on one channel, for the Serve stage
/// ("notification delivery outcomes", §10.9.5) and the alert's history.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeliveryOutcome {
    pub channel: Channel,
    pub result: Result<(), DeliveryError>,
    pub attempts: u32,
    pub elapsed: Duration,
}

/// Delivers `n` through `notifier`, retrying transient failures per
/// `policy`.
pub async fn deliver(
    notifier: &dyn Notifier,
    n: &Notification,
    policy: &RetryPolicy,
) -> DeliveryOutcome {
    let start = tokio::time::Instant::now();
    let max = policy.max_attempts.max(1);
    let mut attempts = 0;
    let result = loop {
        attempts += 1;
        match notifier.attempt(n).await {
            Ok(()) => break Ok(()),
            Err(e) if e.retry && attempts < max => {
                let wait = policy.delay(attempts, e.retry_after);
                tracing::debug!(
                    channel = ?notifier.channel(),
                    attempt = attempts,
                    error = %e.error,
                    wait_ms = wait.as_millis() as u64,
                    "notification attempt failed; retrying"
                );
                tokio::time::sleep(wait).await;
            }
            Err(e) => break Err(e.error),
        }
    };
    if let Err(e) = &result {
        tracing::info!(channel = ?notifier.channel(), attempts, error = %e, "notification not delivered");
    }
    DeliveryOutcome {
        channel: notifier.channel(),
        result,
        attempts,
        elapsed: start.elapsed(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[derive(Debug)]
    struct Scripted(Mutex<Vec<Result<(), AttemptError>>>);

    impl Notifier for Scripted {
        fn channel(&self) -> Channel {
            Channel::Ntfy
        }

        fn attempt<'a>(&'a self, _: &'a Notification) -> BoxFuture<'a, Result<(), AttemptError>> {
            let next = self.0.lock().unwrap().remove(0);
            Box::pin(async move { next })
        }
    }

    const FAST: RetryPolicy = RetryPolicy {
        max_attempts: 3,
        base_delay: Duration::from_millis(1),
        max_delay: Duration::from_millis(5),
    };

    fn n() -> Notification {
        Notification::new(NotificationKind::Test, "t", "b")
    }

    #[tokio::test]
    async fn retries_transient_failures_then_succeeds() {
        let s = Scripted(Mutex::new(vec![
            Err(AttemptError::transient(DeliveryError::Timeout)),
            Err(AttemptError::transient(DeliveryError::Unreachable)),
            Ok(()),
        ]));
        let out = deliver(&s, &n(), &FAST).await;
        assert_eq!(out.result, Ok(()));
        assert_eq!(out.attempts, 3);
        assert_eq!(out.channel, Channel::Ntfy);
    }

    #[tokio::test]
    async fn stops_at_the_attempt_limit_and_on_fatal_errors() {
        let s = Scripted(Mutex::new(vec![
            Err(AttemptError::transient(DeliveryError::Timeout)),
            Err(AttemptError::transient(DeliveryError::Timeout)),
            Err(AttemptError::transient(DeliveryError::Unreachable)),
            Ok(()),
        ]));
        let out = deliver(&s, &n(), &FAST).await;
        assert_eq!(out.result, Err(DeliveryError::Unreachable));
        assert_eq!(out.attempts, 3);

        let s = Scripted(Mutex::new(vec![Err(AttemptError::fatal(
            DeliveryError::Rejected { status: 404 },
        ))]));
        let out = deliver(&s, &n(), &FAST).await;
        assert_eq!(out.result, Err(DeliveryError::Rejected { status: 404 }));
        assert_eq!(out.attempts, 1);
    }

    #[test]
    fn backoff_doubles_with_jitter_and_honours_retry_after() {
        let p = RetryPolicy::default();
        for _ in 0..50 {
            let d1 = p.delay(1, None);
            assert!(d1 >= Duration::from_secs(1) && d1 <= Duration::from_secs(2));
            let d3 = p.delay(3, None);
            assert!(d3 >= Duration::from_secs(4) && d3 <= Duration::from_secs(8));
            let d20 = p.delay(20, None);
            assert!(d20 <= p.max_delay);
            assert!(p.delay(1, Some(Duration::from_secs(30))) >= Duration::from_secs(30));
            assert!(p.delay(1, Some(Duration::from_secs(3600))) <= p.max_delay);
        }
    }

    #[test]
    fn statuses() {
        let h = HeaderMap::new();
        assert!(check_status(StatusCode::OK, &h).is_ok());
        assert!(check_status(StatusCode::NO_CONTENT, &h).is_ok());
        for (status, retry) in [
            (400, false),
            (401, false),
            (404, false),
            (408, true),
            (410, false),
            (429, true),
            (500, true),
            (503, true),
        ] {
            let e = check_status(StatusCode::from_u16(status).unwrap(), &h).unwrap_err();
            assert_eq!(e.retry, retry, "{status}");
            assert_eq!(e.error, DeliveryError::Rejected { status });
        }
        let mut h = HeaderMap::new();
        h.insert(RETRY_AFTER, "7".parse().unwrap());
        let e = check_status(StatusCode::TOO_MANY_REQUESTS, &h).unwrap_err();
        assert_eq!(e.retry_after, Some(Duration::from_secs(7)));
    }

    #[test]
    fn private_items_are_withheld() {
        let mut n = n();
        n.items = vec![
            NotificationItem {
                name: "Public".into(),
                info_hash: "a".repeat(40),
                size_bytes: Some(1),
                private_source: false,
            },
            NotificationItem {
                name: "Private".into(),
                info_hash: "b".repeat(40),
                size_bytes: None,
                private_source: true,
            },
        ];
        let names: Vec<_> = n.shareable_items().map(|i| i.name.as_str()).collect();
        assert_eq!(names, ["Public"]);
        assert_eq!(n.withheld(), 1);
        assert_eq!(n.id().len(), 32);
        assert_ne!(
            n.id(),
            Notification::new(NotificationKind::Test, "", "").id()
        );
    }

    #[test]
    fn net_errors_map_to_retry_decisions() {
        assert!(!AttemptError::from(NetError::Blocked(AddrClass::Loopback)).retry);
        assert!(!AttemptError::from(NetError::InvalidUrl("x")).retry);
        assert!(!AttemptError::from(NetError::Redirect).retry);
        assert!(AttemptError::from(NetError::Timeout).retry);
        assert!(AttemptError::from(NetError::Connect).retry);
        assert!(AttemptError::from(NetError::Resolve).retry);
    }
}
