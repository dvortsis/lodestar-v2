//! TMDB search answer parsing (ENR-tmdb) never panics, never keeps more
//! than the caps allow, and the plugin's choice never names a result the
//! answer didn't hold, nor one marked adult. The first byte picks the form
//! (bit 0: TV, else film) and tight caps (bit 1), so fuzzed answers reach
//! every cap; the rest is the body. `crates/sources-feeds/tests/tmdb.rs`
//! runs the same checks over a mutated corpus.
#![no_main]

use libfuzzer_sys::fuzz_target;
use lodestar_metainfo::hidden_char;
use lodestar_sources::identify::{Lookup, MediaKind, Query};
use lodestar_sources_feeds::tmdb::{Form, LIMITS, Limits, MAX_LANGUAGE, MAX_TITLE, parse_search};
use lodestar_sources_plugins::tmdb::choose;

fuzz_target!(|data: &[u8]| {
    let (pick, body) = data.split_first().map_or((0, data), |(p, b)| (*p, b));
    let (form, kind) = if pick & 1 == 1 {
        (Form::Tv, MediaKind::Tv)
    } else {
        (Form::Movie, MediaKind::Movie)
    };
    let l = if pick & 2 == 2 {
        Limits {
            max_bytes: 64 + usize::from(pick >> 2) * 64,
            max_depth: 2 + usize::from(pick >> 5),
            max_values: 4 + usize::from(pick >> 2) * 8,
            max_array: 1 + usize::from(pick >> 4),
            max_text: 1 + usize::from(pick >> 3),
            max_items: usize::from(pick >> 6),
            max_genres: usize::from(pick >> 5),
        }
    } else {
        LIMITS
    };
    let s = parse_search(body, form, &l);
    assert!(s.hits.len() <= l.max_items);
    for h in &s.hits {
        assert!(h.id > 0 && h.genre_ids.len() <= l.max_genres);
        for t in [&h.title, &h.original_title].into_iter().flatten() {
            assert!(!t.is_empty() && t.len() <= MAX_TITLE);
            assert!(!t.chars().any(hidden_char));
        }
        if let Some(lang) = &h.original_language {
            assert!((2..=MAX_LANGUAGE).contains(&lang.len()));
            assert!(lang.bytes().all(|b| b.is_ascii_lowercase()));
        }
        assert!(h.year.is_none_or(|y| (1..=9999).contains(&y)));
    }
    // The plugin's choice, for each result's own title and year.
    for h in s.hits.iter().take(4) {
        let Some(t) = &h.title else { continue };
        let Some(q) = Query::new(t, h.year, kind) else {
            continue;
        };
        if let Lookup::Match(i) = choose(&q, &s.hits) {
            let id = i.ids[0].value.parse::<u64>().unwrap();
            assert!(s.hits.iter().any(|x| x.id == id && !x.adult));
            assert_eq!(i.ids.len(), 1);
        }
    }
});
