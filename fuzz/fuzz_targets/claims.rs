//! The stored-claims decoder (SRC-1 and ENR-0, `lodestar_store::claims`)
//! never panics on any blob or body, version 1 or 2, and what it accepts
//! keeps the format's promises: within every limit, a canonical plain body
//! (it re-encodes to the same bytes), one blob version per value (version
//! 2 exactly when there's an adult-enrichment section), and a blob that
//! round-trips through the encoder.
//!
//! Seeds: `fuzz/seeds/claims/` (version 1 and 2 blobs and plain bodies,
//! raw and zstd, and the SRC-1 golden blob).
#![no_main]

use libfuzzer_sys::fuzz_target;
use lodestar_store::claims;

fuzz_target!(|data: &[u8]| {
    // As a whole stored blob: version, codec, body.
    if let Ok(c) = claims::decode(data) {
        assert!(c.check().is_ok());
        assert_eq!(data[0], c.version(), "one version per value");
        assert_eq!(c.version() == claims::VERSION_ADULT, c.adult.is_some());
        if let Some(a) = &c.adult {
            assert!(a.check().is_ok());
            assert!(a.performers.len() <= claims::MAX_PERFORMERS);
        }
        let blob = claims::encode(&c, 1).expect("decoded claims encode");
        assert!(blob.len() <= claims::MAX_STORED);
        assert_eq!(blob[0], data[0]);
        assert_eq!(claims::decode(&blob).expect("re-decodes"), c);
    }
    // As a plain body, the layer that must be canonical.
    if let Ok(c) = claims::decode_plain(data) {
        assert!(c.check().is_ok());
        assert!(c.text_len() <= claims::MAX_TOTAL);
        assert_eq!(claims::encode_plain(&c).expect("re-encodes"), data);
    }
});
