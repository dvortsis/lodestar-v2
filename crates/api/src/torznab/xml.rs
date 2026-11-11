//! A small XML writer for the Torznab documents: element and attribute
//! names are this module's callers' own constants, and every value
//! (names, file names, categories, numbers) goes through [`escape`].
//!
//! **Never invalid for a hostile title:**
//! - `&`, `<`, `>`, `"` and `'` are escaped in text and attributes alike,
//!   so `]]>` and markup in a name stay text (no CDATA is written);
//! - control characters (Unicode `Cc`, C0 and C1) are removed; tab, line
//!   feed and carriage return become a space, so a title stays one line;
//! - U+FFFE and U+FFFF, which XML 1.0 doesn't allow, are removed, and so
//!   are the bidirectional overrides and isolates (U+202A–U+202E,
//!   U+2066–U+2069) and the byte-order mark, so a name can't display
//!   reversed or hide text in an *arr app;
//! - names arrive as Rust strings, with invalid UTF-8 already replaced by
//!   the store, so no other character XML forbids can reach here.
//!
//! Open elements are tracked, so a document is closed exactly as it was
//! opened ([`Xml::finish`] closes what's left).

/// An XML document being written.
#[derive(Debug)]
pub struct Xml {
    out: String,
    open: Vec<&'static str>,
}

impl Default for Xml {
    fn default() -> Self {
        Xml::new()
    }
}

impl Xml {
    /// A document, with its declaration.
    pub fn new() -> Xml {
        Xml {
            out: String::from("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n"),
            open: Vec::new(),
        }
    }

    fn start(&mut self, name: &'static str, attrs: &[(&'static str, &str)]) {
        debug_assert!(is_name(name), "{name}");
        self.out.push('<');
        self.out.push_str(name);
        for (k, v) in attrs {
            debug_assert!(is_name(k), "{k}");
            self.out.push(' ');
            self.out.push_str(k);
            self.out.push_str("=\"");
            escape(&mut self.out, v);
            self.out.push('"');
        }
    }

    /// Opens `name`; close it with [`Xml::close`].
    pub fn open(&mut self, name: &'static str, attrs: &[(&'static str, &str)]) -> &mut Self {
        self.start(name, attrs);
        self.out.push('>');
        self.open.push(name);
        self
    }

    /// Closes the innermost open element.
    pub fn close(&mut self) -> &mut Self {
        if let Some(name) = self.open.pop() {
            self.out.push_str("</");
            self.out.push_str(name);
            self.out.push('>');
        }
        self
    }

    /// `<name …/>`.
    pub fn empty(&mut self, name: &'static str, attrs: &[(&'static str, &str)]) -> &mut Self {
        self.start(name, attrs);
        self.out.push_str("/>");
        self
    }

    /// `<name …>text</name>`.
    pub fn text(
        &mut self,
        name: &'static str,
        attrs: &[(&'static str, &str)],
        text: &str,
    ) -> &mut Self {
        self.start(name, attrs);
        self.out.push('>');
        escape(&mut self.out, text);
        self.out.push_str("</");
        self.out.push_str(name);
        self.out.push('>');
        self
    }

    /// The document, every open element closed.
    pub fn finish(mut self) -> String {
        while !self.open.is_empty() {
            self.close();
        }
        self.out.push('\n');
        self.out
    }
}

/// Whether `name` is a plain XML name (ASCII letters, digits, `-`, `_`,
/// and one `prefix:`), which every name here is.
fn is_name(name: &str) -> bool {
    let mut parts = name.splitn(2, ':');
    parts.all(|p| {
        p.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
            && p.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    })
}

/// Appends `s` to `out` as XML text or attribute content.
pub fn escape(out: &mut String, s: &str) {
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            '\t' | '\n' | '\r' => out.push(' '),
            c if c.is_control() => {}
            '\u{FFFE}' | '\u{FFFF}' | '\u{FEFF}' => {}
            '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}' => {}
            c => out.push(c),
        }
    }
}

/// `t` (unix seconds, UTC) as RFC 822 (`Tue, 14 Nov 2023 22:13:20 +0000`),
/// RSS's `pubDate`.
pub fn rfc822(t: i64) -> String {
    const DAYS: [&str; 7] = ["Thu", "Fri", "Sat", "Sun", "Mon", "Tue", "Wed"];
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let days = t.div_euclid(86_400);
    let secs = t.rem_euclid(86_400);
    // Howard Hinnant's civil_from_days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{}, {d:02} {} {y:04} {:02}:{:02}:{:02} +0000",
        DAYS[days.rem_euclid(7) as usize],
        MONTHS[(m - 1) as usize],
        secs / 3600,
        secs / 60 % 60,
        secs % 60
    )
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    fn esc(s: &str) -> String {
        let mut out = String::new();
        escape(&mut out, s);
        out
    }

    #[test]
    fn markup_and_cdata_ends_stay_text() {
        assert_eq!(
            esc("a]]><b x='1'>&amp;\""),
            "a]]&gt;&lt;b x=&apos;1&apos;&gt;&amp;amp;&quot;"
        );
    }

    #[test]
    fn controls_and_nonchars_are_removed() {
        assert_eq!(esc("a\u{0}b\u{1b}[31mc\u{7f}\u{85}\u{9f}d"), "ab[31mcd");
        assert_eq!(esc("one\ttwo\nthree\r"), "one two three ");
        assert_eq!(esc("x\u{FFFE}\u{FFFF}y\u{FFFD}"), "xy\u{FFFD}");
        assert_eq!(esc("a\u{202E}b\u{2066}c\u{2069}d\u{FEFF}e"), "abcde");
        // Other non-ASCII text is kept as it is.
        assert_eq!(esc("Ünïcödé 日本 🎬"), "Ünïcödé 日本 🎬");
    }

    #[test]
    fn documents_close_what_they_open() {
        let mut x = Xml::new();
        x.open("rss", &[("version", "2.0")]).open("channel", &[]);
        x.text("title", &[], "<t>")
            .empty("torznab:attr", &[("name", "a&b")]);
        let doc = x.finish();
        assert_eq!(
            doc,
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<rss version=\"2.0\"><channel>\
             <title>&lt;t&gt;</title><torznab:attr name=\"a&amp;b\"/></channel></rss>\n"
        );
    }

    #[test]
    fn names_are_plain() {
        assert!(is_name("torznab:attr") && is_name("tv-search") && is_name("a1"));
        assert!(!is_name("") && !is_name("1a") && !is_name("a b") && !is_name("a:b:c"));
    }

    #[test]
    fn dates_are_rfc_822_in_utc() {
        assert_eq!(rfc822(0), "Thu, 01 Jan 1970 00:00:00 +0000");
        assert_eq!(rfc822(1_700_000_000), "Tue, 14 Nov 2023 22:13:20 +0000");
        assert_eq!(rfc822(951_782_400), "Tue, 29 Feb 2000 00:00:00 +0000");
        assert_eq!(rfc822(-1), "Wed, 31 Dec 1969 23:59:59 +0000");
    }

    /// What escaped text must never hold.
    fn forbidden(c: char) -> bool {
        c.is_control() || c == '\u{FFFE}' || c == '\u{FFFF}'
    }

    proptest! {
        /// Whatever the text, its escaped form holds only characters XML
        /// 1.0 allows, no markup and no control characters, and no `&`
        /// that doesn't start one of the five entities.
        #[test]
        fn escaped_text_is_always_safe(s in any::<String>()) {
            let out = esc(&s);
            prop_assert!(!out.contains(['<', '>', '"', '\'']));
            prop_assert!(!out.chars().any(forbidden));
            for (i, _) in out.match_indices('&') {
                let rest = &out[i..];
                prop_assert!(["&amp;", "&lt;", "&gt;", "&quot;", "&apos;"]
                    .iter()
                    .any(|e| rest.starts_with(e)));
            }
        }
    }
}
