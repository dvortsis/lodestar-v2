//! An uploaded `.torrent` (UP-0) never panics `local_torrent::read`, the
//! one function the upload route's body reaches before the filter. The
//! body is untrusted: `read` parses it under its own caps, re-reads the
//! info dict's raw span with no encoding hint and derives the key from
//! that span. The target checks what `read` hands on: a key that is the
//! raw dict's own primary hash, a dict within the cap, no tracker or
//! comment for a private torrent, and only trackers in the shared guard's
//! form.
#![no_main]

use libfuzzer_sys::fuzz_target;
use lodestar_metainfo::Info;
use lodestar_pipeline::local_torrent::{LIMITS, MAX_BYTES, read};

const _: () = assert!(LIMITS.max_torrent_bytes == MAX_BYTES);

fuzz_target!(|body: &[u8]| {
    let Ok(l) = read(body) else {
        return;
    };
    assert!(l.info_raw.len() <= body.len());
    let info = Info::parse(&l.info_raw).expect("read's dict parses again");
    assert_eq!(info.primary_hash(), l.key);
    assert_eq!(info.private, l.private);
    if l.private {
        assert!(l.claims.trackers.is_empty() && l.claims.description.is_none());
    }
    for t in &l.claims.trackers {
        assert_eq!(
            lodestar_integrations::net::public_tracker_url(t).as_deref(),
            Some(t.as_str())
        );
        assert!(!t.contains(['?', '#', '@']));
    }
});
