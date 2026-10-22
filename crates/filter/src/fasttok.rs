//! The search tokenizer's output for rule matching, without a `String`
//! per token (§20.8).
//!
//! [`lodestar_search::tokenizer::tokenize`] costs about 5 µs for a
//! release path, so a rule that read a 30-file torrent's paths through it
//! would spend the whole budget alone. For ASCII text (most names), a
//! byte scan here produces exactly the same tokens: texts, positions and
//! kinds, in the same order (property-tested against the tokenizer).
//! Other text goes through the tokenizer itself. Token texts are kept in
//! one buffer per text, each with its [`term_hash`], so a comparison is a
//! hash check before the bytes.

use lodestar_search::tokenizer::{MAX_TOKEN_BYTES, TokKind, tokenize};

use crate::hash::term_hash;

/// Words longer than this (in chars) aren't split (the tokenizer's
/// `MAX_SPLIT_CHARS`).
const MAX_SPLIT_CHARS: usize = 24;

/// One token of a [`Text`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Tok {
    pub hash: u64,
    pub position: u32,
    pub kind: TokKind,
    start: u32,
    len: u32,
}

/// A tokenized text: its tokens and their texts.
#[derive(Debug, Clone, Default)]
pub(crate) struct Text {
    buf: String,
    pub toks: Vec<Tok>,
}

impl Text {
    pub fn text(&self, t: &Tok) -> &str {
        &self.buf[t.start as usize..(t.start + t.len) as usize]
    }

    /// Whether `t`'s text is `text`, whose hash is `hash`.
    pub fn is(&self, t: &Tok, hash: u64, text: &str) -> bool {
        t.hash == hash && self.text(t) == text
    }

    fn push(&mut self, position: usize, kind: TokKind, text: impl FnOnce(&mut String)) {
        let start = self.buf.len();
        text(&mut self.buf);
        let len = self.buf.len() - start;
        if len == 0 || len > MAX_TOKEN_BYTES {
            self.buf.truncate(start);
            return;
        }
        self.toks.push(Tok {
            hash: term_hash(&self.buf[start..]),
            position: position as u32,
            kind,
            start: start as u32,
            len: len as u32,
        });
    }
}

/// The tokens [`tokenize`] produces for `text`.
pub(crate) fn tokens(text: &str) -> Text {
    let mut out = Text::default();
    if !text.is_ascii() {
        for t in tokenize(text) {
            out.push(t.position, t.kind, |b| b.push_str(&t.text));
        }
        return out;
    }
    let b = text.as_bytes();
    let lower = |buf: &mut String, bytes: &[u8]| {
        buf.extend(bytes.iter().map(|c| char::from(c.to_ascii_lowercase())));
    };
    // Atoms: runs of letters and digits.
    let mut atoms: Vec<(usize, usize)> = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if !b[i].is_ascii_alphanumeric() {
            i += 1;
            continue;
        }
        let s = i;
        while i < b.len() && b[i].is_ascii_alphanumeric() {
            i += 1;
        }
        atoms.push((s, i));
    }
    let mut pos = 0;
    for (k, &(s, e)) in atoms.iter().enumerate() {
        let word = &b[s..e];
        let first = pos;
        let split = word.len() <= MAX_SPLIT_CHARS && cuts(word).next().is_some();
        if split {
            out.push(first, TokKind::Whole, |buf| lower(buf, word));
            let mut from = 0;
            for cut in cuts(word).chain(std::iter::once(word.len())) {
                out.push(pos, TokKind::Part, |buf| lower(buf, &word[from..cut]));
                from = cut;
                pos += 1;
            }
        } else {
            out.push(pos, TokKind::Part, |buf| lower(buf, word));
            pos += 1;
        }
        let pluses = b[e..].iter().take_while(|&&c| c == b'+').count();
        if (1..=2).contains(&pluses) {
            out.push(first, TokKind::Compound, |buf| {
                lower(buf, word);
                buf.push_str(&"++"[..pluses]);
            });
        }
        if let Some(&(ns, ne)) = atoms.get(k + 1) {
            if ns == e + 1 && joins(word, b[e], &b[ns..ne]) {
                out.push(first, TokKind::Compound, |buf| {
                    lower(buf, word);
                    lower(buf, &b[ns..ne]);
                });
            }
        }
    }
    out
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Class {
    Upper,
    Lower,
    Digit,
}

fn class(c: u8) -> Class {
    if c.is_ascii_digit() {
        Class::Digit
    } else if c.is_ascii_uppercase() {
        Class::Upper
    } else {
        Class::Lower
    }
}

/// The tokenizer's part boundaries in an ASCII word: letter/digit edges,
/// camelCase, and before the last capital of a capital run.
fn cuts(word: &[u8]) -> impl Iterator<Item = usize> + '_ {
    (1..word.len()).filter(move |&i| {
        let (a, b) = (class(word[i - 1]), class(word[i]));
        let digit_edge = (a == Class::Digit) != (b == Class::Digit);
        let lower_upper = a == Class::Lower && b == Class::Upper;
        let acronym_end = a == Class::Upper
            && b == Class::Upper
            && word.get(i + 1).is_some_and(|&n| class(n) == Class::Lower);
        digit_edge || lower_upper || acronym_end
    })
}

/// The tokenizer's rule for two words joined by `sep` (5.1, H.264,
/// WEB-DL, 24-96).
fn joins(a: &[u8], sep: u8, b: &[u8]) -> bool {
    let digits = |w: &[u8]| w.len() <= 3 && w.iter().all(u8::is_ascii_digit);
    let letters = |w: &[u8]| w.iter().all(u8::is_ascii_alphabetic);
    match sep {
        b'.' => (digits(a) && digits(b)) || (letters(a) && a.len() <= 2 && digits(b)),
        b'-' => {
            (digits(a) && digits(b))
                || (letters(a) && a.len() <= 4 && (letters(b) || digits(b)) && b.len() <= 4)
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn ours(text: &str) -> Vec<(String, usize, TokKind)> {
        let t = tokens(text);
        t.toks
            .iter()
            .map(|x| {
                assert_eq!(x.hash, term_hash(t.text(x)));
                (t.text(x).to_owned(), x.position as usize, x.kind)
            })
            .collect()
    }

    fn theirs(text: &str) -> Vec<(String, usize, TokKind)> {
        tokenize(text)
            .into_iter()
            .map(|t| (t.text, t.position, t.kind))
            .collect()
    }

    proptest! {
        /// Exactly the tokenizer's tokens, in its order.
        #[test]
        fn ascii_is_the_tokenizers(text in "[ -~]{0,160}") {
            prop_assert_eq!(ours(&text), theirs(&text));
        }

        #[test]
        fn release_like_is_the_tokenizers(
            parts in prop::collection::vec("[A-Za-z]{1,6}|[0-9]{1,4}|[A-Z][a-z]{1,5}|x26[45]|DD\\+\\+?|5\\.1|WEB-DL|[a-z]{1,3}[A-Z]{2,4}[a-z]{1,3}|[a-zA-Z0-9]{20,70}", 1..14),
            seps in prop::collection::vec("[ ._\\-+/()\\[\\]]{1,3}", 14),
        ) {
            let mut text = String::new();
            for (i, p) in parts.iter().enumerate() {
                text.push_str(p);
                text.push_str(&seps[i]);
            }
            prop_assert_eq!(ours(&text), theirs(&text));
        }

        #[test]
        fn any_text_is_the_tokenizers(text in "\\PC{0,80}") {
            prop_assert_eq!(ours(&text), theirs(&text));
        }
    }

    #[test]
    fn examples() {
        for t in [
            "Show.Name.S01E02.1080p.WEB-DL.DD+5.1.x265-GROUP",
            "C++ a+++b 24-96 H.264 AC3 iPhone HTTPServer",
            "Show.Name.S01/Show.Name.S01E01.WEB-DL.mkv",
            "",
            "....",
        ] {
            assert_eq!(ours(t), theirs(t), "{t:?}");
        }
    }
}
