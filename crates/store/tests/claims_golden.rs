//! Golden claim blobs, one per layout (SRC-1's version 1, ENR-0's version
//! 2, ENR-9's version 3), committed as bytes under `fixtures/claims/`.
//! Versions 1 and 2 were written by `origin/main`'s encoder before ENR-9,
//! version 3 by ENR-9's. Each was written by the encoder of its day: they must keep decoding to
//! the same value, and the encoder must keep laying that value out byte
//! for byte (the plain body; zstd's own output may change between zstd
//! releases, so the compressed bytes aren't compared).

use lodestar_store::claims::{
    self, AdultEnrichment, AdultKind, AltTitle, ClaimedFile, Claims, Confidence, Details,
    ExternalId, Provenance, ReleaseDate,
};

const V1: &[u8] = include_bytes!("fixtures/claims/v1.bin");
const V2: &[u8] = include_bytes!("fixtures/claims/v2.bin");
const V3: &[u8] = include_bytes!("fixtures/claims/v3.bin");

fn v1() -> Claims {
    Claims {
        name: Some("Some.Show.S01E01.1080p".into()),
        size: Some(1_234_567_890),
        files: vec![
            ClaimedFile {
                path: "Some.Show.S01E01.mkv".into(),
                size: Some(1_234_000_000),
            },
            ClaimedFile {
                path: String::new(),
                size: None,
            },
        ],
        trackers: vec!["udp://tracker.example:1337/announce".into()],
        ids: vec![ExternalId {
            scheme: "imdb".into(),
            value: "tt0111161".into(),
        }],
        description: Some("é".repeat(100)),
        publisher: Some("uploader".into()),
        provenance: Provenance {
            external_ref: Some("https://feed.example/item/1".into()),
            published_at: Some(-5),
            confidence: Confidence::Medium,
        },
        adult: None,
        details: None,
    }
}

fn v2() -> Claims {
    Claims {
        ids: Vec::new(),
        description: None,
        publisher: None,
        adult: Some(AdultEnrichment {
            scene_id: "0b9e4c1a-6c1f-4f7e-9d0a-2f3e4d5c6b7a".into(),
            kind: Some(AdultKind::Scene),
            title: Some("A Scene Title".into()),
            studio: Some("Studio Name".into()),
            released: ReleaseDate::new(2024, Some(2), Some(29)),
            performers: vec!["Performer One".into(), "Performer Two".into()],
            performers_truncated: true,
        }),
        ..v1()
    }
}

/// A usenet item a plugin enriched: every details field set.
fn v3() -> Claims {
    Claims {
        ids: vec![
            ExternalId {
                scheme: "imdb".into(),
                value: "tt0111161".into(),
            },
            ExternalId {
                scheme: "tmdb".into(),
                value: "278".into(),
            },
        ],
        details: Some(Box::new(Details {
            source_category: Some("TV > HD".into()),
            posted_at: Some(1_700_000_000),
            newsgroups: vec!["alt.binaries.example".into(), "alt.binaries.hdtv".into()],
            alt_titles: vec![
                AltTitle {
                    language: "ja".into(),
                    title: "某番組".into(),
                },
                AltTitle {
                    language: "zh-hant-tw".into(),
                    title: "某節目".into(),
                },
                AltTitle {
                    language: "und".into(),
                    title: "Some Show".into(),
                },
            ],
            canonical_title: Some("Some Show".into()),
            genres: vec!["drama".into(), "science fiction".into(), "ドラマ".into()],
            year: Some(2011),
            match_confidence: Some(87),
        })),
        ..v1()
    }
}

fn plain(blob: &[u8]) -> Vec<u8> {
    match blob[1] {
        0 => blob[2..].to_vec(),
        1 => zstd::bulk::decompress(&blob[2..], claims::MAX_PLAIN).unwrap(),
        c => panic!("codec {c}"),
    }
}

#[test]
fn every_golden_blob_still_decodes() {
    for (blob, want, version) in [
        (V1, v1(), claims::VERSION),
        (V2, v2(), claims::VERSION_ADULT),
        (V3, v3(), claims::VERSION_DETAILS),
    ] {
        assert_eq!(blob[0], version);
        let c = claims::decode(blob).unwrap();
        assert_eq!(c, want, "version {version}");
        assert_eq!(
            claims::encode_plain(&c).unwrap(),
            plain(blob),
            "version {version}"
        );
        assert_eq!(claims::encode(&c, 3).unwrap()[0], version);
    }
}

/// Each golden blob is refused under any other version byte: one version
/// per value.
#[test]
fn golden_blobs_keep_their_version() {
    for blob in [V1, V2, V3] {
        for v in [1u8, 2, 3] {
            if v == blob[0] {
                continue;
            }
            let mut b = blob.to_vec();
            b[0] = v;
            assert!(claims::decode(&b).is_err(), "{} as {v}", blob[0]);
        }
    }
}
