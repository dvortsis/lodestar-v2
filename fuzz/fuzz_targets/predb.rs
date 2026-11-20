//! PreDB answer parsing (ENR-predb) never panics, never keeps more than
//! the caps allow, and neither the plugin's choice nor the local match
//! names anything the answer didn't hold. The first byte picks tight caps
//! (bit 0), so fuzzed answers reach every cap; the rest is the body.
//! `crates/sources-feeds/tests/predb.rs` runs the same checks over a
//! mutated corpus.
#![no_main]

use libfuzzer_sys::fuzz_target;
use lodestar_metainfo::hidden_char;
use lodestar_sources::identify::{Lookup, MediaKind, Query};
use lodestar_sources_feeds::predb::{
    Limits, MAX_GENRE, MAX_PRETIME, MAX_REASON, MIN_PRETIME, Nuke, SEARCH_LIMITS, parse_search,
    release_name, section,
};
use lodestar_sources_plugins::predb::{choose, fold, pre_for};

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
        }
    } else {
        SEARCH_LIMITS
    };
    let s = parse_search(body, &l);
    assert!(s.releases.len() <= l.max_items);
    for r in &s.releases {
        assert_eq!(release_name(&r.name), Some(r.name.as_str()));
        assert!(r.section.as_deref().is_none_or(|t| section(t).is_some()));
        assert!(r.pretime.is_none_or(|t| (MIN_PRETIME..=MAX_PRETIME).contains(&t)));
        for (t, max) in [(&r.reason, MAX_REASON), (&r.genre, MAX_GENRE)] {
            if let Some(t) = t {
                assert!(!t.is_empty() && t.len() <= max);
                assert!(t.trim() == t && !t.chars().any(hidden_char));
            }
        }
        assert!(r.reason.is_none() || r.nuke != Nuke::Clean);
    }
    // The choice names only the answer's years and genres; the local match
    // only one of its releases.
    let refs: Vec<_> = s.releases.iter().collect();
    for r in s.releases.iter().take(2) {
        let words = fold(&r.name);
        let title: String = words.split(' ').take(2).collect::<Vec<_>>().join(" ");
        for kind in [MediaKind::Movie, MediaKind::Tv] {
            let Some(q) = Query::new(&title, None, kind) else {
                continue;
            };
            if let Lookup::Match(i) = choose(&q, &refs) {
                assert!(i.ids.is_empty() && i.canonical_title.is_none());
                assert!(
                    i.genres
                        .iter()
                        .all(|g| s.releases.iter().any(|r| r.genre.as_ref() == Some(g)))
                );
                assert!(i.into_claims().details.is_some());
            }
        }
        if let Some(c) = pre_for(&r.name, &s.releases) {
            assert!(c.release.eq_ignore_ascii_case(&r.name));
            assert!(s.releases.iter().any(|x| x.name == c.release));
        }
    }
});
