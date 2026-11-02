//! Property tests: never panics, never exceeds a cap, uses an item whole
//! or not at all, and reads back what a well-formed feed says.

use lodestar_metainfo::hidden_char;
use lodestar_sources_feeds::rss::{
    Feed, Hashes, LIMITS, Limits, MAX_CATEGORY, MAX_REF, MAX_TITLE, parse, parse_each,
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
    assert_eq!(s.error, None);
    for it in &f.items {
        assert!(it.hashes.key().is_some());
        assert!(
            it.title
                .as_ref()
                .is_none_or(|t| t.len() <= MAX_TITLE && !t.is_empty())
        );
        assert!(!it.title.iter().flat_map(|t| t.chars()).any(hidden_char));
        assert!(it.category.as_ref().is_none_or(|c| c.len() <= MAX_CATEGORY));
        for r in [&it.guid, &it.link].into_iter().flatten() {
            assert!(r.len() <= MAX_REF);
            let r = r.to_ascii_lowercase();
            assert!(!r.contains("passkey=") && !r.starts_with("magnet:"), "{r}");
        }
    }
}

/// Pieces of RSS- and Atom-shaped XML, well-formed or not.
fn fragment() -> impl Strategy<Value = String> {
    let h = "0123456789abcdef0123456789abcdef01234567";
    prop_oneof![
        Just("<item>".to_string()),
        Just("</item>".to_string()),
        Just(r#"<entry xmlns="http://www.w3.org/2005/Atom">"#.to_string()),
        Just("</entry>".to_string()),
        Just("<title>".to_string()),
        Just("</title>".to_string()),
        Just(r#"<x xmlns:nyaa="https://nyaa.si/xmlns/nyaa">"#.to_string()),
        Just(r#"<x xmlns:nyaa="urn:other">"#.to_string()),
        Just("</x>".to_string()),
        Just(format!("<nyaa:infoHash>{h}</nyaa:infoHash>")),
        Just("<nyaa:size>1.5 GiB</nyaa:size>".to_string()),
        Just("<nyaa:categoryId>1_2</nyaa:categoryId>".to_string()),
        Just(format!("<link>magnet:?xt=urn:btih:{h}</link>")),
        Just(format!(r#"<link href="magnet:?xt=urn:btih:{h}"/>"#)),
        Just("<guid>https://a.example/x?passkey=K&amp;id=1</guid>".to_string()),
        Just(r#"<enclosure url="https://a.example/t?passkey=K"/>"#.to_string()),
        Just("<!DOCTYPE x>".to_string()),
        Just("&unknown;".to_string()),
        Just("&#x202E;".to_string()),
        Just("&#x200B;&#xFEFF;&#x2028;&#xE0041;&#xAD;".to_string()),
        Just("<![CDATA[x]]>".to_string()),
        "[a-z<>&;/\"= \u{202E}\u{200B}\u{200D}\u{FEFF}\u{2028}\u{E0041}]{0,12}",
    ]
}

proptest! {
    #[test]
    fn any_bytes_are_safe(body in proptest::collection::vec(any::<u8>(), 0..512)) {
        check(&parse(&body), &LIMITS);
    }

    #[test]
    fn any_fragments_are_safe(
        parts in proptest::collection::vec(fragment(), 0..40),
        pick in any::<u8>(),
    ) {
        let body = format!(
            r#"<rss xmlns:nyaa="https://nyaa.si/xmlns/nyaa"><channel>{}</channel></rss>"#,
            parts.concat()
        );
        let small = Limits {
            max_bytes: 1 + usize::from(pick) * 8,
            max_depth: 1 + usize::from(pick % 8),
            max_elements: 1 + usize::from(pick),
            max_items: 1 + usize::from(pick % 4),
            max_attrs: usize::from(pick % 5),
            max_attr_len: usize::from(pick),
            max_text: usize::from(pick) * 2,
        };
        for limits in [&LIMITS, &small] {
            check(&with(body.as_bytes(), limits), limits);
        }
    }

    #[test]
    fn well_formed_items_read_back(
        items in proptest::collection::vec(
            (any::<[u8; 20]>(), "[A-Za-z0-9 .\\[\\]-]{1,40}", any::<u32>(), any::<bool>()),
            0..20,
        ),
        atom in any::<bool>(),
    ) {
        let hex = |h: &[u8; 20]| h.iter().map(|b| format!("{b:02x}")).collect::<String>();
        let body: String = items
            .iter()
            .map(|(h, title, seeders, magnet)| {
                let hash = if *magnet && atom {
                    format!(r#"<link href="magnet:?xt=urn:btih:{}"/>"#, hex(h))
                } else if *magnet {
                    format!("<link>magnet:?xt=urn:btih:{}</link>", hex(h))
                } else {
                    format!("<nyaa:infoHash>{}</nyaa:infoHash>", hex(h))
                };
                let (open, close) = if atom { ("<entry>", "</entry>") } else { ("<item>", "</item>") };
                format!("{open}<title>{title}</title>{hash}<nyaa:seeders>{seeders}</nyaa:seeders>{close}")
            })
            .collect();
        let body = if atom {
            format!(r#"<feed xmlns="http://www.w3.org/2005/Atom" xmlns:nyaa="https://nyaa.si/xmlns/nyaa">{body}</feed>"#)
        } else {
            format!(r#"<rss xmlns:nyaa="https://nyaa.si/xmlns/nyaa"><channel>{body}</channel></rss>"#)
        };
        let f = parse(body.as_bytes());
        // An all-zero hash is a placeholder and is skipped.
        let want: Vec<_> = items.iter().filter(|(h, ..)| *h != [0; 20]).collect();
        prop_assert_eq!(f.items.len(), want.len());
        prop_assert_eq!(f.summary.stop, None);
        for (it, (h, title, seeders, _)) in f.items.iter().zip(want) {
            let want_hash = Hashes { v1: Some(hex(h).parse().unwrap()), v2: None };
            prop_assert_eq!(it.hashes, want_hash);
            prop_assert_eq!(it.title.as_deref(), Some(title.trim()).filter(|t| !t.is_empty()));
            prop_assert_eq!(it.seeders, Some(*seeders));
        }
    }
}
