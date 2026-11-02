//! Namespaces by URI, every cap, DTDs and entities, and malformed input.

use lodestar_sources_feeds::rss::{
    Feed, LIMITS, Limits, MAX_NS, MAX_TITLE, Skipped, Stop, parse, parse_each,
};

const HASH: &str = "0123456789abcdef0123456789abcdef01234567";
const NYAA: &str = r#"xmlns:nyaa="https://nyaa.si/xmlns/nyaa""#;

fn item(title: &str) -> String {
    format!("<item><title>{title}</title><nyaa:infoHash>{HASH}</nyaa:infoHash></item>")
}

fn feed(items: &str) -> String {
    format!(r#"<?xml version="1.0"?><rss {NYAA}><channel>{items}</channel></rss>"#)
}

fn with(body: &str, limits: &Limits) -> Feed {
    let mut items = Vec::new();
    let summary = parse_each(body.as_bytes(), limits, |i| items.push(i));
    Feed { items, summary }
}

fn titles(f: &Feed) -> Vec<&str> {
    f.items.iter().filter_map(|i| i.title.as_deref()).collect()
}

#[test]
fn namespaces_match_by_uri_not_prefix() {
    let h = format!("<infoHash>{HASH}</infoHash>");
    // Another prefix bound to Nyaa's URI counts.
    let other = format!(
        r#"<rss xmlns:x="https://nyaa.si/xmlns/nyaa"><channel><item><x:seeders>3</x:seeders>{}</item></channel></rss>"#,
        h.replace("<infoHash>", "<x:infoHash>")
            .replace("</infoHash>", "</x:infoHash>")
    );
    let f = parse(other.as_bytes());
    assert_eq!((f.summary.kept, f.items[0].seeders), (1, Some(3)));
    // `nyaa:` unbound, or bound to another URI, doesn't.
    let unbound = format!("<rss><channel>{}</channel></rss>", item("a"));
    assert_eq!(parse(unbound.as_bytes()).summary.skipped.no_hash, 1);
    let wrong = format!(
        r#"<rss xmlns:nyaa="https://evil.example/ns"><channel>{}</channel></rss>"#,
        item("a")
    );
    assert_eq!(parse(wrong.as_bytes()).summary.skipped.no_hash, 1);
    // A rebinding inside an item shadows the outer one, then ends.
    let shadow = feed(&format!(
        r#"<item><nyaa:seeders xmlns:nyaa="urn:other">9</nyaa:seeders><nyaa:infoHash>{HASH}</nyaa:infoHash></item>"#
    ));
    let f = parse(shadow.as_bytes());
    assert_eq!((f.summary.kept, f.items[0].seeders), (1, None));
    // A default namespace takes RSS elements out of RSS.
    let defaulted = format!(
        r#"<rss xmlns="urn:x" {NYAA}><channel>{}</channel></rss>"#,
        item("a")
    );
    assert_eq!(parse(defaulted.as_bytes()).summary.kept, 0);
    // An Atom `entry` without the Atom namespace isn't one.
    let not_atom =
        format!("<feed><entry><link href=\"magnet:?xt=urn:btih:{HASH}\"/></entry></feed>");
    assert_eq!(parse(not_atom.as_bytes()).summary.kept, 0);
}

#[test]
fn namespace_cap_stops() {
    let decls: String = (0..=MAX_NS)
        .map(|i| format!(r#" xmlns:p{i}="https://nyaa.si/xmlns/nyaa""#))
        .collect();
    let body = format!("<rss{decls}><channel>{}</channel></rss>", item("a"));
    assert_eq!(parse(body.as_bytes()).summary.stop, Some(Stop::Namespaces));
    // Unknown URIs on fresh prefixes aren't tracked, so cost nothing.
    let many: String = (0..10_000)
        .map(|i| format!(r#" xmlns:q{i}="urn:{i}""#))
        .collect();
    let body = format!("<rss {NYAA}{many}><channel>{}</channel></rss>", item("a"));
    let f = parse(body.as_bytes());
    assert_eq!((f.summary.stop, f.summary.kept), (None, 1));
}

#[test]
fn structural_caps_stop_and_keep_finished_items() {
    let body = feed(&[item("a"), item("b"), item("c")].concat());
    let cut = body.find("<title>c").unwrap();
    let f = with(
        &body,
        &Limits {
            max_bytes: cut,
            ..LIMITS
        },
    );
    assert_eq!(
        (titles(&f), f.summary.stop),
        (vec!["a", "b"], Some(Stop::Bytes))
    );
    let f = with(
        &body,
        &Limits {
            max_items: 2,
            ..LIMITS
        },
    );
    assert_eq!(
        (titles(&f), f.summary.stop),
        (vec!["a", "b"], Some(Stop::Items))
    );
    let f = with(
        &body,
        &Limits {
            max_elements: 9,
            ..LIMITS
        },
    );
    assert_eq!(
        (titles(&f), f.summary.stop),
        (vec!["a", "b"], Some(Stop::Elements))
    );
    let deep = feed(&format!("{}<item><a><b><c/></b></a></item>", item("a")));
    let f = with(
        &deep,
        &Limits {
            max_depth: 5,
            ..LIMITS
        },
    );
    assert_eq!((titles(&f), f.summary.stop), (vec!["a"], Some(Stop::Depth)));
}

#[test]
fn field_caps_skip_the_item() {
    let limits = Limits {
        max_text: 8,
        ..LIMITS
    };
    // The hash is 40 bytes: over the text cap, so every item is skipped.
    let f = with(&feed(&item("a")), &limits);
    assert_eq!(f.summary.skipped.over_cap, 1);
    let limits = Limits {
        max_attr_len: 30, // fits the Nyaa URI (26)
        ..LIMITS
    };
    let body = feed(&format!(
        r#"<item><enclosure url="https://long.example/some/long/path"/><nyaa:infoHash>{HASH}</nyaa:infoHash></item>{}"#,
        item("b")
    ));
    let f = with(&body, &limits);
    assert_eq!((f.summary.skipped.over_cap, titles(&f)), (1, vec!["b"]));
    let f = with(
        &body,
        &Limits {
            max_attrs: 0,
            ..LIMITS
        },
    );
    assert_eq!(f.summary.skipped.over_cap, 1);
}

#[test]
fn doctype_and_entities() {
    let body = format!(
        r#"<?xml version="1.0"?><!DOCTYPE rss [<!ENTITY x "boom">]><rss {NYAA}><channel>{}</channel></rss>"#,
        item("&x;")
    );
    let f = parse(body.as_bytes());
    assert_eq!((f.summary.kept, f.summary.stop), (0, Some(Stop::Doctype)));
    let body = feed(&[item("&x;"), item("&#x41;&lt;&amp;")].concat());
    let f = parse(body.as_bytes());
    assert_eq!(titles(&f), ["A<&"]);
    assert_eq!(
        f.summary.skipped,
        Skipped {
            malformed: 1,
            ..Skipped::default()
        }
    );
    // An entity outside an item is ignored.
    let body = format!(
        "<rss {NYAA}><channel><title>&x;</title>{}</channel></rss>",
        item("a")
    );
    assert_eq!(parse(body.as_bytes()).summary.kept, 1);
}

#[test]
fn malformed_input_stops() {
    let body = feed(&item("a")).replace("</rss>", "");
    let f = parse(body.as_bytes());
    assert_eq!((f.summary.kept, f.summary.stop), (1, Some(Stop::Malformed)));
    let body = format!(
        r#"<rss {NYAA} xmlns:x=oops><channel>{}</channel></rss>"#,
        item("a")
    );
    assert_eq!(parse(body.as_bytes()).summary.stop, Some(Stop::Malformed));
    let mut bytes = feed(&[item("a"), item("b")].concat()).into_bytes();
    let at = bytes.windows(8).position(|w| w == b"<title>b").unwrap();
    bytes[at + 7] = 0xff;
    let f = parse(&bytes);
    assert_eq!((f.summary.kept, f.summary.stop), (1, Some(Stop::NotUtf8)));
}

#[test]
fn hashes_and_titles() {
    // A present but invalid infoHash skips even with a good magnet.
    let bad = feed(&format!(
        "<item><nyaa:infoHash>{}</nyaa:infoHash><link>magnet:?xt=urn:btih:{HASH}</link></item>",
        "0".repeat(40)
    ));
    assert_eq!(parse(bad.as_bytes()).summary.skipped.bad_hash, 1);
    let long = "\u{202E}é".repeat(MAX_TITLE);
    let f = parse(feed(&item(&long)).as_bytes());
    let t = f.items[0].title.as_deref().unwrap();
    assert!(t.len() <= MAX_TITLE && !t.contains('\u{202E}'));
    // Credentials leave links.
    let body = feed(&format!(
        "<item><link>https://u:p@t.example/dl?id=1&amp;passkey=S</link><nyaa:infoHash>{HASH}</nyaa:infoHash></item>"
    ));
    let f = parse(body.as_bytes());
    assert_eq!(
        f.items[0].link.as_deref(),
        Some("https://t.example/dl?id=1")
    );
}

#[test]
fn namespace_scope_and_undeclaring() {
    // A binding on an empty element ends with it.
    let body = feed(&format!(
        r#"<item><x xmlns:nyaa="urn:o"/><nyaa:infoHash>{HASH}</nyaa:infoHash></item>"#
    ));
    assert_eq!(parse(body.as_bytes()).summary.kept, 1);
    // `xmlns=""` brings elements back into no namespace.
    let body = format!(
        r#"<rss xmlns="urn:x" {NYAA}><channel xmlns="">{}</channel></rss>"#,
        item("a")
    );
    assert_eq!(parse(body.as_bytes()).summary.kept, 1);
    // `xmlns:=` and `xmlns:p=""` aren't valid declarations.
    for decl in [r#"xmlns:="urn:x""#, r#"xmlns:p="""#] {
        let body = format!("<rss {NYAA} {decl}><channel>{}</channel></rss>", item("a"));
        assert_eq!(
            parse(body.as_bytes()).summary.stop,
            Some(Stop::Malformed),
            "{decl}"
        );
    }
}

#[test]
fn empty_infohash_is_bad_and_magnets_are_searched_in_order() {
    let body = feed(&format!(
        "<item><nyaa:infoHash> </nyaa:infoHash><link>magnet:?xt=urn:btih:{HASH}</link></item>"
    ));
    assert_eq!(parse(body.as_bytes()).summary.skipped.bad_hash, 1);
    // The Atom link's magnet comes before the RSS link's, then the
    // enclosure's, then the guid's.
    let other = "1123456789abcdef0123456789abcdef01234567";
    let body = format!(
        r#"<rss xmlns:a="http://www.w3.org/2005/Atom"><channel><item>
<guid>magnet:?xt=urn:btih:{other}</guid><enclosure url="magnet:?xt=urn:btih:{other}"/>
<link>magnet:?xt=urn:btih:{other}</link><a:link href="magnet:?xt=urn:btih:{HASH}"/>
</item><item><guid>magnet:?xt=urn:btih:{other}</guid><enclosure url="magnet:?xt=urn:btih:{HASH}"/></item>
</channel></rss>"#
    );
    let f = parse(body.as_bytes());
    let want = Some(HASH.parse().unwrap());
    assert_eq!(f.items.len(), 2);
    assert!(f.items.iter().all(|i| i.hashes.v1 == want));
}
