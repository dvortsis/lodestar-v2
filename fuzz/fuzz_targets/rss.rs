//! RSS 2.0/Atom parsing never panics, and never keeps more than the caps
//! allow. The first byte picks tight caps, so fuzzed documents reach
//! every cap; the rest is the body. The body is also used as the value of
//! a bare `<infohash>` (Academic Torrents), which must get the same
//! verdict as the same value in `nyaa:infoHash`.
#![no_main]

use libfuzzer_sys::fuzz_target;
use lodestar_metainfo::hidden_char;
use lodestar_sources_feeds::rss::{self, LIMITS, Limits, MAX_CATEGORY, MAX_REF, MAX_TITLE};

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
        let summary = rss::parse_each(body, limits, |it| {
            kept += 1;
            assert!(it.hashes.key().is_some());
            assert!(it.title.as_ref().is_none_or(|t| t.len() <= MAX_TITLE));
            assert!(!it.title.iter().flat_map(|t| t.chars()).any(hidden_char));
            assert!(it.category.as_ref().is_none_or(|c| c.len() <= MAX_CATEGORY));
            for r in [&it.guid, &it.link].into_iter().flatten() {
                assert!(r.len() <= MAX_REF);
                assert!(!r.get(..7).is_some_and(|p| p.eq_ignore_ascii_case("magnet:")));
            }
        });
        assert_eq!(summary.kept as usize, kept);
        assert!(kept + summary.skipped.total() as usize <= limits.max_items);
    }

    // The bare element validates exactly as Nyaa's does. Short values
    // only, so the two documents sit on the same side of every cap.
    if body.len() > 4096 {
        return;
    }
    let value = String::from_utf8_lossy(body)
        .replace('&', "&amp;")
        .replace('<', "&lt;");
    let verdict = |el: &str| {
        let doc = format!(
            r#"<rss xmlns:nyaa="https://nyaa.si/xmlns/nyaa"><channel><item><{el}>{value}</{el}></item></channel></rss>"#
        );
        let mut hashes = Vec::new();
        let s = rss::parse_each(doc.as_bytes(), &LIMITS, |it| hashes.push(it.hashes));
        (hashes, s.skipped, s.stop)
    };
    assert_eq!(verdict("infohash"), verdict("nyaa:infoHash"));
});
