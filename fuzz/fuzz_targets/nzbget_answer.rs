//! NZBGet's JSON-RPC answers (UN-2) never panic, and what they yield is
//! clean: a job id that is a positive `i32` in decimal, a short printable
//! version, and bounded, sorted, distinct category names.
//!
//! Input: one byte gives the request id the answer must match, two bytes
//! (little-endian) the HTTP status, then the body.
#![no_main]

use libfuzzer_sys::fuzz_target;
use lodestar_integrations::clients::nzbget::{
    MAX_CATEGORIES, parse_append, parse_categories, parse_version,
};

fuzz_target!(|data: &[u8]| {
    let [id, a, b, body @ ..] = data else {
        return;
    };
    let id = u64::from(*id);
    let status = u16::from_le_bytes([*a, *b]);
    if let Ok(job) = parse_append(id, status, body) {
        let n: i32 = job.as_str().parse().expect("a decimal i32");
        assert!(n > 0 && n.to_string() == job.as_str());
    }
    if let Ok(v) = parse_version(id, status, body) {
        assert!(!v.is_empty() && v.len() <= 64 && !v.chars().any(char::is_control));
    }
    if let Ok(cats) = parse_categories(id, status, body) {
        assert!(cats.len() <= MAX_CATEGORIES);
        for (i, c) in cats.iter().enumerate() {
            assert!(!c.name.is_empty() && c.name.chars().count() <= 256);
            assert!(!c.name.chars().any(char::is_control));
            assert!(i == 0 || cats[i - 1].name < c.name);
        }
    }
});
