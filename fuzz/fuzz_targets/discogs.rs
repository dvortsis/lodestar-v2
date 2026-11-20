//! Discogs search answer parsing (ENR-discogs) never panics, never keeps
//! more than the caps allow, and the plugin's choice never names a master
//! the answer didn't hold. The first byte picks tight caps (bit 1), so
//! fuzzed answers reach every cap; the rest is the body.
//! `crates/sources-feeds/tests/discogs.rs` runs the parser checks (not
//! the plugin's choice) over a mutated corpus.
#![no_main]

use libfuzzer_sys::fuzz_target;
use lodestar_metainfo::hidden_char;
use lodestar_sources::identify::{Lookup, MediaKind, Query};
use lodestar_sources_feeds::discogs::{LIMITS, Limits, MAX_TITLE, parse_search};
use lodestar_sources_plugins::discogs::{choose, fold};

fuzz_target!(|data: &[u8]| {
    let (pick, body) = data.split_first().map_or((0, data), |(p, b)| (*p, b));
    let l = if pick & 2 == 2 {
        Limits {
            max_bytes: 64 + usize::from(pick >> 2) * 64,
            max_depth: 2 + usize::from(pick >> 5),
            max_values: 4 + usize::from(pick >> 2) * 8,
            max_array: 1 + usize::from(pick >> 4),
            max_text: 1 + usize::from(pick >> 3),
            max_items: usize::from(pick >> 6),
        }
    } else {
        LIMITS
    };
    let s = parse_search(body, &l);
    assert!(s.hits.len() <= l.max_items);
    for h in &s.hits {
        assert!(h.id > 0);
        assert!(!h.title.is_empty() && h.title.len() <= MAX_TITLE);
        assert!(!h.title.chars().any(hidden_char));
        assert!(h.year.is_none_or(|y| (1..=9999).contains(&y)));
    }
    // The plugin's choice, for each result's own title and year.
    for h in s.hits.iter().take(4) {
        let Some(q) = Query::new(&h.title, h.year, MediaKind::Music) else {
            continue;
        };
        if let Lookup::Match(i) = choose(&q, &s.hits) {
            assert_eq!(i.ids.len(), 1);
            assert_eq!(i.ids[0].scheme, "discogs");
            let id = i.ids[0].value.strip_prefix('m').unwrap();
            let id = id.parse::<u64>().unwrap();
            // An answer may repeat an id, and `choose` skips a hit whose
            // title or year disagrees, so the chosen title need only
            // belong to some hit with that id (seeds/discogs/repeated_id.json).
            let t = i.canonical_title.unwrap();
            assert!(!fold(&t).is_empty());
            assert!(
                s.hits
                    .iter()
                    .any(|x| x.id == id && x.title.contains(t.as_str()))
            );
        }
    }
});
