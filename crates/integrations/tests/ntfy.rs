//! The ntfy sender through the crate's public API. The wire exchanges
//! (fixtures, retries, redirects) are unit tests in `src/notify/ntfy.rs`,
//! because the notification policy never allows loopback, so a local
//! receiver can only be reached with the crate-private test policy. Nothing
//! here touches the network: the resolver answers with addresses the policy
//! refuses.

use std::io;
use std::net::IpAddr;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use lodestar_integrations::notify::ntfy::{MAX_MESSAGE_BYTES, Ntfy, NtfyAuth, NtfyConfig};
use lodestar_integrations::{
    AddrClass, BoxFuture, DeliveryError, GuardedClient, NetPolicy, Notification, NotificationItem,
    NotificationKind, Notifier, Resolve, RetryPolicy, Secret, deliver,
};

const TOPIC: &str = "TOPICSECRET-ab12cd";
const TOKEN: &str = "tk_TOKENSECRET9";
const USER: &str = "USERSECRETname";
const PASS: &str = "PASSSECRETword";

struct Fixed(IpAddr);

impl Resolve for Fixed {
    fn resolve<'a>(&'a self, _: &'a str) -> BoxFuture<'a, io::Result<Vec<IpAddr>>> {
        Box::pin(async move { Ok(vec![self.0]) })
    }
}

fn client(ip: &str, allow_private: bool) -> GuardedClient {
    GuardedClient::with_resolver(
        NetPolicy::notification(allow_private),
        Duration::from_secs(5),
        Arc::new(Fixed(ip.parse().unwrap())),
    )
    .unwrap()
}

fn alert() -> Notification {
    let mut n = Notification::new(NotificationKind::Alert, "New results", "Two new results.");
    n.items = vec![
        NotificationItem {
            name: "Public.Release".into(),
            info_hash: "a".repeat(40),
            size_bytes: Some(1_048_576),
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
    max_attempts: 3,
    base_delay: Duration::from_millis(1),
    max_delay: Duration::from_millis(5),
};

fn capture_logs() -> Arc<Mutex<Vec<u8>>> {
    struct W(Arc<Mutex<Vec<u8>>>);
    impl io::Write for W {
        fn write(&mut self, b: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    static BUF: OnceLock<Arc<Mutex<Vec<u8>>>> = OnceLock::new();
    Arc::clone(BUF.get_or_init(|| {
        let buf = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&buf);
        let subscriber = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::TRACE)
            .with_writer(move || W(Arc::clone(&sink)))
            .finish();
        tracing::subscriber::set_global_default(subscriber).unwrap();
        // Every event on, at every level, so a secret in any of them fails.
        for e in lodestar_integrations::log_events::CATALOGUE.events {
            e.set_on(true);
        }
        buf
    }))
}

#[test]
fn private_items_are_withheld_and_counted_in_the_message() {
    let m = Ntfy::message(&alert());
    assert!(m.contains("Public.Release (1.00\u{a0}MiB)"));
    assert!(!m.contains("Private.Tracker") && !m.contains("bbbb"));
    assert!(m.contains("1 result from a private source isn't included."));
}

#[test]
fn the_message_never_exceeds_ntfys_limit() {
    let mut n = Notification::new(NotificationKind::Digest, "d", "日本語".repeat(5000));
    n.items = (0..100)
        .map(|i| NotificationItem {
            name: format!("🎬 {i} ").repeat(30),
            info_hash: format!("{i:040x}"),
            size_bytes: Some(i),
            private_source: false,
        })
        .collect();
    let m = Ntfy::message(&n);
    assert!(m.len() <= MAX_MESSAGE_BYTES);
    assert!(m.contains('…') && m.ends_with("…and 80 more"), "{m}");
    // Valid UTF-8 by construction; the cut is at a character boundary.
    assert!(std::str::from_utf8(m.as_bytes()).is_ok());
}

#[test]
fn refused_endpoints_fail_when_the_channel_is_saved() {
    for (url, private, want) in [
        ("http://127.0.0.1:9/t", true, AddrClass::Loopback),
        ("http://[::1]/t", true, AddrClass::Loopback),
        ("http://169.254.169.254/t", true, AddrClass::Metadata),
        ("http://192.168.1.5/t", false, AddrClass::Private),
    ] {
        let mut config = NtfyConfig::new(Secret::new(url));
        config.allow_private = private;
        assert_eq!(
            Ntfy::new(&config).unwrap_err(),
            DeliveryError::Blocked(want),
            "{url}"
        );
    }
    let mut config = NtfyConfig::new(Secret::new("http://192.168.1.5/t"));
    config.allow_private = true;
    assert!(
        Ntfy::new(&config).is_ok(),
        "a LAN server, when the user opts in"
    );
    assert!(Ntfy::new(&NtfyConfig::new(Secret::new("https://ntfy.sh/my-topic"))).is_ok());
}

#[tokio::test]
async fn a_name_resolving_to_a_refused_address_is_never_contacted() {
    for (ip, private, want) in [
        ("10.0.0.5", false, AddrClass::Private),
        ("127.0.0.1", true, AddrClass::Loopback),
        ("169.254.169.254", true, AddrClass::Metadata),
    ] {
        let mut config = NtfyConfig::new(Secret::new("https://ntfy.example/topic"));
        config.allow_private = private;
        let ntfy = Ntfy::with_client(&config, client(ip, private)).unwrap();
        let out = deliver(&ntfy, &alert(), &FAST).await;
        assert_eq!(out.result, Err(DeliveryError::Blocked(want)), "{ip}");
        assert_eq!(out.attempts, 1, "a refused address isn't retried");
    }
}

#[tokio::test]
async fn no_secret_in_logs_debug_or_errors() {
    let logs = capture_logs();
    let mut text = String::new();
    for auth in [
        NtfyAuth::None,
        NtfyAuth::Bearer(Secret::new(TOKEN)),
        NtfyAuth::Basic {
            username: Secret::new(USER),
            password: Secret::new(PASS),
        },
    ] {
        let mut config = NtfyConfig::new(Secret::new(format!("https://ntfy.example/{TOPIC}")));
        config.auth = auth;
        let ntfy = Ntfy::with_client(&config, client("10.0.0.5", false)).unwrap();
        let out = deliver(&ntfy, &alert(), &FAST).await;
        let boxed: Box<dyn Notifier> = Box::new(ntfy);
        text.push_str(&format!("{config:?} {boxed:?} {out:?} "));
        text.push_str(&out.result.unwrap_err().to_string());
    }
    let refused = Ntfy::new(&NtfyConfig::new(Secret::new(format!(
        "http://192.168.1.5/{TOPIC}"
    ))))
    .unwrap_err();
    text.push_str(&format!("{refused:?} {refused}"));

    let logged = String::from_utf8_lossy(&logs.lock().unwrap()).into_owned();
    assert!(
        logged.contains("notification not delivered"),
        "capture works: {logged}"
    );
    for secret in [TOPIC, TOKEN, USER, PASS] {
        assert!(!text.contains(secret), "{secret} in {text}");
        assert!(!logged.contains(secret), "{secret} in the logs: {logged}");
    }
    assert!(text.contains("https://ntfy.example/…"));
}

#[test]
fn the_withheld_count_survives_an_overflowing_message() {
    let mut n = Notification::new(NotificationKind::Digest, "d", "b".repeat(300));
    n.items = (0..22)
        .map(|i| NotificationItem {
            name: "n".repeat(280),
            info_hash: format!("{i:040x}"),
            size_bytes: None,
            private_source: i >= 20,
        })
        .collect();
    let m = Ntfy::message(&n);
    assert!(m.len() <= MAX_MESSAGE_BYTES);
    assert!(
        m.ends_with("2 results from private sources aren't included."),
        "{m}"
    );
}
