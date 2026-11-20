//! MusicBrainz answer parsing (ENR-mb) never panics, never keeps more
//! than the caps allow, and the plugin's choice never names a group the
//! answer didn't hold. The first byte picks tight caps (bit 0), so fuzzed
//! answers reach every cap; the rest is the body.
//! `crates/sources-feeds/tests/musicbrainz.rs` runs the same checks over
//! a mutated corpus.
#![no_main]

use libfuzzer_sys::fuzz_target;
use lodestar_metainfo::hidden_char;
use lodestar_sources::identify::{Lookup, MediaKind, Query};
use lodestar_sources_feeds::musicbrainz::{Limits, MAX_TITLE, SEARCH_LIMITS, parse_search};
use lodestar_sources_plugins::musicbrainz::{choose, lucene, wanted};

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
            max_credits: usize::from(pick >> 5),
        }
    } else {
        SEARCH_LIMITS
    };
    let s = parse_search(body, &l);
    assert!(s.hits.len() <= l.max_items);
    for h in &s.hits {
        let id = h.id.as_str();
        assert!(id.len() == 36 && !id.bytes().any(|b| b.is_ascii_uppercase()));
        assert!(!h.title.is_empty() && h.title.len() <= MAX_TITLE);
        assert!(h.title.trim() == h.title && !h.title.chars().any(hidden_char));
        assert!(h.year.is_none_or(|y| (1..=9999).contains(&y)));
        assert!(h.artists.len() <= 2 * l.max_credits);
        assert!(!h.truncated || h.credit.is_none());
        for a in h.artists.iter().chain(&h.credit) {
            assert!(!a.is_empty() && a.len() <= MAX_TITLE && !a.chars().any(hidden_char));
        }
    }
    // Whatever the plugin picks is one of the answer's groups, and its
    // claims are bounded; any text it's handed builds a query.
    let refs: Vec<_> = s.hits.iter().collect();
    for h in s.hits.iter().take(2) {
        let titles = [
            h.title.clone(),
            format!("{} - {}", h.artists.first().map_or("x", |a| a.as_str()), h.title),
        ];
        for t in titles {
            let Some(q) = Query::new(&t, h.year, MediaKind::Music) else { continue };
            if let Some(w) = wanted(q.title()) {
                assert!(lucene(&w).starts_with("releasegroup:\""));
            }
            if let Lookup::Match(i) = choose(&q, &refs) {
                let id = &i.ids[0];
                assert_eq!(id.scheme, "mbid");
                assert!(s.hits.iter().any(|g| g.id.as_str() == id.value));
                assert!(i.into_claims().details.is_some());
            }
        }
    }
});
