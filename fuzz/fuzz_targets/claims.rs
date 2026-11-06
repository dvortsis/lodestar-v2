//! The stored-claims decoder (SRC-1, ENR-0 and ENR-9,
//! `lodestar_store::claims`) never panics on any blob or body, version 1, 2
//! or 3, and what it accepts keeps the format's promises: within every
//! limit, a canonical plain body (it re-encodes to the same bytes), one
//! blob version per value (version 2 exactly when there's an
//! adult-enrichment section, 3 exactly when there's a details section,
//! never both), enrichment ids and no description or publisher beside a
//! details section, and a blob
//! that round-trips through the encoder.
//!
//! Seeds: `fuzz/seeds/claims/` (version 1, 2 and 3 blobs and plain bodies,
//! raw and zstd, and the golden blobs from `crates/store/tests/fixtures`).
#![no_main]

use libfuzzer_sys::fuzz_target;
use lodestar_store::claims::{self, Claims};

/// What every accepted value promises, whichever layer it came from.
fn promises(c: &Claims) {
    assert!(c.check().is_ok());
    assert!(c.text_len() <= claims::MAX_TOTAL);
    assert!(
        !(c.adult.is_some() && c.details.is_some()),
        "one section a row"
    );
    if let Some(a) = &c.adult {
        assert!(a.check().is_ok());
        assert!(a.performers.len() <= claims::MAX_PERFORMERS);
    }
    if let Some(d) = &c.details {
        assert!(d.check().is_ok());
        assert!(!d.is_empty());
        assert!(d.newsgroups.len() <= claims::MAX_NEWSGROUPS);
        assert!(d.alt_titles.len() <= claims::MAX_ALT_TITLES);
        assert!(d.genres.len() <= claims::MAX_GENRES);
        assert!(
            d.posted_at
                .is_none_or(|t| (0..=claims::MAX_POSTED_AT).contains(&t))
        );
        assert!(
            d.alt_titles
                .iter()
                .all(|a| claims::language_ok(&a.language))
        );
        assert!(d.genres.iter().all(|g| claims::genre_ok(g)));
        assert!(d.newsgroups.iter().all(|g| claims::newsgroup_ok(g)));
        assert!(c.ids.iter().all(|i| claims::id_ok(&i.scheme, &i.value)));
        assert!(
            c.description.is_none() && c.publisher.is_none(),
            "nothing personal beside details"
        );
    }
}

fuzz_target!(|data: &[u8]| {
    // As a whole stored blob: version, codec, body.
    if let Ok(c) = claims::decode(data) {
        promises(&c);
        assert_eq!(data[0], c.version(), "one version per value");
        assert_eq!(c.version() == claims::VERSION_ADULT, c.adult.is_some());
        assert_eq!(c.version() == claims::VERSION_DETAILS, c.details.is_some());
        let blob = claims::encode(&c, 1).expect("decoded claims encode");
        assert!(blob.len() <= claims::MAX_STORED);
        assert_eq!(blob[0], data[0]);
        assert_eq!(claims::decode(&blob).expect("re-decodes"), c);
    }
    // As a plain body, the layer that must be canonical.
    if let Ok(c) = claims::decode_plain(data) {
        promises(&c);
        assert_eq!(claims::encode_plain(&c).expect("re-encodes"), data);
    }
});
