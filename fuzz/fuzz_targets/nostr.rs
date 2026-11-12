//! Nostr relay-message parsing never panics, and an event it returns has
//! the id its canonical serialisation hashes to, within the caps. The
//! first byte picks tight caps, so fuzzed messages reach every cap; the
//! rest is the message. `crates/sources-feeds/tests/nostr.rs` runs the
//! same checks as property tests.
#![no_main]

use libfuzzer_sys::fuzz_target;
use lodestar_metainfo::hidden_char;
use lodestar_sources_feeds::nostr::{
    self, LIMITS, Limits, MAX_DESCRIPTION, MAX_FILES, MAX_IDS, MAX_KEPT_TAGS, MAX_NAME, MAX_PATH,
    MAX_REASON, MAX_SUB_ID, MAX_TAG, MAX_TRACKER, MAX_TRACKERS, Message,
};

fuzz_target!(|data: &[u8]| {
    let (pick, body) = data.split_first().map_or((0, data), |(p, b)| (*p, b));
    let small = Limits {
        max_bytes: 1 + usize::from(pick) * 64,
        max_tags: usize::from(pick % 8),
        max_tag_items: 1 + usize::from(pick % 4),
    };
    for limits in [&LIMITS, &small] {
        let Ok(m) = nostr::parse(body, limits) else {
            continue;
        };
        assert!(body.len() <= limits.max_bytes);
        match m {
            Message::Event { sub, event } => {
                assert!(!sub.is_empty() && sub.len() <= MAX_SUB_ID);
                assert!(event.tags.len() <= limits.max_tags);
                assert!(
                    event
                        .tags
                        .iter()
                        .all(|t| !t.is_empty() && t.len() <= limits.max_tag_items)
                );
                assert_eq!(
                    nostr::event_id(
                        &event.pubkey,
                        event.created_at,
                        event.kind,
                        &event.tags,
                        &event.content
                    ),
                    event.id
                );
                if let Some(r) = nostr::release(&event) {
                    assert_eq!(event.kind, nostr::KIND_TORRENT);
                    assert!(
                        r.name
                            .as_ref()
                            .is_none_or(|n| !n.is_empty() && n.len() <= MAX_NAME)
                    );
                    assert!(r.files.len() <= MAX_FILES);
                    for f in &r.files {
                        assert!(!f.path.is_empty() && f.path.len() <= MAX_PATH);
                        assert!(!f.path.chars().any(hidden_char));
                    }
                    assert!(r.trackers.len() <= MAX_TRACKERS);
                    assert!(r.trackers.iter().all(|t| t.len() <= MAX_TRACKER));
                    assert!(r.ids.len() <= MAX_IDS);
                    assert!(r.tags.len() <= MAX_KEPT_TAGS);
                    assert!(r.tags.iter().all(|t| !t.is_empty() && t.len() <= MAX_TAG));
                    assert!(
                        r.description
                            .as_ref()
                            .is_none_or(|d| d.len() <= MAX_DESCRIPTION)
                    );
                    assert!(r.publisher.starts_with("npub1"));
                }
            }
            Message::Ok { reason, .. } | Message::Closed { reason, .. } => {
                assert!(reason.len() <= MAX_REASON);
            }
            Message::Notice { text } => assert!(text.len() <= MAX_REASON),
            Message::Eose { sub } => assert!(!sub.is_empty() && sub.len() <= MAX_SUB_ID),
        }
    }
});
