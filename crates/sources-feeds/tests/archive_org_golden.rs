//! Golden tests: synthetic answers in the shapes Internet Archive's
//! `advancedsearch`, scrape and `metadata` APIs return, and what each
//! parses to. No real item's metadata is copied.

use lodestar_metainfo::InfoHash;
use lodestar_sources_feeds::archive_org::{File, Item, Skipped, parse};

fn fixture(name: &str) -> Vec<u8> {
    let path = format!(
        "{}/tests/fixtures/archive_org/{name}",
        env!("CARGO_MANIFEST_DIR")
    );
    std::fs::read(path).unwrap()
}

fn v1(hex: &str) -> Option<InfoHash> {
    Some(hex.parse().unwrap())
}

fn strings(s: &[&str]) -> Vec<String> {
    s.iter().map(|s| s.to_string()).collect()
}

#[test]
fn advancedsearch() {
    let f = parse(&fixture("advancedsearch.json"));
    let p = &f.page;
    assert_eq!(p.summary.stop, None);
    assert_eq!(
        (p.summary.kept, p.total, p.cursor.as_deref()),
        (3, Some(5), None)
    );
    assert_eq!(
        p.summary.skipped,
        Skipped {
            no_hash: 1,
            bad_hash: 1,
            ..Skipped::default()
        }
    );
    let one = &f.items[0];
    assert_eq!(
        one,
        &Item {
            hashes: one.hashes,
            identifier: Some("open-movie-example-1".into()),
            title: Some("Open Movie Example — Part One".into()),
            size: Some(734_003_200),
            published: Some(1_793_348_100),
            mediatype: Some("movies".into()),
            // The collection with a space isn't an identifier.
            collections: strings(&["opensource_movies", "community"]),
            link: Some("https://archive.org/details/open-movie-example-1".into()),
            files: vec![],
            files_truncated: false,
        }
    );
    assert_eq!(
        (one.hashes.v1, one.hashes.v2),
        (v1("0123456789abcdef0123456789abcdef01234567"), None)
    );
    // Base32 `btih`, an array title, a string size and `addeddate`.
    let two = &f.items[1];
    assert_eq!(
        two.hashes.v1,
        v1("c9e15763f722f23e98a29decdfae341b98d53056")
    );
    assert_eq!(two.title.as_deref(), Some("Public Domain Audio Two"));
    assert_eq!(two.size, Some(52_428_800));
    assert_eq!(two.published, Some(1_793_275_200));
    assert_eq!(two.collections, strings(&["audio_music"]));
    // A bad identifier, date and media type are dropped, not the item;
    // hidden characters leave the title.
    let five = &f.items[2];
    assert_eq!(
        (five.identifier.as_deref(), five.link.as_deref()),
        (None, None)
    );
    assert_eq!(five.title.as_deref(), Some("Hidden and zerowidth"));
    assert_eq!((five.published, five.mediatype.as_deref()), (None, None));
}

#[test]
fn scrape() {
    let f = parse(&fixture("scrape.json"));
    let p = &f.page;
    assert_eq!((p.summary.stop, p.summary.kept), (None, 1));
    assert_eq!(p.summary.skipped.no_hash, 1);
    assert_eq!(p.total, Some(1234));
    assert_eq!(
        p.cursor.as_deref(),
        Some("W3siaWRlbnRpZmllciI6InNjcmFwZS1leGFtcGxlLTIifV0=")
    );
    assert_eq!(f.items[0].identifier.as_deref(), Some("scrape-example-1"));
    assert_eq!(f.items[0].published, Some(1_793_491_200));
}

#[test]
fn metadata() {
    let f = parse(&fixture("metadata.json"));
    let p = &f.page;
    assert_eq!((p.summary.stop, p.summary.kept, p.total), (None, 1, None));
    let it = &f.items[0];
    // The archive torrent wins over an uploader's `.torrent` listed first.
    assert_eq!(it.hashes.v1, v1("0123456789abcdef0123456789abcdef01234567"));
    assert_eq!(it.identifier.as_deref(), Some("open-movie-example-1"));
    assert_eq!(it.title.as_deref(), Some("Open Movie Example — Part One"));
    assert_eq!(it.size, Some(734_003_200));
    // `publicdate` in the metadata form, read as UTC.
    assert_eq!(it.published, Some(1_793_348_100));
    assert_eq!(it.collections, strings(&["opensource_movies", "community"]));
    let file = |p: &str, s: Option<u64>| File {
        path: p.into(),
        size: s,
    };
    assert_eq!(
        it.files,
        vec![
            file("open-movie-example-1.mp4", Some(734_000_000)),
            file("extras/making-of.ogv", Some(3100)),
            file("subs/eng.srt", None),
            file("open-movie-example-1_meta.xml", Some(1200)),
            file("open-movie-example-1_files.xml", None),
        ]
    );
    assert!(!it.files_truncated);
}

#[test]
fn missing_item() {
    // IA answers `{}` for an identifier it doesn't have.
    let f = parse(&fixture("metadata_missing.json"));
    assert_eq!(f.items, vec![]);
    assert_eq!(f.page.summary, Default::default());
}
