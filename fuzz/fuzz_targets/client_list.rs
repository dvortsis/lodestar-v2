//! The download clients' torrent lists (qBittorrent, Transmission and
//! Deluge JSON) never panic and never break their bounds, and every
//! listed torrent maps to a private find, with the rest of the input as
//! its exported `.torrent`. The first byte picks the client.
#![no_main]

use libfuzzer_sys::fuzz_target;
use lodestar_sources_plugins::clients::{
    ClientKind, MAX_LISTED, MAX_LISTED_NAME, MAX_SIZE, STRATEGY_EXPORT, parse_list, to_found,
};

fuzz_target!(|data: &[u8]| {
    let (pick, body) = data.split_first().map_or((0, data), |(p, b)| (*p, b));
    let kind = match pick % 3 {
        0 => ClientKind::QBittorrent,
        1 => ClientKind::Transmission,
        _ => ClientKind::Deluge,
    };
    let Ok(list) = parse_list(kind, body) else {
        return;
    };
    assert!(list.torrents.len() <= MAX_LISTED);
    for (n, t) in list.torrents.iter().enumerate() {
        assert!(t.hash.len() == 40);
        assert!(t.hash.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)));
        assert!(list.torrents[..n].iter().all(|u| u.hash != t.hash));
        if let Some(name) = &t.name {
            assert!(!name.is_empty() && name.len() <= MAX_LISTED_NAME);
            assert!(!name.chars().any(char::is_control));
        }
        assert!(t.size.is_none_or(|s| s > 0));
        let f = to_found(t, Some(body)).expect("a listed hash is a key");
        assert!(f.private_source && f.peers.is_empty() && !f.interest);
        // The body isn't this torrent's `.torrent` unless its info dict
        // hashes to the key, which a fuzzer won't find.
        assert!(f.strategy != STRATEGY_EXPORT || f.key.to_string() == t.hash);
        let c = f.claims.as_deref();
        assert!(c.and_then(|c| c.size).is_none_or(|s| (1..=MAX_SIZE).contains(&s)));
        assert!(c.is_none_or(|c| c.trackers.is_empty()));
        let p = f.provenance.as_deref().expect("provenance");
        assert!(p.external_ref.is_none());
    }
});
