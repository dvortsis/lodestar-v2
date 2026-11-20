//! TheTVDB answer parsing (ENR-tvdb) never panics, never keeps more than
//! the caps allow, and the plugin's choice never names a series the
//! answer didn't hold. The first byte picks the parser (bit 0: login,
//! else search) and tight caps (bit 1), so fuzzed answers reach every
//! cap; the rest is the body. `crates/sources-feeds/tests/tvdb.rs` runs
//! the same checks over a mutated corpus.
#![no_main]

use libfuzzer_sys::fuzz_target;
use lodestar_metainfo::hidden_char;
use lodestar_sources::identify::{Lookup, MediaKind, Query};
use lodestar_sources_feeds::tvdb::{
    LOGIN_LIMITS, Limits, MAX_LANGUAGE, MAX_TITLE, MAX_TOKEN, SEARCH_LIMITS, parse_login,
    parse_search,
};
use lodestar_sources_plugins::tvdb::choose;

fuzz_target!(|data: &[u8]| {
    let (pick, body) = data.split_first().map_or((0, data), |(p, b)| (*p, b));
    let base = if pick & 1 == 1 {
        LOGIN_LIMITS
    } else {
        SEARCH_LIMITS
    };
    let l = if pick & 2 == 2 {
        Limits {
            max_bytes: 64 + usize::from(pick >> 2) * 64,
            max_depth: 2 + usize::from(pick >> 5),
            max_values: 4 + usize::from(pick >> 2) * 8,
            max_array: 1 + usize::from(pick >> 4),
            max_text: 1 + usize::from(pick >> 3),
            max_items: usize::from(pick >> 6),
            max_texts: usize::from(pick >> 5),
            max_ids: usize::from(pick >> 5),
            max_genres: usize::from(pick >> 6),
        }
    } else {
        base
    };
    if pick & 1 == 1 {
        let got = parse_login(body, &l);
        if let Some(t) = got.token {
            assert!(got.page.whole());
            let t = t.into_inner();
            assert!(!t.is_empty() && t.len() <= MAX_TOKEN.min(l.max_text));
            assert!(t.bytes().all(|b| b.is_ascii_graphic()));
        }
        return;
    }
    let s = parse_search(body, &l);
    assert!(s.results.len() <= l.max_items);
    for r in &s.results {
        assert!(!r.tvdb_id.is_empty() && r.tvdb_id.len() <= 20 && !r.tvdb_id.starts_with('0'));
        assert!(r.tvdb_id.bytes().all(|b| b.is_ascii_digit()));
        assert!(r.titles().count() <= l.max_texts);
        assert!(r.genres.len() <= l.max_genres);
        assert!(r.imdb.len() + r.tmdb.len() <= l.max_ids);
        for t in r.titles().chain(r.genres.iter().map(String::as_str)) {
            assert!(!t.is_empty() && t.len() <= MAX_TITLE);
            assert!(t.trim() == t && !t.chars().any(hidden_char));
        }
        for t in &r.translations {
            assert!((2..=MAX_LANGUAGE).contains(&t.language.len()));
        }
        assert!(r.imdb.iter().all(|s| s.starts_with("tt") && s.len() <= 12));
        assert!(r.tmdb.iter().all(|n| !n.starts_with('0') && n.len() <= 20));
        assert!(r.years.iter().all(|y| (1..=9999).contains(y)));
    }
    // Whatever the plugin picks is one of the answer's series, and its
    // claims are bounded.
    for title in s.results.iter().filter_map(|r| r.name.as_deref()).take(2) {
        let Some(q) = Query::new(title, None, MediaKind::Tv) else {
            continue;
        };
        if let Lookup::Match(i) = choose(&q, &s.results) {
            let id = &i.ids[0];
            assert_eq!(id.scheme, "tvdb");
            assert!(s.results.iter().any(|r| r.tvdb_id == id.value));
            assert!(i.into_claims().details.is_some());
        }
    }
});
