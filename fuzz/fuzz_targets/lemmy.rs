//! Lemmy post-list parsing never panics, and never keeps more than the
//! caps allow. The first byte picks tight caps, so fuzzed answers reach
//! every cap; the rest is the body. `crates/sources-feeds/tests/lemmy.rs`
//! runs the same checks over a mutated corpus.
#![no_main]

use libfuzzer_sys::fuzz_target;
use lodestar_metainfo::hidden_char;
use lodestar_sources_feeds::lemmy::{
    self, LIMITS, Limits, MAX_CURSOR, MAX_ERROR, MAX_REF, MAX_TITLE, MAX_TORRENTS,
};

fuzz_target!(|data: &[u8]| {
    let (pick, body) = data.split_first().map_or((0, data), |(p, b)| (*p, b));
    let small = Limits {
        max_bytes: 1 + usize::from(pick) * 64,
        max_depth: 1 + usize::from(pick % 8),
        max_values: 1 + usize::from(pick) * 4,
        max_posts: 1 + usize::from(pick % 4),
        max_text: usize::from(pick) * 4,
    };
    for limits in [&LIMITS, &small] {
        let mut posts = Vec::new();
        let page = lemmy::parse_each(body, limits, |p| posts.push(p));
        let s = page.summary;
        assert_eq!(s.kept as usize, posts.len());
        assert!((s.kept + s.skipped.total() + page.removed) as usize <= limits.max_posts);
        assert!(page.next_page.is_none_or(|c| c.len() <= MAX_CURSOR));
        assert!(page.error.is_none_or(|c| c.len() <= MAX_ERROR));
        for p in &posts {
            assert!(p.id > 0 && page.newest_id >= Some(p.id));
            assert!((1..=MAX_TORRENTS).contains(&p.torrents.len()));
            for (i, t) in p.torrents.iter().enumerate() {
                assert!(t.hashes.key().is_some());
                assert!(
                    p.torrents[..i]
                        .iter()
                        .all(|o| o.hashes.key() != t.hashes.key())
                );
                assert!(
                    t.name
                        .as_ref()
                        .is_none_or(|n| !n.is_empty() && n.len() <= MAX_TITLE)
                );
                assert!(!t.name.iter().flat_map(|n| n.chars()).any(hidden_char));
            }
            assert!(
                p.title
                    .as_ref()
                    .is_none_or(|t| !t.is_empty() && t.len() <= MAX_TITLE)
            );
            assert!(!p.title.iter().flat_map(|t| t.chars()).any(hidden_char));
            for r in [&p.link, &p.community.actor_id, &p.creator.actor_id] {
                assert!(r.as_ref().is_none_or(|l| l.len() <= MAX_REF));
            }
        }
    }
});
