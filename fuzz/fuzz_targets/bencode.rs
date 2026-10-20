//! Lenient and strict decoding never panic; strict success means the input
//! was canonical; lenient success re-encodes to a stable canonical form.
#![no_main]

use lodestar_bencode::{decode, decode_prefix, decode_strict, to_vec};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(v) = decode_strict(data) {
        assert_eq!(to_vec(&v), data, "strict decode accepted non-canonical input");
    }
    if let Ok(v) = decode(data) {
        let canonical = to_vec(&v);
        let again = decode_strict(&canonical).expect("canonical re-encoding must decode strictly");
        assert_eq!(again.to_owned_value(), v.to_owned_value());
    }
    if let Ok((_, used)) = decode_prefix(data, &Default::default()) {
        assert!(used <= data.len());
    }
});
