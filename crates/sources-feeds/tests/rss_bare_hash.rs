//! The bare, no-namespace `<infohash>` element Academic Torrents' RSS
//! carries: a scrubbed recording of the real feed, then golden cases for
//! the value rules and for precedence against `nyaa:infoHash` and magnets.

use lodestar_sources_feeds::rss::{Feed, Hashes, LIMITS, Limits, Skipped, parse, parse_each};

const HEX: &str = "1f2e3d4c5b6a79880123456789abcdeffedcba98";
const HEX2: &str = "00112233445566778899aabbccddeeff00112233";
const B32: &str = "D4XD2TC3NJ4YQAJDIVTYTK6N577NZOUY";
const V2: &str = "a1a2a3a4a5a6a7a8a9b0b1b2b3b4b5b6b7b8b9c0c1c2c3c4c5c6c7c8c9d0d1d2";
const ZERO: &str = "0000000000000000000000000000000000000000";
const NYAA: &str = r#"xmlns:nyaa="https://nyaa.si/xmlns/nyaa""#;

fn fixture(name: &str) -> Vec<u8> {
    let path = format!("{}/tests/fixtures/rss/{name}", env!("CARGO_MANIFEST_DIR"));
    std::fs::read(path).unwrap()
}

fn v1(hex: &str) -> Hashes {
    Hashes {
        v1: Some(hex.parse().unwrap()),
        v2: None,
    }
}

fn rss(items: &str) -> Feed {
    parse(format!("<rss {NYAA} version=\"2.0\"><channel>{items}</channel></rss>").as_bytes())
}

/// One item with `inner`, and what became of it.
fn one(inner: &str) -> Result<Hashes, Skipped> {
    let f = rss(&format!("<item><title>t</title>{inner}</item>"));
    assert_eq!(f.summary.stop, None, "{f:#?}");
    match f.items.as_slice() {
        [it] => Ok(it.hashes),
        [] => Err(f.summary.skipped),
        _ => panic!("{f:#?}"),
    }
}

fn bare(v: &str) -> String {
    format!("<infohash>{v}</infohash>")
}

fn skipped_bad() -> Skipped {
    Skipped {
        bad_hash: 1,
        ..Skipped::default()
    }
}

#[test]
fn academic_torrents_recording() {
    // Five of the feed's items as served on 2026-11-04, descriptions
    // replaced. guid and link are `/details/<hash>` pages; no magnet.
    let f = parse(&fixture("academictorrents.xml"));
    assert_eq!(f.summary.kept, 5, "{f:#?}");
    assert_eq!(f.summary.skipped, Skipped::default());
    assert_eq!((f.summary.stop, f.summary.error), (None, None));
    let want = [
        (
            "447df5f09ca5c48b0cf2844cfffc1425ae18aaa4",
            "Pl@ntNet-300K: a plant image dataset with high label ambiguity and a long-tailed distribution",
        ),
        (
            "470cfb013ee680b099b27a3ac6ae92733bcaf8f1",
            "AMOS Unlabeled Data Part I: Abdominal CT Scans",
        ),
        (
            "28ed43c7e0c349337bfb0ff3e01f03a3f2d8a492",
            "MUSDB18-HQ: Uncompressed Music Source Separation Dataset (150 Songs with Stems)",
        ),
        (
            "6321c31bdac7d0a136e98fd67b52af5f8d8600d1",
            "Ultrasound Medial Elbow Dataset",
        ),
        (
            "887a4582525607c24f975666b22118030af533ed",
            "Histo-Seg: H&E Whole Slide Image Segmentation Dataset",
        ),
    ];
    for (it, (hash, title)) in f.items.iter().zip(want) {
        assert_eq!(it.hashes, v1(hash));
        assert_eq!(it.title.as_deref(), Some(title));
        let page = format!("https://academictorrents.com/details/{hash}");
        assert_eq!(it.guid.as_deref(), Some(page.as_str()));
        assert_eq!(it.link.as_deref(), Some(page.as_str()));
        // `<size>` and `<category>` here aren't Nyaa's, so aren't read.
        assert_eq!((it.size, it.category.as_deref()), (None, None));
        assert_eq!(it.published, None);
    }
}

#[test]
fn valid_values() {
    assert_eq!(one(&bare(HEX)), Ok(v1(HEX)));
    // Whitespace around the value and CDATA are fine, as for `infoHash`.
    assert_eq!(one(&bare(&format!("\n  {HEX}\t"))), Ok(v1(HEX)));
    assert_eq!(one(&bare(&format!("<![CDATA[{HEX}]]>"))), Ok(v1(HEX)));
    assert_eq!(one(&bare(B32)), Ok(v1(HEX)));
    assert_eq!(one(&bare(&B32.to_lowercase())), Ok(v1(HEX)));
    let v2 = Hashes {
        v1: None,
        v2: Some(V2.parse().unwrap()),
    };
    assert_eq!(one(&bare(V2)), Ok(v2));
}

#[test]
fn uppercase_value() {
    assert_eq!(one(&bare(&HEX.to_uppercase())), Ok(v1(HEX)));
    assert_eq!(one(&bare(&V2.to_uppercase())).map(|h| h.v1), Ok(None));
}

#[test]
fn wrong_length_and_bad_values_are_bad_hash() {
    for v in [
        &HEX[..39],
        &format!("{HEX}0"),
        &HEX[..32], // 32 hex digits read as base32: `0`, `1`, `8`, `9` aren't base32
        &format!("{HEX}{HEX}")[..63],
        &format!("{}g", &HEX[..39]),
        &format!("{} {}", &HEX[..20], &HEX[20..]),
        "",
        "   ",
        "&#x30;",
    ] {
        assert_eq!(one(&bare(v)), Err(skipped_bad()), "{v:?}");
    }
    // An empty element is present, so it's bad too.
    assert_eq!(one("<infohash/>"), Err(skipped_bad()));
}

#[test]
fn zero_hashes_are_bad_hash() {
    for v in [ZERO, &"A".repeat(32), &"0".repeat(64)] {
        assert_eq!(one(&bare(v)), Err(skipped_bad()), "{v}");
    }
}

#[test]
fn a_bad_bare_hash_is_not_rescued_by_a_magnet() {
    // As for `nyaa:infoHash`: present but invalid skips the item.
    let magnet = format!("<link>magnet:?xt=urn:btih:{HEX}</link>");
    assert_eq!(one(&format!("{}{magnet}", bare(ZERO))), Err(skipped_bad()));
    assert_eq!(one(&format!("{magnet}{}", bare("x"))), Err(skipped_bad()));
}

#[test]
fn duplicate_bare_hashes_first_wins() {
    // A repeated element is read once, as every field is.
    assert_eq!(one(&format!("{}{}", bare(HEX), bare(HEX2))), Ok(v1(HEX)));
    assert_eq!(one(&format!("{}{}", bare(HEX2), bare(HEX))), Ok(v1(HEX2)));
    // A bad first one isn't replaced by a good repeat.
    assert_eq!(
        one(&format!("{}{}", bare(ZERO), bare(HEX))),
        Err(skipped_bad())
    );
}

#[test]
fn nyaa_info_hash_comes_first() {
    let nyaa = |v: &str| format!("<nyaa:infoHash>{v}</nyaa:infoHash>");
    // Disagreeing: Nyaa's wins, in either order, as over a magnet.
    assert_eq!(one(&format!("{}{}", nyaa(HEX), bare(HEX2))), Ok(v1(HEX)));
    assert_eq!(one(&format!("{}{}", bare(HEX2), nyaa(HEX))), Ok(v1(HEX)));
    // A bad bare one under a good Nyaa one is ignored, like a bad magnet.
    assert_eq!(one(&format!("{}{}", bare(ZERO), nyaa(HEX))), Ok(v1(HEX)));
    // A bad Nyaa one skips, whatever the bare one says.
    assert_eq!(
        one(&format!("{}{}", nyaa(ZERO), bare(HEX))),
        Err(skipped_bad())
    );
    // Agreeing on the version they share, the other version merges in.
    let hybrid = Hashes {
        v1: Some(HEX.parse().unwrap()),
        v2: Some(V2.parse().unwrap()),
    };
    assert_eq!(one(&format!("{}{}", nyaa(HEX), bare(V2))), Ok(hybrid));
    assert_eq!(one(&format!("{}{}", nyaa(HEX), bare(B32))), Ok(v1(HEX)));
}

#[test]
fn bare_hash_comes_before_a_magnet() {
    let magnet =
        |v1: &str| format!("<link>magnet:?xt=urn:btih:{v1}&amp;xt=urn:btmh:1220{V2}</link>");
    let hybrid = |v1: &str| Hashes {
        v1: Some(v1.parse().unwrap()),
        v2: Some(V2.parse().unwrap()),
    };
    // Agreeing: the magnet's v2 merges in.
    assert_eq!(
        one(&format!("{}{}", magnet(HEX), bare(HEX))),
        Ok(hybrid(HEX))
    );
    // Disagreeing: the bare hash wins and the magnet adds nothing.
    assert_eq!(one(&format!("{}{}", magnet(HEX2), bare(HEX))), Ok(v1(HEX)));
}

#[test]
fn only_the_no_namespace_lowercase_name_counts() {
    let no_hash = Skipped {
        no_hash: 1,
        ..Skipped::default()
    };
    for inner in [
        format!("<infoHash>{HEX}</infoHash>"),
        format!("<InfoHash>{HEX}</InfoHash>"),
        format!("<INFOHASH>{HEX}</INFOHASH>"),
        format!(r#"<at:infohash xmlns:at="https://academictorrents.com">{HEX}</at:infohash>"#),
        format!(r#"<infohash xmlns="urn:other">{HEX}</infohash>"#),
        // Not a child of the item.
        format!("<x>{}</x>", bare(HEX)),
    ] {
        assert_eq!(one(&inner), Err(no_hash), "{inner}");
    }
    // Outside any item it's ignored.
    let f = rss(&format!("{}<item>{}</item>", bare(HEX), bare(HEX2)));
    assert_eq!(f.items[0].hashes, v1(HEX2));
}

#[test]
fn atom_entries_read_it_without_a_namespace() {
    let feed = |inner: &str| {
        format!(
            r#"<feed xmlns="http://www.w3.org/2005/Atom"><entry><title>t</title>{inner}</entry></feed>"#
        )
    };
    let f = parse(feed(&format!(r#"<infohash xmlns="">{HEX}</infohash>"#)).as_bytes());
    assert_eq!(f.items[0].hashes, v1(HEX));
    // In Atom's default namespace it's an Atom name Atom doesn't have.
    let f = parse(feed(&bare(HEX)).as_bytes());
    assert_eq!(f.summary.skipped.no_hash, 1);
}

#[test]
fn the_text_cap_applies() {
    let tight = Limits {
        max_text: 39,
        ..LIMITS
    };
    let body = format!("<rss><channel><item>{}</item></channel></rss>", bare(HEX));
    let mut n = 0;
    let s = parse_each(body.as_bytes(), &tight, |_| n += 1);
    assert_eq!(n, 0);
    assert_eq!(
        s.skipped,
        Skipped {
            over_cap: 1,
            ..Skipped::default()
        }
    );
    // A huge value is cut off at the cap, not buffered.
    let huge = format!(
        "<rss><channel><item>{}</item></channel></rss>",
        bare(&"a".repeat(1 << 20))
    );
    let s = parse_each(huge.as_bytes(), &LIMITS, |_| n += 1);
    assert_eq!(s.skipped.over_cap, 1);
}
