//! The name tokenizer (architecture §8, "the most important quality
//! lever"). Torrent names are dense with punctuation and run-together
//! words: `Show.Name.S01E02.1080p.WEB-DL.DD+5.1.x265-GROUP`.
//!
//! - Split on whitespace and punctuation, then inside each word at
//!   camelCase and letter/digit boundaries (`S01E02` → `s 01 e 02`).
//! - Also emit the compound, at the position of its first part, so both
//!   `s01e02` and the phrase `s 01 e 02` match: `x265`, `1080p`,
//!   `showname`. Short pairs joined by `.` or `-` become one compound too
//!   (`5.1` → `51`, `WEB-DL` → `webdl`, `H.264` → `h264`, `24-96` →
//!   `2496`), and a trailing `+` is kept (`DD+` → `dd+`).
//! - Fold: compatibility decomposition (full-width `１０８０ｐ` → `1080p`),
//!   diacritics removed, a few letters expanded (`ß` → `ss`), lowercase.
//! - CJK runs become overlapping bigrams (a dictionary segmenter is an
//!   optional download later, §11).
//!
//! The same function tokenizes queries, so both sides always agree.
//! Input is untrusted: output is bounded and offsets always land on char
//! boundaries of the original text.

use unicode_normalization::UnicodeNormalization;
use unicode_normalization::char::is_combining_mark;

/// Tokens longer than this (in bytes) are dropped.
pub const MAX_TOKEN_BYTES: usize = 64;
/// Words longer than this (in chars) aren't split into parts: they're
/// hashes or junk, and their parts would only bloat the index.
const MAX_SPLIT_CHARS: usize = 24;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokKind {
    /// A word or a piece of one.
    Part,
    /// A whole word that also split into parts at case or digit
    /// boundaries (`x265`, `s01e02`, `showname`), at its first part's
    /// position. Indexing always emits it with its parts, so a query can
    /// match it instead of phrasing the parts (which may be very common:
    /// the `p` of `1080p`).
    Whole,
    /// Words joined across punctuation (`webdl`, `51`) or with a trailing
    /// `+` (`dd+`), at the first word's position.
    Compound,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tok {
    pub text: String,
    /// Byte range in the original text.
    pub from: usize,
    pub to: usize,
    pub position: usize,
    pub kind: TokKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Class {
    Upper,
    Lower,
    /// Letters without case (Arabic, Thai, …).
    Letter,
    Digit,
    Cjk,
    Plus,
    Sep,
}

/// One normalized char with the byte range of the original char it came
/// from.
#[derive(Debug, Clone, Copy)]
struct NChar {
    c: char,
    from: usize,
    to: usize,
    class: Class,
}

fn is_cjk(c: char) -> bool {
    matches!(c as u32,
        0x3040..=0x30FF   // Hiragana, Katakana
        | 0x31F0..=0x31FF // Katakana extensions
        | 0x3400..=0x4DBF // CJK extension A
        | 0x4E00..=0x9FFF // CJK unified
        | 0xAC00..=0xD7AF // Hangul syllables
        | 0x1100..=0x11FF // Hangul jamo
        | 0x3130..=0x318F // Hangul compatibility jamo
        | 0xF900..=0xFAFF // CJK compatibility
        | 0x20000..=0x2A6DF)
}

fn classify(c: char) -> Class {
    if c == '+' {
        Class::Plus
    } else if is_cjk(c) {
        Class::Cjk
    } else if c.is_numeric() {
        Class::Digit
    } else if c.is_uppercase() {
        Class::Upper
    } else if c.is_lowercase() {
        Class::Lower
    } else if c.is_alphabetic() {
        Class::Letter
    } else {
        Class::Sep
    }
}

/// Letters NFKD doesn't decompose.
fn expand(c: char) -> Option<&'static str> {
    Some(match c {
        'ß' => "ss",
        'ẞ' => "SS",
        'æ' => "ae",
        'Æ' => "AE",
        'œ' => "oe",
        'Œ' => "OE",
        'ø' => "o",
        'Ø' => "O",
        'đ' | 'ð' => "d",
        'Đ' | 'Ð' => "D",
        'ł' => "l",
        'Ł' => "L",
        'þ' => "th",
        'Þ' => "TH",
        'ı' => "i",
        _ => return None,
    })
}

fn normalize(text: &str) -> Vec<NChar> {
    let mut out = Vec::with_capacity(text.len());
    for (from, ch) in text.char_indices() {
        let to = from + ch.len_utf8();
        let mut push = |c: char| {
            out.push(NChar {
                c,
                from,
                to,
                class: classify(c),
            })
        };
        if let Some(s) = expand(ch) {
            s.chars().for_each(&mut push);
            continue;
        }
        // NFKD would split Hangul syllables into jamo; CJK needs no folding.
        if ch.is_ascii() || is_cjk(ch) {
            push(ch);
            continue;
        }
        for c in std::iter::once(ch).nfkd() {
            if !is_combining_mark(c) {
                match expand(c) {
                    Some(s) => s.chars().for_each(&mut push),
                    None => push(c),
                }
            }
        }
    }
    out
}

/// A run of letters and digits, or of CJK chars.
struct Atom {
    start: usize,
    end: usize,
    cjk: bool,
}

/// Splits a word into parts at camelCase and letter/digit boundaries.
fn parts(chars: &[NChar]) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut start = 0;
    for i in 1..chars.len() {
        // Chars expanded from one original char (Æ → AE) stay together.
        if chars[i].from == chars[i - 1].from {
            continue;
        }
        let (a, b) = (chars[i - 1].class, chars[i].class);
        let digit_edge = (a == Class::Digit) != (b == Class::Digit);
        let lower_upper = a == Class::Lower && b == Class::Upper;
        // "XMLParser": break before the last capital of a capital run.
        let acronym_end = a == Class::Upper
            && b == Class::Upper
            && chars.get(i + 1).is_some_and(|n| n.class == Class::Lower);
        if digit_edge || lower_upper || acronym_end {
            out.push((start, i));
            start = i;
        }
    }
    out.push((start, chars.len()));
    out
}

fn lower(chars: &[NChar]) -> String {
    chars.iter().flat_map(|n| n.c.to_lowercase()).collect()
}

fn all(chars: &[NChar], class: Class) -> bool {
    chars.iter().all(|n| n.class == class)
}

fn is_letters(chars: &[NChar]) -> bool {
    chars
        .iter()
        .all(|n| matches!(n.class, Class::Upper | Class::Lower | Class::Letter))
}

/// Whether two words joined by `sep` are one token in practice.
fn joins(a: &[NChar], sep: char, b: &[NChar]) -> bool {
    let digits = |w: &[NChar]| all(w, Class::Digit) && w.len() <= 3;
    match sep {
        // 5.1, 7.1, 2.0; H.264.
        '.' => (digits(a) && digits(b)) || (is_letters(a) && a.len() <= 2 && digits(b)),
        // 24-96; WEB-DL, Blu-ray, DTS-HD; AC-3.
        '-' => {
            (digits(a) && digits(b))
                || (is_letters(a) && a.len() <= 4 && (is_letters(b) || digits(b)) && b.len() <= 4)
        }
        _ => false,
    }
}

/// Appends the tokens of `text` to `out`, numbering positions from
/// `position`, and returns the next free position. Stops after `max`
/// tokens in total in `out`.
pub fn tokenize_into(text: &str, position: usize, out: &mut Vec<Tok>, max: usize) -> usize {
    let chars = normalize(text);
    let mut atoms = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let class = chars[i].class;
        if matches!(class, Class::Sep | Class::Plus) {
            i += 1;
            continue;
        }
        let cjk = class == Class::Cjk;
        let start = i;
        while i < chars.len()
            && (chars[i].class == Class::Cjk) == cjk
            && !matches!(chars[i].class, Class::Sep | Class::Plus)
        {
            i += 1;
        }
        atoms.push(Atom { start, end: i, cjk });
    }

    let mut pos = position;
    let emit = |out: &mut Vec<Tok>, text: String, from: usize, to: usize, position: usize, kind| {
        if out.len() < max && !text.is_empty() && text.len() <= MAX_TOKEN_BYTES {
            out.push(Tok {
                text,
                from,
                to,
                position,
                kind,
            });
        }
    };
    for (k, atom) in atoms.iter().enumerate() {
        if out.len() >= max {
            break;
        }
        let word = &chars[atom.start..atom.end];
        let (from, to) = (word[0].from, word[word.len() - 1].to);
        if atom.cjk {
            if word.len() == 1 {
                emit(out, lower(word), from, to, pos, TokKind::Part);
                pos += 1;
            } else {
                for (j, pair) in word.windows(2).enumerate() {
                    emit(
                        out,
                        lower(pair),
                        pair[0].from,
                        pair[1].to,
                        pos + j,
                        TokKind::Part,
                    );
                }
                pos += word.len() - 1;
            }
            continue;
        }
        let first = pos;
        let split = if word.len() > MAX_SPLIT_CHARS {
            vec![(0, word.len())]
        } else {
            parts(word)
        };
        if split.len() > 1 {
            emit(out, lower(word), from, to, first, TokKind::Whole);
        }
        for (s, e) in &split {
            let p = &word[*s..*e];
            emit(
                out,
                lower(p),
                p[0].from,
                p[p.len() - 1].to,
                pos,
                TokKind::Part,
            );
            pos += 1;
        }
        // A trailing plus: DD+, C++.
        let pluses = chars[atom.end..]
            .iter()
            .take_while(|n| n.class == Class::Plus)
            .count();
        if (1..=2).contains(&pluses) {
            let end = chars[atom.end + pluses - 1].to;
            emit(
                out,
                lower(word) + &"+".repeat(pluses),
                from,
                end,
                first,
                TokKind::Compound,
            );
        }
        // Joined to the next word by a single '.' or '-'.
        if let Some(next) = atoms.get(k + 1) {
            if !next.cjk && next.start == atom.end + 1 {
                let sep = chars[atom.end].c;
                let other = &chars[next.start..next.end];
                if joins(word, sep, other) {
                    let text = lower(word) + &lower(other);
                    emit(
                        out,
                        text,
                        from,
                        other[other.len() - 1].to,
                        first,
                        TokKind::Compound,
                    );
                }
            }
        }
    }
    pos
}

/// The tokens of `text`, positions from 0.
pub fn tokenize(text: &str) -> Vec<Tok> {
    let mut out = Vec::new();
    tokenize_into(text, 0, &mut out, usize::MAX);
    out
}

/// The same chain as a Tantivy tokenizer, registered for the text fields.
#[derive(Debug, Clone, Default)]
pub struct NameTokenizer {
    buf: Vec<tantivy::tokenizer::Token>,
}

pub const NAME_TOKENIZER: &str = "lodestar_name";

/// Tokens per value, so a hostile 64 KiB name can't explode the index.
pub const MAX_NAME_TOKENS: usize = 512;

#[derive(Debug)]
pub struct NameTokenStream<'a> {
    tokens: &'a [tantivy::tokenizer::Token],
    index: Option<usize>,
}

impl tantivy::tokenizer::Tokenizer for NameTokenizer {
    type TokenStream<'a> = NameTokenStream<'a>;

    fn token_stream<'a>(&'a mut self, text: &'a str) -> NameTokenStream<'a> {
        self.buf.clear();
        let mut toks = Vec::new();
        tokenize_into(text, 0, &mut toks, MAX_NAME_TOKENS);
        self.buf.extend(toks.into_iter().map(to_tantivy));
        NameTokenStream {
            tokens: &self.buf,
            index: None,
        }
    }
}

impl tantivy::tokenizer::TokenStream for NameTokenStream<'_> {
    fn advance(&mut self) -> bool {
        let next = self.index.map_or(0, |i| i + 1);
        self.index = Some(next);
        next < self.tokens.len()
    }

    fn token(&self) -> &tantivy::tokenizer::Token {
        &self.tokens[self.index.expect("advance first")]
    }

    fn token_mut(&mut self) -> &mut tantivy::tokenizer::Token {
        unreachable!("token filters aren't used with this tokenizer")
    }
}

pub(crate) fn to_tantivy(t: Tok) -> tantivy::tokenizer::Token {
    tantivy::tokenizer::Token {
        offset_from: t.from,
        offset_to: t.to,
        position: t.position,
        text: t.text,
        position_length: 1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(s: &str) -> Vec<String> {
        tokenize(s).into_iter().map(|t| t.text).collect()
    }

    fn at(s: &str) -> Vec<(String, usize)> {
        tokenize(s)
            .into_iter()
            .map(|t| (t.text, t.position))
            .collect()
    }

    #[test]
    fn release_names() {
        let t = texts("Show.Name.S01E02.1080p.WEB-DL.DD+5.1.x265-GROUP");
        for want in [
            "show", "name", "s01e02", "s", "01", "e", "02", "1080p", "1080", "p", "web", "dl",
            "webdl", "dd", "dd+", "5", "1", "51", "x265", "x", "265", "group",
        ] {
            assert!(t.contains(&want.to_string()), "{want} missing from {t:?}");
        }
        // "265-GROUP" isn't a compound (GROUP is too long).
        assert!(!t.contains(&"265group".to_string()));
    }

    #[test]
    fn compounds_share_the_first_part_position() {
        assert_eq!(
            at("S01E02 x265"),
            vec![
                ("s01e02".into(), 0),
                ("s".into(), 0),
                ("01".into(), 1),
                ("e".into(), 2),
                ("02".into(), 3),
                ("x265".into(), 4),
                ("x".into(), 4),
                ("265".into(), 5),
            ]
        );
        assert_eq!(
            at("WEB-DL H.264"),
            vec![
                ("web".into(), 0),
                ("webdl".into(), 0),
                ("dl".into(), 1),
                ("h".into(), 2),
                ("h264".into(), 2),
                ("264".into(), 3),
            ]
        );
    }

    #[test]
    fn camel_case_and_acronyms() {
        assert_eq!(texts("ShowName"), ["showname", "show", "name"]);
        assert_eq!(texts("XMLParser"), ["xmlparser", "xml", "parser"]);
        assert_eq!(texts("iPhone"), ["iphone", "i", "phone"]);
        assert_eq!(
            texts("C++ 24-96 2019-2020"),
            ["c", "c++", "24", "2496", "96", "2019", "2020"]
        );
    }

    #[test]
    fn folding() {
        assert_eq!(texts("Amélie"), ["amelie"]);
        assert_eq!(texts("Straße"), ["strasse"]);
        assert_eq!(texts("Ærø Łódź"), ["aero", "lodz"]);
        assert_eq!(texts("１０８０ｐ"), ["1080p", "1080", "p"]);
        assert_eq!(texts("ΣΟΦΙΑ"), ["σοφια"]);
    }

    #[test]
    fn cjk_bigrams() {
        assert_eq!(at("千と千尋の神隠し"), {
            let s: Vec<char> = "千と千尋の神隠し".chars().collect();
            s.windows(2)
                .enumerate()
                .map(|(i, w)| (w.iter().collect::<String>(), i))
                .collect::<Vec<_>>()
        });
        assert_eq!(texts("第01集"), ["第", "01", "集"]);
        assert_eq!(texts("한국어 Drama"), ["한국", "국어", "drama"]);
    }

    #[test]
    fn hashes_stay_whole_and_limits_hold() {
        let hex = "a8f3c9e1b2d4f6a8c0e2b4d6f8a0c2e4b6d8f0a2";
        assert_eq!(texts(hex), [hex]);
        assert!(texts(&"x".repeat(100)).is_empty(), "over the length cap");
        let mut out = Vec::new();
        tokenize_into(&"a ".repeat(1000), 0, &mut out, 10);
        assert_eq!(out.len(), 10);
    }

    #[test]
    fn offsets_point_into_the_original() {
        let s = "Amélie.2001.１０８０ｐ";
        for t in tokenize(s) {
            assert!(
                s.is_char_boundary(t.from) && s.is_char_boundary(t.to),
                "{t:?}"
            );
        }
        let toks = tokenize(s);
        assert_eq!(&s[toks[0].from..toks[0].to], "Amélie");
    }
}
