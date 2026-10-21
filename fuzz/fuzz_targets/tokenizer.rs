//! The name tokenizer never panics on any string, keeps its bounds, and
//! its offsets always slice the original text on char boundaries.
#![no_main]

use libfuzzer_sys::fuzz_target;
use lodestar_search::tokenizer::{MAX_TOKEN_BYTES, tokenize_into};

fuzz_target!(|data: &[u8]| {
    let text = String::from_utf8_lossy(data);
    let mut out = Vec::new();
    tokenize_into(&text, 0, &mut out, 4096);
    assert!(out.len() <= 4096);
    for t in &out {
        assert!(!t.text.is_empty() && t.text.len() <= MAX_TOKEN_BYTES);
        assert!(t.from < t.to && t.to <= text.len());
        assert!(text.is_char_boundary(t.from) && text.is_char_boundary(t.to));
        assert_eq!(t.text, t.text.to_lowercase(), "tokens are lowercase");
    }
});
