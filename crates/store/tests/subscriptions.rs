//! Blocklist subscriptions (§20.13).

use lodestar_store::{NewSubscription, Refresh, Store, StoreConfig};

fn new<'a>(url: &'a str) -> NewSubscription<'a> {
    NewSubscription {
        url,
        name: "List",
        kind: 1,
        format: Some("p2p"),
        action: 4,
        floor: false,
        interval_secs: 86_400,
    }
}

#[test]
fn lifecycle() {
    let mut s = Store::open_in_memory(StoreConfig::default()).unwrap();
    let id = s.add_subscription(&new("https://a/list.gz")).unwrap();
    assert_eq!(s.add_subscription(&new("https://a/list.gz")).unwrap(), id);
    let other = s.add_subscription(&new("https://b/list")).unwrap();
    let sub = s.subscription(id).unwrap().unwrap();
    assert_eq!(
        (sub.status.as_str(), sub.entries, sub.enabled),
        ("new", 0, true)
    );

    s.record_refresh(
        id,
        100,
        &Refresh::Updated {
            data: b"v1",
            entries: 10,
            etag: Some("\"e1\""),
            last_modified: None,
        },
    )
    .unwrap();
    let sub = s.subscription(id).unwrap().unwrap();
    assert_eq!((sub.status.as_str(), sub.entries), ("ok", 10));
    assert_eq!(sub.etag.as_deref(), Some("\"e1\""));
    assert_eq!(sub.updated_at, Some(100));

    // A suspicious version waits; the applied one stays.
    s.record_refresh(
        id,
        200,
        &Refresh::Held {
            data: b"v2",
            entries: 1,
            etag: Some("\"e2\""),
            last_modified: None,
            why: "shrank by 90%",
        },
    )
    .unwrap();
    s.record_refresh(id, 250, &Refresh::Unchanged).unwrap();
    let sub = s.subscription(id).unwrap().unwrap();
    assert_eq!((sub.status.as_str(), sub.entries), ("held", 10));
    assert_eq!(s.subscription_data(id, false).unwrap().unwrap(), b"v1");
    assert_eq!(s.subscription_data(id, true).unwrap().unwrap(), b"v2");
    assert!(s.resolve_held(id, true, 300).unwrap());
    assert!(!s.resolve_held(id, true, 300).unwrap());
    let sub = s.subscription(id).unwrap().unwrap();
    assert_eq!((sub.status.as_str(), sub.entries), ("ok", 1));
    assert_eq!(s.subscription_data(id, false).unwrap().unwrap(), b"v2");
    assert!(s.subscription_data(id, true).unwrap().is_none());

    s.record_refresh(other, 300, &Refresh::Failed("HTTP 404"))
        .unwrap();
    let sub = s.subscription(other).unwrap().unwrap();
    assert_eq!(
        (sub.status.as_str(), sub.error.as_deref()),
        ("error", Some("HTTP 404"))
    );

    assert!(s.set_subscription_enabled(id, false).unwrap());
    assert!(!s.subscription(id).unwrap().unwrap().enabled);
    assert!(s.remove_subscription(id).unwrap());
    assert!(s.subscription_data(id, false).unwrap().is_none());
    assert_eq!(s.subscriptions().unwrap().len(), 1);
}
