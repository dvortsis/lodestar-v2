//! How a line looks on each output: text (the console's default) or JSON
//! (the file's, and the console's option). JSON marks every field with its
//! kind, `{"kind": "name", "value": "[hidden]"}`, so B-log2's `redact=true`
//! can strip the identifying ones from a file written with them on.

use std::fmt::Write as _;
use std::time::{SystemTime, UNIX_EPOCH};

use lodestar_log::{Level, Mask, Record, Reference, Rendered, Value};
use serde_json::{Map, Value as Json};

/// A line with its time, ready for an output.
#[derive(Debug, Clone, Copy)]
pub struct Stamp {
    pub millis: i64,
}

impl Stamp {
    pub fn now() -> Stamp {
        let millis = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_millis() as i64);
        Stamp { millis }
    }

    /// RFC 3339 in UTC with milliseconds: `2026-10-23T10:15:00.123Z`.
    pub fn rfc3339(self) -> String {
        let secs = self.millis.div_euclid(1000);
        let ms = self.millis.rem_euclid(1000);
        let days = secs.div_euclid(86_400);
        let rem = secs.rem_euclid(86_400);
        let (y, m, d) = civil_from_days(days);
        format!(
            "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}.{ms:03}Z",
            rem / 3600,
            rem % 3600 / 60,
            rem % 60
        )
    }

    /// For file names: `20261023T101500.123Z`.
    pub fn compact(self) -> String {
        self.rfc3339().replace(['-', ':'], "")
    }
}

/// Howard Hinnant's days-to-civil: a day count since 1970-01-01 to
/// (year, month, day).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

fn upper(level: Level) -> &'static str {
    match level {
        Level::Error => "ERROR",
        Level::Warn => " WARN",
        Level::Info => " INFO",
        Level::Debug => "DEBUG",
        Level::Trace => "TRACE",
    }
}

/// A catalogued event as one text line (no newline).
pub fn event_text(
    stamp: Stamp,
    record: &Record<'_>,
    mask: Mask,
    refs: Option<Reference<'_>>,
) -> String {
    let pairs = record.pairs_torrent();
    let mut line = format!(
        "{} {} {}: {}",
        stamp.rfc3339(),
        upper(record.event.level),
        record.event.id,
        record.event.message
    );
    for (name, value) in record.fields {
        let shown = value.render(mask, pairs, refs);
        match value {
            // `… enter setup token ABCD-…`: the token right after the
            // message, as `tests/serve.rs` and people read it.
            Value::Secret(_) => {
                let _ = write!(line, " {shown}");
            }
            _ => {
                let _ = write!(line, " {name}={}", quote(&shown.to_string()));
            }
        }
    }
    line
}

/// A catalogued event as one JSON line (no newline).
pub fn event_json(
    stamp: Stamp,
    record: &Record<'_>,
    mask: Mask,
    refs: Option<Reference<'_>>,
) -> String {
    let pairs = record.pairs_torrent();
    let mut fields = Map::new();
    for (name, value) in record.fields {
        let shown = value.render(mask, pairs, refs);
        let mut f = Map::new();
        f.insert("kind".into(), value.kind().name().into());
        f.insert("value".into(), json_of(&shown));
        fields.insert((*name).to_string(), Json::Object(f));
    }
    let mut m = Map::new();
    m.insert("ts".into(), stamp.rfc3339().into());
    m.insert("level".into(), record.event.level.name().into());
    m.insert("component".into(), record.event.component.name().into());
    m.insert("event".into(), record.event.id.into());
    m.insert("msg".into(), record.event.message.into());
    m.insert("fields".into(), Json::Object(fields));
    json_line(&Json::Object(m))
}

fn json_of(shown: &Rendered) -> Json {
    match shown {
        Rendered::Number(n) => Json::from(*n),
        Rendered::Signed(n) => Json::from(*n),
        Rendered::Flag(b) => Json::from(*b),
        other => Json::from(other.to_string()),
    }
}

/// `s` on one line: control characters escaped, so a message can't start
/// a line of its own.
fn one_line(s: &str) -> String {
    match s.chars().any(unsafe_char) {
        true => s.chars().map(escape).collect(),
        false => s.to_string(),
    }
}

/// What a terminal would act on rather than show: control characters
/// (ESC starts a sequence, BEL rings), the bidirectional overrides, which
/// can make a line read differently from what it holds, and the line and
/// paragraph separators, which some readers break a line at.
fn unsafe_char(c: char) -> bool {
    c.is_control()
        || matches!(
            c,
            '\u{200e}'
                | '\u{200f}'
                | '\u{061c}'
                | '\u{202a}'..='\u{202e}'
                | '\u{2066}'..='\u{2069}'
                | '\u{2028}'
                | '\u{2029}'
        )
}

fn escape(c: char) -> String {
    match c {
        '\n' => "\\n".into(),
        '\r' => "\\r".into(),
        '\t' => "\\t".into(),
        c if unsafe_char(c) => format!("\\u{{{:x}}}", c as u32),
        c => c.to_string(),
    }
}

/// A value with spaces quoted, so `k=v` pairs stay readable, and quoted
/// with its unsafe characters escaped whenever it holds one.
fn quote(s: &str) -> String {
    let needs = s.is_empty()
        || s.contains(char::is_whitespace)
        || s.contains('"')
        || s.chars().any(unsafe_char);
    if !needs {
        return s.to_string();
    }
    let mut out = String::from('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c => out.push_str(&escape(c)),
        }
    }
    out.push('"');
    out
}

/// A JSON line with the bidirectional overrides escaped too (serde_json
/// escapes the control characters already); still valid JSON.
fn json_line(v: &Json) -> String {
    let text = v.to_string();
    match text.chars().any(unsafe_char) {
        true => text
            .chars()
            .map(|c| match unsafe_char(c) {
                true => format!("\\u{:04x}", c as u32),
                false => c.to_string(),
            })
            .collect(),
        false => text,
    }
}

/// A line the code didn't type (a third-party crate's, or a first-party
/// one the sweep hasn't reached): already scrubbed.
#[derive(Debug, Clone)]
pub struct Untyped {
    pub level: Level,
    pub target: String,
    pub component: &'static str,
    pub message: String,
    pub fields: Vec<(String, String)>,
}

pub fn untyped_text(stamp: Stamp, u: &Untyped) -> String {
    let mut line = format!(
        "{} {} {}: {}",
        stamp.rfc3339(),
        upper(u.level),
        u.target,
        one_line(&u.message)
    );
    for (k, v) in &u.fields {
        let _ = write!(line, " {k}={}", quote(v));
    }
    line
}

pub fn untyped_json(stamp: Stamp, u: &Untyped) -> String {
    let mut fields = Map::new();
    for (k, v) in &u.fields {
        let mut f = Map::new();
        f.insert("kind".into(), "untyped".into());
        f.insert("value".into(), v.clone().into());
        fields.insert(k.clone(), Json::Object(f));
    }
    let mut m = Map::new();
    m.insert("ts".into(), stamp.rfc3339().into());
    m.insert("level".into(), u.level.name().into());
    m.insert("component".into(), u.component.into());
    m.insert("target".into(), u.target.clone().into());
    m.insert("msg".into(), u.message.clone().into());
    m.insert("fields".into(), Json::Object(fields));
    json_line(&Json::Object(m))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_controls_and_bidi_overrides_are_escaped() {
        let evil = "Name\u{1b}[31mred\u{7}\u{202e}gpj.exe\u{2028}forged\u{2029}";
        let q = quote(evil);
        assert!(!q.chars().any(unsafe_char), "{q}");
        assert!(
            q.starts_with('"')
                && q.contains("\\u{1b}")
                && q.contains("\\u{202e}")
                && q.contains("\\u{2028}")
                && q.contains("\\u{2029}"),
            "{q}"
        );
        assert_eq!(quote("plain"), "plain");
        assert_eq!(quote("a \"b\""), "\"a \\\"b\\\"\"");
        let line = json_line(&serde_json::json!({ "v": evil }));
        assert!(!line.chars().any(unsafe_char), "{line}");
        let back: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(back["v"], evil, "still valid JSON with the same value");
    }

    #[test]
    fn an_untyped_message_stays_on_one_line() {
        let u = Untyped {
            level: Level::Warn,
            target: "hyper".into(),
            component: "libraries",
            message: "bad\n2026-01-01T00:00:00.000Z ERROR forged: line".into(),
            fields: vec![("k".into(), "a\nb".into())],
        };
        let line = untyped_text(Stamp { millis: 0 }, &u);
        assert!(!line.contains('\n'), "{line}");
        assert!(line.contains("bad\\n2026"), "{line}");
    }

    #[test]
    fn times_are_rfc3339_utc() {
        assert_eq!(Stamp { millis: 0 }.rfc3339(), "1970-01-01T00:00:00.000Z");
        assert_eq!(
            Stamp {
                millis: 1_792_750_500_123
            }
            .rfc3339(),
            "2026-10-23T10:15:00.123Z"
        );
        assert_eq!(
            Stamp {
                millis: 951_782_400_000
            }
            .rfc3339(),
            "2000-02-29T00:00:00.000Z"
        );
        assert_eq!(
            Stamp {
                millis: 1_792_750_500_123
            }
            .compact(),
            "20261023T101500.123Z"
        );
    }
}
