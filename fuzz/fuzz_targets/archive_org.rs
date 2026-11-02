//! Internet Archive JSON parsing never panics, and never keeps more than
//! the caps allow. The first byte picks tight caps, so fuzzed answers
//! reach every cap; the rest is the body. `crates/sources-feeds/tests/
//! archive_org_props.rs` runs the same checks over a mutated corpus.
#![no_main]

use libfuzzer_sys::fuzz_target;
use lodestar_metainfo::hidden_char;
use lodestar_sources_feeds::archive_org::{
    self, DETAILS, LIMITS, Limits, MAX_COLLECTIONS, MAX_CURSOR, MAX_IDENTIFIER, MAX_PATH, MAX_REF,
    MAX_TITLE,
};

fuzz_target!(|data: &[u8]| {
    let (pick, body) = data.split_first().map_or((0, data), |(p, b)| (*p, b));
    let small = Limits {
        max_bytes: 1 + usize::from(pick) * 64,
        max_depth: 1 + usize::from(pick % 8),
        max_values: 1 + usize::from(pick) * 4,
        max_items: 1 + usize::from(pick % 4),
        max_array: usize::from(pick % 6),
        max_text: usize::from(pick) * 4,
        max_files: usize::from(pick % 5),
        max_name_bytes: usize::from(pick) * 2,
    };
    for limits in [&LIMITS, &small] {
        let mut kept = 0usize;
        let page = archive_org::parse_each(body, limits, |it| {
            kept += 1;
            assert!(it.hashes.v1.is_some() && it.hashes.v2.is_none());
            assert!(
                it.title
                    .as_ref()
                    .is_none_or(|t| !t.is_empty() && t.len() <= MAX_TITLE)
            );
            assert!(!it.title.iter().flat_map(|t| t.chars()).any(hidden_char));
            assert!(
                it.identifier
                    .as_ref()
                    .is_none_or(|i| i.len() <= MAX_IDENTIFIER)
            );
            assert_eq!(
                it.link,
                it.identifier.as_ref().map(|i| format!("{DETAILS}{i}"))
            );
            assert!(it.link.as_ref().is_none_or(|l| l.len() <= MAX_REF));
            assert!(it.collections.len() <= MAX_COLLECTIONS);
            assert!(it.files.len() <= limits.max_files);
            let names: usize = it.files.iter().map(|f| f.path.len()).sum();
            assert!(names <= limits.max_name_bytes);
            for f in &it.files {
                assert!(!f.path.is_empty() && f.path.len() <= MAX_PATH);
                assert!(!f.path.chars().any(hidden_char));
            }
        });
        let s = page.summary;
        assert_eq!(s.kept as usize, kept);
        assert!(kept + s.skipped.total() as usize <= limits.max_items);
        assert!(page.cursor.is_none_or(|c| c.len() <= MAX_CURSOR));
    }
});
