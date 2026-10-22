//! The RSS renderer through the crate's public API: it is the security
//! boundary for attacker-controlled torrent names, and it must never carry
//! a private-source item. The golden output and the property tests are in
//! `src/notify/rss.rs`.

use lodestar_integrations::notify::rss::{
    RssChannel, RssError, RssItem, items_from_notification, render,
};
use lodestar_integrations::{Notification, NotificationItem, NotificationKind};

fn channel() -> RssChannel {
    RssChannel {
        title: "Lodestar: saved search".into(),
        link: "http://lodestar.lan/searches/7".into(),
        description: "New results".into(),
        built_at: 1_761_134_400,
    }
}

#[test]
fn a_feed_built_from_a_notification_has_only_shareable_items() {
    let mut n = Notification::new(NotificationKind::Alert, "Two new", "body");
    n.link = Some("http://lodestar.lan/alerts/7".into());
    n.items = vec![
        NotificationItem {
            name: "Public.<b>One</b> & co".into(),
            info_hash: "a".repeat(40),
            size_bytes: Some(2048),
            private_source: false,
        },
        NotificationItem {
            name: "Private.Tracker.Exclusive".into(),
            info_hash: "b".repeat(40),
            size_bytes: Some(1),
            private_source: true,
        },
    ];
    let xml = render(&channel(), &items_from_notification(&n)).unwrap();
    assert_eq!(xml.matches("<item>").count(), 1);
    assert!(xml.contains("Public.&lt;b&gt;One&lt;/b&gt; &amp; co"));
    assert!(xml.contains(&format!(
        "<guid isPermaLink=\"false\">{}</guid>",
        "a".repeat(40)
    )));
    assert!(!xml.contains("Private.Tracker") && !xml.contains(&"b".repeat(40)));
    assert!(!xml.contains("<b>") && !xml.contains("CDATA"));
}

#[test]
fn markup_in_every_field_is_escaped() {
    let hostile = "</title><script>alert(1)</script>]]><!-- & \" '";
    let c = RssChannel {
        title: hostile.into(),
        description: hostile.into(),
        ..channel()
    };
    let item = RssItem {
        title: hostile.into(),
        link: Some("javascript:alert(1)".into()),
        guid: hostile.into(),
        published: 0,
        description: hostile.into(),
        private_source: false,
    };
    let xml = render(&c, &[item]).unwrap();
    assert!(!xml.contains("<script"), "{xml}");
    assert!(!xml.contains("javascript:"));
    assert!(!xml.contains("<!--"));
    // Title (channel and item), guid and channel link-free fields escape once;
    // descriptions twice, for readers that decode them as HTML.
    assert_eq!(xml.matches("&lt;/title&gt;&lt;script&gt;").count(), 3);
    assert_eq!(
        xml.matches("&amp;lt;/title&amp;gt;&amp;lt;script&amp;gt;")
            .count(),
        2
    );
}

#[test]
fn a_feed_needs_an_http_link() {
    let mut c = channel();
    c.link = "javascript:alert(1)".into();
    assert_eq!(render(&c, &[]), Err(RssError::InvalidLink));
}

#[test]
fn a_caller_cant_leak_a_private_item_through_the_renderer() {
    let item = |name: &str, private| RssItem {
        title: name.into(),
        link: None,
        guid: name.into(),
        published: 0,
        description: String::new(),
        private_source: private,
    };
    let xml = render(
        &channel(),
        &[item("Public", false), item("Secret.Tracker", true)],
    )
    .unwrap();
    assert!(xml.contains("Public") && !xml.contains("Secret.Tracker"));
}
