//! A companion's output: a ring bounded in bytes, filled one line at a
//! time, with credentials redacted before a line enters it (plan Q7).
//!
//! - **Bounded.** [`RING_BYTES`] per companion, counting each line's bytes
//!   plus one for its newline; the oldest lines go first. A line is cut at
//!   [`LINE_BYTES`], so output without a newline can't grow without limit.
//! - **Redacted first.** A [`Lines`] assembler joins reads into whole lines
//!   before [`Redactor::redact`] sees them, so a secret split across two
//!   reads is still found. A long line's matches are found over a window
//!   [`MAX_SECRET`] bytes wider than the cut, and only raw text before the
//!   cut is kept, so a secret up to that long leaves no prefix at the cut.
//! - **Lossy.** Output that isn't UTF-8 is read lossily; nothing panics.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, PoisonError};

use lodestar_integrations::secret::Secret;
use zeroize::Zeroizing;

/// The ring's size per companion (plan Q7: 1 MB).
pub const RING_BYTES: usize = 1 << 20;
/// The longest line kept, in bytes; the rest of the line is dropped.
pub const LINE_BYTES: usize = 8 * 1024;
/// The longest secret [`Redactor`] guarantees to find at a line's cut.
/// Longer known secrets are still redacted wherever they're whole.
pub const MAX_SECRET: usize = 512;
/// What a redacted value becomes.
pub const REDACTED: &str = "[redacted]";
/// Appended to a line that was cut.
const CUT: &str = " [cut]";
/// Known secrets shorter than this are matched only as whole tokens
/// (between bytes that can't be part of a word): anywhere, they would
/// redact parts of ordinary words.
const MIN_SECRET: usize = 6;

/// Keys whose value is redacted wherever it follows `=` or `:`
/// (matched without case; `-` and `_` are ignored inside a key).
const KEYS: &[&str] = &[
    "password",
    "passwd",
    "pass",
    "pwd",
    "apikey",
    "token",
    "accesstoken",
    "secret",
    "clientsecret",
    "sid",
    "session",
];
/// Keys whose whole remaining line is redacted (`Authorization: Basic …`,
/// every pair of a `Cookie:` header).
const LINE_KEYS: &[&str] = &[
    "authorization",
    "proxyauthorization",
    "xapikey",
    "cookie",
    "setcookie",
];

/// Removes credentials from a line of a companion's output.
#[derive(Default)]
pub struct Redactor {
    secrets: Vec<Zeroizing<String>>,
}

impl std::fmt::Debug for Redactor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Redactor({} secrets)", self.secrets.len())
    }
}

impl Redactor {
    /// Redacts these secrets (the companion's generated and pasted
    /// credentials, CMP-3) as well as the common shapes.
    pub fn new<'a>(secrets: impl IntoIterator<Item = &'a Secret>) -> Redactor {
        let mut secrets: Vec<Zeroizing<String>> = secrets
            .into_iter()
            .map(|s| s.expose())
            .filter(|s| !s.is_empty())
            .map(|s| Zeroizing::new(s.to_owned()))
            .collect();
        secrets.sort_by(|a, b| a.as_str().cmp(b.as_str()));
        secrets.dedup_by(|a, b| a.as_str() == b.as_str());
        Redactor { secrets }
    }

    /// The line with every known secret and every value after a known key
    /// replaced by [`REDACTED`].
    pub fn redact(&self, line: &str) -> String {
        self.redact_to(line, line.len())
    }

    /// The first `cut` bytes of `line`, redacted. Matches are found in the
    /// whole of `line`, so one that crosses the cut is still replaced and
    /// leaves no prefix; the result never holds text from past the cut.
    pub fn redact_to(&self, line: &str, cut: usize) -> String {
        let mut cut = cut.min(line.len());
        while !line.is_char_boundary(cut) {
            cut -= 1;
        }
        let mut out = String::with_capacity(cut);
        let mut copied = 0;
        for (start, end) in self.spans(line) {
            if start >= cut {
                break;
            }
            out.push_str(&line[copied..start]);
            out.push_str(REDACTED);
            copied = end;
        }
        if copied < cut {
            out.push_str(&line[copied..cut]);
        }
        out
    }

    /// Byte ranges to redact, sorted and merged.
    fn spans(&self, line: &str) -> Vec<(usize, usize)> {
        let b = line.as_bytes();
        let mut spans = key_spans(line);
        for s in &self.secrets {
            for (at, m) in line.match_indices(s.as_str()) {
                let end = at + m.len();
                let token = s.len() >= MIN_SECRET
                    || ((at == 0 || !key_byte(b[at - 1])) && (end == b.len() || !key_byte(b[end])));
                if token {
                    spans.push((at, end));
                }
            }
        }
        spans.sort_unstable();
        let mut merged: Vec<(usize, usize)> = Vec::with_capacity(spans.len());
        for (s, e) in spans {
            match merged.last_mut() {
                Some(last) if s <= last.1 => last.1 = last.1.max(e),
                _ => merged.push((s, e)),
            }
        }
        merged
    }
}

/// Whether `b` ends a value after a key.
fn ends_value(b: u8) -> bool {
    b.is_ascii_whitespace() || matches!(b, b'&' | b'"' | b'\'' | b',' | b';' | b'}' | b'<' | b')')
}

/// Whether `b` may be part of a key name.
fn key_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'-' || b == b'_'
}

/// The value after each key in [`KEYS`] and the rest of the line after
/// each key in [`LINE_KEYS`]. A key is a run of letters, digits, `-` and
/// `_` (compared without case, ignoring `-` and `_`), followed by optional
/// quotes and spaces, then `=` or `:`.
fn key_spans(line: &str) -> Vec<(usize, usize)> {
    let b = line.as_bytes();
    let mut spans = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if !key_byte(b[i]) || (i > 0 && key_byte(b[i - 1])) {
            i += 1;
            continue;
        }
        let start = i;
        while i < b.len() && key_byte(b[i]) {
            i += 1;
        }
        let name: String = line[start..i]
            .bytes()
            .filter(|c| c.is_ascii_alphanumeric())
            .map(|c| c.to_ascii_lowercase() as char)
            .collect();
        let whole_line = LINE_KEYS.contains(&name.as_str());
        if !whole_line && !KEYS.contains(&name.as_str()) {
            continue;
        }
        let mut j = i;
        while j < b.len() && matches!(b[j], b'"' | b'\'' | b' ' | b'\t') {
            j += 1;
        }
        if j >= b.len() || !matches!(b[j], b'=' | b':') {
            continue;
        }
        j += 1;
        while j < b.len() && matches!(b[j], b'"' | b'\'' | b' ' | b'\t') {
            j += 1;
        }
        let end = if whole_line {
            b.len()
        } else {
            let mut k = j;
            while k < b.len() && !ends_value(b[k]) {
                k += 1;
            }
            k
        };
        if end > j {
            spans.push((j, end));
        }
        i = end.max(i);
    }
    spans
}

/// The ring itself: whole, redacted lines, oldest first.
#[derive(Debug)]
struct Ring {
    lines: VecDeque<String>,
    bytes: usize,
    cap: usize,
}

/// One companion's log ring, shared between its output readers and
/// whoever shows it. The lock is held only to push or copy lines, never
/// across an await.
#[derive(Debug, Clone)]
pub struct LogRing(Arc<Mutex<Ring>>);

impl Default for LogRing {
    fn default() -> LogRing {
        LogRing::with_capacity(RING_BYTES)
    }
}

impl LogRing {
    /// A ring holding at most `cap` bytes.
    pub fn with_capacity(cap: usize) -> LogRing {
        LogRing(Arc::new(Mutex::new(Ring {
            lines: VecDeque::new(),
            bytes: 0,
            cap,
        })))
    }

    /// Adds one line that has already been redacted and cut.
    fn push(&self, line: String) {
        let mut ring = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        let cost = line.len() + 1;
        if cost > ring.cap {
            return;
        }
        while ring.bytes + cost > ring.cap {
            match ring.lines.pop_front() {
                Some(old) => ring.bytes -= old.len() + 1,
                None => break,
            }
        }
        ring.bytes += cost;
        ring.lines.push_back(line);
    }

    /// A copy of the lines, oldest first.
    pub fn lines(&self) -> Vec<String> {
        let ring = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        ring.lines.iter().cloned().collect()
    }

    /// The bytes the ring holds, newlines included.
    pub fn bytes(&self) -> usize {
        self.0.lock().unwrap_or_else(PoisonError::into_inner).bytes
    }
}

/// Joins one stream's reads into lines, redacts them and pushes them into
/// a ring.
#[derive(Debug)]
pub struct Lines {
    ring: LogRing,
    redactor: Arc<Redactor>,
    buf: Vec<u8>,
    /// The current line was cut: drop the rest of it.
    dropping: bool,
}

impl Lines {
    pub fn new(ring: LogRing, redactor: Arc<Redactor>) -> Lines {
        Lines {
            ring,
            redactor,
            buf: Vec::new(),
            dropping: false,
        }
    }

    /// Feeds one read.
    pub fn feed(&mut self, mut data: &[u8]) {
        while !data.is_empty() {
            let newline = data.iter().position(|&b| b == b'\n');
            let take = newline.map_or(data.len(), |n| n + 1);
            let (chunk, rest) = data.split_at(take);
            data = rest;
            if !self.dropping {
                self.buf.extend_from_slice(chunk);
            }
            if newline.is_some() {
                if !self.dropping {
                    self.emit(false);
                }
                self.buf.clear();
                self.dropping = false;
            } else if self.buf.len() >= LINE_BYTES + MAX_SECRET {
                self.emit(true);
                self.buf.clear();
                self.dropping = true;
            }
        }
    }

    /// Pushes a line left without a newline (the stream ended).
    pub fn finish(&mut self) {
        if !self.buf.is_empty() && !self.dropping {
            self.emit(false);
        }
        self.buf.clear();
        self.dropping = false;
    }

    fn emit(&mut self, cut: bool) {
        let text = String::from_utf8_lossy(&self.buf);
        let text = text.trim_end_matches(['\n', '\r']);
        let cut = cut || text.len() > LINE_BYTES;
        // Only raw text before the cut can reach the ring; matches are
        // found over the whole window.
        let mut line = self.redactor.redact_to(text, LINE_BYTES);
        if cut {
            // Replacements can lengthen it; what's trimmed now is already
            // redacted.
            let mut at = LINE_BYTES.min(line.len());
            while !line.is_char_boundary(at) {
                at -= 1;
            }
            line.truncate(at);
            line.push_str(CUT);
        }
        self.ring.push(line);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines_with(secrets: &[&str]) -> (LogRing, Lines) {
        let secrets: Vec<Secret> = secrets.iter().map(|s| Secret::new(*s)).collect();
        let ring = LogRing::default();
        let lines = Lines::new(ring.clone(), Arc::new(Redactor::new(&secrets)));
        (ring, lines)
    }

    #[test]
    fn a_secret_split_across_two_reads_is_redacted() {
        let (ring, mut lines) = lines_with(&["s3cr3t-Value-42"]);
        lines.feed(b"login with s3cr3t-Va");
        lines.feed(b"lue-42 ok\nnext\n");
        assert_eq!(ring.lines(), ["login with [redacted] ok", "next"]);
    }

    #[test]
    fn common_shapes_are_redacted() {
        let r = Redactor::new([]);
        assert_eq!(
            r.redact("GET /x?apikey=abc123&q=1"),
            "GET /x?apikey=[redacted]&q=1"
        );
        assert_eq!(
            r.redact("Authorization: Basic dXNlcjpwYXNz"),
            "Authorization: [redacted]"
        );
        assert_eq!(
            r.redact(r#"{"Password": "hunter22"}"#),
            r#"{"Password": "[redacted]"}"#
        );
        assert_eq!(r.redact("X-Api-Key: 0123abcd"), "X-Api-Key: [redacted]");
        assert_eq!(r.redact("api_key = zzz; ok"), "api_key = [redacted]; ok");
        assert_eq!(r.redact("Set-Cookie: SID=abc"), "Set-Cookie: [redacted]");
        assert_eq!(
            r.redact("token=a password=b"),
            "token=[redacted] password=[redacted]"
        );
        // Not keys: part of a longer word, or no separator.
        assert_eq!(
            r.redact("passport=1 password reset"),
            "passport=1 password reset"
        );
        assert_eq!(r.redact("bypass=yes"), "bypass=yes");
    }

    #[test]
    fn short_secrets_are_matched_as_whole_tokens_only() {
        let (ring, mut lines) = lines_with(&["abc", "longer-secret"]);
        lines.feed(b"abc longer-secret abcd xabc (abc)\n");
        assert_eq!(
            ring.lines(),
            ["[redacted] [redacted] abcd xabc ([redacted])"]
        );
    }

    #[test]
    fn earlier_redactions_pull_no_secret_prefix_under_the_cut() {
        // Each replacement shortens the line; a secret partly past the
        // window must still not show at the cut.
        let s = "S".repeat(50) + &"T".repeat(50);
        let (ring, mut lines) = lines_with(&[&s]);
        let mut line = s.repeat(6).into_bytes();
        line.extend_from_slice(&[b'a'; 8054]);
        line.extend_from_slice(&s.as_bytes()[..50]);
        for chunk in line.chunks(13) {
            lines.feed(chunk);
        }
        let got = ring.lines();
        assert_eq!(got.len(), 1);
        assert!(!got[0].contains('S'), "a prefix leaked");
        assert!(!got[0].contains('T'));
    }

    #[test]
    fn every_cookie_pair_is_redacted() {
        let r = Redactor::new([]);
        assert_eq!(
            r.redact("Cookie: SID=x; other_session=y"),
            "Cookie: [redacted]"
        );
    }

    #[test]
    fn a_secret_containing_another_is_redacted_whole() {
        let (ring, mut lines) = lines_with(&["secret1", "secret1-and-more"]);
        lines.feed(b"x secret1-and-more y\n");
        assert_eq!(ring.lines(), ["x [redacted] y"]);
    }

    #[test]
    fn a_long_line_is_cut_and_the_rest_dropped() {
        let (ring, mut lines) = lines_with(&[]);
        for _ in 0..100 {
            lines.feed(&[b'a'; 1000]);
        }
        lines.feed(b"tail\nnext line\n");
        let got = ring.lines();
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].len(), LINE_BYTES + CUT.len());
        assert!(got[0].ends_with(CUT));
        assert_eq!(got[1], "next line");
    }

    #[test]
    fn a_secret_at_the_cut_leaves_no_prefix() {
        let secret = "Z9-secret-at-the-cut-Z9";
        let (ring, mut lines) = lines_with(&[secret]);
        let mut line = vec![b'a'; LINE_BYTES - 5];
        line.extend_from_slice(secret.as_bytes());
        line.extend_from_slice(&[b'b'; MAX_SECRET]);
        line.push(b'\n');
        for chunk in line.chunks(7) {
            lines.feed(chunk);
        }
        let got = ring.lines();
        assert_eq!(got.len(), 1);
        assert!(!got[0].contains("Z9-"), "a prefix leaked");
        assert!(got[0].len() <= LINE_BYTES + CUT.len());
    }

    #[test]
    fn the_ring_stays_within_its_cap() {
        let ring = LogRing::with_capacity(100);
        let mut lines = Lines::new(ring.clone(), Arc::new(Redactor::default()));
        for i in 0..50 {
            lines.feed(format!("line number {i}\n").as_bytes());
        }
        assert!(ring.bytes() <= 100);
        let got = ring.lines();
        assert_eq!(got.last().map(String::as_str), Some("line number 49"));
        assert_eq!(ring.bytes(), got.iter().map(|l| l.len() + 1).sum::<usize>());
    }

    #[test]
    fn the_default_ring_is_one_megabyte() {
        let ring = LogRing::default();
        let mut lines = Lines::new(ring.clone(), Arc::new(Redactor::default()));
        let line = [b'x'; 1000];
        for _ in 0..2000 {
            lines.feed(&line);
            lines.feed(b"\n");
        }
        assert!(ring.bytes() <= RING_BYTES);
        assert!(ring.bytes() > RING_BYTES - 1001);
    }

    #[test]
    fn non_utf8_output_is_read_lossily() {
        let (ring, mut lines) = lines_with(&[]);
        lines.feed(&[0xff, 0xfe, b'o', b'k', 0xc3]);
        lines.feed(b"\r\n");
        lines.feed(b"unterminated");
        lines.finish();
        assert_eq!(ring.lines(), ["\u{fffd}\u{fffd}ok\u{fffd}", "unterminated"]);
    }
}
