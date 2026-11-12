//! SABnzbd's API answers (UN-2) never panic, and what they yield is clean:
//! one short ASCII job id, a short printable version, and bounded,
//! sorted, distinct category names without the default `*`.
//!
//! Input: two bytes (little-endian) give the HTTP status, then the body.
#![no_main]

use libfuzzer_sys::fuzz_target;
use lodestar_integrations::clients::sabnzbd::{
    MAX_CATEGORIES, parse_add, parse_auth, parse_categories, parse_version,
};

fuzz_target!(|data: &[u8]| {
    let [a, b, body @ ..] = data else {
        return;
    };
    let status = u16::from_le_bytes([*a, *b]);
    if let Ok(job) = parse_add(status, body) {
        let id = job.as_str();
        assert!((1..=64).contains(&id.len()));
        assert!(
            id.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
        );
    }
    if let Ok(v) = parse_version(status, body) {
        assert!(!v.is_empty() && v.len() <= 64 && !v.chars().any(char::is_control));
    }
    let _ = parse_auth(status, body);
    if let Ok(cats) = parse_categories(status, body) {
        assert!(cats.len() <= MAX_CATEGORIES);
        for (i, c) in cats.iter().enumerate() {
            assert!(c.name != "*" && !c.name.is_empty() && c.name.chars().count() <= 256);
            assert!(!c.name.chars().any(char::is_control));
            assert!(c.save_path.is_none());
            assert!(i == 0 || cats[i - 1].name < c.name);
        }
    }
});
