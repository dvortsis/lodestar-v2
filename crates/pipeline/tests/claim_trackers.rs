//! Claimed trackers from feeds and plugins pass the shared host guard
//! before storage (X-claim-trackers): none on a literal LAN, loopback or
//! link-local address or a local name. What's stored is the guard's
//! canonical form, the address it judged, once each (X-claim-store).

use lodestar_metainfo::InfoHash;
use lodestar_pipeline::unverified::{store_claims, stored_trackers, tracker_url};
use lodestar_sources::{Discovery, Found, claims};
use lodestar_store::SourceId;
use lodestar_store::claims::{MAX_PATH, MAX_TOTAL, MAX_TRACKERS};

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
        stored_trackers(&raw, MAX_TOTAL),
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
    let out = stored_trackers(&raw, MAX_TOTAL);
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

/// A private find's own reference and its trackers can carry a passkey:
/// neither is stored, whatever the plugin sent; a public find's are
/// (runner, L3).
#[test]
fn a_private_finds_external_ref_and_trackers_are_not_stored() {
    for private in [false, true] {
        let mut found = Found::key(InfoHash([8; 20]), 0).with_provenance(claims::Provenance {
            external_ref: Some("https://t.example/details?passkey=secret".into()),
            published_at: Some(1_600_000_000),
            confidence: claims::Confidence::High,
        });
        found.private_source = private;
        found.claims = Some(Box::new(claims::Claims {
            name: Some("Sintel".into()),
            trackers: vec!["https://t.example/announce/0123456789abcdef".into()],
            ..claims::Claims::default()
        }));
        let d = Discovery {
            source: SourceId::new(1).unwrap(),
            found,
            at: 0,
            reply: false,
        };
        let c = store_claims(&d).expect("claims");
        let want = (!private).then(|| "https://t.example/details?passkey=secret".to_owned());
        assert_eq!(c.provenance.external_ref, want, "private {private}");
        let want: &[&str] = if private {
            &[]
        } else {
            &["https://t.example/announce/0123456789abcdef"]
        };
        assert_eq!(c.trackers, want, "private {private}");
        // The rest of the provenance is kept either way.
        assert_eq!(c.provenance.published_at, Some(1_600_000_000));
        assert_eq!(c.name.as_deref(), Some("Sintel"));
    }
    // A usenet find (UN-1) is always private: no reference, no trackers.
    let mut found = Found::usenet("7", "a1b2c3", 0)
        .unwrap()
        .with_provenance(claims::Provenance {
            external_ref: Some("https://idx.example/api?t=get&id=a1b2c3&apikey=secret".into()),
            published_at: Some(1_600_000_000),
            confidence: claims::Confidence::Low,
        });
    found.claims = Some(Box::new(claims::Claims {
        name: Some("Sintel".into()),
        trackers: vec!["https://t.example/announce/0123456789abcdef".into()],
        ..claims::Claims::default()
    }));
    let d = Discovery {
        source: SourceId::new(1).unwrap(),
        found,
        at: 0,
        reply: false,
    };
    let c = store_claims(&d).expect("claims");
    assert_eq!(c.provenance.external_ref, None, "usenet");
    assert!(c.trackers.is_empty(), "usenet");
    assert_eq!(c.provenance.published_at, Some(1_600_000_000));
}

/// A discovery whose claims leave exactly `room` bytes under the total
/// for trackers, with `trackers` claimed.
fn near_full(room: usize, trackers: &[&str]) -> Discovery {
    let name = "Pack";
    let mut left = MAX_TOTAL - name.len() - room;
    let mut files = Vec::new();
    while left > 0 {
        let n = left.min(MAX_PATH);
        files.push(claims::ClaimedFile {
            path: format!("{}", files.len() % 10).repeat(n),
            size: Some(1),
        });
        left -= n;
    }
    let mut found = Found::key(InfoHash([9; 20]), 0);
    found.claims = Some(Box::new(claims::Claims {
        name: Some(name.into()),
        files,
        trackers: trackers.iter().map(|t| (*t).to_owned()).collect(),
        ..claims::Claims::default()
    }));
    Discovery {
        source: SourceId::new(1).unwrap(),
        found,
        at: 0,
        reply: false,
    }
}

/// The canonical form can be longer than the raw text the emitter's clamp
/// measured: a tracker that would take the claims over the total is
/// dropped, and the rest of the claims are still stored.
#[test]
fn a_tracker_that_grows_past_the_total_is_dropped_not_the_claims() {
    // `http://t.example` (16 bytes) is stored as `http://t.example/` (17).
    // `http://u.ex` (11) as `http://u.ex/` (12): a later, shorter one
    // still fits.
    for (room, want) in [
        (11, vec![]),
        (16, vec!["http://u.ex/"]),
        (17, vec!["http://t.example/"]),
        (17 + 11, vec!["http://t.example/"]),
        (17 + 12, vec!["http://t.example/", "http://u.ex/"]),
    ] {
        let d = near_full(room, &["http://t.example", "http://u.ex"]);
        let c = store_claims(&d).expect("claims");
        assert_eq!(c.trackers, want, "room {room}");
        assert!(c.text_len() <= MAX_TOTAL);
        assert!(c.check().is_ok(), "room {room}: the claims are stored");
        assert_eq!(c.files.len(), d.found.claims.as_ref().unwrap().files.len());
    }
}

/// First-fit in the room left: in a near-full pack, canonicalising can
/// trade one tracker for another. Both fit the clamp's 35 raw bytes; the
/// canonical forms take 36, so the earlier one is kept.
#[test]
fn first_fit_keeps_the_earlier_tracker_in_a_near_full_pack() {
    let raw = ["http://t.example", "udp://u.example:1/a"].map(String::from);
    assert_eq!(raw.iter().map(String::len).sum::<usize>(), 35);
    assert_eq!(stored_trackers(&raw, 35), ["http://t.example/"]);
    assert_eq!(
        stored_trackers(&raw, 36),
        ["http://t.example/", "udp://u.example:1/a"]
    );
}

mod props {
    //! Property tests over mixed spellings (X-claim-store-polish): the
    //! pipeline's own check that what it stores is the guard's fixed
    //! point, within every cap, once each and in the claim's order. The
    //! `verified_claims` fuzz target checks the guard's fixed point on the
    //! `torrent_cache` path; this checks the pipeline functions themselves.

    use super::*;
    use proptest::prelude::*;

    /// A scheme, in any case; `wss` and `ftp` are always refused.
    fn scheme() -> impl Strategy<Value = String> {
        prop_oneof![
            4 => Just("http"),
            4 => Just("udp"),
            3 => Just("https"),
            1 => Just("HTTP"),
            1 => Just("Udp"),
            1 => Just("HTTPS"),
            1 => Just("wss"),
            1 => Just("ftp"),
        ]
        .prop_map(str::to_owned)
    }

    /// Hosts: public, non-public, IDNA (`ü`, `ⓛ`, the ideographic stop
    /// `。`), percent-encoded, IP literals in and out of `[]`, and broken.
    fn host() -> impl Strategy<Value = String> {
        let label = prop_oneof![
            4 => (0u16..1000).prop_map(|n| format!("t{n}")),
            1 => Just("T".to_owned()),
            1 => Just("Tracker".to_owned()),
            1 => Just("bücher".to_owned()),
            1 => Just("ⓛocalhost".to_owned()),
            1 => Just("%74".to_owned()),
            1 => Just("%6e%61%73".to_owned()),
            1 => Just("x-1".to_owned()),
            1 => Just("localhost".to_owned()),
            1 => Just("nas".to_owned()),
        ];
        let tld = prop_oneof![
            6 => Just(".example"),
            1 => Just(".EXAMPLE"),
            1 => Just("。example"),
            1 => Just(".example."),
            1 => Just(".local"),
            1 => Just(".localhost"),
            1 => Just(""),
        ];
        prop_oneof![
            24 => (label, tld).prop_map(|(l, t)| format!("{l}{t}")),
            1 => Just("93.184.215.14".to_owned()),
            1 => Just("127.0.0.1".to_owned()),
            1 => Just("192.168.1.1".to_owned()),
            1 => Just("[2a00:1450:4001::1]".to_owned()),
            1 => Just("[::1]".to_owned()),
            1 => Just("[::ffff:127.0.0.1]".to_owned()),
            1 => Just("[2a00:1450:4001::1".to_owned()),
            1 => Just("2a00:1450:4001::1".to_owned()),
            1 => Just("user@t.example".to_owned()),
            1 => Just("t.example%".to_owned()),
        ]
    }

    /// No port, or one from 0 to 70,000 (past the 65,535 limit).
    fn port() -> impl Strategy<Value = String> {
        prop_oneof![
            4 => Just(String::new()),
            1 => Just(":".to_owned()),
            3 => (0u32..=70_000).prop_map(|p| format!(":{p}")),
            1 => prop::sample::select(vec![80u32, 443, 6969, 65_535, 65_536])
                .prop_map(|p| format!(":{p}")),
        ]
    }

    /// Paths with the characters parsers treat specially, and sometimes
    /// long enough to reach the 2,048-byte limit once encoded.
    fn path() -> impl Strategy<Value = String> {
        let piece = prop_oneof![
            4 => Just("/"),
            4 => Just("a"),
            4 => Just("announce"),
            1 => Just("@"),
            1 => Just("?"),
            1 => Just("#"),
            1 => Just("\\"),
            1 => Just("\""),
            1 => Just("^"),
            1 => Just(" "),
            1 => Just("%"),
            1 => Just("%41"),
            1 => Just("["),
            1 => Just("]"),
            1 => Just("ü"),
            1 => Just("@127.0.0.1"),
        ];
        prop_oneof![
            // Most start with `/`; some run on into the authority.
            8 => prop::collection::vec(piece.clone(), 0..8).prop_map(|p| format!("/{}", p.concat())),
            2 => prop::collection::vec(piece, 0..4).prop_map(|p| p.concat()),
            1 => (1usize..=700).prop_map(|n| format!("/{}", "\"".repeat(n))),
            1 => (2_000usize..=2_100).prop_map(|n| format!("/{}", "a".repeat(n))),
        ]
    }

    fn tracker() -> impl Strategy<Value = String> {
        (scheme(), host(), port(), path()).prop_map(|(s, h, p, x)| format!("{s}://{h}{p}{x}"))
    }

    /// Spellings of one public tracker that canonicalise alike: scheme
    /// and host case, a default port, an encoded host letter.
    fn spelling(i: usize) -> String {
        [
            "http://t.example/a",
            "HTTP://T.Example/a",
            "http://t.example:80/a",
            "http://%74.example/a",
            "Http://t.EXAMPLE:80/a",
            "udp://t.example:1/a",
            "UDP://T.EXAMPLE:1/a",
            "udp://%74.example:1/a",
        ][i % 8]
            .to_owned()
    }

    /// A claim's raw list: mixed trackers with 0–120 colliding spellings
    /// spread through it.
    fn claim() -> impl Strategy<Value = Vec<String>> {
        (
            prop::collection::vec(tracker(), 0..300),
            prop::collection::vec((any::<prop::sample::Index>(), 0usize..8), 0..=120),
        )
            .prop_map(|(mut raw, dups)| {
                for (at, which) in dups {
                    let i = at.index(raw.len() + 1);
                    raw.insert(i, spelling(which));
                }
                raw
            })
    }

    proptest! {
        #![proptest_config(ProptestConfig {
            cases: 256,
            // Failing cases are saved beside this file for replay.
            failure_persistence: Some(Box::new(
                proptest::test_runner::FileFailurePersistence::WithSource("proptest-regressions"),
            )),
            ..ProptestConfig::default()
        })]

        #[test]
        fn a_kept_tracker_is_a_fixed_point_within_the_limit(raw in tracker()) {
            if let Some(k) = tracker_url(&raw) {
                prop_assert!(k.len() <= claims::MAX_TRACKER, "{raw:?} → {} bytes", k.len());
                let again = tracker_url(&k);
                prop_assert_eq!(again.as_deref(), Some(k.as_str()), "{:?}", raw);
            }
        }

        #[test]
        fn stored_trackers_are_capped_unique_fixed_and_in_order(
            raw in claim(),
            room in prop_oneof![3 => 0usize..=300, 1 => Just(MAX_TOTAL)],
        ) {
            // `room` up to 300 bytes for near-full packs; the whole total
            // too, so the 64-entry cap is reached.
            let out = stored_trackers(&raw, room);
            prop_assert!(out.len() <= MAX_TRACKERS);
            prop_assert!(out.iter().map(String::len).sum::<usize>() <= room);
            let unique: std::collections::HashSet<_> = out.iter().collect();
            prop_assert_eq!(unique.len(), out.len(), "a repeat in {:?}", out);
            for k in &out {
                prop_assert!(!k.is_empty() && k.len() <= claims::MAX_TRACKER);
                let again = tracker_url(k);
                prop_assert_eq!(again.as_deref(), Some(k.as_str()));
            }
            // The claim's order: each entry's first canonical spelling
            // comes after the previous entry's.
            let canon: Vec<Option<String>> = raw.iter().map(|t| tracker_url(t)).collect();
            let mut last = None;
            for k in &out {
                let at = canon.iter().position(|c| c.as_deref() == Some(k.as_str()));
                prop_assert!(at.is_some(), "{k} isn't the canonical form of a claimed tracker");
                prop_assert!(at > last, "{k} is out of the claim's order in {out:?}");
                last = at;
            }
        }
    }
}
