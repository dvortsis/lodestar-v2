//! srrDB answer parsing (ENR-srrdb) never panics, never keeps more than
//! the caps allow, and the plugin's choice is always an id the answer
//! held. The first byte picks the parser (bit 0) and tight caps (bit 1),
//! so fuzzed answers reach every cap; the rest is the body.
//! `crates/sources-feeds/tests/srrdb.rs` runs the same checks over a
//! mutated corpus.
#![no_main]

use libfuzzer_sys::fuzz_target;
use lodestar_metainfo::hidden_char;
use lodestar_sources::identify::{Lookup, MediaKind, Query};
use lodestar_sources_feeds::srrdb::{
    DETAILS_LIMITS, Limits, MAX_RELEASE, SEARCH_LIMITS, parse_details, parse_search,
};
use lodestar_sources_plugins::srrdb::{choose, fold, scene_title, search_request};

fn name_ok(n: &str) -> bool {
    !n.is_empty() && n.len() <= MAX_RELEASE && n.trim() == n && !n.chars().any(hidden_char)
}

fuzz_target!(|data: &[u8]| {
    let (pick, body) = data.split_first().map_or((0, data), |(p, b)| (*p, b));
    let base = if pick & 1 == 1 { DETAILS_LIMITS } else { SEARCH_LIMITS };
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
        base
    };
    if pick & 1 == 1 {
        let d = parse_details(body, &l);
        assert!(d.files.len() <= l.max_items && d.archived.len() <= l.max_items);
        assert!(d.name.as_deref().is_none_or(name_ok));
        for f in d.files.iter().chain(&d.archived) {
            assert!(name_ok(&f.name));
        }
        return;
    }
    let s = parse_search(body, &l);
    assert!(s.hits.len() <= l.max_items);
    for h in &s.hits {
        assert!(name_ok(&h.release));
        if let Some(i) = &h.imdb {
            let d = i.as_str().strip_prefix("tt").unwrap_or_default();
            assert!(d.bytes().all(|b| b.is_ascii_digit()) && (7..=10).contains(&d.len()));
        }
    }
    // Whatever the plugin picks is an id one of the releases carried;
    // any title builds a request of plain segments.
    for h in s.hits.iter().take(2) {
        let Some((title, year)) = scene_title(&h.release) else { continue };
        assert_eq!(fold(&title), title);
        for y in [Some(year), None] {
            let Some(q) = Query::new(&title, y, MediaKind::Movie) else { continue };
            if let Ok(Some(req)) = search_request(&q) {
                assert!(req.url.starts_with("https://api.srrdb.com/v1/search/"));
                assert!(!req.url.contains('?') && !req.url.contains('#'));
            }
            if let Lookup::Match(i) = choose(&q, &s.hits) {
                let id = &i.ids[0];
                assert_eq!(id.scheme, "imdb");
                assert!(s.hits.iter().any(|g| g.imdb.as_ref().is_some_and(|x| x.as_str() == id.value)));
                assert!(i.into_claims().details.is_some());
            }
        }
    }
});
