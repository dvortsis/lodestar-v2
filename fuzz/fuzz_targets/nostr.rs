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

/// The caps a kept release must hold.
fn check(r: &nostr::Release) {
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

fuzz_target!(|data: &[u8]| {
    // Fuzzed bytes almost never carry a matching id, so the claims
    // mapping also gets a kind 2003 event built from them directly: one
    // tag a line, its strings split by tabs, and the id computed.
    let text = String::from_utf8_lossy(data);
    let tags: Vec<Vec<String>> = text
        .lines()
        .take(LIMITS.max_tags)
        .map(|l| l.split('\t').map(str::to_owned).collect())
        .collect();
    let pubkey = [1u8; 32];
    let event = nostr::Event {
        id: nostr::event_id(&pubkey, 0, nostr::KIND_TORRENT, &tags, &text),
        pubkey,
        created_at: 0,
        kind: nostr::KIND_TORRENT,
        tags,
        content: text.to_string(),
        sig: [0; 64],
    };
    if let Some(r) = nostr::release(&event) {
        check(&r);
    }

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
                    check(&r);
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
