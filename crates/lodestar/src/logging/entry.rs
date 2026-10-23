//! One record, rendered once for an output's mask (architecture §12.1):
//! the structured line the ring buffer keeps and the file and console
//! lines are written from.
//!
//! - **No raw value is ever kept.** Each field holds what the output shows
//!   (`[hidden]`, `ref:…`, or the value its kind allows), so nothing a
//!   mask hid can be recovered, or probed by a filter, later.
//! - **`shown`** is the set of kinds that rendered as values, a peer
//!   beside a torrent counting as peer activity, so turning a kind off
//!   can find and rewrite exactly the fields that carry it.
//! - **The ring's copy** has the setup token as `[redacted]` and values cut
//!   to [`MAX_ENTRY`] accounted bytes; the file line keeps the whole value.

use std::borrow::Cow;
use std::hash::{Hash, Hasher};

use lodestar_log::{Component, Event, FieldKind, Kind, Level, Mask, Record, Reference, Rendered};
use serde_json::{Map, Value as Json};

use super::format::{Stamp, Untyped, json_line, one_line, quote, upper};

/// What a value that's been hidden shows as.
pub const HIDDEN: &str = lodestar_log::HIDDEN;
/// What the setup token shows as wherever the API can read it.
pub const REDACTED: &str = "[redacted]";
/// The most one entry may account for in the ring; longer values are cut.
pub const MAX_ENTRY: usize = 16 << 10;
/// An entry's accounted cost beside its strings: the struct, its `Arc`,
/// the ring's slot and the field vector's header.
pub const ENTRY_OVERHEAD: usize = 160;
/// Each field's accounted cost beside its strings.
pub const FIELD_OVERHEAD: usize = 48;

/// What a field carries, as the file's `"kind"` names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FieldType {
    Data(Kind),
    Error,
    Label,
    /// The setup token: a label in the file, written bare on the console.
    Secret,
    /// A field of a line the code didn't type.
    Untyped,
}

impl FieldType {
    pub fn name(self) -> &'static str {
        match self {
            FieldType::Data(k) => k.name(),
            FieldType::Error => "error",
            FieldType::Label | FieldType::Secret => "label",
            FieldType::Untyped => "untyped",
        }
    }
}

/// A value as an output shows it.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Shown {
    Text(String),
    Number(u64),
    Signed(i64),
    Flag(bool),
}

impl Shown {
    pub fn json(&self) -> Json {
        match self {
            Shown::Text(s) => Json::from(s.as_str()),
            Shown::Number(n) => Json::from(*n),
            Shown::Signed(n) => Json::from(*n),
            Shown::Flag(b) => Json::from(*b),
        }
    }

    pub fn text(&self) -> Cow<'_, str> {
        match self {
            Shown::Text(s) => Cow::Borrowed(s),
            Shown::Number(n) => Cow::Owned(n.to_string()),
            Shown::Signed(n) => Cow::Owned(n.to_string()),
            Shown::Flag(b) => Cow::Owned(b.to_string()),
        }
    }

    fn len(&self) -> usize {
        match self {
            Shown::Text(s) => s.len(),
            _ => 0,
        }
    }
}

/// One field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Field {
    pub name: Cow<'static, str>,
    pub kind: FieldType,
    pub value: Shown,
    /// The kind whose switch let this value show (peer activity for a
    /// peer beside a torrent); `None` for references, hidden values,
    /// errors and labels.
    pub shown_as: Option<Kind>,
}

/// Where a line comes from.
#[derive(Debug, Clone)]
pub enum Source {
    Event(&'static Event),
    /// An untyped line's `tracing` target.
    Target(String),
}

impl PartialEq for Source {
    fn eq(&self, o: &Source) -> bool {
        match (self, o) {
            (Source::Event(a), Source::Event(b)) => std::ptr::eq(*a, *b),
            (Source::Target(a), Source::Target(b)) => a == b,
            _ => false,
        }
    }
}

impl Eq for Source {}

impl Source {
    pub fn name(&self) -> &str {
        match self {
            Source::Event(e) => e.id,
            Source::Target(t) => t,
        }
    }
}

/// A fold summary's count (architecture §12.1, "Floods").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Repeated {
    pub count: u64,
    pub seconds: u64,
    /// The sequence of the first line, when the output gives ids.
    pub first: Option<u64>,
}

/// One record as an output shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// Given as it goes into the ring; 0 before.
    pub seq: u64,
    pub millis: i64,
    pub level: Level,
    pub component: Component,
    pub source: Source,
    pub msg: Cow<'static, str>,
    pub fields: Vec<Field>,
    /// The kinds that rendered as values.
    pub shown: Mask,
    /// The event's opt-in, or peer activity when a peer showed beside a
    /// torrent: lines of an opt-in turned off are removed, not rewritten.
    pub opt_in: Option<Kind>,
    /// A value was cut to fit [`MAX_ENTRY`] (ring, stream and history only).
    pub truncated: bool,
    pub repeated: Option<Repeated>,
}

impl Entry {
    /// `record` rendered under `mask`.
    pub fn typed(stamp: Stamp, r: &Record<'_>, mask: Mask, refs: Option<Reference<'_>>) -> Entry {
        let pairs = r.pairs_torrent();
        let mut shown = Mask::NONE;
        let fields = r
            .fields
            .iter()
            .map(|(name, v)| {
                let kind = match v {
                    lodestar_log::Value::Secret(_) => FieldType::Secret,
                    _ => match v.kind() {
                        FieldKind::Data(k) => FieldType::Data(k),
                        FieldKind::Error => FieldType::Error,
                        FieldKind::Label => FieldType::Label,
                    },
                };
                let rendered = v.render(mask, pairs, refs);
                let shown_as = match (kind, &rendered) {
                    (_, Rendered::Hidden | Rendered::Reference(_)) => None,
                    (FieldType::Data(Kind::Peer), _) if pairs => Some(Kind::PeerActivity),
                    (FieldType::Data(k), _) => Some(k),
                    _ => None,
                };
                if let Some(k) = shown_as {
                    shown = shown.with(k);
                }
                Field {
                    name: Cow::Borrowed(*name),
                    kind,
                    value: match rendered {
                        Rendered::Hidden => Shown::Text(HIDDEN.into()),
                        Rendered::Reference(s) | Rendered::Text(s) => Shown::Text(s),
                        Rendered::Number(n) => Shown::Number(n),
                        Rendered::Signed(n) => Shown::Signed(n),
                        Rendered::Flag(b) => Shown::Flag(b),
                    },
                    shown_as,
                }
            })
            .collect();
        let opt_in = r
            .event
            .opt_in
            .or_else(|| shown.has(Kind::PeerActivity).then_some(Kind::PeerActivity));
        Entry {
            seq: 0,
            millis: stamp.millis,
            level: r.event.level,
            component: r.event.component,
            source: Source::Event(r.event),
            msg: Cow::Borrowed(r.event.message),
            fields,
            shown,
            opt_in,
            truncated: false,
            repeated: None,
        }
    }

    /// A line the code didn't type (already scrubbed).
    pub fn untyped(stamp: Stamp, u: &Untyped) -> Entry {
        Entry {
            seq: 0,
            millis: stamp.millis,
            level: u.level,
            component: Component::parse(u.component).unwrap_or(Component::Libraries),
            source: Source::Target(u.target.clone()),
            msg: Cow::Owned(u.message.clone()),
            fields: u
                .fields
                .iter()
                .map(|(k, v)| Field {
                    name: Cow::Owned(k.clone()),
                    kind: FieldType::Untyped,
                    value: Shown::Text(v.clone()),
                    shown_as: None,
                })
                .collect(),
            shown: Mask::NONE,
            opt_in: None,
            truncated: false,
            repeated: None,
        }
    }

    /// The summary of a run of lines like `self` (no fields, so it never
    /// shows more than the first line did).
    pub fn summary(&self, millis: i64, repeated: Repeated) -> Entry {
        Entry {
            seq: 0,
            millis,
            level: self.level,
            component: self.component,
            source: self.source.clone(),
            msg: self.msg.clone(),
            fields: Vec::new(),
            shown: Mask::NONE,
            opt_in: None,
            truncated: false,
            repeated: Some(repeated),
        }
    }

    /// The line's identity for folding: everything the output shows but
    /// the time and the id.
    pub fn identity(&self, h: &mut impl Hasher) {
        self.source.name().hash(h);
        self.level.hash(h);
        self.msg.hash(h);
        for f in &self.fields {
            f.name.hash(h);
            f.kind.hash(h);
            f.value.hash(h);
        }
    }

    /// The bytes it accounts for in the ring.
    pub fn accounted(&self) -> usize {
        let own = |c: &Cow<'static, str>| match c {
            Cow::Owned(s) => s.len(),
            Cow::Borrowed(_) => 0,
        };
        let target = match &self.source {
            Source::Target(t) => t.len(),
            Source::Event(_) => 0,
        };
        ENTRY_OVERHEAD
            + own(&self.msg)
            + target
            + self
                .fields
                .iter()
                .map(|f| FIELD_OVERHEAD + own(&f.name) + f.value.len())
                .sum::<usize>()
    }

    /// The copy the ring keeps, when it differs: the setup token
    /// `[redacted]` and values cut to [`MAX_ENTRY`]. `None`: keep `self`.
    pub fn ring_copy(&self) -> Option<Entry> {
        let token = self.fields.iter().any(|f| f.kind == FieldType::Secret);
        let untyped_token = match &self.source {
            Source::Target(_) => redact_setup_token(&self.msg),
            Source::Event(_) => None,
        };
        let over = self.accounted() > MAX_ENTRY;
        if !token && untyped_token.is_none() && !over {
            return None;
        }
        let mut e = self.clone();
        for f in &mut e.fields {
            if f.kind == FieldType::Secret {
                f.value = Shown::Text(REDACTED.into());
            }
        }
        if let Some(msg) = untyped_token {
            e.msg = Cow::Owned(msg);
        }
        if e.accounted() > MAX_ENTRY {
            e.truncate();
        }
        Some(e)
    }

    /// Cuts the message and the values, in order, to fit [`MAX_ENTRY`].
    fn truncate(&mut self) {
        let fixed = ENTRY_OVERHEAD
            + match &self.source {
                Source::Target(t) => t.len().min(256),
                Source::Event(_) => 0,
            }
            + self
                .fields
                .iter()
                .map(|f| {
                    FIELD_OVERHEAD
                        + match &f.name {
                            Cow::Owned(s) => s.len(),
                            Cow::Borrowed(_) => 0,
                        }
                })
                .sum::<usize>();
        let mut budget = MAX_ENTRY.saturating_sub(fixed);
        if let Source::Target(t) = &mut self.source {
            cut(t, 256);
        }
        if let Cow::Owned(m) = &mut self.msg {
            let keep = budget.min(4096);
            cut(m, keep);
            budget = budget.saturating_sub(m.len());
        }
        for f in &mut self.fields {
            if let Cow::Owned(n) = &mut f.name {
                cut(n, 256);
            }
            if let Shown::Text(s) = &mut f.value {
                cut(s, budget);
                budget = budget.saturating_sub(s.len());
            }
        }
        self.truncated = true;
    }

    /// The file's JSON line (one line, no newline). `id` is `<epoch>-<seq>`
    /// for an output that gives ids.
    pub fn json(&self, id: Option<&str>, epoch: Option<&str>) -> String {
        let mut fields = Map::new();
        for f in &self.fields {
            let mut v = Map::new();
            v.insert("kind".into(), f.kind.name().into());
            v.insert("value".into(), f.value.json());
            fields.insert(f.name.to_string(), Json::Object(v));
        }
        let mut m = Map::new();
        if let Some(id) = id {
            m.insert("id".into(), id.into());
        }
        m.insert(
            "ts".into(),
            Stamp {
                millis: self.millis,
            }
            .rfc3339()
            .into(),
        );
        m.insert("level".into(), self.level.name().into());
        m.insert("component".into(), self.component.name().into());
        match &self.source {
            Source::Event(e) => m.insert("event".into(), e.id.into()),
            Source::Target(t) => m.insert("target".into(), t.as_str().into()),
        };
        m.insert("msg".into(), self.msg.as_ref().into());
        m.insert("fields".into(), Json::Object(fields));
        if let Some(r) = self.repeated {
            let mut rep = Map::new();
            rep.insert("count".into(), r.count.into());
            rep.insert("seconds".into(), r.seconds.into());
            if let (Some(first), Some(epoch)) = (r.first, epoch) {
                rep.insert("first".into(), format!("{epoch}-{first}").into());
            }
            m.insert("repeated".into(), Json::Object(rep));
        }
        json_line(&Json::Object(m))
    }

    /// The console's text line (no newline).
    pub fn text(&self) -> String {
        use std::fmt::Write as _;
        let mut line = format!(
            "{} {} {}: {}",
            Stamp {
                millis: self.millis
            }
            .rfc3339(),
            upper(self.level),
            self.source.name(),
            one_line(&self.msg)
        );
        for f in &self.fields {
            match f.kind {
                // `… enter setup token ABCD-…`: the token right after the
                // message, as `tests/serve.rs` and people read it.
                FieldType::Secret => {
                    let _ = write!(line, " {}", f.value.text());
                }
                _ => {
                    let _ = write!(line, " {}={}", f.name, quote(&f.value.text()));
                }
            }
        }
        if let Some(r) = self.repeated {
            let _ = write!(line, " {}", repeated_suffix(r.count, r.seconds));
        }
        line
    }
}

/// `(repeated 1,204 times in 10 s)`.
pub fn repeated_suffix(count: u64, seconds: u64) -> String {
    format!(
        "(repeated {} {} in {seconds} s)",
        thousands(count),
        if count == 1 { "time" } else { "times" }
    )
}

/// `1204` → `1,204`.
pub fn thousands(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// Cuts `s` to at most `max` bytes at a character boundary, ending in `…`.
fn cut(s: &mut String, max: usize) {
    if s.len() <= max {
        return;
    }
    let mut end = max.saturating_sub('…'.len_utf8());
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    s.truncate(end);
    s.push('…');
}

/// The plain start-up line's token, until B-log1b logs it as
/// `auth.setup_token`: the word after `setup token ` becomes
/// `[redacted]`. `None` when there's none.
pub fn redact_setup_token(msg: &str) -> Option<String> {
    const MARK: &str = "setup token ";
    let at = msg.find(MARK)? + MARK.len();
    let rest = &msg[at..];
    let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
    if end == 0 {
        return None;
    }
    Some(format!("{}{REDACTED}{}", &msg[..at], &rest[end..]))
}

/// An info hash as the file shows it with hashes on: 40 hex digits.
pub fn parse_hex_hash(s: &str) -> Option<[u8; 20]> {
    if s.len() != 40 || !s.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let mut out = [0u8; 20];
    for (i, b) in out.iter_mut().enumerate() {
        *b = u8::from_str_radix(&s[2 * i..2 * i + 2], 16).ok()?;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use lodestar_log::Value;

    lodestar_log::catalogue! {
        "entry-tests";
        E {
            id: "test.entry",
            component: Fetch,
            level: Info,
            message: "entry",
            fields: { torrent: Hash, name: Name, peer: Peer, size: Size, what: Label },
        }
        TOKEN {
            id: "test.token",
            component: Auth,
            level: Warn,
            message: "enter setup token",
            fields: { token: SetupToken },
        }
    }

    fn reference(h: &[u8; 20]) -> String {
        format!("ref:{:02x}{:02x}{:02x}{:02x}", h[0], h[1], h[2], h[3])
    }

    fn record_entry(mask: Mask) -> Entry {
        let h = [0xab; 20];
        let fields = [
            ("torrent", Value::Torrent(&h)),
            ("name", Value::Name("Secret.Name")),
            ("peer", Value::Peer("203.0.113.7:1".parse().unwrap())),
            ("size", Value::Size(5)),
            ("what", Value::Label("x")),
        ];
        let r = Record {
            event: &E::EVENT,
            fields: &fields,
        };
        Entry::typed(Stamp { millis: 0 }, &r, mask, Some(&reference))
    }

    #[test]
    fn values_are_kept_only_as_shown() {
        let e = record_entry(Mask::PRIVATE);
        let dump = format!("{e:?}");
        assert!(
            !dump.contains("Secret.Name") && !dump.contains("203.0.113.7"),
            "{dump}"
        );
        assert!(!dump.contains(&"ab".repeat(20)), "{dump}");
        assert_eq!(e.shown, Mask::NONE.with(Kind::Size));
        assert_eq!(e.opt_in, None);
        assert_eq!(e.fields[0].value, Shown::Text("ref:abababab".into()));
        let all = record_entry(Mask(u32::MAX));
        assert!(all.shown.has(Kind::PeerActivity) && !all.shown.has(Kind::Peer));
        assert_eq!(all.opt_in, Some(Kind::PeerActivity));
        assert!(all.shown.has(Kind::Hash) && all.shown.has(Kind::Name));
    }

    #[test]
    fn the_file_line_has_its_id_and_the_old_shape() {
        let e = record_entry(Mask::PRIVATE);
        let line = e.json(Some("00ff-7"), None);
        let v: Json = serde_json::from_str(&line).unwrap();
        assert_eq!(v["id"], "00ff-7");
        assert_eq!(v["event"], "test.entry");
        assert_eq!(
            v["fields"]["size"],
            serde_json::json!({"kind": "size", "value": 5})
        );
        assert_eq!(v["fields"]["name"]["value"], HIDDEN);
        assert!(v.get("repeated").is_none());
        let s = e.summary(
            10_000,
            Repeated {
                count: 1204,
                seconds: 10,
                first: Some(7),
            },
        );
        let v: Json = serde_json::from_str(&s.json(Some("00ff-9"), Some("00ff"))).unwrap();
        assert_eq!(
            v["repeated"],
            serde_json::json!({"count": 1204, "seconds": 10, "first": "00ff-7"})
        );
        assert_eq!(v["fields"], serde_json::json!({}));
        assert!(
            s.text()
                .ends_with("test.entry: entry (repeated 1,204 times in 10 s)"),
            "{}",
            s.text()
        );
    }

    #[test]
    fn the_ring_never_keeps_the_setup_token() {
        let fields = [("token", Value::Secret("ABCD-EFGH-IJKL"))];
        let r = Record {
            event: &TOKEN::EVENT,
            fields: &fields,
        };
        let e = Entry::typed(Stamp { millis: 0 }, &r, Mask::PRIVATE, None);
        assert!(
            e.text().contains("setup token ABCD-EFGH-IJKL"),
            "the console has it"
        );
        let ring = e.ring_copy().unwrap();
        assert!(!format!("{ring:?}").contains("ABCD"));
        assert_eq!(ring.fields[0].value, Shown::Text(REDACTED.into()));
        let u = Untyped {
            level: Level::Warn,
            target: "lodestar::services".into(),
            component: "system",
            message: "first run: open http://x and enter setup token ABCD-EFGH to create".into(),
            fields: vec![],
        };
        let ring = Entry::untyped(Stamp { millis: 0 }, &u).ring_copy().unwrap();
        assert_eq!(
            ring.msg,
            "first run: open http://x and enter setup token [redacted] to create"
        );
        assert_eq!(redact_setup_token("no token here"), None);
    }

    #[test]
    fn long_entries_are_cut_in_the_ring_only() {
        let u = Untyped {
            level: Level::Warn,
            target: "hyper".into(),
            component: "libraries",
            message: "m".repeat(10_000),
            fields: vec![("a".into(), "é".repeat(20_000))],
        };
        let e = Entry::untyped(Stamp { millis: 0 }, &u);
        assert!(e.accounted() > MAX_ENTRY);
        let ring = e.ring_copy().unwrap();
        assert!(
            ring.truncated && ring.accounted() <= MAX_ENTRY,
            "{}",
            ring.accounted()
        );
        assert!(ring.msg.ends_with('…'));
        assert!(e.json(None, None).len() > 40_000, "the file keeps it whole");
    }

    #[test]
    fn helpers() {
        assert_eq!(thousands(0), "0");
        assert_eq!(thousands(999), "999");
        assert_eq!(thousands(1204), "1,204");
        assert_eq!(thousands(1_000_000), "1,000,000");
        assert_eq!(repeated_suffix(1, 3), "(repeated 1 time in 3 s)");
        assert_eq!(parse_hex_hash(&"ab".repeat(20)), Some([0xab; 20]));
        assert_eq!(parse_hex_hash("ref:abcd"), None);
    }
}
