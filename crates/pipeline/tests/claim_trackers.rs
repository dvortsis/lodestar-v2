//! Claimed trackers from feeds and plugins pass the shared host guard
//! before storage (X-claim-trackers): none on a literal LAN, loopback or
//! link-local address or a local name. What's stored is the guard's
//! canonical form, the address it judged, once each (X-claim-store).

use lodestar_metainfo::InfoHash;
use lodestar_pipeline::unverified::{store_claims, stored_trackers, tracker_url};
use lodestar_sources::{Discovery, Found, claims};
use lodestar_store::SourceId;
use lodestar_store::claims::MAX_TRACKERS;

fn kept(raw: &str) -> Option<String> {
    let k = tracker_url(raw);
    if let Some(k) = &k {
        // A fixed point: the stored form maps to itself.
        assert_eq!(tracker_url(k).as_deref(), Some(k.as_str()), "{raw} → {k}");
    }
    k
}

#[test]
fn claimed_trackers_on_public_hosts_are_kept() {
    for ok in [
        "udp://tracker.example:1337/announce",
        "https://tracker.example/announce",
        "http://tracker.example:6969/announce",
        "https://t.example/a?passkey=x",
        "http://93.184.215.14:6969/announce",
        "udp://[2a00:1450:4001::1]:6969/announce",
    ] {
        assert_eq!(kept(ok).as_deref(), Some(ok), "{ok}");
    }
}

/// A spelling the `url` crate rewrites is kept in the rewritten form: the
/// default port dropped, the scheme and host lower-cased, a `udp` host
/// decoded and lower-cased as it was judged.
#[test]
fn claimed_trackers_are_kept_in_canonical_form() {
    for (raw, want) in [
        (
            "https://t.example:443/announce",
            "https://t.example/announce",
        ),
        ("http://t.example:80/announce", "http://t.example/announce"),
        (
            "HTTP://Tracker.Example/announce",
            "http://tracker.example/announce",
        ),
        ("http://tracker.example", "http://tracker.example/"),
        ("UDP://T.Example:6969/a", "udp://t.example:6969/a"),
        (
            "udp://T.EXAMPLE:6969/announce",
            "udp://t.example:6969/announce",
        ),
        (
            "udp://%74.example:6969/announce",
            "udp://t.example:6969/announce",
        ),
        (
            "udp://tracker.example.:6969/a",
            "udp://tracker.example.:6969/a",
        ),
        (
            "https://bücher.example/a",
            "https://xn--bcher-kva.example/a",
        ),
    ] {
        assert_eq!(kept(raw).as_deref(), Some(want), "{raw}");
    }
}

#[test]
fn claimed_trackers_that_reach_the_users_network_are_dropped() {
    for bad in [
        // Private, loopback, link-local, shared and mapped addresses.
        "udp://192.168.1.1:80/announce",
        "http://10.0.0.1/announce",
        "http://127.0.0.1/",
        "http://127.1/announce",
        "http://0x7f.1/announce",
        "udp://[::1]:6969/announce",
        "http://203.0.113.9/announce",
        "udp://[2001:db8::1]:6969/announce",
        "udp://[::ffff:127.0.0.1]:6969/announce",
        "http://[::127.0.0.1]/announce",
        "http://[fe80::1]/announce",
        "http://169.254.169.254/latest/meta-data/",
        "udp://169.254.1.1:6969/announce",
        "http://100.64.0.1/announce",
        // Local names: localhost, single labels, the LAN domains.
        "http://localhost/announce",
        "udp://LOCALHOST:6969/announce",
        "http://foo.localhost/announce",
        "http://ⓛocalhost/announce",
        "http://router/announce",
        "udp://NAS:6969/announce",
        "http://nas.local/announce",
        "udp://Printer.LOCAL:6969/announce",
        "udp://%6e%61%73.local:6969/announce",
        // Characters no hostname holds.
        "udp://127.0.0.1,t.example:6969/announce",
        "udp://t.example;127.0.0.1:6969/announce",
        "http://t.example,127.0.0.1/announce",
        // User names, and hosts hidden behind `@`.
        "https://user@t.example/announce",
        "https://user:pw@t.example/announce",
        "http://t.example\\@127.0.0.1/announce",
        "udp://t.example:6969?@192.168.1.1:80/announce",
        "udp://t.example:6969?@127.0.0.1:6969",
        "udp://t.example:6969#@127.0.0.1:6969",
        // Other schemes and shapes.
        "wss://t.example/announce",
        "ftp://t.example/announce",
        "http:1.1/announce",
        "http:/t.example/announce",
        "http:///t.example/announce",
        "t.example/announce",
        "",
        // Whitespace and control characters, which the parser would strip.
        " http://t.example/announce",
        "http://t.exa\tmple/announce",
        "http://t.example/announce\n",
    ] {
        assert_eq!(tracker_url(bad), None, "{bad:?}");
    }
}

#[test]
fn the_length_limit_holds_before_and_after_canonicalising() {
    let path = "a".repeat(claims::MAX_TRACKER);
    let long = format!("http://t.example/{path}");
    assert_eq!(
        tracker_url(&long[..claims::MAX_TRACKER]).map(|k| k.len()),
        Some(claims::MAX_TRACKER)
    );
    assert_eq!(tracker_url(&long[..claims::MAX_TRACKER + 1]), None);
    // Within the limit as written, over it once percent-encoded.
    let spaced = format!("http://t.example/{}", "\"".repeat(1000));
    assert!(spaced.len() <= claims::MAX_TRACKER);
    assert_eq!(tracker_url(&spaced), None);
}

#[test]
fn two_spellings_of_one_tracker_are_stored_once() {
    let raw: Vec<String> = [
        "UDP://T.Example:6969/a",
        "http://localhost/announce",
        "https://t.example:443/announce",
        "udp://t.example:6969/a",
        "https://T.EXAMPLE/announce",
        "udp://%74.example:6969/a",
        "http://other.example/",
    ]
    .map(String::from)
    .to_vec();
    assert_eq!(
        stored_trackers(&raw),
        [
            "udp://t.example:6969/a",
            "https://t.example/announce",
            "http://other.example/",
        ]
    );
}

#[test]
fn the_cap_applies_after_the_dedup() {
    // Duplicates first, then more distinct trackers than the cap: the
    // duplicates don't use up places.
    let mut raw: Vec<String> = (0..MAX_TRACKERS)
        .map(|_| "HTTP://T.Example/a".to_owned())
        .collect();
    raw.extend((0..MAX_TRACKERS + 3).map(|i| format!("http://t{i}.example/a")));
    let out = stored_trackers(&raw);
    assert_eq!(out.len(), MAX_TRACKERS);
    assert_eq!(out[0], "http://t.example/a");
    assert_eq!(out[1], "http://t0.example/a");
    assert_eq!(
        out[MAX_TRACKERS - 1],
        format!("http://t{}.example/a", MAX_TRACKERS - 2)
    );
}

#[test]
fn store_claims_stores_the_canonical_trackers() {
    let mut found = Found::key(InfoHash([7; 20]), 0);
    found.claims = Some(Box::new(claims::Claims {
        name: Some("Sintel".into()),
        trackers: vec![
            "https://t.example:443/announce".into(),
            "http://192.168.1.1/announce".into(),
            "HTTPS://T.example/announce".into(),
            "udp://Tracker.Example:1337".into(),
        ],
        ..claims::Claims::default()
    }));
    let d = Discovery {
        source: SourceId::new(1).unwrap(),
        found,
        at: 0,
        reply: false,
    };
    let c = store_claims(&d).expect("claims");
    assert_eq!(
        c.trackers,
        ["https://t.example/announce", "udp://tracker.example:1337"]
    );
    assert!(
        c.check().is_ok(),
        "the stored claims are within every limit"
    );
}
