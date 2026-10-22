//! The email sender through the crate's public API. The SMTP exchanges
//! (STARTTLS, implicit TLS, AUTH, retries, header injection on the wire)
//! are unit tests in `src/notify/email.rs`, because the notification policy
//! never allows loopback, so a local test server can only be reached with
//! the crate-private test policy. Nothing here touches the network: the
//! resolver answers with addresses the policy refuses.

use std::io;
use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;

use lodestar_integrations::notify::email::{
    Email, EmailConfig, MAX_MESSAGE_BYTES, SmtpCredentials, SmtpSecurity,
};
use lodestar_integrations::{
    AddrClass, BoxFuture, Channel, DeliveryError, Notification, NotificationItem, NotificationKind,
    Notifier, Resolve, RetryPolicy, Secret, deliver,
};

const USER: &str = "USERSECRETname";
const PASS: &str = "PASSSECRETword";

struct Fixed(IpAddr);

impl Resolve for Fixed {
    fn resolve<'a>(&'a self, _: &'a str) -> BoxFuture<'a, io::Result<Vec<IpAddr>>> {
        Box::pin(async move { Ok(vec![self.0]) })
    }
}

fn config() -> EmailConfig {
    let mut c = EmailConfig::new(
        "smtp.example.com",
        SmtpSecurity::StartTls,
        "lodestar@example.com",
        vec!["me@example.com".into()],
    );
    c.credentials = Some(SmtpCredentials {
        username: Secret::new(USER),
        password: Secret::new(PASS),
    });
    c
}

fn sender(config: &EmailConfig, ip: &str) -> Email {
    Email::with_resolver(config, Arc::new(Fixed(ip.parse().unwrap()))).unwrap()
}

fn alert() -> Notification {
    let mut n = Notification::new(NotificationKind::Alert, "New results", "Two new results.");
    n.items = vec![
        NotificationItem {
            name: "Public.Release\r\nBcc: attacker@example.com".into(),
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

#[test]
fn defaults_and_ports() {
    assert_eq!(SmtpSecurity::Tls.default_port(), 465);
    assert_eq!(SmtpSecurity::StartTls.default_port(), 587);
    assert_eq!(SmtpSecurity::None.default_port(), 25);
    let c = config();
    assert_eq!(c.port, 587);
    assert!(!c.allow_private);
    assert_eq!(sender(&c, "93.184.215.14").channel(), Channel::Email);
}

#[test]
fn refused_addresses_fail_when_saved() {
    for (host, private, want) in [
        ("127.0.0.1", true, AddrClass::Loopback),
        ("[::1]", true, AddrClass::Loopback),
        ("169.254.169.254", true, AddrClass::Metadata),
        ("10.1.2.3", false, AddrClass::Private),
    ] {
        let mut c = config();
        c.host = host.into();
        c.allow_private = private;
        assert_eq!(
            Email::new(&c).unwrap_err(),
            DeliveryError::Blocked(want),
            "{host}"
        );
    }
    let mut c = config();
    c.from = "lodestar".into();
    assert!(matches!(
        Email::new(&c),
        Err(DeliveryError::InvalidEndpoint(_))
    ));
    let mut c = config();
    c.security = SmtpSecurity::None;
    assert!(matches!(
        Email::new(&c),
        Err(DeliveryError::InvalidEndpoint(_))
    ));
}

#[tokio::test]
async fn a_name_resolving_to_a_refused_address_is_never_connected() {
    for (ip, want) in [
        ("169.254.169.254", AddrClass::Metadata),
        ("127.0.0.1", AddrClass::Loopback),
        ("10.0.0.1", AddrClass::Private),
    ] {
        for security in [SmtpSecurity::StartTls, SmtpSecurity::Tls] {
            let mut c = config();
            c.security = security;
            let out = deliver(&sender(&c, ip), &alert(), &FAST).await;
            assert_eq!(out.result, Err(DeliveryError::Blocked(want)), "{ip}");
            assert_eq!(out.attempts, 1, "a refusal is final");
        }
    }
}

#[test]
fn the_message_keeps_private_items_and_injected_headers_out() {
    let n = alert();
    let raw = sender(&config(), "93.184.215.14").message(&n).unwrap();
    assert!(raw.len() <= MAX_MESSAGE_BYTES);
    let text = String::from_utf8(raw).unwrap();
    assert!(text.is_ascii());
    let headers = text.split("\r\n\r\n").next().unwrap();
    assert!(
        !headers
            .lines()
            .any(|l| l.to_ascii_lowercase().starts_with("bcc"))
    );
    assert!(headers.contains(&format!("Message-ID: <{}.lodestar@example.com>", n.id())));
    assert!(headers.contains("Content-Type: multipart/alternative"));
    assert!(!text.contains("Private.Tracker"));
    // The same notification gives the same Message-ID every time.
    let again = sender(&config(), "93.184.215.14").message(&n).unwrap();
    let id = |m: &str| {
        m.lines()
            .find(|l| l.starts_with("Message-ID"))
            .unwrap()
            .to_owned()
    };
    assert_eq!(id(&text), id(&String::from_utf8(again).unwrap()));
}

#[test]
fn credentials_never_appear_in_debug_or_errors() {
    let c = config();
    let s = sender(&c, "93.184.215.14");
    let boxed: Box<dyn Notifier> = Box::new(sender(&c, "93.184.215.14"));
    let mut bad = config();
    bad.security = SmtpSecurity::None;
    let err = Email::new(&bad).unwrap_err();
    let text = format!("{s:?} {c:?} {boxed:?} {err} {err:?}");
    for secret in [USER, PASS] {
        assert!(!text.contains(secret), "{text}");
    }
    assert!(text.contains("smtp.example.com"));
}
