//! Open Library answer parsing (ENR-ol) never panics, never keeps more
//! than the caps allow, and the plugin's choice never names a work the
//! answer didn't hold. The first byte picks tight caps (bit 0), so fuzzed
//! answers reach every cap; the rest is the body.
//! `crates/sources-feeds/tests/openlibrary.rs` runs the same checks over a
//! mutated corpus.
#![no_main]

use libfuzzer_sys::fuzz_target;
use lodestar_metainfo::hidden_char;
use lodestar_sources::identify::{Lookup, MediaKind, Query};
use lodestar_sources_feeds::openlibrary::{Limits, MAX_TITLE, Olid, SEARCH_LIMITS, parse_search};
use lodestar_sources_plugins::openlibrary::choose;

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
            max_years: usize::from(pick >> 5),
        }
    } else {
        SEARCH_LIMITS
    };
    let s = parse_search(body, &l);
    assert!(s.works.len() <= l.max_items);
    for w in &s.works {
        assert_eq!(Olid::parse(&w.id.to_string()), Some(w.id));
        assert!(w.title.iter().count() + w.alternatives.len() <= l.max_texts);
        assert!(w.years.len() <= l.max_years);
        for t in w.title.iter().chain(&w.alternatives) {
            assert!(!t.is_empty() && t.len() <= MAX_TITLE);
            assert!(t.trim() == t && !t.chars().any(hidden_char));
        }
        assert!(w.first_year.is_none_or(|y| (1..=9999).contains(&y)));
        assert!(w.years.iter().all(|y| (1..=9999).contains(y)));
    }
    // Whatever the plugin picks is one of the answer's works, and its
    // claims are bounded.
    let refs: Vec<_> = s.works.iter().collect();
    for t in s.works.iter().filter_map(|w| w.title.as_ref()).take(2) {
        let Some(q) = Query::new(t, None, MediaKind::Ebook) else {
            continue;
        };
        if let Lookup::Match(i) = choose(&q, &refs) {
            assert_eq!(i.ids.len(), 1);
            assert_eq!(i.ids[0].scheme, "olid");
            assert!(s.works.iter().any(|w| w.id.to_string() == i.ids[0].value));
            assert!(i.into_claims().details.is_some());
        }
    }
});
