//! Sonarr and Radarr webhook bodies (SRC-11b) never panic and never break
//! their bounds, and every find they make is private.
//!
//! Input: one byte picks the app (even Sonarr, odd Radarr), then the body.
#![no_main]

use libfuzzer_sys::fuzz_target;
use lodestar_sources_plugins::arr::{
    App, EventType, LIMITS, MAX_ID, MAX_SIZE, MAX_TITLE, parse, to_found,
};

fuzz_target!(|data: &[u8]| {
    let [pick, body @ ..] = data else {
        return;
    };
    let app = if pick % 2 == 0 { App::Sonarr } else { App::Radarr };
    let Ok(event) = parse(app, body) else {
        return;
    };
    assert!(body.len() <= LIMITS.max_bytes);
    if let Some(t) = &event.title {
        assert!(!t.is_empty() && t.len() <= MAX_TITLE);
        assert!(!t.chars().any(char::is_control));
    }
    if let Some(s) = event.size {
        assert!((1..=MAX_SIZE).contains(&s));
    }
    assert!(event.ids.len() <= 3);
    for (_, v) in &event.ids {
        assert!(!v.is_empty() && v.len() <= MAX_ID + 2);
        assert!(v.bytes().all(|b| b.is_ascii_alphanumeric()));
    }
    match to_found(&event) {
        Some(f) => {
            assert!(matches!(event.event_type, EventType::Grab | EventType::Download));
            assert_eq!(Some(f.key), event.key);
            assert!(f.private_source && f.peers.is_empty() && !f.interest);
            let p = f.provenance.as_deref().unwrap();
            assert!(p.external_ref.is_none() && p.published_at.is_none());
            if let Some(c) = f.claims.as_deref() {
                assert!(c.trackers.is_empty() && c.files.is_empty() && c.publisher.is_none());
            }
        }
        None => assert!(
            event.key.is_none()
                || !matches!(event.event_type, EventType::Grab | EventType::Download)
        ),
    }
});
