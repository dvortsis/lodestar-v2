//! Steam app-list parsing (ENR-steam) never panics and never keeps more
//! than the caps allow. The first byte picks tight caps, so fuzzed pages
//! reach every cap; the rest is the body. `crates/sources-feeds/tests/
//! steam.rs` runs the same checks over a mutated corpus.
#![no_main]

use libfuzzer_sys::fuzz_target;
use lodestar_sources_feeds::steam::{LIMITS, Limits, MAX_NAME, parse_page};

fuzz_target!(|data: &[u8]| {
    let (pick, body) = data.split_first().map_or((0, data), |(p, b)| (*p, b));
    let l = if pick & 1 == 1 {
        Limits {
            max_bytes: 64 + usize::from(pick >> 1) * 64,
            max_depth: 2 + usize::from(pick >> 5),
            max_values: 4 + usize::from(pick >> 2) * 8,
            max_items: usize::from(pick >> 4),
        }
    } else {
        LIMITS
    };
    let p = parse_page(body, &l);
    assert!(p.apps.len() <= l.max_items);
    for a in &p.apps {
        assert!(a.id > 0 && !a.name.is_empty() && a.name.len() <= MAX_NAME);
        assert!(a.name.trim() == a.name && !a.name.chars().any(char::is_control));
    }
});
