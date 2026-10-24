//! What the API shows of a line (architecture §12.1): one function masks
//! a file line for history and for a redacted download alike, taking the
//! mask; another shows a ring entry through the mask in force now.
//!
//! **Masking a file line** ([`file_line`]):
//! - a field whose kind isn't in the mask reads `[hidden]` (a hash too; a
//!   `ref:` value already written stays). A peer beside a hash, name or
//!   path is peer activity;
//! - a line of an opt-in event whose opt-in isn't in the mask is left out;
//! - `client`, `size`, `error` and `label` fields follow the mask or stay
//!   as written (errors and labels always stay);
//! - untyped messages and values go through today's `url::scrub_text`, so
//!   older lines get the newer URL rules, and an unknown kind is hidden
//!   (fail closed);
//! - the setup token reads `[redacted]`.

use lodestar_api::logs::{
    LineFilter, LogComponent, LogDataKind, LogField, LogFieldKind, LogLevel, LogLine, LogRepeated,
    LogValue,
};
use lodestar_log::url::scrub_text;
use lodestar_log::{Component, Kind, Level, Mask};
use serde_json::{Map, Value as Json};

use super::entry::{
    self, ENTRY_OVERHEAD, Entry, FIELD_OVERHEAD, FieldType, HIDDEN, MAX_ENTRY, REDACTED, Shown,
    Source, redact_setup_token,
};
use super::format::{Stamp, json_line, one_line, quote, upper};

/// What a redacted download may show: sizes. Stricter than Private:
/// client addresses go too, since a shared log mustn't carry the admin's
/// own addresses (runner's decision, §2.19).
pub const REDACT: Mask = Mask(Kind::Size.bit());

/// What a line in a file that doesn't parse becomes in a redacted
/// download.
pub const UNREADABLE: &str = "[unreadable line]";

pub fn level(l: Level) -> LogLevel {
    match l {
        Level::Error => LogLevel::Error,
        Level::Warn => LogLevel::Warn,
        Level::Info => LogLevel::Info,
        Level::Debug => LogLevel::Debug,
        Level::Trace => LogLevel::Trace,
    }
}

fn level_of(l: LogLevel) -> Level {
    match l {
        LogLevel::Error => Level::Error,
        LogLevel::Warn => Level::Warn,
        LogLevel::Info => Level::Info,
        LogLevel::Debug => Level::Debug,
        LogLevel::Trace => Level::Trace,
    }
}

pub fn component(c: Component) -> LogComponent {
    match c {
        Component::System => LogComponent::System,
        Component::Dht => LogComponent::Dht,
        Component::Fetch => LogComponent::Fetch,
        Component::Pipeline => LogComponent::Pipeline,
        Component::Search => LogComponent::Search,
        Component::Api => LogComponent::Api,
        Component::Sources => LogComponent::Sources,
        Component::Filters => LogComponent::Filters,
        Component::Integrations => LogComponent::Integrations,
        Component::Workers => LogComponent::Workers,
        Component::Companions => LogComponent::Companions,
        Component::Auth => LogComponent::Auth,
        Component::Libraries => LogComponent::Libraries,
    }
}

fn component_of(c: LogComponent) -> Component {
    Component::ALL
        .into_iter()
        .find(|x| component(*x) == c)
        .unwrap_or(Component::Libraries)
}

pub fn data_kind(k: Kind) -> LogDataKind {
    match k {
        Kind::Hash => LogDataKind::Hash,
        Kind::Name => LogDataKind::Name,
        Kind::Path => LogDataKind::Path,
        Kind::Url => LogDataKind::Url,
        Kind::Query => LogDataKind::Query,
        Kind::Client => LogDataKind::Client,
        Kind::Peer => LogDataKind::Peer,
        Kind::Size => LogDataKind::Size,
        Kind::PeerActivity => LogDataKind::PeerActivity,
        Kind::FilterChange => LogDataKind::FilterChanges,
    }
}

fn field_kind(k: FieldType) -> LogFieldKind {
    match k {
        FieldType::Data(k) => match k {
            Kind::Hash => LogFieldKind::Hash,
            Kind::Name => LogFieldKind::Name,
            Kind::Path => LogFieldKind::Path,
            Kind::Url => LogFieldKind::Url,
            Kind::Query => LogFieldKind::Query,
            Kind::Client => LogFieldKind::Client,
            Kind::Peer => LogFieldKind::Peer,
            Kind::Size => LogFieldKind::Size,
            Kind::PeerActivity => LogFieldKind::PeerActivity,
            Kind::FilterChange => LogFieldKind::FilterChanges,
        },
        FieldType::Error => LogFieldKind::Error,
        FieldType::Label | FieldType::Listen | FieldType::Secret => LogFieldKind::Label,
        FieldType::Untyped => LogFieldKind::Untyped,
    }
}

fn value(s: &Shown) -> LogValue {
    match s {
        Shown::Text(t) => LogValue::Text(t.clone()),
        Shown::Number(n) => LogValue::Number(*n),
        Shown::Signed(n) => LogValue::Signed(*n),
        Shown::Flag(b) => LogValue::Flag(*b),
    }
}

/// A ring entry through `mask` (the live mask now, with its read-time
/// deadlines): `None` when it's a line of an opt-in that's off.
pub fn ring_line(e: &Entry, mask: Mask, epoch: &str) -> Option<LogLine> {
    if e.opt_in.is_some_and(|k| !mask.has(k)) {
        return None;
    }
    let (event, target) = match &e.source {
        Source::Event(ev) => (Some(ev.id.to_string()), None),
        Source::Target(t) => (None, Some(t.clone())),
    };
    Some(LogLine {
        component: component(e.component),
        event,
        fields: e
            .fields
            .iter()
            .map(|f| LogField {
                kind: field_kind(f.kind),
                name: f.name.to_string(),
                value: match f.shown_as {
                    Some(k) if !mask.has(k) => LogValue::Text(HIDDEN.into()),
                    _ => value(&f.value),
                },
            })
            .collect(),
        id: Some(format!("{epoch}-{}", e.seq)),
        level: level(e.level),
        msg: e.msg.to_string(),
        opt_in: e.opt_in.map(data_kind),
        repeated: e.repeated.map(|r| LogRepeated {
            count: r.count,
            first: r.first.map(|f| format!("{epoch}-{f}")),
            seconds: r.seconds,
        }),
        target,
        truncated: e.truncated,
        ts: Stamp { millis: e.millis }.rfc3339(),
    })
}

/// Cuts a line read from a file as the ring cuts its entries: at most
/// [`MAX_ENTRY`] accounted (messages to 4 KiB, then values, then whole
/// fields), marked `truncated`.
pub fn cut(l: &mut LogLine) {
    let text = |v: &LogValue| match v {
        LogValue::Text(t) => t.len(),
        _ => 0,
    };
    let accounted = |l: &LogLine| {
        ENTRY_OVERHEAD
            + l.msg.len()
            + l.target.as_ref().map_or(0, String::len)
            + l.fields
                .iter()
                .map(|f| FIELD_OVERHEAD + f.name.len() + text(&f.value))
                .sum::<usize>()
    };
    if accounted(l) <= MAX_ENTRY {
        return;
    }
    entry::cut(&mut l.msg, 4096);
    if let Some(t) = &mut l.target {
        entry::cut(t, 256);
    }
    for f in &mut l.fields {
        entry::cut(&mut f.name, 256);
    }
    let fixed = accounted(l) - l.fields.iter().map(|f| text(&f.value)).sum::<usize>();
    let mut budget = MAX_ENTRY.saturating_sub(fixed);
    for f in &mut l.fields {
        if let LogValue::Text(t) = &mut f.value {
            entry::cut(t, budget);
            budget = budget.saturating_sub(t.len());
        }
    }
    while accounted(l) > MAX_ENTRY && l.fields.pop().is_some() {}
    l.truncated = true;
}

/// Whether `l` passes `f`, matched as shown.
pub fn matches(l: &LogLine, f: &LineFilter) -> bool {
    if !f.level_ok(level_of(l.level)) || !f.component_ok(component_of(l.component)) {
        return false;
    }
    let Some(text) = &f.text else { return true };
    let has = |s: &str| s.to_lowercase().contains(text.as_str());
    has(&l.msg)
        || l.event.as_deref().is_some_and(has)
        || l.target.as_deref().is_some_and(has)
        || l.fields.iter().any(|fld| {
            has(&fld.name)
                || match &fld.value {
                    LogValue::Text(t) => has(t),
                    LogValue::Number(n) => has(&n.to_string()),
                    LogValue::Signed(n) => has(&n.to_string()),
                    LogValue::Flag(b) => has(&b.to_string()),
                }
        })
}

/// A file line, parsed and masked. `Err` when it doesn't parse as the
/// file's JSON shape; `Ok(None)` when masking leaves it out. `mask`
/// `None` shows it as written (an unredacted text download).
#[allow(clippy::result_unit_err)]
pub fn file_line(bytes: &[u8], mask: Option<Mask>) -> Result<Option<LogLine>, ()> {
    let v: Json = serde_json::from_slice(bytes).map_err(|_| ())?;
    let o = v.as_object().ok_or(())?;
    let text = |k: &str| o.get(k).and_then(Json::as_str).ok_or(());
    let opt_text = |k: &str| -> Result<Option<String>, ()> {
        match o.get(k) {
            None => Ok(None),
            Some(Json::String(s)) => Ok(Some(s.clone())),
            Some(_) => Err(()),
        }
    };
    let ts = text("ts")?.to_string();
    let lvl = Level::parse(text("level")?).ok_or(())?;
    let comp = Component::parse(text("component")?).ok_or(())?;
    let event = opt_text("event")?;
    let target = opt_text("target")?;
    if event.is_some() == target.is_some() {
        return Err(());
    }
    let id = opt_text("id")?;
    let msg = text("msg")?;
    let fields = match o.get("fields") {
        Some(Json::Object(m)) => m,
        _ => return Err(()),
    };
    let repeated = match o.get("repeated") {
        None => None,
        Some(Json::Object(r)) => Some(LogRepeated {
            count: r.get("count").and_then(Json::as_u64).ok_or(())?,
            seconds: r.get("seconds").and_then(Json::as_u64).ok_or(())?,
            first: match r.get("first") {
                None => None,
                Some(Json::String(s)) => Some(s.clone()),
                Some(_) => return Err(()),
            },
        }),
        Some(_) => return Err(()),
    };
    let catalogued = event.as_deref().and_then(super::find);
    let mut parsed = Vec::with_capacity(fields.len());
    for (name, f) in fields {
        let f = f.as_object().ok_or(())?;
        let kind = f.get("kind").and_then(Json::as_str).ok_or(())?;
        let value = match f.get("value").ok_or(())? {
            Json::String(s) => LogValue::Text(s.clone()),
            Json::Bool(b) => LogValue::Flag(*b),
            Json::Number(n) => match (n.as_u64(), n.as_i64()) {
                (Some(u), _) => LogValue::Number(u),
                (None, Some(i)) => LogValue::Signed(i),
                _ => LogValue::Text(n.to_string()),
            },
            _ => return Err(()),
        };
        parsed.push((name.clone(), kind.to_string(), value));
    }
    // The line's opt-in: its event's, or, for an event this build doesn't
    // know, filter-change detail when it carries any.
    let event_opt_in = match catalogued {
        Some(e) => e.opt_in,
        None => parsed
            .iter()
            .any(|(_, k, _)| k == Kind::FilterChange.name())
            .then_some(Kind::FilterChange),
    };
    let pairs = parsed.iter().any(|(_, k, _)| {
        Kind::parse(k).is_some_and(|k| matches!(k, Kind::Hash | Kind::Name | Kind::Path))
    });
    let setup_token = event.as_deref() == Some("auth.setup_token");
    if let (Some(mask), Some(k)) = (mask, event_opt_in)
        && !mask.has(k)
    {
        return Ok(None);
    }
    let mut peer_activity = false;
    let out_fields = parsed
        .into_iter()
        .map(|(name, kind, value)| {
            let data = Kind::parse(&kind).filter(|k| !k.opt_in() || *k == Kind::FilterChange);
            let lkind = match (data, kind.as_str()) {
                (Some(k), _) => field_kind(FieldType::Data(k)),
                (None, "error") => LogFieldKind::Error,
                (None, "label" | "setup_token") => LogFieldKind::Label,
                (None, "untyped") => LogFieldKind::Untyped,
                // Unknown: fail closed.
                (None, _) => LogFieldKind::Untyped,
            };
            let shown = |v: &LogValue| {
                !matches!(v, LogValue::Text(t) if t == HIDDEN || t.starts_with("ref:"))
            };
            let value = match (mask, data, kind.as_str()) {
                (None, _, _) => value,
                // The setup token, by its kind (or, in lines written before
                // it had one, by its event and name).
                (Some(_), _, "setup_token") => LogValue::Text(REDACTED.into()),
                (Some(_), _, _) if setup_token && name == "token" => {
                    LogValue::Text(REDACTED.into())
                }
                (Some(mask), Some(k), _) => {
                    let effective = match k {
                        Kind::Peer if pairs => Kind::PeerActivity,
                        k => k,
                    };
                    let is_ref = matches!(&value, LogValue::Text(t) if t.starts_with("ref:"));
                    if mask.has(effective) {
                        if effective == Kind::PeerActivity && shown(&value) {
                            peer_activity = true;
                        }
                        value
                    } else if k == Kind::Hash && is_ref {
                        value
                    } else {
                        LogValue::Text(HIDDEN.into())
                    }
                }
                (Some(_), None, "error" | "label") => value,
                (Some(_), None, "untyped") => match value {
                    LogValue::Text(t) => LogValue::Text(scrub_text(&t)),
                    other => other,
                },
                (Some(_), None, _) => LogValue::Text(HIDDEN.into()),
            };
            LogField {
                kind: lkind,
                name,
                value,
            }
        })
        .collect();
    let msg = match (mask, &target) {
        (Some(_), Some(_)) => scrub_text(msg),
        _ => msg.to_string(),
    };
    let msg = match mask {
        Some(_) => redact_setup_token(&msg).unwrap_or(msg),
        None => msg,
    };
    let opt_in = event_opt_in.or(peer_activity.then_some(Kind::PeerActivity));
    Ok(Some(LogLine {
        component: component(comp),
        event,
        fields: out_fields,
        id,
        level: level(lvl),
        msg,
        opt_in: opt_in.map(data_kind),
        repeated,
        target,
        truncated: false,
        ts,
    }))
}

fn json_value(v: &LogValue) -> Json {
    match v {
        LogValue::Text(t) => Json::from(t.as_str()),
        LogValue::Number(n) => Json::from(*n),
        LogValue::Signed(n) => Json::from(*n),
        LogValue::Flag(b) => Json::from(*b),
    }
}

fn kind_name(k: LogFieldKind) -> &'static str {
    match k {
        LogFieldKind::Hash => "hash",
        LogFieldKind::Name => "name",
        LogFieldKind::Path => "path",
        LogFieldKind::Url => "url",
        LogFieldKind::Query => "query",
        LogFieldKind::Client => "client",
        LogFieldKind::Peer => "peer",
        LogFieldKind::Size => "size",
        LogFieldKind::PeerActivity => "peer_activity",
        LogFieldKind::FilterChanges => "filter_changes",
        LogFieldKind::Error => "error",
        LogFieldKind::Label => "label",
        LogFieldKind::Untyped => "untyped",
    }
}

fn level_name(l: LogLevel) -> &'static str {
    level_of(l).name()
}

/// A line in the file's JSON shape (a redacted download's).
pub fn to_json(l: &LogLine) -> String {
    let mut fields = Map::new();
    for f in &l.fields {
        let mut v = Map::new();
        v.insert("kind".into(), kind_name(f.kind).into());
        v.insert("value".into(), json_value(&f.value));
        fields.insert(f.name.clone(), Json::Object(v));
    }
    let mut m = Map::new();
    if let Some(id) = &l.id {
        m.insert("id".into(), id.as_str().into());
    }
    m.insert("ts".into(), l.ts.as_str().into());
    m.insert("level".into(), level_name(l.level).into());
    m.insert("component".into(), component_of(l.component).name().into());
    if let Some(e) = &l.event {
        m.insert("event".into(), e.as_str().into());
    }
    if let Some(t) = &l.target {
        m.insert("target".into(), t.as_str().into());
    }
    m.insert("msg".into(), l.msg.as_str().into());
    m.insert("fields".into(), Json::Object(fields));
    if let Some(r) = &l.repeated {
        let mut rep = Map::new();
        rep.insert("count".into(), r.count.into());
        rep.insert("seconds".into(), r.seconds.into());
        if let Some(f) = &r.first {
            rep.insert("first".into(), f.as_str().into());
        }
        m.insert("repeated".into(), Json::Object(rep));
    }
    json_line(&Json::Object(m))
}

/// A line as the console's text format shows it, `repeated` as its
/// suffix; control and bidirectional characters escaped.
pub fn to_text(l: &LogLine) -> String {
    use std::fmt::Write as _;
    let source = l.event.as_deref().or(l.target.as_deref()).unwrap_or("");
    let mut line = format!(
        "{} {} {}: {}",
        one_line(&l.ts),
        upper(level_of(l.level)),
        one_line(source),
        one_line(&l.msg)
    );
    for f in &l.fields {
        let v = match &f.value {
            LogValue::Text(t) => t.clone(),
            LogValue::Number(n) => n.to_string(),
            LogValue::Signed(n) => n.to_string(),
            LogValue::Flag(b) => b.to_string(),
        };
        let _ = write!(line, " {}={}", one_line(&f.name), quote(&v));
    }
    if let Some(r) = &l.repeated {
        let _ = write!(
            line,
            " {}",
            super::entry::repeated_suffix(r.count, r.seconds)
        );
    }
    line
}

/// A line that didn't parse, in text: escaped, never acted on by a
/// terminal.
pub fn raw_text(bytes: &[u8]) -> String {
    one_line(&String::from_utf8_lossy(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(fields: &str, event: &str) -> Vec<u8> {
        format!(
            r#"{{"id":"0011223344556677-9","ts":"2026-10-23T10:15:00.000Z","level":"info","component":"fetch",{event},"msg":"m","fields":{{{fields}}}}}"#
        )
        .into_bytes()
    }

    const HASH: &str = "cdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcd";

    fn values(l: &LogLine) -> Vec<(String, String)> {
        l.fields
            .iter()
            .map(|f| {
                (
                    f.name.clone(),
                    match &f.value {
                        LogValue::Text(t) => t.clone(),
                        other => format!("{other:?}"),
                    },
                )
            })
            .collect()
    }

    #[test]
    fn redaction_per_kind() {
        let raw = line(
            &format!(
                r#""torrent":{{"kind":"hash","value":"{HASH}"}},"other":{{"kind":"hash","value":"ref:1234abcd"}},"name":{{"kind":"name","value":"Fixture.Name"}},"path":{{"kind":"path","value":"a/b.mkv"}},"url":{{"kind":"url","value":"udp://t.example/x"}},"query":{{"kind":"query","value":"q"}},"client":{{"kind":"client","value":"192.0.2.1"}},"peer":{{"kind":"peer","value":"203.0.113.9:1"}},"size":{{"kind":"size","value":42}},"error":{{"kind":"error","value":"io:NotFound"}},"what":{{"kind":"label","value":"x"}},"odd":{{"kind":"novel","value":"leak"}}"#
            ),
            r#""event":"test.unknown""#,
        );
        let l = file_line(&raw, Some(REDACT)).unwrap().unwrap();
        let v = values(&l);
        let get = |n: &str| v.iter().find(|(k, _)| k == n).unwrap().1.clone();
        for hidden in [
            "torrent", "name", "path", "url", "query", "client", "peer", "odd",
        ] {
            assert_eq!(get(hidden), HIDDEN, "{hidden}");
        }
        assert_eq!(get("other"), "ref:1234abcd", "a reference stays");
        assert_eq!(get("size"), "Number(42)");
        assert_eq!(
            (get("error"), get("what")),
            ("io:NotFound".into(), "x".into())
        );
        assert_eq!(l.opt_in, None);
        // History through a mask with names on: names show, the peer
        // beside a torrent still needs peer activity.
        let mask = Mask::PRIVATE.with(Kind::Name).with(Kind::Peer);
        let l = file_line(&raw, Some(mask)).unwrap().unwrap();
        let v = values(&l);
        let get = |n: &str| v.iter().find(|(k, _)| k == n).unwrap().1.clone();
        assert_eq!(get("name"), "Fixture.Name");
        assert_eq!(get("peer"), HIDDEN);
        assert_eq!(get("client"), "192.0.2.1");
        let l = file_line(&raw, Some(mask.with(Kind::PeerActivity)))
            .unwrap()
            .unwrap();
        assert_eq!(l.opt_in, Some(LogDataKind::PeerActivity));
        // As written.
        let l = file_line(&raw, None).unwrap().unwrap();
        assert!(values(&l).iter().any(|(_, v)| v == HASH));
    }

    #[test]
    fn opt_in_lines_are_dropped_and_untyped_rescrubbed() {
        let detail = line(
            r#""change":{"kind":"filter_changes","value":"term x"}"#,
            r#""event":"filters.change_detail""#,
        );
        assert_eq!(file_line(&detail, Some(REDACT)), Ok(None));
        assert_eq!(file_line(&detail, Some(Mask::PRIVATE)), Ok(None));
        let on = file_line(&detail, Some(Mask::PRIVATE.with(Kind::FilterChange)))
            .unwrap()
            .unwrap();
        assert_eq!(on.opt_in, Some(LogDataKind::FilterChanges));
        let unknown = line(
            r#""change":{"kind":"filter_changes","value":"term x"}"#,
            r#""event":"filters.some_future_detail""#,
        );
        assert_eq!(file_line(&unknown, Some(REDACT)), Ok(None));
        let untyped = line(
            r#""url":{"kind":"untyped","value":"https://u:pw@h.example/a?passkey=abc"}"#,
            r#""target":"hyper""#,
        );
        let mut untyped = String::from_utf8(untyped).unwrap();
        untyped = untyped.replace(
            r#""msg":"m""#,
            r#""msg":"GET https://h.example/x?apikey=secret1""#,
        );
        let l = file_line(untyped.as_bytes(), Some(REDACT))
            .unwrap()
            .unwrap();
        let text = to_text(&l);
        assert!(
            !text.contains("pw") && !text.contains("passkey=abc"),
            "{text}"
        );
        assert!(!text.contains("secret1"), "{text}");
    }

    #[test]
    fn the_setup_token_is_redacted() {
        let typed = line(
            r#""token":{"kind":"label","value":"ABCD-EFGH"}"#,
            r#""event":"auth.setup_token""#,
        );
        let l = file_line(&typed, Some(Mask(u32::MAX))).unwrap().unwrap();
        assert!(!serde_json::to_string(&l).unwrap().contains("ABCD"));
        // By its kind, whatever the event or the field's name.
        let kinded = line(
            r#""code":{"kind":"setup_token","value":"WXYZ-1234"}"#,
            r#""event":"some.future_event""#,
        );
        let l = file_line(&kinded, Some(Mask(u32::MAX))).unwrap().unwrap();
        assert!(!serde_json::to_string(&l).unwrap().contains("WXYZ"));
        let mut plain = String::from_utf8(line("", r#""target":"lodestar::services""#)).unwrap();
        plain = plain.replace(
            r#""msg":"m""#,
            r#""msg":"open x and enter setup token ABCD-EFGH to go""#,
        );
        let l = file_line(plain.as_bytes(), Some(Mask(u32::MAX)))
            .unwrap()
            .unwrap();
        assert_eq!(l.msg, "open x and enter setup token [redacted] to go");
        let l = file_line(plain.as_bytes(), None).unwrap().unwrap();
        assert!(l.msg.contains("ABCD"), "as written");
    }

    #[test]
    fn shapes_that_dont_parse() {
        for bad in [
            &b"not json"[..],
            br#"[1,2]"#,
            br#"{"ts":"t","level":"info","component":"fetch","msg":"m","fields":{}}"#,
            br#"{"ts":"t","level":"loud","component":"fetch","event":"e","msg":"m","fields":{}}"#,
            br#"{"ts":"t","level":"info","component":"fetch","event":"e","msg":"m","fields":{"a":1}}"#,
            br#"{"ts":"t","level":"info","component":"fetch","event":"e","target":"t","msg":"m","fields":{}}"#,
            br#"{"ts":"t","level":"info","component":"fetch","event":"e","msg":"m","fields":{"a":{"kind":"name","value":[1]}}}"#,
        ] {
            assert_eq!(file_line(bad, Some(REDACT)), Err(()), "{}", String::from_utf8_lossy(bad));
        }
    }

    #[test]
    fn text_escapes_controls_and_bidi() {
        let raw = line(
            "\"what\":{\"kind\":\"label\",\"value\":\"a\\u001b[31mred\u{202e}gpj.exe\"}",
            r#""event":"test.x""#,
        );
        let l = file_line(&raw, None).unwrap().unwrap();
        let t = to_text(&l);
        assert!(!t.contains('\u{1b}') && !t.contains('\u{202e}'), "{t}");
        assert!(t.contains("\\u{1b}") && t.contains("\\u{202e}"), "{t}");
        assert!(!raw_text(b"x\x1b[2Jy\nz").contains('\u{1b}'));
        // The JSON shape round-trips.
        let back = file_line(to_json(&l).as_bytes(), None).unwrap().unwrap();
        assert_eq!(back, l);
    }
}
