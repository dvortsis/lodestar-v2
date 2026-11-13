//! Wikidata answer parsing (ENR-14) never panics, never keeps more than
//! the caps allow, and the plugin's choice never names an item the
//! answer didn't hold. The first byte picks the parser (bit 0: entities,
//! else search) and tight caps, so fuzzed answers reach every cap; the
//! rest is the body. `crates/sources-feeds/tests/wikidata.rs` runs the
//! same checks over a mutated corpus.
#![no_main]

use libfuzzer_sys::fuzz_target;
use lodestar_metainfo::hidden_char;
use lodestar_sources::identify::{Lookup, MediaKind, Query};
use lodestar_sources_feeds::wikidata::{
    ENTITY_LIMITS, Limits, MAX_LANGUAGE, MAX_TITLE, SEARCH_LIMITS, parse_entities, parse_search,
};
use lodestar_sources_plugins::wikidata::choose;

fuzz_target!(|data: &[u8]| {
    let (pick, body) = data.split_first().map_or((0, data), |(p, b)| (*p, b));
    let base = if pick & 1 == 1 { ENTITY_LIMITS } else { SEARCH_LIMITS };
    let l = if pick & 2 == 2 {
        Limits {
            max_bytes: 64 + usize::from(pick >> 2) * 64,
            max_depth: 2 + usize::from(pick >> 5),
            max_values: 4 + usize::from(pick >> 2) * 8,
            max_array: 1 + usize::from(pick >> 4),
            max_text: 1 + usize::from(pick >> 3),
            max_items: usize::from(pick >> 6),
            max_texts: usize::from(pick >> 5),
            max_statements: usize::from(pick >> 5),
        }
    } else {
        base
    };
    if pick & 1 == 0 {
        let s = parse_search(body, &l);
        assert!(s.hits.len() <= l.max_items);
        for h in &s.hits {
            assert!(h.id.0 > 0 && h.texts.len() <= l.max_texts);
            assert!(h.texts.iter().all(|t| !t.is_empty() && t.len() <= MAX_TITLE));
        }
        return;
    }
    let e = parse_entities(body, &l);
    assert!(e.entities.len() <= l.max_items);
    for f in &e.entities {
        assert!(f.id.0 > 0);
        assert!(f.labels.len() + f.aliases.len() <= l.max_texts);
        for t in f.labels.iter().chain(&f.aliases) {
            assert!(!t.value.is_empty() && t.value.len() <= MAX_TITLE);
            assert!(t.value.trim() == t.value && !t.value.chars().any(hidden_char));
            assert!((2..=MAX_LANGUAGE).contains(&t.language.len()));
        }
        assert!(f.imdb.iter().all(|s| s.starts_with("tt") && s.len() <= 12));
        for n in f.tmdb_movie.iter().chain(&f.tmdb_tv).chain(&f.tvdb) {
            assert!(!n.starts_with('0') && n.len() <= 20 && n.bytes().all(|b| b.is_ascii_digit()));
        }
        assert!(f.years.iter().all(|y| (1..=9999).contains(y)));
    }
    // Whatever the plugin picks is one of the answer's items, and its
    // claims are bounded.
    let refs: Vec<_> = e.entities.iter().collect();
    for kind in [MediaKind::Movie, MediaKind::Tv] {
        for t in e.entities.iter().flat_map(|f| f.labels.first()).take(2) {
            let Some(q) = Query::new(&t.value, None, kind) else { continue };
            if let Lookup::Match(i) = choose(&q, &refs) {
                let id = &i.ids[0];
                assert_eq!(id.scheme, "wikidata");
                assert!(e.entities.iter().any(|f| f.id.to_string() == id.value));
                let c = i.into_claims();
                assert!(c.details.is_some());
            }
        }
    }
});
