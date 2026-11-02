//! Property tests: never panics, never exceeds a cap, uses an item whole
//! or not at all, and reads back what a well-formed feed says.

use lodestar_metainfo::InfoHash;
use lodestar_sources_feeds::torznab::{
    Feed, Hashes, Kind, LIMITS, Limits, MAX_CATEGORIES, MAX_REF, MAX_TAGS, MAX_TITLE, parse,
    parse_each,
};
use proptest::prelude::*;

fn with(body: &[u8], limits: &Limits) -> Feed {
    let mut items = Vec::new();
    let summary = parse_each(body, limits, |i| items.push(i));
    Feed { items, summary }
}

/// What holds for any input.
fn check(f: &Feed, limits: &Limits) {
    let s = &f.summary;
    assert_eq!(s.kept as usize, f.items.len());
    assert!((s.kept + s.skipped.total()) as usize <= limits.max_items);
    for it in &f.items {
        assert!(
            it.title
                .as_ref()
                .is_none_or(|t| t.len() <= MAX_TITLE && !t.is_empty())
        );
        assert!(it.categories.len() <= MAX_CATEGORIES);
        assert!(it.tags.len() <= MAX_TAGS);
        for r in [&it.guid, &it.link].into_iter().flatten() {
            assert!(r.len() <= MAX_REF);
            assert!(!r.to_ascii_lowercase().contains("apikey="), "{r}");
            assert!(!r.to_ascii_lowercase().starts_with("magnet:"), "{r}");
        }
        if let Kind::Torrent(h) = it.kind {
            assert!(h.key().is_some());
        }
    }
}

/// Pieces of Torznab-shaped XML, well-formed or not.
fn fragment() -> impl Strategy<Value = String> {
    prop_oneof![
        Just("<item>".to_string()),
        Just("</item>".to_string()),
        Just("<title>".to_string()),
        Just("</title>".to_string()),
        Just("<channel>".to_string()),
        Just("</channel>".to_string()),
        Just("<guid>https://a.example/x?apikey=K&amp;id=1</guid>".to_string()),
        Just(
            "<link>magnet:?xt=urn:btih:0123456789abcdef0123456789abcdef01234567</link>".to_string()
        ),
        Just(
            r#"<torznab:attr name="infohash" value="0123456789abcdef0123456789abcdef01234567"/>"#
                .to_string()
        ),
        Just(r#"<torznab:attr name="seeders" value="12"/>"#.to_string()),
        Just(r#"<newznab:attr name="size" value="99"/>"#.to_string()),
        Just(
            r#"<enclosure url="magnet:?xt=urn:btih:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA" length="5"/>"#
                .to_string()
        ),
        Just("<category>2000</category>".to_string()),
        Just("<pubDate>Sun, 01 Nov 2026 00:00:00 +0000</pubDate>".to_string()),
        Just("<![CDATA[x<y]]>".to_string()),
        Just("&amp;".to_string()),
        Just("&bogus;".to_string()),
        Just("&#x1F600;".to_string()),
        Just("<!DOCTYPE x>".to_string()),
        Just("<!-- c -->".to_string()),
        Just("<a b=\"1\" b=\"2\">".to_string()),
        Just("<x>".to_string()),
        Just("</x>".to_string()),
        "[ -~]{0,12}",
    ]
}

/// Tight caps, so the fuzzed documents reach them.
const SMALL: Limits = Limits {
    max_bytes: 4096,
    max_depth: 6,
    max_elements: 64,
    max_items: 5,
    max_attrs: 2,
    max_attr_len: 64,
    max_text: 32,
};

fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

#[derive(Debug, Clone)]
struct Gen {
    title: String,
    hash: [u8; 20],
    seeders: u32,
    size: u64,
}

fn gen_item() -> impl Strategy<Value = Gen> {
    (
        "[a-zA-Z0-9 .&<>'\"\u{e9}\u{6f22}-]{1,40}",
        any::<[u8; 20]>(),
        any::<u32>(),
        any::<u64>(),
    )
        .prop_map(|(title, hash, seeders, size)| Gen {
            title,
            hash,
            seeders,
            size,
        })
}

fn render(items: &[Gen]) -> String {
    let mut s = String::from(
        r#"<?xml version="1.0" encoding="utf-8"?><rss version="2.0" xmlns:torznab="http://torznab.com/schemas/2015/feed"><channel><title>t</title>"#,
    );
    for g in items {
        s.push_str(&format!(
            r#"<item><title>{}</title><size>{}</size><torznab:attr name="seeders" value="{}"/><torznab:attr name="infohash" value="{}"/></item>"#,
            escape(&g.title),
            g.size,
            g.seeders,
            InfoHash(g.hash),
        ));
    }
    s.push_str("</channel></rss>");
    s
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    #[test]
    fn any_bytes(body in proptest::collection::vec(any::<u8>(), 0..2048)) {
        check(&parse(&body), &LIMITS);
        check(&with(&body, &SMALL), &SMALL);
    }

    #[test]
    fn any_fragments(parts in proptest::collection::vec(fragment(), 0..80)) {
        let body = parts.concat();
        check(&parse(body.as_bytes()), &LIMITS);
        check(&with(body.as_bytes(), &SMALL), &SMALL);
    }

    #[test]
    fn well_formed_feeds_read_back(items in proptest::collection::vec(gen_item(), 0..12)) {
        let f = parse(render(&items).as_bytes());
        prop_assert_eq!(f.summary.stop, None);
        prop_assert_eq!(f.items.len(), items.len());
        for (got, want) in f.items.iter().zip(&items) {
            prop_assert_eq!(got.title.as_deref(), Some(want.title.trim()).filter(|t| !t.is_empty()));
            prop_assert_eq!(got.size, Some(want.size));
            prop_assert_eq!(got.seeders, Some(want.seeders));
            prop_assert_eq!(got.kind, Kind::Torrent(Hashes { v1: Some(InfoHash(want.hash)), v2: None }));
        }
    }

    #[test]
    fn a_cut_feed_yields_a_prefix(items in proptest::collection::vec(gen_item(), 1..8), at in any::<prop::sample::Index>()) {
        let body = render(&items);
        let full = parse(body.as_bytes());
        let cut = &body.as_bytes()[..at.index(body.len() + 1)];
        let part = parse(cut);
        prop_assert!(part.items.len() <= full.items.len());
        prop_assert_eq!(&part.items[..], &full.items[..part.items.len()]);
    }
}
