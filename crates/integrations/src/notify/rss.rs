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
//! - no CDATA sections. The one attribute built from input is the magnet
//!   enclosure's `url`, which [`render`] assembles itself from a checked
//!   info-hash and a percent-encoded name (see [`RssMagnet`]), then
//!   escapes; `isPermaLink` is always `false`;
//! - item titles go through `clean_line`: control characters become
//!   spaces and bidirectional overrides are dropped, so a name can't
//!   break a reader's list or disguise itself;
//! - an item's `description` is plain text, but RSS readers decode it as
//!   HTML after the XML is parsed, so `&lt;img onerror=…&gt;` would become a
//!   live tag. It is HTML-escaped first and XML-escaped second, so a reader
//!   shows the markup characters literally;
//! - links must be absolute `http(s)` URLs: a feed reader opens them, so
//!   `javascript:` and the like are left out.
//!
//! **Private results stay local (§18.7):** [`render`] drops every item whose
//! [`RssItem::private_source`] is set, and [`items_from_notification`] builds
//! items from [`Notification::shareable_items`] only, so a private-source
//! item never reaches a feed whichever way the caller built its items.

use std::fmt::Write;

use super::Notification;
use reqwest::Url;

use super::text::{clean_line, format_bytes, web_link};

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
    /// Plain text. [`render`] escapes it for readers that treat a feed's
    /// description as HTML.
    pub description: String,
    /// Found only through a private source (§18.7): [`render`] leaves the
    /// item out.
    pub private_source: bool,
    /// The torrent as a magnet link, written as the item's enclosure so
    /// feed readers and download clients can fetch it.
    pub magnet: Option<RssMagnet>,
}

/// What a magnet enclosure is built from. [`render`] accepts only this
/// shape, `magnet:?xt=urn:btih:<40 hex>&dn=<percent-encoded name>`, typed
/// `application/x-bittorrent` with the torrent's size as its length; an
/// info-hash that isn't 40 hex digits leaves the enclosure out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RssMagnet {
    /// The v1 info-hash (or a v2 hash truncated to 20 bytes), as hex.
    pub info_hash: String,
    /// The display name; cleaned as a title is, then percent-encoded.
    pub name: String,
    /// Bytes.
    pub size: u64,
}

/// The magnet URL for `m`, or `None` when its hash isn't 40 hex digits.
fn magnet_url(m: &RssMagnet) -> Option<String> {
    let hash = &m.info_hash;
    if hash.len() != 40 || !hash.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let mut url = format!("magnet:?xt=urn:btih:{}&dn=", hash.to_ascii_lowercase());
    for b in clean_line(&m.name).bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            url.push(char::from(b));
        } else {
            let _ = write!(url, "%{b:02X}");
        }
    }
    Some(url)
}

/// `scheme://host[:port]` for a feed's links, from a request's `Host`
/// value: only a host name or IP address and an optional port pass, read
/// through [`Url`] (so it's normalized: lower case, IDNA, no default
/// port). Anything else (a path, credentials, a query, stray characters)
/// is `None`, and the caller falls back to a fixed origin.
pub fn feed_origin(https: bool, host: &str) -> Option<String> {
    let host = host.trim();
    let allowed = |c: char| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | ':' | '[' | ']');
    if host.is_empty() || host.len() > 255 || !host.chars().all(allowed) {
        return None;
    }
    let scheme = if https { "https" } else { "http" };
    let url = Url::parse(&format!("{scheme}://{host}/")).ok()?;
    let plain = url.username().is_empty()
        && url.password().is_none()
        && url.path() == "/"
        && url.query().is_none()
        && url.fragment().is_none();
    let name = url.host_str().filter(|h| plain && !h.is_empty())?;
    Some(match url.port() {
        Some(port) => format!("{scheme}://{name}:{port}"),
        None => format!("{scheme}://{name}"),
    })
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
            private_source: false,
            magnet: None,
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
    element(
        &mut out,
        1,
        "description",
        &html_escape(&channel.description),
    );
    element(&mut out, 1, "lastBuildDate", &rfc2822(channel.built_at));
    for item in items.iter().filter(|i| !i.private_source) {
        out.push_str("  <item>\n");
        element(&mut out, 2, "title", &clean_line(&item.title));
        if let Some(link) = item.link.as_deref().and_then(web_link) {
            element(&mut out, 2, "link", &link);
        }
        out.push_str("    <guid isPermaLink=\"false\">");
        escape_into(&mut out, &item.guid);
        out.push_str("</guid>\n");
        element(&mut out, 2, "pubDate", &rfc2822(item.published));
        element(&mut out, 2, "description", &html_escape(&item.description));
        if let Some((url, m)) = item.magnet.as_ref().and_then(|m| Some((magnet_url(m)?, m))) {
            out.push_str("    <enclosure url=\"");
            escape_into(&mut out, &url);
            let _ = writeln!(
                out,
                "\" type=\"application/x-bittorrent\" length=\"{}\"/>",
                m.size
            );
        }
        out.push_str("  </item>\n");
    }
    out.push_str("</channel>\n</rss>\n");
    Ok(out)
}

/// `s` with the characters HTML treats as markup written as entities.
fn html_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
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
            private_source: false,
            magnet: None,
        }
    }

    #[test]
    fn golden_output() {
        let mut hostile = item(1);
        hostile.title =
            "Tom & Jerry <script>alert('x')</script>\u{202E} \"quoted\"\u{0}\u{8}\r\n\tend".into();
        hostile.magnet = Some(RssMagnet {
            info_hash: "AB".repeat(20),
            name: "Tom & Jerry \"x\" <y>\r\n".into(),
            size: 1234,
        });
        let got = render(&channel(), &[item(0), hostile]).unwrap();
        let want = "\
<?xml version=\"1.0\" encoding=\"UTF-8\"?>
<rss version=\"2.0\">
<channel>
  <title>Lodestar: 4K nature docs</title>
  <link>http://lodestar.lan/searches/7</link>
  <description>New results for the saved search &amp;quot;4K nature docs&amp;quot;.</description>
  <lastBuildDate>Wed, 22 Oct 2025 12:00:00 GMT</lastBuildDate>
  <item>
    <title>Planet.Earth.III.0.2160p</title>
    <link>http://lodestar.lan/t/aaaa</link>
    <guid isPermaLink=\"false\">aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa</guid>
    <pubDate>Wed, 22 Oct 2025 12:00:00 GMT</pubDate>
    <description>Two new results (4.37\u{a0}GiB)</description>
  </item>
  <item>
    <title>Tom &amp; Jerry &lt;script&gt;alert(&apos;x&apos;)&lt;/script&gt; &quot;quoted&quot;     end</title>
    <link>http://lodestar.lan/t/aaaa</link>
    <guid isPermaLink=\"false\">aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa</guid>
    <pubDate>Wed, 22 Oct 2025 12:00:01 GMT</pubDate>
    <description>Two new results (4.37\u{a0}GiB)</description>
    <enclosure url=\"magnet:?xt=urn:btih:abababababababababababababababababababab&amp;dn=Tom%20%26%20Jerry%20%22x%22%20%3Cy%3E\" type=\"application/x-bittorrent\" length=\"1234\"/>
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
    fn markup_in_a_description_stays_text_for_html_readers() {
        let mut i = item(0);
        i.description = "<img src=x onerror=alert(1)> & <script>".into();
        let xml = render(&channel(), &[i]).unwrap();
        check_well_formed(&xml).unwrap();
        // XML-decoded once (what a feed parser hands the reader), the
        // description holds entities, not tags.
        let start = xml.rfind("<description>").unwrap() + "<description>".len();
        let end = xml.rfind("</description>").unwrap();
        let decoded = unescape(&xml[start..end]);
        assert_eq!(
            decoded,
            "&lt;img src=x onerror=alert(1)&gt; &amp; &lt;script&gt;"
        );
        assert!(!decoded.contains('<'));
    }

    #[test]
    fn private_items_are_dropped_by_the_renderer() {
        let mut private = item(1);
        private.title = "Private.Tracker.Exclusive".into();
        private.guid = "b".repeat(40);
        private.private_source = true;
        let xml = render(&channel(), &[item(0), private]).unwrap();
        assert_eq!(xml.matches("<item>").count(), 1);
        assert!(!xml.contains("Private.Tracker") && !xml.contains(&"b".repeat(40)));
        let only_private = RssItem {
            private_source: true,
            ..item(2)
        };
        assert!(
            !render(&channel(), &[only_private])
                .unwrap()
                .contains("<item>")
        );
    }

    #[test]
    fn empty_feeds_are_valid() {
        let xml = render(&channel(), &[]).unwrap();
        check_well_formed(&xml).unwrap();
        assert!(!xml.contains("<item>"));
    }

    #[test]
    fn only_the_magnet_shape_is_written() {
        let with = |hash: &str, name: &str| {
            let mut i = item(0);
            i.magnet = Some(RssMagnet {
                info_hash: hash.into(),
                name: name.into(),
                size: 7,
            });
            render(&channel(), &[i]).unwrap()
        };
        let good = with(&"0123456789abcdefABCDEF".repeat(2)[..40], "a b/é");
        assert!(good.contains(
            "<enclosure url=\"magnet:?xt=urn:btih:0123456789abcdefabcdef0123456789abcdefab&amp;dn=a%20b%2F%C3%A9\" type=\"application/x-bittorrent\" length=\"7\"/>"
        ));
        check_well_formed(&good).unwrap();
        for bad in [
            "a".repeat(39),
            "a".repeat(41),
            "g".repeat(40),
            format!("{}\"", "a".repeat(39)),
            format!("{}&x=", "a".repeat(37)),
            "é".repeat(20),
            String::new(),
        ] {
            let xml = with(&bad, "x");
            assert!(!xml.contains("enclosure"), "{bad:?}");
        }
    }

    #[test]
    fn origins_are_a_host_and_port_only() {
        for (https, host, want) in [
            (false, "lodestar.lan:8090", Some("http://lodestar.lan:8090")),
            (true, "LODESTAR.example", Some("https://lodestar.example")),
            (false, "192.168.1.5", Some("http://192.168.1.5")),
            (false, "[::1]:8090", Some("http://[::1]:8090")),
            // The scheme's default port is dropped.
            (false, "host:80", Some("http://host")),
            (true, "host:443", Some("https://host")),
            (false, "", None),
            (false, "user@host", None),
            (false, "host/path", None),
            (false, "host?x=1", None),
            (false, "host#x", None),
            (false, "host:99999", None),
            (false, "a b", None),
            (false, "host\"><x", None),
            (false, "javascript:alert(1)", None),
        ] {
            assert_eq!(feed_origin(https, host).as_deref(), want, "{host:?}");
        }
        assert!(feed_origin(false, &"a".repeat(256)).is_none());
    }

    /// An element as parsed: its name, its text and its attributes.
    type Parsed = (String, String, Vec<(String, String)>);

    /// The document as a real XML parser reads it: every element's name,
    /// its text (references resolved) and its attributes, in order. Any
    /// DOCTYPE, CDATA, comment or processing instruction is an error, so
    /// "no live markup" means exactly the renderer's own elements.
    fn parse_with_quick_xml(xml: &str) -> Result<Vec<Parsed>, String> {
        use quick_xml::Reader;
        use quick_xml::events::Event;
        let mut reader = Reader::from_str(xml);
        let mut out: Vec<Parsed> = Vec::new();
        let mut open: Vec<usize> = Vec::new();
        loop {
            let event = reader.read_event().map_err(|e| e.to_string())?;
            let element = |e: &quick_xml::events::BytesStart<'_>| -> Result<_, String> {
                let name = e.name().as_ref().to_owned();
                let mut attrs = Vec::new();
                for a in e.attributes() {
                    let a = a.map_err(|e| e.to_string())?;
                    let k = a.key.as_ref().to_owned();
                    let v = a
                        .normalized_value(quick_xml::XmlVersion::Implicit1_0)
                        .map_err(|e| e.to_string())?
                        .into_owned();
                    attrs.push((k, v));
                }
                Ok((name, String::new(), attrs))
            };
            match event {
                Event::Decl(_) => {}
                Event::Start(e) => {
                    out.push(element(&e)?);
                    open.push(out.len() - 1);
                }
                Event::Empty(e) => out.push(element(&e)?),
                Event::End(_) => {
                    open.pop().ok_or("unbalanced end")?;
                }
                Event::Text(t) => {
                    let text = t.xml10_content().into_owned();
                    if let Some(&i) = open.last() {
                        out[i].1.push_str(&text);
                    } else if !text.trim().is_empty() {
                        return Err("text outside the root".into());
                    }
                }
                Event::GeneralRef(r) => {
                    let c = match r.resolve_char_ref().map_err(|e| e.to_string())? {
                        Some(c) => c.to_string(),
                        None => match &*r {
                            "amp" => "&".into(),
                            "lt" => "<".into(),
                            "gt" => ">".into(),
                            "quot" => "\"".into(),
                            "apos" => "'".into(),
                            other => return Err(format!("unknown entity {other}")),
                        },
                    };
                    let i = *open.last().ok_or("a reference outside the root")?;
                    out[i].1.push_str(&c);
                }
                Event::Eof => break,
                other => return Err(format!("unexpected {other:?}")),
            }
        }
        if open.is_empty() {
            Ok(out)
        } else {
            Err("unclosed element".into())
        }
    }

    /// Percent-decodes a magnet's `dn`.
    fn percent_decode(s: &str) -> String {
        let b = s.as_bytes();
        let mut out = Vec::new();
        let mut i = 0;
        while i < b.len() {
            if b[i] == b'%' {
                out.push(u8::from_str_radix(&s[i + 1..i + 3], 16).unwrap());
                i += 3;
            } else {
                out.push(b[i]);
                i += 1;
            }
        }
        String::from_utf8(out).unwrap()
    }

    fn hostile_name() -> impl Strategy<Value = String> {
        prop::collection::vec(
            prop_oneof![
                3 => any::<char>(),
                3 => prop::sample::select(vec![
                    '&', '<', '>', '"', '\'', '\r', '\n', '\t', '\0', '\u{1}', '\u{1b}',
                    '\u{7f}', '\u{85}', '\u{202e}', '\u{2066}', '\u{200f}', '\u{fffe}', '%',
                    '=', '?', ']', ';', '#', '/', ' ',
                ]),
            ],
            0..48,
        )
        .prop_map(|v| v.into_iter().collect::<String>())
        .prop_map(|s| {
            // A CDATA terminator and a fake tag, somewhere in most names.
            if s.len() % 3 == 0 {
                format!("{s}]]><x>{s}")
            } else {
                s
            }
        })
    }

    proptest! {
        /// Arbitrary torrent names (markup, controls, CR, bidi, `]]>`)
        /// give XML a real parser reads back with the name intact (as
        /// `clean_line` leaves it), in the title and the magnet's `dn`,
        /// and no element the renderer didn't write.
        #[test]
        fn hostile_names_parse_back_intact(name in hostile_name(), size in any::<u64>()) {
            let i = RssItem {
                title: name.clone(),
                link: Some("http://lodestar.lan/t/aaaa".into()),
                guid: "ab".repeat(20),
                published: 0,
                description: name.clone(),
                private_source: false,
                magnet: Some(RssMagnet { info_hash: "ab".repeat(20), name: name.clone(), size }),
            };
            let xml = render(&channel(), &[i]).unwrap();
            let parsed = parse_with_quick_xml(&xml).map_err(|e| TestCaseError::fail(format!("{e}\n{xml}")))?;
            let names: Vec<&str> = parsed.iter().map(|(n, _, _)| n.as_str()).collect();
            prop_assert_eq!(
                names,
                ["rss", "channel", "title", "link", "description", "lastBuildDate",
                 "item", "title", "link", "guid", "pubDate", "description", "enclosure"]
            );
            let want = expected(&clean_line(&name));
            prop_assert_eq!(&parsed[7].1, &want);
            // The description: one more HTML layer, which a reader undoes.
            prop_assert_eq!(html_unescape(&parsed[11].1), expected(&name));
            prop_assert!(!parsed[11].1.contains('<'));
            let attrs = &parsed[12].2;
            prop_assert_eq!(attrs.len(), 3);
            prop_assert_eq!(&attrs[1], &("type".to_string(), "application/x-bittorrent".to_string()));
            prop_assert_eq!(&attrs[2], &("length".to_string(), size.to_string()));
            let url = &attrs[0].1;
            let dn = url
                .strip_prefix(&format!("magnet:?xt=urn:btih:{}&dn=", "ab".repeat(20)))
                .ok_or_else(|| TestCaseError::fail(url.clone()))?;
            prop_assert!(dn.bytes().all(|b| b.is_ascii_alphanumeric() || b"-._~%".contains(&b)));
            prop_assert_eq!(percent_decode(dn), clean_line(&name));
        }
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
                // `<enclosure …/>` is the one empty element.
                let (tag, empty) = match tag.strip_suffix('/') {
                    Some(t) => (t, true),
                    None => (tag, false),
                };
                let name = tag.split(' ').next().unwrap_or("");
                if name.is_empty() || tag.starts_with('!') || (empty && name != "enclosure") {
                    return Err(format!("unexpected tag <{tag}>"));
                }
                // Attribute values: quoted, and holding no raw `<`, `&`
                // or quote.
                if let Some((_, attrs)) = tag.split_once(' ') {
                    for attr in attrs.split(' ') {
                        let (k, v) = attr.split_once('=').ok_or("bare attribute")?;
                        let v = v.strip_prefix('"').and_then(|v| v.strip_suffix('"'));
                        let v = v.ok_or("unquoted attribute")?;
                        if k.is_empty() || v.contains('"') {
                            return Err(format!("bad attribute {attr}"));
                        }
                        check_text(v)?;
                    }
                }
                if !empty {
                    stack.push(name);
                }
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

    /// What an HTML reader makes of the description after the XML layer.
    fn html_unescape(s: &str) -> String {
        s.replace("&lt;", "<")
            .replace("&gt;", ">")
            .replace("&quot;", "\"")
            .replace("&#39;", "'")
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
                private_source: false,
                magnet: None,
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
            prop_assert_eq!(text_of("title", 1), expected(&clean_line(&title)));
            prop_assert_eq!(text_of("guid", 0), expected(&guid));
            // A reader decodes the XML, then the HTML: both layers undone
            // give back the input.
            let shown = text_of("description", 1);
            prop_assert_eq!(&shown, &html_escape(&expected(&description)));
            prop_assert_eq!(html_unescape(&shown), expected(&description));
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
            "<a b=\"x & y\"></a>",
            "<a/>",
            "<a><![CDATA[x]]></a>",
        ] {
            assert!(check_well_formed(bad).is_err(), "{bad}");
        }
        assert!(check_well_formed("<?xml version=\"1.0\"?><a b=\"c\">x &amp; y</a>").is_ok());
    }
}
