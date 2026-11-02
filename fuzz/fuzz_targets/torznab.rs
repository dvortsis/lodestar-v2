//! Torznab/Newznab parsing never panics, and never keeps more than the
//! caps allow. The first byte picks tight caps, so fuzzed documents reach
//! every cap; the rest is the body.
#![no_main]

use libfuzzer_sys::fuzz_target;
use lodestar_metainfo::hidden_char;
use lodestar_sources_feeds::torznab::{
    self, Kind, LIMITS, Limits, MAX_CATEGORIES, MAX_REF, MAX_TAGS, MAX_TITLE,
};

fuzz_target!(|data: &[u8]| {
    let (pick, body) = data.split_first().map_or((0, data), |(p, b)| (*p, b));
    let small = Limits {
        max_bytes: 1 + usize::from(pick) * 64,
        max_depth: 1 + usize::from(pick % 8),
        max_elements: 1 + usize::from(pick) * 2,
        max_items: 1 + usize::from(pick % 4),
        max_attrs: usize::from(pick % 5),
        max_attr_len: usize::from(pick),
        max_text: usize::from(pick) * 4,
    };
    for limits in [&LIMITS, &small] {
        let mut kept = 0usize;
        let summary = torznab::parse_each(body, limits, |it| {
            kept += 1;
            assert!(it.title.as_ref().is_none_or(|t| t.len() <= MAX_TITLE));
            let shown = it.title.iter().chain(&it.tags);
            assert!(!shown.flat_map(|t| t.chars()).any(hidden_char));
            assert!(it.categories.len() <= MAX_CATEGORIES && it.tags.len() <= MAX_TAGS);
            for r in [&it.guid, &it.link].into_iter().flatten() {
                assert!(r.len() <= MAX_REF);
            }
            if let Kind::Torrent(h) = it.kind {
                assert!(h.key().is_some());
            }
        });
        assert_eq!(summary.kept as usize, kept);
        assert!(kept + summary.skipped.total() as usize <= limits.max_items);
    }
});
