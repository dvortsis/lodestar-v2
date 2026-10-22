//! Text helpers every channel shares: one-line and multi-line cleaning of
//! untrusted names, the body-plus-items composition that keeps the
//! withheld-private note through truncation (§18.7), the truncation
//! helpers, sizes per `docs/metrics.md`, and the link check.

use reqwest::Url;

use super::Notification;

/// Items listed in the message before "and N more".
pub(crate) const MAX_LISTED_ITEMS: usize = 20;

/// The body, the shareable items and the closing notes of `n`, within
/// `budget` as `len` measures it. The notes (how many items were left out
/// as private, and how many more there were) are never cut: when the text
/// is too long, only the body and the list are truncated, with `truncate`.
/// `escape` is applied to every piece of untrusted text.
pub(crate) fn compose(
    n: &Notification,
    escape: fn(&str) -> String,
    budget: usize,
    len: fn(&str) -> usize,
    truncate: fn(&str, usize) -> String,
) -> String {
    let mut text = escape(&clean_text(&n.body));
    let mut listed = 0;
    let mut more = 0;
    for item in n.shareable_items() {
        if listed == MAX_LISTED_ITEMS {
            more += 1;
            continue;
        }
        if listed == 0 && !text.is_empty() {
            text.push_str("\n\n");
        } else if listed > 0 {
            text.push('\n');
        }
        text.push_str("• ");
        text.push_str(&escape(&clean_line(&item.name)));
        if let Some(size) = item.size_bytes {
            text.push_str(" (");
            text.push_str(&format_bytes(size));
            text.push(')');
        }
        listed += 1;
    }
    let mut notes = Vec::new();
    if more > 0 {
        notes.push(format!("…and {more} more"));
    }
    match n.withheld() {
        0 => {}
        1 => notes.push("1 result from a private source isn't included.".to_owned()),
        w => notes.push(format!("{w} results from private sources aren't included.")),
    }
    let notes = notes.join("\n\n");
    if notes.is_empty() {
        return truncate(&text, budget);
    }
    // Reserve room for the notes (and the blank line before them) first.
    let reserved = len(&notes) + len("\n\n");
    let text = truncate(&text, budget.saturating_sub(reserved));
    if text.is_empty() {
        notes
    } else {
        format!("{text}\n\n{notes}")
    }
}

/// `link` as an absolute `http(s)` URL, or `None`: a `Click` header opens
/// whatever it holds, so `javascript:` and the like are never passed on.
pub(crate) fn web_link(link: &str) -> Option<String> {
    let url = Url::parse(link.trim()).ok()?;
    let web = matches!(url.scheme(), "http" | "https") && url.host_str().is_some();
    web.then(|| url.to_string())
}

/// `s` on one line: control characters become spaces, and the bidirectional
/// overrides that could disguise a name are dropped.
pub(crate) fn clean_line(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if is_bidi_control(c) {
            continue;
        }
        out.push(if c.is_control() { ' ' } else { c });
    }
    out.trim().to_owned()
}

/// `s` with newlines kept and every other control character dropped.
pub(crate) fn clean_text(s: &str) -> String {
    let s = s.replace("\r\n", "\n");
    let cleaned: String = s
        .chars()
        .filter(|&c| c == '\n' || !(c.is_control() || is_bidi_control(c)))
        .collect();
    cleaned.trim().to_owned()
}

fn is_bidi_control(c: char) -> bool {
    matches!(c, '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}')
}

/// At most `max` bytes, cut at a character boundary, ending in `…` when
/// anything was cut.
pub(crate) fn truncate_bytes(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_owned();
    }
    let mut end = max.saturating_sub('…'.len_utf8());
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &s[..end])
}

/// At most `max` characters, ending in `…` when anything was cut.
pub(crate) fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_owned();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// Sizes the way `docs/metrics.md` writes them: IEC units, two digits under
/// 10, one under 100, none from 100, joined to the unit by U+00A0.
pub(crate) fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 6] = ["B", "KiB", "MiB", "GiB", "TiB", "PiB"];
    if bytes < 1024 {
        return format!("{bytes}\u{a0}B");
    }
    let mut unit = 1;
    while unit < 5 && bytes >= 1u64 << (10 * (unit + 1)) {
        unit += 1;
    }
    loop {
        let div = 1u128 << (10 * unit);
        let b = u128::from(bytes);
        let digits: u32 = if b < 10 * div {
            2
        } else if b < 100 * div {
            1
        } else {
            0
        };
        let scale = 10u128.pow(digits);
        // Round half up, in integers, so ties don't depend on float
        // representation.
        let scaled = (b * scale * 2 + div) / (div * 2);
        if scaled >= 1024 * scale && unit < 5 {
            unit += 1;
            continue;
        }
        let whole = scaled / scale;
        return if digits == 0 {
            format!("{whole}\u{a0}{}", UNITS[unit])
        } else {
            let frac = scaled % scale;
            format!(
                "{whole}.{frac:0width$}\u{a0}{}",
                UNITS[unit],
                width = digits as usize
            )
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::notify::{NotificationItem, NotificationKind};

    #[test]
    fn lines_and_texts_lose_controls_and_bidi_overrides() {
        assert_eq!(
            clean_line(" a\r\nb\u{0}c\u{202e}d "),
            "a  b c d".replace("c d", "cd")
        );
        assert_eq!(clean_text("a\r\nb\u{7}\u{2066}c\n"), "a\nbc");
    }

    #[test]
    fn truncation_keeps_whole_characters() {
        assert_eq!(truncate_chars("abcdef", 6), "abcdef");
        assert_eq!(truncate_chars("日本語日本語", 3), "日本…");
        assert_eq!(truncate_bytes("日本語", 7), "日…");
    }

    #[test]
    fn compose_escapes_untrusted_text_but_keeps_the_notes() {
        let mut n = Notification::new(NotificationKind::Alert, "t", "<b>");
        n.items = vec![
            NotificationItem {
                name: "<i>".into(),
                info_hash: "a".repeat(40),
                size_bytes: None,
                private_source: false,
            },
            NotificationItem {
                name: "secret".into(),
                info_hash: "b".repeat(40),
                size_bytes: None,
                private_source: true,
            },
        ];
        let upper = |s: &str| s.to_uppercase();
        let text = compose(&n, upper, 1000, str::len, truncate_bytes);
        assert_eq!(
            text,
            "<B>\n\n• <I>\n\n1 result from a private source isn't included."
        );
        let short = compose(&n, upper, 20, str::len, truncate_bytes);
        assert!(short.ends_with("isn't included."), "{short}");
    }
}
