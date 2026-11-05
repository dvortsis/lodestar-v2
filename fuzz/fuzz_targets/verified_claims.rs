//! `Verified::claims` and `Verified::provenance` never panic, and keep
//! only what a third party's file may give us (SRC-13): the fields outside
//! the info dict aren't covered by the hash, so they're untrusted text.
//!
//! The first byte picks the shape. Even: the rest is a whole `.torrent`
//! body. Odd: the rest builds one around a fixed info dict, so the
//! fuzzer spends its time on the trackers and the comment rather than on
//! reaching a valid dict. Its next byte's bit 0 marks the dict `private`,
//! bit 1 adds a `creation date` from the next 8 bytes (little-endian);
//! what's left splits on `0x00` into the comment, then one tracker per
//! segment, each its own tier, the first also as `announce`.
//!
//! Every body that `verify_body` accepts under its own v1 (or v2) key is
//! checked: trackers are the file's own, in the `url` crate's form (the
//! address as checked), in order, once each, within the caps, and none
//! with a query, fragment, user name or another scheme, nor on a literal
//! address `classify` doesn't call public or a `localhost` name, read
//! again from the stored text (X-claim-trackers); nothing comes from a
//! private dict; the comment is the file's, cut to
//! the cap; and `creation date` is kept only within the believable range.
#![no_main]

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use libfuzzer_sys::fuzz_target;
use lodestar_integrations::net::{AddrClass, classify};
use lodestar_metainfo::{Info, ParseOptions, TorrentFile};
use lodestar_sources::claims::{self, Claims, Confidence};
use lodestar_sources_plugins::torrent_cache::{
    LIMITS, Lookup, MAX_CREATED, MAX_TRACKER_URL, MIN_CREATED, Why, verify_body,
};

fn bytes(out: &mut Vec<u8>, s: &[u8]) {
    out.extend_from_slice(format!("{}:", s.len()).as_bytes());
    out.extend_from_slice(s);
}

/// A `.torrent` around a fixed one-file info dict, keys sorted.
fn built(data: &[u8]) -> Vec<u8> {
    let Some((&flags, rest)) = data.split_first() else {
        return Vec::new();
    };
    let (date, rest) = if flags & 2 != 0 && rest.len() >= 8 {
        let (d, r) = rest.split_at(8);
        (Some(i64::from_le_bytes(d.try_into().unwrap())), r)
    } else {
        (None, rest)
    };
    let mut parts = rest.split(|&b| b == 0);
    let comment = parts.next().unwrap_or_default();
    let trackers: Vec<&[u8]> = parts.collect();

    let mut v = b"d".to_vec();
    if let Some(first) = trackers.first() {
        v.extend_from_slice(b"8:announce");
        bytes(&mut v, first);
        v.extend_from_slice(b"13:announce-listl");
        for t in &trackers {
            v.push(b'l');
            bytes(&mut v, t);
            v.push(b'e');
        }
        v.push(b'e');
    }
    if !comment.is_empty() {
        v.extend_from_slice(b"7:comment");
        bytes(&mut v, comment);
    }
    if let Some(d) = date {
        v.extend_from_slice(format!("13:creation datei{d}e").as_bytes());
    }
    v.extend_from_slice(b"4:infod6:lengthi1048576e4:name6:Sintel12:piece lengthi262144e6:pieces");
    bytes(&mut v, &[7u8; 80]);
    if flags & 1 != 0 {
        v.extend_from_slice(b"7:privatei1e");
    }
    v.extend_from_slice(b"ee");
    v
}

/// The host a client would dial for a kept tracker, read again from the
/// stored text: `udp`'s opaque host as the same host over `http`.
fn dialed_host(t: &str) -> String {
    let u = url::Url::parse(t).expect("a kept tracker parses");
    let u = if u.scheme() == "udp" {
        url::Url::parse(&format!("http://{}/", u.host_str().unwrap_or_default()))
            .expect("a kept udp tracker's host parses over http")
    } else {
        u
    };
    u.host_str().expect("a kept tracker has a host").to_owned()
}

/// Whether `host` reaches the user's own network: a literal address that
/// `classify` doesn't call public, or a `localhost` name.
fn non_public(host: &str) -> bool {
    let literal = match host.strip_prefix('[').and_then(|h| h.strip_suffix(']')) {
        Some(v6) => v6.parse::<Ipv6Addr>().ok().map(IpAddr::V6),
        None => host.parse::<Ipv4Addr>().ok().map(IpAddr::V4),
    };
    let name = host.trim_end_matches('.');
    literal.is_some_and(|ip| classify(ip) != AddrClass::Public)
        || name == "localhost"
        || name.ends_with(".localhost")
}

fn has_scheme(t: &str) -> bool {
    ["http://", "https://", "udp://"]
        .iter()
        .any(|s| t.starts_with(s))
}

fuzz_target!(|data: &[u8]| {
    let Some((&mode, rest)) = data.split_first() else {
        return;
    };
    let body = if mode & 1 == 0 {
        rest.to_vec()
    } else {
        built(rest)
    };
    // The key is the dict's own hash, so every parsable body verifies.
    let Ok(raw) = TorrentFile::raw_info(&body, &LIMITS) else {
        return;
    };
    let Ok(info) = Info::parse_with(
        raw,
        &ParseOptions {
            limits: LIMITS,
            encoding_hint: None,
        },
    ) else {
        return;
    };
    let Some(key) = info.v1.or(info.v2.map(|h| h.truncated())) else {
        return;
    };
    let lookup = Lookup::new(key, Why::Unverified, false).expect("a public key has a lookup");
    let Ok(v) = verify_body(body, &lookup, "cache.example") else {
        // Only the file parse can still refuse it: say, a bad `announce`.
        return;
    };
    let t = v.torrent();
    let c = v.claims();
    assert_eq!(c, v.claims(), "claims are deterministic");

    // Only trackers and the comment, and nothing a clamp would cut.
    let mut clamped = c.clone();
    clamped.clamp();
    assert_eq!(clamped, c, "claims come clamped");
    assert_eq!(
        Claims {
            trackers: Vec::new(),
            description: None,
            ..c.clone()
        },
        Claims::default(),
        "only trackers and the comment are claimed"
    );

    if t.info.private {
        assert!(c.trackers.is_empty(), "a private dict's trackers were kept");
        assert!(c.description.is_none(), "a private dict's comment was kept");
    }

    // The file's own trackers, in its order, once each, within the caps.
    assert!(c.trackers.len() <= claims::MAX_TRACKERS);
    // Kept in the `url` crate's form, the address as it was checked; a
    // `udp` host as it reads over `http` (X-claim-trackers).
    let listed: Vec<String> = t
        .announce
        .iter()
        .flatten()
        .filter_map(|l| url::Url::parse(l).ok())
        .map(|mut u| {
            if u.scheme() == "udp"
                && let Some(h) = u.host_str().map(str::to_owned)
                && let Ok(j) = url::Url::parse(&format!("http://{h}/"))
                && let Some(jh) = j.host_str()
            {
                let _ = u.set_host(Some(jh));
            }
            String::from(u)
        })
        .collect();
    let mut at = 0;
    for (i, tr) in c.trackers.iter().enumerate() {
        assert!(!c.trackers[..i].contains(tr), "a tracker is kept twice");
        let Some(p) = listed[at..].iter().position(|l| l == tr) else {
            panic!("a kept tracker isn't the file's, in order");
        };
        at += p + 1;
        assert!(!tr.is_empty() && tr.len() <= MAX_TRACKER_URL);
        assert!(has_scheme(tr), "a tracker of another scheme was kept");
        assert!(
            !tr.contains('?') && !tr.contains('#'),
            "a tracker with a query or fragment was kept"
        );
        let authority = tr.split_once("://").map_or("", |(_, r)| r);
        let authority = authority.split('/').next().unwrap_or_default();
        assert!(
            !authority.contains('@'),
            "a tracker with a user name was kept"
        );
        let host = dialed_host(tr);
        assert!(
            !non_public(&host),
            "a tracker on a non-public host was kept"
        );
        assert!(
            host.starts_with('[')
                || host.bytes().all(|b| b.is_ascii_lowercase()
                    || b.is_ascii_digit()
                    || b == b'-'
                    || b == b'.'),
            "a tracker host with characters no hostname holds was kept"
        );
        assert_eq!(
            url::Url::parse(tr)
                .ok()
                .and_then(|u| u.host_str().map(str::to_owned)),
            Some(host),
            "a udp tracker was kept with a host other than the one judged"
        );
    }

    // The file's comment, cut to the cap, never empty.
    match (&c.description, &t.comment) {
        (Some(d), Some(comment)) => {
            assert!(!d.is_empty() && d.len() <= claims::MAX_DESCRIPTION);
            assert!(
                comment.starts_with(d.as_str()),
                "the comment isn't the file's"
            );
        }
        (Some(_), None) => panic!("a comment came from nowhere"),
        (None, Some(comment)) => {
            assert!(
                comment.is_empty() || t.info.private,
                "a public comment was lost"
            );
        }
        (None, None) => {}
    }

    let p = v.provenance();
    assert_eq!(p.confidence, Confidence::Medium);
    assert_eq!(p.external_ref, None);
    let believable = t
        .creation_date
        .filter(|d| (MIN_CREATED..=MAX_CREATED).contains(d));
    assert_eq!(p.published_at, believable);
});
