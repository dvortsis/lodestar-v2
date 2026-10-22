//! The Discord sender through the crate's public API. The wire exchanges
//! (fixtures, rate limits, retries) are unit tests in
//! `src/notify/discord.rs`, because the notification policy never allows
//! loopback, so a local receiver can only be reached with the crate-private
//! test policy. Nothing here touches the network.

use std::io;
use std::net::IpAddr;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use lodestar_integrations::notify::discord::{
    Discord, DiscordConfig, MAX_DESCRIPTION, MAX_EMBED_TOTAL, MAX_TITLE,
};
use lodestar_integrations::{
    AddrClass, BoxFuture, DeliveryError, GuardedClient, NetPolicy, Notification, NotificationItem,
    NotificationKind, Notifier, Resolve, RetryPolicy, Secret, deliver,
};

const ID: &str = "123456789012345678";
const TOKEN: &str = "Tk3n-SECRET_value";

struct Fixed(IpAddr);

impl Resolve for Fixed {
    fn resolve<'a>(&'a self, _: &'a str) -> BoxFuture<'a, io::Result<Vec<IpAddr>>> {
        Box::pin(async move { Ok(vec![self.0]) })
    }
}

fn client(ip: &str) -> GuardedClient {
    GuardedClient::with_resolver(
        NetPolicy::notification(false),
        Duration::from_secs(5),
        Arc::new(Fixed(ip.parse().unwrap())),
    )
    .unwrap()
}

fn config(host: &str) -> DiscordConfig {
    DiscordConfig {
        webhook_url: Secret::new(format!("https://{host}/api/webhooks/{ID}/{TOKEN}")),
    }
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
            size_bytes: None,
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
        buf
    }))
}

#[test]
fn the_payload_withholds_private_items_and_allows_no_mentions() {
    let p = Discord::payload(&alert());
    assert_eq!(p["allowed_mentions"]["parse"], serde_json::json!([]));
    let d = p["embeds"][0]["description"].as_str().unwrap();
    assert!(d.contains("Public.Release"));
    assert!(!d.contains("Private.Tracker") && !d.contains("bbbb"));
    assert!(d.contains("1 result from a private source isn't included."));
}

#[test]
fn a_hostile_name_cant_ping_or_link() {
    let mut n = alert();
    n.items[0].name = "@everyone @here <@&99> [free](https://evil.example)".into();
    let p = Discord::payload(&n);
    assert_eq!(p["allowed_mentions"], serde_json::json!({"parse": []}));
    let d = p["embeds"][0]["description"].as_str().unwrap();
    assert!(!d.contains("<@") && !d.contains("](https"), "{d}");
}

#[test]
fn embeds_stay_within_discords_limits() {
    let mut n = Notification::new(
        NotificationKind::Digest,
        "T".repeat(999),
        "x".repeat(99_999),
    );
    n.items = (0..60)
        .map(|i| NotificationItem {
            name: "y".repeat(300),
            info_hash: format!("{i:040x}"),
            size_bytes: None,
            private_source: false,
        })
        .collect();
    let p = Discord::payload(&n);
    let e = &p["embeds"][0];
    let t = e["title"].as_str().unwrap().encode_utf16().count();
    let d = e["description"].as_str().unwrap().encode_utf16().count();
    assert!(t <= MAX_TITLE && d <= MAX_DESCRIPTION && t + d <= MAX_EMBED_TOTAL);
    assert!(e["description"].as_str().unwrap().ends_with('…'));
}

#[test]
fn only_discord_hosts_are_accepted_and_never_the_private_network() {
    for host in [
        "discord.com",
        "discordapp.com",
        "canary.discord.com",
        "ptb.discord.com",
        "canary.discordapp.com",
        "ptb.discordapp.com",
    ] {
        assert!(Discord::new(&config(host)).is_ok(), "{host}");
    }
    for host in [
        "evil.example",
        "discord.com.evil.example",
        "notdiscord.com",
        "discord.com@evil.example",
    ] {
        assert!(
            matches!(
                Discord::new(&config(host)),
                Err(DeliveryError::InvalidEndpoint(_))
            ),
            "{host}"
        );
    }
    for url in [
        format!("https://127.0.0.1/api/webhooks/{ID}/{TOKEN}"),
        format!("https://192.168.1.5/api/webhooks/{ID}/{TOKEN}"),
        format!("https://169.254.169.254/api/webhooks/{ID}/{TOKEN}"),
    ] {
        assert!(
            matches!(
                Discord::new(&DiscordConfig {
                    webhook_url: Secret::new(url.clone())
                }),
                Err(DeliveryError::Blocked(_))
            ),
            "{url}"
        );
    }
}

#[tokio::test]
async fn a_name_resolving_to_a_refused_address_is_never_contacted() {
    for (ip, want) in [
        ("10.0.0.5", AddrClass::Private),
        ("127.0.0.1", AddrClass::Loopback),
        ("169.254.169.254", AddrClass::Metadata),
    ] {
        let sender = Discord::with_client(&config("discord.com"), client(ip)).unwrap();
        let out = deliver(&sender, &alert(), &FAST).await;
        assert_eq!(out.result, Err(DeliveryError::Blocked(want)), "{ip}");
        assert_eq!(out.attempts, 1);
    }
}

#[tokio::test]
async fn no_token_in_logs_debug_or_errors() {
    let logs = capture_logs();
    let cfg = config("discord.com");
    let sender = Discord::with_client(&cfg, client("10.0.0.5")).unwrap();
    let out = deliver(&sender, &alert(), &FAST).await;
    let boxed: Box<dyn Notifier> = Box::new(sender);
    let refused = Discord::new(&config("evil.example")).unwrap_err();
    let text = format!(
        "{cfg:?} {boxed:?} {out:?} {} {refused:?} {refused}",
        out.result.as_ref().unwrap_err()
    );
    let logged = String::from_utf8_lossy(&logs.lock().unwrap()).into_owned();
    assert!(
        logged.contains("notification not delivered"),
        "capture works: {logged}"
    );
    for secret in [TOKEN, ID] {
        assert!(!text.contains(secret), "{secret} in {text}");
        assert!(!logged.contains(secret), "{secret} in the logs: {logged}");
    }
    assert!(text.contains("https://discord.com/…"));
}
