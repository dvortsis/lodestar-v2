//! Every cap in `Limits`, DTDs and entities, and malformed input.

use lodestar_sources_feeds::torznab::{
    Feed, LIMITS, Limits, MAX_REF, MAX_TITLE, Skipped, Stop, parse, parse_each,
};

const HASH: &str = "0123456789abcdef0123456789abcdef01234567";

fn item(title: &str) -> String {
    format!(r#"<item><title>{title}</title><torznab:attr name="infohash" value="{HASH}"/></item>"#)
}

fn feed(items: &str) -> String {
    format!(r#"<?xml version="1.0"?><rss><channel>{items}</channel></rss>"#)
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
fn shared_limits_match_the_contract() {
    use lodestar_sources::{claims, http};
    assert_eq!(LIMITS.max_bytes, http::MAX_BODY);
    assert_eq!(MAX_TITLE, claims::MAX_NAME);
    assert_eq!(MAX_REF, claims::MAX_EXTERNAL_REF);
}

#[test]
fn bytes_cap_keeps_items_before_the_cut() {
    let body = feed(&[item("a"), item("b"), item("c")].concat());
    let cut = body.find("<title>c").unwrap();
    let limits = Limits {
        max_bytes: cut,
        ..LIMITS
    };
    let f = with(&body, &limits);
    assert_eq!(titles(&f), ["a", "b"]);
    assert_eq!(f.summary.stop, Some(Stop::Bytes));
}

#[test]
fn depth_cap_stops() {
    let limits = Limits {
        max_depth: 6,
        ..LIMITS
    };
    // rss > channel > item > x > y > z is six deep: fine.
    let ok = feed(&format!(
        r#"<item><x><y><z/></y></x><torznab:attr name="infohash" value="{HASH}"/></item>"#
    ));
    assert_eq!(with(&ok, &limits).summary.stop, None);
    assert_eq!(with(&ok, &limits).items.len(), 1);
    let deep = feed(&format!(
        "{}<item><x><y><z><w/></z></y></x></item>",
        item("a")
    ));
    let f = with(&deep, &limits);
    assert_eq!(titles(&f), ["a"]);
    assert_eq!(f.summary.stop, Some(Stop::Depth));
    // The default holds up against a very deep document.
    let bomb = "<a>".repeat(100_000);
    assert_eq!(parse(bomb.as_bytes()).summary.stop, Some(Stop::Depth));
}

#[test]
fn element_cap_stops() {
    let limits = Limits {
        max_elements: 10,
        ..LIMITS
    };
    // rss, channel, then three elements per item.
    let body = feed(&[item("a"), item("b"), item("c"), item("d")].concat());
    let f = with(&body, &limits);
    assert_eq!(titles(&f), ["a", "b"]);
    assert_eq!(f.summary.stop, Some(Stop::Elements));
}

#[test]
fn item_cap_counts_skipped_items_too() {
    let limits = Limits {
        max_items: 3,
        ..LIMITS
    };
    let body = feed(&[item("a"), "<item/>".into(), item("b"), item("c")].concat());
    let f = with(&body, &limits);
    assert_eq!(titles(&f), ["a", "b"]);
    assert_eq!(f.summary.skipped.no_hash, 1);
    assert_eq!(f.summary.stop, Some(Stop::Items));
}

#[test]
fn attribute_count_cap_skips_the_item() {
    let limits = Limits {
        max_attrs: 3,
        ..LIMITS
    };
    let many = format!(
        r#"<item><title>x</title><torznab:attr name="infohash" value="{HASH}" a="1" b="2"/></item>"#
    );
    // Attributes on elements the parser doesn't read are never counted.
    let ignored = format!(
        r#"<item><description a="" b="" c="" d=""/>{}"#,
        &item("y")[6..]
    );
    let f = with(&feed(&[many, ignored, item("z")].concat()), &limits);
    assert_eq!(titles(&f), ["y", "z"]);
    assert_eq!(f.summary.skipped.over_cap, 1);
    assert_eq!(f.summary.stop, None);
}

#[test]
fn attribute_length_cap_skips_the_item() {
    let limits = Limits {
        max_attr_len: 100,
        ..LIMITS
    };
    let long = format!(
        r#"<item><title>x</title><torznab:attr name="magneturl" value="magnet:?xt=urn:btih:{HASH}&amp;dn={}"/></item>"#,
        "a".repeat(100)
    );
    let f = with(&feed(&[long, item("y")].concat()), &limits);
    assert_eq!(titles(&f), ["y"]);
    assert_eq!(f.summary.skipped.over_cap, 1);
}

#[test]
fn text_cap_skips_the_item_and_titles_are_cut() {
    let limits = Limits {
        max_text: 4096,
        ..LIMITS
    };
    // Over the text cap, across text, entity and CDATA pieces: skipped.
    let over = item(&format!(
        "{}&amp;<![CDATA[{}]]>",
        "a".repeat(2048),
        "b".repeat(2048)
    ));
    // Under it but over the title limit: cut on a character boundary.
    let long = item(&format!("a{}", "\u{e9}".repeat(1000)));
    let f = with(&feed(&[over, long].concat()), &limits);
    assert_eq!(f.summary.skipped.over_cap, 1);
    let t = f.items[0].title.as_deref().unwrap();
    assert_eq!(t.len(), MAX_TITLE - 1);
    assert!(t.starts_with('a') && t.ends_with('\u{e9}'));
    // Text in elements the parser doesn't read isn't held or capped.
    let desc = format!(
        "<item><description>{}</description>{}",
        "d".repeat(10_000),
        &item("kept")[6..]
    );
    assert_eq!(titles(&with(&feed(&desc), &limits)), ["kept"]);
}

#[test]
fn small_fields_are_capped_without_skipping() {
    let mut extra = String::new();
    for i in 0..40 {
        extra.push_str(&format!(
            r#"<category>{i}</category><torznab:attr name="tag" value="t{i}"/>"#
        ));
    }
    extra.push_str(&format!(
        r#"<torznab:attr name="tag" value="{}"/>"#,
        "x".repeat(65)
    ));
    extra.push_str(&format!(
        r#"<guid>https://a.example/{}</guid>"#,
        "p".repeat(MAX_REF)
    ));
    let body = feed(&format!(
        r#"<item>{extra}<torznab:attr name="infohash" value="{HASH}"/></item>"#
    ));
    let f = parse(body.as_bytes());
    let it = &f.items[0];
    assert_eq!(it.categories.len(), 16);
    assert_eq!(it.tags.len(), 8);
    assert_eq!(it.guid, None, "an over-long reference is dropped, not cut");
}

#[test]
fn doctypes_are_refused() {
    let laughs = r#"<?xml version="1.0"?>
<!DOCTYPE rss [<!ENTITY a "aaaaaaaaaa"><!ENTITY b "&a;&a;&a;&a;&a;&a;&a;&a;&a;&a;">]>
<rss><channel><item><title>&b;</title></item></channel></rss>"#;
    let f = parse(laughs.as_bytes());
    assert!(f.items.is_empty());
    assert_eq!(f.summary.stop, Some(Stop::Doctype));
    let xxe = r#"<!DOCTYPE rss SYSTEM "file:///etc/passwd"><rss/>"#;
    assert_eq!(parse(xxe.as_bytes()).summary.stop, Some(Stop::Doctype));
}

#[test]
fn only_the_five_entities_and_character_references_resolve() {
    let ok = item("&lt;&gt;&amp;&quot;&apos;&#65;&#x42;");
    let bad = item("&xxe;");
    let bad_char = item("&#xFFFFFFFF;");
    let body = feed(&format!(
        "<description>&nbsp;</description>{ok}{bad}{bad_char}"
    ));
    let f = parse(body.as_bytes());
    assert_eq!(titles(&f), ["<>&\"'AB"]);
    assert_eq!(f.summary.skipped.malformed, 2);
    assert_eq!(f.summary.stop, None);
    let attr = format!(r#"<item><torznab:attr name="infohash" value="&xxe;{HASH}"/></item>"#);
    assert_eq!(parse(feed(&attr).as_bytes()).summary.skipped.malformed, 1);
}

#[test]
fn invalid_utf8_parses_the_valid_prefix() {
    let mut body = feed(&[item("a"), item("b")].concat()).into_bytes();
    let at = body.windows(8).position(|w| w == b"<title>b").unwrap();
    body.insert(at, 0xff);
    let f = parse(&body);
    assert_eq!(titles(&f), ["a"]);
    assert_eq!(f.summary.stop, Some(Stop::NotUtf8));
}

#[test]
fn malformed_input_stops_with_what_was_finished() {
    for bad in [
        format!("{}<item></channel>", item("a")),
        format!("{}<item><title>b</ti></item>", item("a")),
        format!("{}<item a=\"1\" a=\"2\">", item("a")),
        format!("{}</rss></rss>", item("a")),
    ] {
        let f = parse(feed(&bad).as_bytes());
        assert_eq!(titles(&f), ["a"], "{bad}");
        assert_eq!(f.summary.stop, Some(Stop::Malformed), "{bad}");
    }
    assert_eq!(parse(b"").summary, Default::default());
    assert_eq!(parse(b"not xml").summary.skipped, Skipped::default());
}

#[test]
fn byte_order_mark_is_fine() {
    let body = format!("\u{feff}{}", feed(&item("a")));
    let f = parse(body.as_bytes());
    assert_eq!(titles(&f), ["a"]);
    assert_eq!(f.summary.stop, None);
}
