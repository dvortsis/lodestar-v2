//! IRC announce definitions and the matcher (IRC-1) never panic on
//! untrusted text, and what they yield is bounded and clean.
//!
//! - A definition (which may be the user's) parses or is refused; a
//!   definition that parses has every list within its cap.
//! - A server line is read against the fuzzed definition, when it parses,
//!   and always against a fixed one, so the matcher's paths are reached
//!   even when the fuzzed definition is refused. An announce's name is
//!   1–`MAX_NAME` bytes; no field holds a control or format character or
//!   a URL; a size is at most `MAX_SIZE`; a filled torrent URL starts
//!   with `https://` and the definition's `torrent_url_host`, has no
//!   space, control character or brace, and (for the fixed definition,
//!   whose template has none) no `.` or `..` path segment.
//! - Every tag value of a parsed line is free of `CR`, `LF` and `NUL`
//!   (X-irc-tag-crlf), and `clean_text` never grows and leaves no control
//!   or format character.
//!
//! Input: two bytes of definition length (big-endian), the definition's
//! bytes (decoded lossily), then the line's bytes.
#![no_main]

use libfuzzer_sys::fuzz_target;
use lodestar_integrations::Secret;
use lodestar_sources_irc::announce::{
    self, Definition, MAX_ANNOUNCERS, MAX_CHANNELS, MAX_NAME, MAX_SIZE, Matcher, Outcome,
};
use lodestar_sources_irc::line::{self, Message, clean_text, is_format_char, parse};

const FIXED: &str = "\
lodestar-announce = 1
id = fixed
name = Fixed
network = irc.example.net
channel = #announce
announcer = Announcer
pattern = ^New: (?P<name>.+?) \\[(?P<category>[^\\]]*)\\] (?P<size>\\S+ ?\\S*) id=(?P<id>\\S+)$
pattern = (?P<name>.*?)(?: id=(?P<id>\\S*))?$
torrent-url = https://example.net/dl/{id}/{passkey}/{name}
";

fn clean(s: &str) -> bool {
    !s.chars().any(|c| c.is_control() || is_format_char(c))
}

fn read(m: &Matcher, msg: &Message) {
    let Outcome::Announce(a) = m.read(msg) else {
        return;
    };
    assert!(!a.name.is_empty() && a.name.len() <= MAX_NAME);
    for f in [
        Some(a.name.as_str()),
        a.category.as_deref(),
        a.torrent_id.as_deref(),
    ]
    .into_iter()
    .flatten()
    {
        assert!(clean(f));
        assert!(!f.contains("://"));
    }
    assert!(a.size.is_none_or(|s| s <= MAX_SIZE));
    let def = m.definition(&a.definition).expect("its definition");
    if let Some(url) = def.torrent_url(&a, Some(&Secret::new("key"))) {
        let u = url.expose();
        let host = def.torrent_url_host().expect("a template has a host");
        let after = u
            .strip_prefix("https://")
            .and_then(|r| r.strip_prefix(host))
            .expect("the template's host");
        assert!(after.is_empty() || after.starts_with(['/', ':', '?', '#']));
        assert!(!u.contains([' ', '{', '}']) && !u.chars().any(char::is_control));
        if def.id() == "fixed" {
            assert!(!after.split('/').any(|seg| seg == "." || seg == ".."));
        }
    }
}

fuzz_target!(|data: &[u8]| {
    let Some((len, rest)) = data.split_first_chunk::<2>() else {
        return;
    };
    let n = usize::from(u16::from_be_bytes(*len)).min(rest.len());
    let (def_bytes, line_bytes) = rest.split_at(n);
    let fuzzed = Definition::parse(&String::from_utf8_lossy(def_bytes)).ok();
    if let Some(d) = &fuzzed {
        assert!(!d.channels().is_empty() && d.channels().len() <= MAX_CHANNELS);
        assert!(!d.announcers().is_empty() && d.announcers().len() <= MAX_ANNOUNCERS);
        assert!(d.announcer_hosts().len() <= MAX_ANNOUNCERS);
    }
    let _ = announce::parse_size(&String::from_utf8_lossy(line_bytes));
    let Ok(Some(msg)) = parse(line_bytes) else {
        return;
    };
    for t in &msg.tags {
        if let Some(v) = &t.value {
            assert!(!v.contains(['\r', '\n', '\0']));
        }
    }
    for p in &msg.params {
        let c = clean_text(p);
        assert!(c.len() <= p.len() && clean(&c));
    }
    let fixed = Matcher::new(vec![Definition::parse(FIXED).expect("fixed definition")])
        .expect("one definition");
    read(&fixed, &msg);
    if let Some(d) = fuzzed {
        read(&Matcher::new(vec![d]).expect("one definition"), &msg);
    }
    let _ = line::irc_eq("#announce", msg.param(0).unwrap_or(""));
});
