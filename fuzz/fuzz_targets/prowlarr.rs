//! The Prowlarr/Jackett plugin's untrusted inputs never panic and never
//! break their caps: both managers' indexer lists, and Torznab pages
//! mapped to discoveries. The first byte picks the manager, whether the
//! indexer is private, and which input the rest is.
#![no_main]

use libfuzzer_sys::fuzz_target;
use lodestar_sources_feeds::torznab;
use lodestar_sources_plugins::prowlarr::{
    MAX_FUTURE, MAX_ID, MAX_INDEXERS, MAX_SIZE, MIN_PUBLISHED, Manager, PAGE_LIMITS,
    parse_indexers, to_found,
};

const NOW: i64 = 1_800_000_000;

fuzz_target!(|data: &[u8]| {
    let (pick, body) = data.split_first().map_or((0, data), |(p, b)| (*p, b));
    let manager = if pick & 1 == 0 {
        Manager::Prowlarr
    } else {
        Manager::Jackett
    };
    if pick & 4 == 0 {
        if let Ok(list) = parse_indexers(manager, body) {
            assert!(list.len() <= MAX_INDEXERS);
            for (n, i) in list.iter().enumerate() {
                assert!(!i.id.is_empty() && i.id.len() <= MAX_ID);
                assert!(i.id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'));
                assert!(!i.id.eq_ignore_ascii_case("all"));
                assert!(list[..n].iter().all(|j| j.id != i.id));
            }
        }
        return;
    }
    let private = pick & 2 != 0;
    torznab::parse_each(body, &PAGE_LIMITS, |item| {
        let link = item.link.clone();
        let Some(f) = to_found(item, private, "manager.example", NOW) else {
            return;
        };
        assert_eq!(f.private_source, private);
        assert!(f.peers.is_empty() && !f.interest);
        let c = f.claims.as_deref();
        assert!(c.and_then(|c| c.size).is_none_or(|s| (1..=MAX_SIZE).contains(&s)));
        let p = f.provenance.as_deref().expect("provenance");
        let dates = MIN_PUBLISHED..=NOW + MAX_FUTURE;
        assert!(p.published_at.is_none_or(|t| dates.contains(&t)));
        if let Some(r) = &p.external_ref {
            assert!(!private && Some(r) != link.as_ref());
        }
    });
});
