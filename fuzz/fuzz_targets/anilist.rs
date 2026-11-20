//! AniList answer parsing (ENR-anilist) never panics, never keeps more
//! than the caps allow, and the plugin's choice never names a media the
//! answer didn't hold, nor one not marked as not adult. The first byte
//! picks tight caps (bit 0), so fuzzed answers reach every cap; the rest
//! is the body. `crates/sources-feeds/tests/anilist.rs` runs the same
//! checks over a mutated corpus.
#![no_main]

use libfuzzer_sys::fuzz_target;
use lodestar_metainfo::hidden_char;
use lodestar_sources::identify::{Lookup, MediaKind, Query};
use lodestar_sources_feeds::anilist::{Limits, MAX_TITLE, MediaId, SEARCH_LIMITS, parse_search};
use lodestar_sources_plugins::anilist::choose;

fuzz_target!(|data: &[u8]| {
    let (pick, body) = data.split_first().map_or((0, data), |(p, b)| (*p, b));
    let l = if pick & 1 == 1 {
        Limits {
            max_bytes: 64 + usize::from(pick >> 1) * 64,
            max_depth: 2 + usize::from(pick >> 5),
            max_values: 4 + usize::from(pick >> 1) * 8,
            max_array: 1 + usize::from(pick >> 4),
            max_text: 1 + usize::from(pick >> 3),
            max_items: usize::from(pick >> 6),
            max_texts: usize::from(pick >> 5),
        }
    } else {
        SEARCH_LIMITS
    };
    let s = parse_search(body, &l);
    assert!(s.media.len() <= l.max_items);
    for m in &s.media {
        assert_eq!(MediaId::parse(&m.id.to_string()), Some(m.id));
        assert!(m.titles().count() <= l.max_texts);
        for t in m.titles() {
            assert!(!t.is_empty() && t.len() <= MAX_TITLE);
            assert!(t.trim() == t && !t.chars().any(hidden_char));
        }
        assert!(m.season_year.is_none_or(|y| (1..=9999).contains(&y)));
        assert!(m.start_year.is_none_or(|y| (1..=9999).contains(&y)));
        assert!(m.country.as_ref().is_none_or(|c| c.len() == 2));
    }
    // Whatever the plugin picks is one of the answer's media, marked not
    // adult, and its claims are bounded.
    let refs: Vec<_> = s.media.iter().collect();
    for t in s.media.iter().filter_map(|m| m.titles().next()).take(2) {
        for kind in [MediaKind::Tv, MediaKind::Movie] {
            let Some(q) = Query::new(t, None, kind) else {
                continue;
            };
            if let Lookup::Match(i) = choose(&q, &refs) {
                assert_eq!(i.ids.len(), 1);
                assert_eq!(i.ids[0].scheme, "anilist");
                assert!(s.media.iter().any(|m| m.id.to_string() == i.ids[0].value
                    && m.adult == Some(false)));
                assert!(i.into_claims().details.is_some());
            }
        }
    }
});
