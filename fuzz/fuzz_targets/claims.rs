//! The stored-claims decoder (SRC-1, `lodestar_store::claims`) never
//! panics on any blob or body, and what it accepts keeps the format's
//! promises: within every limit, a canonical plain body (it re-encodes to
//! the same bytes), and a blob that round-trips through the encoder.
#![no_main]

use libfuzzer_sys::fuzz_target;
use lodestar_store::claims;

fuzz_target!(|data: &[u8]| {
    // As a whole stored blob: version, codec, body.
    if let Ok(c) = claims::decode(data) {
        assert!(c.check().is_ok());
        let blob = claims::encode(&c, 1).expect("decoded claims encode");
        assert!(blob.len() <= claims::MAX_STORED);
        assert_eq!(claims::decode(&blob).expect("re-decodes"), c);
    }
    // As a plain body, the layer that must be canonical.
    if let Ok(c) = claims::decode_plain(data) {
        assert!(c.check().is_ok());
        assert_eq!(claims::encode_plain(&c).expect("re-encodes"), data);
    }
});
