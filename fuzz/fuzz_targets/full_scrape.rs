//! The full-scrape parser never panics, whatever the reply and however it
//! is split into chunks, and never passes its caps: no batch over the
//! batch size, no more keys than the entry cap, no more bytes than the
//! byte cap.
#![no_main]

use libfuzzer_sys::fuzz_target;
use lodestar_tracker_scrape::full::{LIMITS, Limits, Parser};

fuzz_target!(|data: &[u8]| {
    // The first two bytes pick the chunk size and small caps, so the cap
    // paths are reached as well as the defaults.
    let (head, body) = data.split_at(data.len().min(2));
    let chunk = usize::from(head.first().copied().unwrap_or(0)) + 1;
    let pick = head.get(1).copied().unwrap_or(0);
    let limits = if pick & 1 == 0 {
        LIMITS
    } else {
        Limits {
            max_bytes: u64::from(pick) * 64,
            max_entries: u64::from(pick >> 4),
            max_entry_bytes: u64::from(pick),
            max_depth: usize::from(pick >> 5),
            batch: usize::from(pick & 7),
        }
    };
    let mut p = Parser::new(limits);
    let batch = p.limits().batch;
    let mut keys = 0u64;
    let mut emit = |b: &[lodestar_metainfo::InfoHash]| {
        assert!(!b.is_empty() && b.len() <= batch);
        keys += b.len() as u64;
    };
    for part in body.chunks(chunk) {
        if p.feed(part, &mut emit).is_err() {
            break;
        }
        assert!(p.summary().bytes <= limits.max_bytes);
    }
    if let Ok(s) = p.finish(&mut emit) {
        assert!(s.keys + s.malformed <= limits.max_entries);
    }
    assert!(keys <= limits.max_entries);
});
