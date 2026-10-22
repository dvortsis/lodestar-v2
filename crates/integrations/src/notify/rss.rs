//! The RSS 2.0 renderer (no network).
//!
//! A pure function from a channel and its items to an XML document; the
//! API's per-saved-search feed serves the result. Torrent names are
//! attacker-controlled, so **escaping is the security boundary**:
//!
//! - every text node is escaped (`& < > " '`), and a carriage return is
//!   written as `&#xD;` so parsers don't fold it into a newline;
//! - characters XML 1.0 forbids (C0 controls other than tab, newline and
//!   carriage return, and U+FFFE and U+FFFF) are dropped. Lone surrogates
//!   can't occur in a Rust string. Nothing else is changed;
//! - no CDATA sections, and no attribute takes input (`isPermaLink` is
//!   always `false`);
//! - links must be absolute `http(s)` URLs: a feed reader opens them, so
//!   `javascript:` and the like are left out.
//!
//! **Private results stay local (§18.7):** [`items_from_notification`] builds
//! items from [`Notification::shareable_items`] only, so a private-source
//! item never reaches a feed.

use std::fmt::Write;

use reqwest::Url;

use super::Notification;
use super::ntfy::{clean_line, format_bytes};

/// What the renderer can't repair.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RssError {
    /// The channel's link isn't an absolute `http(s)` URL; RSS requires one.
    InvalidLink,
}

impl std::fmt::Display for RssError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RssError::InvalidLink => f.write_str("the feed's link isn't an http(s) URL"),
        }
    }
}

impl std::error::Error for RssError {}

/// The feed's own fields.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RssChannel {
    pub title: String,
    /// Where the feed's subject lives: the saved search in the web UI.
    pub link: String,
    pub description: String,
    /// `lastBuildDate`, Unix seconds. The caller supplies it, so the output
    /// depends only on the input.
    pub built_at: u64,
}

/// One feed item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RssItem {
    pub title: String,
    /// Left out when it isn't an absolute `http(s)` URL.
    pub link: Option<String>,
    /// The item's stable ID, so readers show it once: the info-hash for a
    /// torrent. Never a permalink.
    pub guid: String,
    /// Unix seconds.
    pub published: u64,
    pub description: String,
}

/// The items of `n` a feed may carry: shareable ones only, each with its
/// info-hash as the guid, the notification's time and link, and its size.
pub fn items_from_notification(n: &Notification) -> Vec<RssItem> {
    n.shareable_items()
        .map(|item| RssItem {
            title: clean_line(&item.name),
            link: n.link.clone(),
            guid: item.info_hash.clone(),
            published: n.created_at,
            description: match item.size_bytes {
                Some(size) => format!("{} ({})", clean_line(&n.title), format_bytes(size)),
                None => clean_line(&n.title),
            },
        })
        .collect()
}

/// Renders an RSS 2.0 document.
pub fn render(channel: &RssChannel, items: &[RssItem]) -> Result<String, RssError> {
    let link = web_link(&channel.link).ok_or(RssError::InvalidLink)?;
    let mut out = String::with_capacity(512 + items.len() * 384);
    out.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    out.push_str("<rss version=\"2.0\">\n<channel>\n");
    element(&mut out, 1, "title", &channel.title);
    element(&mut out, 1, "link", &link);
    element(&mut out, 1, "description", &channel.description);
    element(&mut out, 1, "lastBuildDate", &rfc2822(channel.built_at));
    for item in items {
        out.push_str("  <item>\n");
        element(&mut out, 2, "title", &item.title);
        if let Some(link) = item.link.as_deref().and_then(web_link) {
            element(&mut out, 2, "link", &link);
        }
        out.push_str("    <guid isPermaLink=\"false\">");
        escape_into(&mut out, &item.guid);
        out.push_str("</guid>\n");
        element(&mut out, 2, "pubDate", &rfc2822(item.published));
        element(&mut out, 2, "description", &item.description);
        out.push_str("  </item>\n");
    }
    out.push_str("</channel>\n</rss>\n");
    Ok(out)
}

fn element(out: &mut String, depth: usize, name: &str, text: &str) {
    for _ in 0..depth {
        out.push_str("  ");
    }
    let _ = write!(out, "<{name}>");
    escape_into(out, text);
    let _ = writeln!(out, "</{name}>");
}

/// Whether XML 1.0 allows `c` in a document. (Rust strings hold no
/// surrogates, so those are covered.)
fn is_xml_char(c: char) -> bool {
    matches!(c, '\t' | '\n' | '\r' | '\u{20}'..='\u{D7FF}' | '\u{E000}'..='\u{FFFD}' | '\u{10000}'..)
}

/// Appends `s` as XML text, dropping what XML forbids.
pub fn escape_into(out: &mut String, s: &str) {
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            '\r' => out.push_str("&#xD;"),
            c if is_xml_char(c) => out.push(c),
            _ => {}
        }
    }
}

fn web_link(link: &str) -> Option<String> {
    let url = Url::parse(link.trim()).ok()?;
    let web = matches!(url.scheme(), "http" | "https") && url.host_str().is_some();
    web.then(|| url.to_string())
}

/// `Thu, 01 Jan 1970 00:00:00 GMT` for Unix seconds.
fn rfc2822(secs: u64) -> String {
    const DAYS: [&str; 7] = ["Thu", "Fri", "Sat", "Sun", "Mon", "Tue", "Wed"];
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    // Years past 9999 aren't valid RFC 2822; clamp rather than overflow.
    let secs = secs.min(253_402_300_799);
    let days = secs / 86_400;
    let rem = secs % 86_400;
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{}, {day:02} {} {year:04} {:02}:{:02}:{:02} GMT",
        DAYS[(days % 7) as usize],
        MONTHS[(month - 1) as usize],
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::notify::{NotificationItem, NotificationKind};
    use proptest::prelude::*;

    fn channel() -> RssChannel {
        RssChannel {
            title: "Lodestar: 4K nature docs".into(),
            link: "http://lodestar.lan/searches/7".into(),
            description: "New results for the saved search \"4K nature docs\".".into(),
            built_at: 1_761_134_400,
        }
    }

    fn item(i: u64) -> RssItem {
        RssItem {
            title: format!("Planet.Earth.III.{i}.2160p"),
            link: Some("http://lodestar.lan/t/aaaa".into()),
            guid: "a".repeat(40),
            published: 1_761_134_400 + i,
            description: "Two new results (4.37\u{a0}GiB)".into(),
        }
    }

    #[test]
    fn golden_output() {
        let mut hostile = item(1);
        hostile.title = "Tom & Jerry <script>alert('x')</script> \"quoted\"\u{0}\u{8}\r\n\t".into();
        let got = render(&channel(), &[item(0), hostile]).unwrap();
        let want = "\
<?xml version=\"1.0\" encoding=\"UTF-8\"?>
<rss version=\"2.0\">
<channel>
  <title>Lodestar: 4K nature docs</title>
  <link>http://lodestar.lan/searches/7</link>
  <description>New results for the saved search &quot;4K nature docs&quot;.</description>
  <lastBuildDate>Wed, 22 Oct 2025 12:00:00 GMT</lastBuildDate>
  <item>
    <title>Planet.Earth.III.0.2160p</title>
    <link>http://lodestar.lan/t/aaaa</link>
    <guid isPermaLink=\"false\">aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa</guid>
    <pubDate>Wed, 22 Oct 2025 12:00:00 GMT</pubDate>
    <description>Two new results (4.37\u{a0}GiB)</description>
  </item>
  <item>
    <title>Tom &amp; Jerry &lt;script&gt;alert(&apos;x&apos;)&lt;/script&gt; &quot;quoted&quot;&#xD;
\t</title>
    <link>http://lodestar.lan/t/aaaa</link>
    <guid isPermaLink=\"false\">aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa</guid>
    <pubDate>Wed, 22 Oct 2025 12:00:01 GMT</pubDate>
    <description>Two new results (4.37\u{a0}GiB)</description>
  </item>
</channel>
</rss>
";
        assert_eq!(got, want);
    }

    #[test]
    fn dates_are_rfc_2822() {
        for (secs, want) in [
            (0, "Thu, 01 Jan 1970 00:00:00 GMT"),
            (951_782_400, "Tue, 29 Feb 2000 00:00:00 GMT"),
            (1_709_164_799, "Wed, 28 Feb 2024 23:59:59 GMT"),
            (1_709_164_800, "Thu, 29 Feb 2024 00:00:00 GMT"),
            (1_709_251_200, "Fri, 01 Mar 2024 00:00:00 GMT"),
            (1_761_134_400, "Wed, 22 Oct 2025 12:00:00 GMT"),
            (4_102_444_800, "Fri, 01 Jan 2100 00:00:00 GMT"),
            (u64::MAX, "Fri, 31 Dec 9999 23:59:59 GMT"),
        ] {
            assert_eq!(rfc2822(secs), want, "{secs}");
        }
    }

    #[test]
    fn bad_links_are_left_out_and_the_channel_needs_one() {
        let mut bad = item(0);
        for link in [
            "javascript:alert(1)",
            "data:text/html,x",
            "/relative",
            "",
            "file:///etc/passwd",
        ] {
            bad.link = Some(link.into());
            let xml = render(&channel(), &[bad.clone()]).unwrap();
            assert!(!xml.contains("<link>javascript") && !xml.contains("<link>data"));
            // Only the channel's own link remains.
            assert_eq!(xml.matches("<link>").count(), 1, "{link}");
        }
        for link in ["javascript:alert(1)", "", "not a url", "ftp://x.example/"] {
            let mut c = channel();
            c.link = link.into();
            assert_eq!(render(&c, &[]), Err(RssError::InvalidLink), "{link}");
        }
    }

    #[test]
    fn private_items_never_reach_a_feed() {
        let mut n = Notification::new(NotificationKind::Alert, "Alert", "body");
        n.items = vec![
            NotificationItem {
                name: "Public.One".into(),
                info_hash: "a".repeat(40),
                size_bytes: Some(1024),
                private_source: false,
            },
            NotificationItem {
                name: "Private.Tracker.Exclusive".into(),
                info_hash: "b".repeat(40),
                size_bytes: None,
                private_source: true,
            },
        ];
        let items = items_from_notification(&n);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].title, "Public.One");
        assert_eq!(items[0].guid, "a".repeat(40));
        assert_eq!(items[0].published, n.created_at);
        let xml = render(&channel(), &items).unwrap();
        assert!(!xml.contains("Private.Tracker") && !xml.contains(&"b".repeat(40)));
        assert!(xml.contains("Public.One"));
    }

    #[test]
    fn empty_feeds_are_valid() {
        let xml = render(&channel(), &[]).unwrap();
        check_well_formed(&xml).unwrap();
        assert!(!xml.contains("<item>"));
    }

    // A minimal well-formedness check, written here so the test doesn't
    // rely on the renderer's own idea of XML: tags balance, text holds no
    // raw `<` and only the five named entities and `&#xD;` after `&`, and
    // no character XML 1.0 forbids is in the document.
    fn check_well_formed(xml: &str) -> Result<(), String> {
        // Spelled out from the XML 1.0 `Char` production, not shared with
        // the renderer.
        let forbidden = |c: char| {
            (c < ' ' && !matches!(c, '\t' | '\n' | '\r')) || c == '\u{fffe}' || c == '\u{ffff}'
        };
        if let Some(c) = xml.chars().find(|&c| forbidden(c)) {
            return Err(format!("forbidden character {c:?}"));
        }
        let mut stack: Vec<&str> = Vec::new();
        let mut rest = xml;
        while let Some(lt) = rest.find('<') {
            check_text(&rest[..lt])?;
            let gt = rest[lt..].find('>').ok_or("unterminated tag")? + lt;
            let tag = &rest[lt + 1..gt];
            if tag.contains('<') {
                return Err("`<` inside a tag".into());
            }
            if tag.starts_with('?') {
                // the XML declaration
            } else if let Some(name) = tag.strip_prefix('/') {
                if stack.pop() != Some(name) {
                    return Err(format!("unbalanced </{name}>"));
                }
            } else {
                let name = tag.split(' ').next().unwrap_or("");
                if name.is_empty() || tag.ends_with('/') || tag.starts_with('!') {
                    return Err(format!("unexpected tag <{tag}>"));
                }
                // Attribute values: quoted, and holding no raw `<`, `&`
                // or quote.
                if let Some((_, attrs)) = tag.split_once(' ') {
                    for attr in attrs.split(' ') {
                        let (k, v) = attr.split_once('=').ok_or("bare attribute")?;
                        let v = v.strip_prefix('"').and_then(|v| v.strip_suffix('"'));
                        let v = v.ok_or("unquoted attribute")?;
                        if k.is_empty() || v.contains(['"', '&']) {
                            return Err(format!("bad attribute {attr}"));
                        }
                    }
                }
                stack.push(name);
            }
            rest = &rest[gt + 1..];
        }
        check_text(rest)?;
        if stack.is_empty() {
            Ok(())
        } else {
            Err(format!("unclosed {stack:?}"))
        }
    }

    fn check_text(text: &str) -> Result<(), String> {
        if text.contains(['<', '>']) {
            return Err(format!("raw angle bracket in {text:?}"));
        }
        let mut parts = text.split('&');
        parts.next();
        for after in parts {
            let ok = ["amp;", "lt;", "gt;", "quot;", "apos;", "#xD;"]
                .iter()
                .any(|e| after.starts_with(e));
            if !ok {
                return Err(format!("bare ampersand before {after:?}"));
            }
        }
        Ok(())
    }

    /// Decodes what `escape_into` produces.
    fn unescape(s: &str) -> String {
        s.replace("&lt;", "<")
            .replace("&gt;", ">")
            .replace("&quot;", "\"")
            .replace("&apos;", "'")
            .replace("&#xD;", "\r")
            .replace("&amp;", "&")
    }

    /// What `escape_into` is documented to keep.
    fn expected(s: &str) -> String {
        s.chars().filter(|&c| is_xml_char(c)).collect()
    }

    fn any_text() -> impl Strategy<Value = String> {
        // Mostly-arbitrary strings, with the troublesome characters
        // over-represented.
        prop::collection::vec(
            prop_oneof![
                4 => any::<char>(),
                3 => prop::sample::select(vec![
                    '&', '<', '>', '"', '\'', '\r', '\n', '\t', '\0', '\u{1}', '\u{8}',
                    '\u{b}', '\u{c}', '\u{1f}', '\u{7f}', '\u{85}', '\u{fffe}', '\u{ffff}',
                    '\u{fffd}', '\u{feff}', '\u{10000}', '\u{10ffff}', ']', ';', '#',
                ]),
            ],
            0..40,
        )
        .prop_map(|v| v.into_iter().collect())
    }

    proptest! {
        #[test]
        fn any_text_yields_well_formed_xml_that_keeps_every_legal_character(
            title in any_text(),
            description in any_text(),
            guid in any_text(),
            link in any_text(),
            channel_title in any_text(),
        ) {
            let c = RssChannel { title: channel_title.clone(), ..channel() };
            let i = RssItem {
                title: title.clone(),
                link: Some(link),
                guid: guid.clone(),
                published: 0,
                description: description.clone(),
            };
            let xml = render(&c, &[i]).unwrap();
            prop_assert_eq!(check_well_formed(&xml), Ok(()));

            // Every character either survives or is a documented drop.
            let text_of = |tag: &str, nth: usize| -> String {
                let open = format!("<{tag}");
                let (start, _) = xml.match_indices(&open).nth(nth).unwrap();
                let from = xml[start..].find('>').unwrap() + start + 1;
                let to = xml[from..].find(&format!("</{tag}>")).unwrap() + from;
                unescape(&xml[from..to])
            };
            prop_assert_eq!(text_of("title", 0), expected(&channel_title));
            prop_assert_eq!(text_of("title", 1), expected(&title));
            prop_assert_eq!(text_of("guid", 0), expected(&guid));
            prop_assert_eq!(text_of("description", 1), expected(&description));
        }

        #[test]
        fn escape_never_emits_markup_or_forbidden_characters(s in any_text()) {
            let mut out = String::new();
            escape_into(&mut out, &s);
            prop_assert!(!out.contains(['<', '>', '"', '\'']));
            prop_assert!(!out.contains('\r'));
            prop_assert!(out.chars().all(is_xml_char));
            prop_assert_eq!(unescape(&out), expected(&s));
        }
    }

    #[test]
    fn the_checker_itself_rejects_bad_xml() {
        for bad in [
            "<a>x < y</a>",
            "<a>x & y</a>",
            "<a><b></a></b>",
            "<a>",
            "<a>\u{1}</a>",
            "<a b=c></a>",
            "<a><![CDATA[x]]></a>",
        ] {
            assert!(check_well_formed(bad).is_err(), "{bad}");
        }
        assert!(check_well_formed("<?xml version=\"1.0\"?><a b=\"c\">x &amp; y</a>").is_ok());
    }
}
