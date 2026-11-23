//! The IRC client's line handling (IRC-0) never panics on server bytes,
//! and what it yields is bounded: the framer holds less than one line and
//! passes on lines under `MAX_LINE_BYTES`; a parsed message has at most
//! `MAX_PARAMS` parameters and `MAX_TAGS` tags, and what `encode` writes
//! from it parses back to the same command and parameters. CTCP
//! detection, formatting removal and `CAP` lists run over what it yields.
//!
//! Input: the whole input as one line, then as a stream cut into chunks of
//! `first byte + 1` bytes.
#![no_main]

use libfuzzer_sys::fuzz_target;
use lodestar_sources_irc::line::{
    Framed, LineBuffer, MAX_LINE_BYTES, MAX_PARAMS, MAX_TAGS, cap_entries, encode, parse,
    sasl_allows, strip_formatting,
};

fn check(line: &[u8]) {
    let Ok(Some(m)) = parse(line) else {
        return;
    };
    assert!(m.params.len() <= MAX_PARAMS);
    assert!(m.tags.len() <= MAX_TAGS);
    assert!(!m.command.is_empty() && m.command.len() <= 32);
    let _ = (m.nick(), m.numeric(), m.ctcp());
    for p in &m.params {
        assert!(strip_formatting(p).len() <= p.len());
    }
    if let Some(last) = m.params.last() {
        for cap in cap_entries(last) {
            let _ = sasl_allows(cap.value, "PLAIN");
        }
    }
    let params: Vec<&str> = m.params.iter().map(String::as_str).collect();
    if let Ok(out) = encode(&m.command, &params) {
        let again = parse(&out[..out.len() - 1])
            .expect("an encoded line parses")
            .expect("an encoded line isn't blank");
        assert_eq!(again.command, m.command);
        assert_eq!(again.params, m.params);
    }
}

fuzz_target!(|data: &[u8]| {
    check(data);
    let step = data.first().map_or(1, |b| usize::from(*b) + 1);
    let mut lines = LineBuffer::new();
    for chunk in data.chunks(step) {
        lines.feed(chunk, |f| {
            if let Framed::Line(l) = f {
                assert!(l.len() < MAX_LINE_BYTES);
                check(l);
            }
        });
        assert!(lines.pending() < MAX_LINE_BYTES);
    }
});
