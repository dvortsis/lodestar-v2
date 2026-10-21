//! The release-name parser never panics, is deterministic, and every
//! group or version it reports is text from the name.
#![no_main]

use libfuzzer_sys::fuzz_target;
use lodestar_release_parser::{is_uninformative, parse};

fuzz_target!(|data: &[u8]| {
    let name = String::from_utf8_lossy(data);
    let r = parse(&name);
    assert_eq!(r, parse(&name));
    for text in [&r.group, &r.version].into_iter().flatten() {
        assert!(name.contains(text.as_str()), "{text:?}");
    }
    for s in [r.year, r.season, r.episode, r.absolute].into_iter().flatten() {
        assert!(s.first <= s.last);
    }
    let _ = r.fields();
    let _ = is_uninformative(&name);
});
