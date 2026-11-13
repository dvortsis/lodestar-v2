//! The judgments file: queries, their rationale and graded torrents.
//!
//! ```text
//! # a comment
//! ? the query, as typed
//! = one line on what the ideal results are, and why
//! 3 key key …      exactly what's asked
//! 2 key …          a right answer
//! 1 key …          related, acceptable further down
//! 0 key …          judged not relevant (the same as unjudged)
//! x key …          must never be shown
//! ```
//!
//! A key is a corpus key with `--dir` alone, and an info hash with
//! `--data-dir`: 40 hex digits, either case, the v1 hash (for a v2-only
//! torrent, the first 40 digits of its v2 hash, which is what the store
//! keys it by). Key a hybrid torrent by its v1 hash: the first 40 digits
//! of its v2 hash aren't looked up, so it would count as not in the
//! store. A 64-digit v2 hash is refused for the same reason.
//!
//! The file is treated as untrusted: it's capped at [`MAX_BYTES`], the
//! parser never panics, and every error names its line. With `redact`,
//! an error says only the line and what's wrong, never the line's text.

use std::collections::{HashMap, HashSet};
use std::fmt;

/// The largest judgments file read: far beyond any hand-written set,
/// small enough that parsing it stays cheap.
pub const MAX_BYTES: u64 = 16 << 20;

/// What a judgment says about one torrent for one query.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Grade {
    /// 0–3: not relevant, related, a right answer, exactly what's asked.
    Graded(u8),
    /// Must never be in the results (Adult while it's hidden, a negated
    /// word).
    Forbidden,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Judged {
    pub query: String,
    pub why: String,
    pub grades: HashMap<String, Grade>,
    /// The `?` line's number, for errors that mustn't quote the query.
    pub line: usize,
}

/// What a key must be.
pub enum Keys<'a> {
    /// One of the corpus's keys.
    Corpus(&'a HashSet<&'a str>),
    /// An info hash, stored lowercase.
    Hash,
}

/// A judgments error: the line, what's wrong, and the offending text,
/// which [`ParseError::show`] leaves out when redacting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError {
    pub line: usize,
    pub what: &'static str,
    pub detail: Option<String>,
}

impl ParseError {
    pub fn show(&self, redact: bool) -> String {
        match &self.detail {
            Some(d) if !redact => format!("judgments.txt:{}: {}: {d}", self.line, self.what),
            _ => format!("judgments.txt:{}: {}", self.line, self.what),
        }
    }
}

impl std::error::Error for ParseError {}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.show(false))
    }
}

/// Every [`ParseError::what`], so tests can check a redacted error says
/// nothing else.
#[cfg(test)]
pub const MESSAGES: &[&str] = &[
    "empty query",
    "query judged twice",
    "a judgment before any query",
    "a second rationale line",
    "expected `?`, `=`, a grade 0–3 or `x`",
    "unknown torrent",
    "torrent judged twice for this query",
    "query has no rationale",
    "expected a 40-hex-digit info hash",
    "a 64-digit v2 hash: use the v1 hash (or a v2-only torrent's first 40 digits)",
    "a grade line with no torrents",
    "empty rationale",
];

pub fn parse(text: &str, keys: &Keys<'_>) -> Result<Vec<Judged>, ParseError> {
    let mut out: Vec<Judged> = Vec::new();
    let mut seen = HashSet::new();
    // Windows editors may start a UTF-8 file with a byte-order mark.
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    for (n, line) in text.lines().enumerate() {
        let n = n + 1;
        let err = |what, detail: Option<&str>| ParseError {
            line: n,
            what,
            detail: detail.map(str::to_string),
        };
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        // The tag is one character, which needn't be one byte.
        let mut chars = line.chars();
        let tag = chars.next().unwrap_or(' ');
        let rest = chars.as_str().trim();
        if tag == '?' {
            if rest.is_empty() {
                return Err(err("empty query", None));
            }
            if !seen.insert(rest.to_string()) {
                return Err(err("query judged twice", Some(rest)));
            }
            out.push(Judged {
                query: rest.to_string(),
                why: String::new(),
                grades: HashMap::new(),
                line: n,
            });
            continue;
        }
        let last = out
            .last_mut()
            .ok_or_else(|| err("a judgment before any query", None))?;
        let grade = match tag {
            '=' => {
                if !last.why.is_empty() {
                    return Err(err("a second rationale line", None));
                }
                if rest.is_empty() {
                    return Err(err("empty rationale", None));
                }
                last.why = rest.to_string();
                continue;
            }
            '0'..='3' => Grade::Graded(tag as u8 - b'0'),
            'x' => Grade::Forbidden,
            _ => return Err(err("expected `?`, `=`, a grade 0–3 or `x`", None)),
        };
        // A grade's tag stands alone: `3a` isn't grade 3 for `a`.
        if !line[tag.len_utf8()..].starts_with(char::is_whitespace) && !rest.is_empty() {
            return Err(err("expected `?`, `=`, a grade 0–3 or `x`", None));
        }
        if rest.is_empty() && matches!(keys, Keys::Hash) {
            return Err(err("a grade line with no torrents", None));
        }
        for key in rest.split_whitespace() {
            let key = match keys {
                Keys::Corpus(known) => {
                    if !known.contains(key) {
                        return Err(err("unknown torrent", Some(key)));
                    }
                    key.to_string()
                }
                Keys::Hash => hash_key(key).map_err(|what| err(what, Some(key)))?,
            };
            if last.grades.contains_key(&key) {
                return Err(err("torrent judged twice for this query", Some(&key)));
            }
            last.grades.insert(key, grade);
        }
    }
    if let Some(j) = out.iter().find(|j| j.why.is_empty()) {
        return Err(ParseError {
            line: j.line,
            what: "query has no rationale",
            detail: Some(j.query.clone()),
        });
    }
    Ok(out)
}

/// A v1 info hash as the key, lowercased.
fn hash_key(s: &str) -> Result<String, &'static str> {
    let hex = s.bytes().all(|b| b.is_ascii_hexdigit());
    match s.len() {
        40 if hex => Ok(s.to_ascii_lowercase()),
        64 if hex => {
            Err("a 64-digit v2 hash: use the v1 hash (or a v2-only torrent's first 40 digits)")
        }
        _ => Err("expected a 40-hex-digit info hash"),
    }
}

/// A key's 20 bytes: it was checked by [`parse`].
pub fn hash_bytes(key: &str) -> Option<[u8; 20]> {
    let b = key.as_bytes();
    if b.len() != 40 {
        return None;
    }
    let mut out = [0u8; 20];
    for (i, pair) in b.chunks_exact(2).enumerate() {
        let hex = std::str::from_utf8(pair).ok()?;
        out[i] = u8::from_str_radix(hex, 16).ok()?;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::{RngExt, SeedableRng};

    const H1: &str = "0a1b2c3d4e5f60718293a4b5c6d7e8f90a1b2c3d";
    const H2: &str = "f0e1d2c3b4a5968778695a4b3c2d1e0ff0e1d2c3";

    #[test]
    fn corpus_keys_parse_and_validate() {
        let known: HashSet<&str> = ["a", "b", "c"].into();
        let keys = Keys::Corpus(&known);
        let j = parse("? q\n= why\n3 a\n1 b\nx c\n", &keys).unwrap();
        assert_eq!(j[0].grades["a"], Grade::Graded(3));
        assert_eq!(j[0].grades["c"], Grade::Forbidden);
        assert_eq!(j[0].line, 1);
        for bad in [
            "? q\n3 a\n",
            "? q\n= why\n3 zz\n",
            "? q\n= why\n3 a\n2 a\n",
            "? q\n= why\n? q\n= again\n",
            "3 a\n",
            "? q\n= why\n= twice\n",
            "? q\n= why\n4 a\n",
            "? q\n= why\n3a\n",
        ] {
            assert!(parse(bad, &keys).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn hashes_are_keys_lowercased() {
        let text = format!("? q\n= why\n3 {}\nx {H2}\n", H1.to_uppercase());
        let j = parse(&text, &Keys::Hash).unwrap();
        assert_eq!(j[0].grades[H1], Grade::Graded(3));
        assert_eq!(j[0].grades[H2], Grade::Forbidden);
        let bom = format!("\u{feff}{text}");
        assert_eq!(parse(&bom, &Keys::Hash).unwrap(), j);
        assert_eq!(hash_bytes(H1).unwrap()[..3], [0x0a, 0x1b, 0x2c]);
    }

    /// Each malformed case, its line and its message; the redacted form
    /// never quotes the line.
    #[test]
    fn golden_errors() {
        let v2 = "ab".repeat(32);
        let cases: Vec<(String, &str)> = vec![
            ("?\n".into(), "judgments.txt:1: empty query"),
            (
                format!("? q\n= w\n3 {H1}\n? q\n= w\n"),
                "judgments.txt:4: query judged twice",
            ),
            (
                format!("3 {H1}\n"),
                "judgments.txt:1: a judgment before any query",
            ),
            (
                "? q\n= w\n= w2\n".into(),
                "judgments.txt:3: a second rationale line",
            ),
            (
                format!("? q\n= w\n4 {H1}\n"),
                "judgments.txt:3: expected `?`, `=`, a grade 0–3 or `x`",
            ),
            (
                format!("? q\n= w\n3{H1}\n"),
                "judgments.txt:3: expected `?`, `=`, a grade 0–3 or `x`",
            ),
            (
                format!("? q\n= w\n3 {H1}\n\n2 {}\n", H1.to_uppercase()),
                "judgments.txt:5: torrent judged twice for this query",
            ),
            (
                format!("# c\n? q\n3 {H1}\n"),
                "judgments.txt:2: query has no rationale",
            ),
            (
                "? q\n= w\n3 abc\n".into(),
                "judgments.txt:3: expected a 40-hex-digit info hash",
            ),
            (
                format!("? q\n= w\n3 {}\n", "g".repeat(40)),
                "judgments.txt:3: expected a 40-hex-digit info hash",
            ),
            (
                format!("? q\n= w\n3 {v2}\n"),
                "judgments.txt:3: a 64-digit v2 hash: use the v1 hash (or a v2-only torrent's first 40 digits)",
            ),
            (
                "? q\n= w\n3\n".into(),
                "judgments.txt:3: a grade line with no torrents",
            ),
            ("? q\n=\n".into(), "judgments.txt:2: empty rationale"),
        ];
        for (text, want) in cases {
            let e = parse(&text, &Keys::Hash).unwrap_err();
            assert_eq!(e.show(true), want, "{text:?}");
            assert!(e.show(false).starts_with(want), "{text:?}");
        }
    }

    /// Random lines from the grammar's pieces and noise: never a panic,
    /// and a redacted error is only its line and a fixed message.
    #[test]
    fn random_lines_never_panic_or_leak() {
        let pieces = [
            "?", "=", "0", "1", "2", "3", "x", "#", " ", "\t", "\r", "é", "✓", "\u{0}", H1, H2,
            "ABCDEF", "q", "lantern", "3 ", "x ", "? ", "= ", "\u{feff}",
        ];
        let mut rng = rand_chacha::ChaCha8Rng::seed_from_u64(0x5eed);
        for _ in 0..20_000 {
            let lines = rng.random_range(0..8);
            let mut text = String::new();
            for _ in 0..lines {
                for _ in 0..rng.random_range(0..6) {
                    text.push_str(pieces[rng.random_range(0..pieces.len())]);
                }
                text.push('\n');
            }
            if let Err(e) = parse(&text, &Keys::Hash) {
                let shown = e.show(true);
                assert!(MESSAGES.contains(&e.what), "{shown}");
                assert_eq!(shown, format!("judgments.txt:{}: {}", e.line, e.what));
                assert!(e.line >= 1 && e.line <= text.lines().count(), "{text:?}");
            }
        }
    }
}
