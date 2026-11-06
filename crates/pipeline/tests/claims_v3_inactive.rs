//! ENR-9 lands version 3 inactive (plan §4.3, line 57: "decodes v3;
//! nothing writes it until a plugin is enabled"). The sources-to-store
//! mapping drops a details section a source sends, so v1 and v2 claims
//! are stored exactly as before, and nothing is stored as version 3 until
//! ENR-10 judges the section before storage.

use lodestar_metainfo::InfoHash;
use lodestar_pipeline::unverified::store_claims;
use lodestar_sources::{Discovery, Found, claims};
use lodestar_store::SourceId;
use lodestar_store::claims as stored;

fn discovery(c: claims::Claims) -> Discovery {
    let mut found = Found::key(InfoHash([9; 20]), 0);
    found.claims = Some(Box::new(c));
    Discovery {
        source: SourceId::new(1).unwrap(),
        found,
        at: 0,
        reply: false,
    }
}

fn details() -> Box<claims::Details> {
    Box::new(claims::Details {
        source_category: Some("TV > HD".into()),
        alt_titles: vec![claims::AltTitle {
            language: "ja".into(),
            title: "某番組".into(),
        }],
        genres: vec!["drama".into()],
        year: Some(2011),
        match_confidence: Some(90),
        ..claims::Details::default()
    })
}

#[test]
fn v1_and_v2_claims_are_stored_unchanged_and_details_are_dropped() {
    let v1 = claims::Claims {
        name: Some("Some.Show.S01E01".into()),
        size: Some(5),
        ids: vec![claims::ExternalId {
            scheme: "imdb".into(),
            value: "tt0111161".into(),
        }],
        publisher: Some("uploader".into()),
        ..claims::Claims::default()
    };
    let v2 = claims::Claims {
        name: Some("Scene".into()),
        adult: Some(claims::AdultEnrichment::new("123")),
        ..claims::Claims::default()
    };
    for (c, version) in [(v1, stored::VERSION), (v2, stored::VERSION_ADULT)] {
        let without = store_claims(&discovery(c.clone())).expect("claims");
        let mut with = c;
        with.details = Some(details());
        let with = store_claims(&discovery(with)).expect("claims");
        assert!(with.details.is_none(), "the section isn't stored yet");
        assert_eq!(with, without, "the rest is stored as before");
        let blob = stored::encode(&with, 3).unwrap();
        assert_eq!(blob[0], version);
        assert_eq!(blob, stored::encode(&without, 3).unwrap());
    }
}

#[test]
fn claims_holding_only_details_store_nothing_as_version_3() {
    let c = claims::Claims {
        details: Some(details()),
        ..claims::Claims::default()
    };
    if let Some(s) = store_claims(&discovery(c)) {
        assert!(s.details.is_none());
        assert_ne!(stored::encode(&s, 3).unwrap()[0], stored::VERSION_DETAILS);
    }
}
