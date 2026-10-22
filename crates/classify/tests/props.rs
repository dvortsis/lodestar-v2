//! Classification invariants on arbitrary file trees.

use lodestar_classify::{Category, Composition, FileInfo, Record, classify};
use proptest::prelude::*;

fn segment() -> impl Strategy<Value = String> {
    prop_oneof![
        "[A-Za-z0-9 ._\\-\\[\\]()]{0,24}",
        "\\PC{0,12}",
        Just("BDMV".to_owned()),
        Just("VIDEO_TS".to_owned()),
        Just("Sample".to_owned()),
        Just("setup.exe".to_owned()),
        Just("movie.mp4.exe".to_owned()),
        Just("PASSWORD.txt".to_owned()),
        Just("Show.S01E02.1080p.mkv".to_owned()),
        Just("a.r00".to_owned()),
        Just("cover.jpg".to_owned()),
    ]
}

fn file() -> impl Strategy<Value = FileInfo> {
    (
        proptest::collection::vec(segment(), 1..4),
        any::<u64>(),
        any::<bool>(),
    )
        .prop_map(|(path, size, padding)| FileInfo {
            path,
            size,
            padding,
        })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(2000))]

    #[test]
    fn consistent(name in "\\PC{0,40}", files in proptest::collection::vec(file(), 0..20)) {
        let c = classify(&name, &files);
        let real = files.iter().filter(|f| !f.padding).count() as u32;
        prop_assert_eq!(c.composition.total_count(), real);
        prop_assert_eq!(c.composition.padding.count, files.len() as u32 - real);
        for cat in Category::ALL {
            prop_assert!(c.composition.pct_bytes(cat) <= 100);
            prop_assert!(c.composition.pct_count(cat) <= 100);
        }
        if let Some(d) = c.composition.dominant() {
            prop_assert!(c.composition.get(d).count > 0);
        }
        if let Some(l) = c.largest {
            prop_assert!(!files[l].padding);
            prop_assert!(files.iter().filter(|f| !f.padding).all(|f| f.size <= files[l].size));
        }
        let weights: u32 = c.junk.reasons.iter().map(|r| u32::from(r.weight())).sum();
        prop_assert_eq!(u32::from(c.junk.score), weights.min(100));
        prop_assert!(c.confidence <= 100 && c.title.confidence <= 100);
        prop_assert_eq!(Composition::decode(&c.composition.encode()), Ok(c.composition.clone()));
        prop_assert_eq!(&c, &classify(&name, &files), "deterministic");
        // The stored record reads back to the bytes it was written as.
        let stored = Record::new(&c).encode();
        let back = Record::decode(&stored);
        prop_assert!(back.is_ok(), "{back:?}");
        prop_assert_eq!(back.unwrap().encode(), stored);
    }

    /// A stored blob is untrusted on read: decoding never panics, and
    /// anything accepted re-encodes to the same bytes.
    #[test]
    fn decode_is_strict(blob in proptest::collection::vec(any::<u8>(), 0..40)) {
        if let Ok(c) = Composition::decode(&blob) {
            prop_assert_eq!(c.encode(), blob);
        }
    }
}

#[test]
fn scales_to_huge_torrents() {
    // The metainfo limit is 200k files. Classification is one pass plus a
    // few parses, so it stays well under a second even in test builds.
    let files: Vec<FileInfo> = (0..200_000u64)
        .map(|i| {
            FileInfo::new(
                vec![
                    "Pack".into(),
                    format!("dir{}", i % 100),
                    format!("Track {i:06}.flac"),
                ],
                i,
            )
        })
        .collect();
    let start = std::time::Instant::now();
    let c = classify("Huge Pack", &files);
    let took = start.elapsed();
    println!("200k files in {took:?}");
    assert_eq!(c.composition.get(Category::Audio).count, 200_000);
    assert!(took.as_secs_f64() < 2.0, "{took:?}");
}
