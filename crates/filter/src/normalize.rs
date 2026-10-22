//! Normalization for matching (§20.4): text in, hashed tokens out.
//!
//! - Compatibility decomposition, diacritics removed, lowercase
//!   (full-width `ｐ` → `p`, `é` → `e`).
//! - Look-alike letters mapped to Latin (Cyrillic `р` → `p`, Greek `ο` →
//!   `o`): the common confusables, not the whole UTS #39 table.
//! - Zero-width and other invisible format characters removed, so they
//!   can't split a word.
//! - Words split at letter/digit boundaries (`16yo` → `16 yo`), with the
//!   whole word kept at the first part's position.
//! - Letter-for-number swaps undone as an alternative form (`h3ll0` also
//!   reads as `hello`; `@` and `$` count as `a` and `s` inside words).
//! - Spelled-out words joined (`h.e.l.l.o` also reads as `hello`).
//!
//! Alternatives share their word's position, so windows (`within 8
//! tokens`) measure words, however they're written. Input is untrusted:
//! the token count and word length are capped.

use unicode_normalization::UnicodeNormalization;
use unicode_normalization::char::is_combining_mark;

use crate::hash::term_hash;

/// Tokens kept per text (a name or one file name).
pub const MAX_TOKENS: usize = 128;
/// Words longer than this (in chars) are kept whole, not split or
/// re-read: they're hashes or junk.
const MAX_WORD_CHARS: usize = 32;
/// Single letters in a row that read as a spelled-out word.
const MIN_SPELLED: usize = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Token {
    /// Word position in its text.
    pub pos: u32,
    pub hash: u64,
    /// The value of an all-digit token of at most three digits.
    pub num: Option<u16>,
}

impl Token {
    fn new(pos: u32, text: &str) -> Token {
        let num = if text.len() <= 3 && text.bytes().all(|b| b.is_ascii_digit()) {
            text.parse().ok()
        } else {
            None
        };
        Token {
            pos,
            hash: term_hash(text),
            num,
        }
    }
}

/// A look-alike letter's Latin form.
fn confusable(c: char) -> char {
    match c {
        // Cyrillic.
        'а' => 'a',
        'в' => 'b',
        'е' | 'ё' => 'e',
        'к' => 'k',
        'м' => 'm',
        'н' => 'h',
        'о' => 'o',
        'р' => 'p',
        'с' => 'c',
        'т' => 't',
        'у' => 'y',
        'х' => 'x',
        'і' => 'i',
        'ј' => 'j',
        'ѕ' => 's',
        'ԁ' => 'd',
        'ԛ' => 'q',
        'ԝ' => 'w',
        // Greek.
        'α' => 'a',
        'β' => 'b',
        'ε' => 'e',
        'η' => 'n',
        'ι' => 'i',
        'κ' => 'k',
        'ν' => 'v',
        'ο' => 'o',
        'ρ' => 'p',
        'τ' => 't',
        'υ' => 'u',
        'χ' => 'x',
        // Latin look-alikes.
        'ı' => 'i',
        'ł' => 'l',
        'ø' => 'o',
        'đ' => 'd',
        'ħ' => 'h',
        'ŀ' => 'l',
        _ => c,
    }
}

/// Invisible characters that would otherwise split or disguise a word.
fn invisible(c: char) -> bool {
    matches!(
        c,
        '\u{00AD}'
            | '\u{034F}'
            | '\u{061C}'
            | '\u{115F}'
            | '\u{1160}'
            | '\u{17B4}'
            | '\u{17B5}'
            | '\u{180E}'
            | '\u{200B}'..='\u{200F}'
            | '\u{202A}'..='\u{202E}'
            | '\u{2060}'..='\u{2064}'
            | '\u{2066}'..='\u{206F}'
            | '\u{3164}'
            | '\u{FE00}'..='\u{FE0F}'
            | '\u{FEFF}'
            | '\u{FFA0}'
    )
}

fn leet(c: char) -> Option<char> {
    Some(match c {
        '0' => 'o',
        '1' => 'i',
        '3' => 'e',
        '4' => 'a',
        '5' => 's',
        '7' => 't',
        '@' => 'a',
        '$' => 's',
        _ => return None,
    })
}

/// Turns text into tokens, reusing its buffers across calls.
#[derive(Debug, Default)]
pub struct Tokenizer {
    word: String,
    plain: String,
    swapped: String,
    spelled: String,
    spelled_at: u32,
    pos: u32,
    start: usize,
}

impl Tokenizer {
    /// Appends the tokens of `text` to `out`, positions starting at 0.
    pub fn text(&mut self, text: &str, out: &mut Vec<Token>) {
        self.begin(out.len(), 0);
        self.segment(text, out);
    }

    /// Appends the tokens of a file path: one text, segments in a row.
    pub fn path<S: AsRef<str>>(&mut self, segments: &[S], out: &mut Vec<Token>) {
        self.continue_path(segments, out, out.len(), 0);
    }

    /// Continues a path whose first tokens are already in `out` from
    /// index `start`, the next word at position `pos` (see
    /// [`Tokenizer::pos`]).
    pub fn continue_path<S: AsRef<str>>(
        &mut self,
        segments: &[S],
        out: &mut Vec<Token>,
        start: usize,
        pos: u32,
    ) {
        self.begin(start, pos);
        for s in segments {
            self.segment(s.as_ref(), out);
        }
    }

    /// The position the next word would take.
    pub fn pos(&self) -> u32 {
        self.pos
    }

    fn begin(&mut self, start: usize, pos: u32) {
        self.pos = pos;
        self.start = start;
        self.word.clear();
        self.spelled.clear();
    }

    /// A segment ends its last word and any spelled-out run.
    fn segment(&mut self, text: &str, out: &mut Vec<Token>) {
        self.feed(text, out);
        self.flush_spelled(out);
    }

    fn room(&self, out: &[Token]) -> bool {
        out.len() - self.start < MAX_TOKENS
    }

    fn feed(&mut self, text: &str, out: &mut Vec<Token>) {
        if text.is_ascii() {
            for b in text.bytes() {
                self.push_char(char::from(b.to_ascii_lowercase()), out);
            }
        } else {
            for c in text.nfkd() {
                if invisible(c) || is_combining_mark(c) {
                    continue;
                }
                let c = match confusable(c) {
                    'ß' => 's',
                    c => c,
                };
                if c.is_alphanumeric() {
                    for l in c.to_lowercase() {
                        self.push_char(confusable(l), out);
                    }
                } else {
                    self.push_char(c, out);
                }
            }
        }
        self.end_word(out);
    }

    fn push_char(&mut self, c: char, out: &mut Vec<Token>) {
        if c.is_alphanumeric() || (matches!(c, '@' | '$') && !self.word.is_empty()) {
            self.word.push(c);
        } else {
            self.end_word(out);
        }
    }

    /// Emits the word's tokens: its parts, its whole form if it split,
    /// its letter-for-number reading, and joins spelled-out letters.
    fn end_word(&mut self, out: &mut Vec<Token>) {
        if self.word.is_empty() || !self.room(out) {
            self.word.clear();
            return;
        }
        let raw = std::mem::take(&mut self.word);
        self.plain.clear();
        self.plain
            .extend(raw.chars().filter(|c| !matches!(c, '@' | '$')));
        if self.plain.is_empty() {
            self.word = raw;
            self.word.clear();
            return;
        }
        let first = self.pos;
        let long = self.plain.chars().count() > MAX_WORD_CHARS;
        let mut split = 1;
        if long {
            out.push(Token::new(self.pos, &self.plain));
            self.pos += 1;
        } else {
            // Parts at letter/digit boundaries.
            let mut from = 0;
            let mut prev: Option<bool> = None;
            for (i, c) in self.plain.char_indices() {
                let digit = c.is_ascii_digit();
                if prev.is_some_and(|p| p != digit) {
                    if self.room(out) {
                        out.push(Token::new(self.pos, &self.plain[from..i]));
                    }
                    self.pos += 1;
                    split += 1;
                    from = i;
                }
                prev = Some(digit);
            }
            if self.room(out) {
                out.push(Token::new(self.pos, &self.plain[from..]));
            }
            self.pos += 1;
            if split > 1 && self.room(out) {
                out.push(Token::new(first, &self.plain));
            }
            let has_letter = raw.chars().any(char::is_alphabetic);
            let swappable = raw.chars().any(|c| leet(c).is_some());
            if has_letter && swappable {
                self.swapped.clear();
                self.swapped
                    .extend(raw.chars().map(|c| leet(c).unwrap_or(c)));
                if self.swapped != self.plain
                    && self.swapped.chars().all(char::is_alphabetic)
                    && self.room(out)
                {
                    out.push(Token::new(first, &self.swapped));
                }
            }
        }
        let mut letters = self.plain.chars();
        match (letters.next(), letters.next()) {
            (Some(c), None) if c.is_alphabetic() => {
                if self.spelled.is_empty() {
                    self.spelled_at = first;
                }
                self.spelled.push(c);
            }
            _ => self.flush_spelled(out),
        }
        self.word = raw;
        self.word.clear();
    }

    fn flush_spelled(&mut self, out: &mut Vec<Token>) {
        if self.spelled.chars().count() >= MIN_SPELLED && self.room(out) {
            out.push(Token::new(self.spelled_at, &self.spelled));
        }
        self.spelled.clear();
    }
}

/// Appends the tokens of `text` to `out`, positions starting at 0.
pub fn tokens(text: &str, out: &mut Vec<Token>) {
    Tokenizer::default().text(text, out);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn toks(text: &str) -> Vec<Token> {
        let mut out = Vec::new();
        tokens(text, &mut out);
        out
    }

    fn has(text: &str, word: &str, pos: u32) -> bool {
        let h = term_hash(word);
        toks(text).iter().any(|t| t.hash == h && t.pos == pos)
    }

    #[test]
    fn words_and_positions() {
        assert!(has("Hello.World", "hello", 0));
        assert!(has("Hello.World", "world", 1));
        assert!(has("Héllo_WORLD", "hello", 0));
        assert!(has("ｈｅｌｌｏ", "hello", 0));
    }

    #[test]
    fn letter_digit_split() {
        let t = toks("12ab cd");
        assert_eq!(t[0].num, Some(12));
        assert!(has("12ab cd", "ab", 1));
        assert!(has("12ab cd", "12ab", 0));
        assert!(has("12ab cd", "cd", 2));
        assert_eq!(toks("1080p")[0].num, None);
    }

    #[test]
    fn evasions() {
        // Look-alikes: Cyrillic е and о.
        assert!(has("h\u{0435}ll\u{043E}", "hello", 0));
        // Invisible characters don't split.
        assert!(has("hel\u{200B}lo", "hello", 0));
        assert!(has("hel\u{00AD}lo", "hello", 0));
        // Letter-for-number swaps.
        assert!(has("h3ll0 world", "hello", 0));
        assert!(has("he$$@", "hessa", 0));
        assert!(has("he$$@", "he", 0));
        // Spelled out.
        assert!(has("h.e.l.l.o world", "hello", 0));
        assert!(!has("a b", "ab", 0));
    }

    #[test]
    fn numbers_stay_numbers() {
        // An all-digit word isn't re-read as letters.
        let t = toks("15 1080");
        assert_eq!(t.len(), 2);
        assert_eq!(t[0].num, Some(15));
    }

    #[test]
    fn paths_continue_positions() {
        let mut out = Vec::new();
        Tokenizer::default().path(&["one two", "three.mkv"], &mut out);
        assert!(
            out.iter()
                .any(|t| t.hash == term_hash("three") && t.pos == 2)
        );
        // A segment boundary ends a word.
        Tokenizer::default().path(&["ab", "cd"], &mut out);
        assert!(!out.iter().any(|t| t.hash == term_hash("abcd")));
    }

    #[test]
    fn bounded() {
        let long = "word ".repeat(10_000);
        assert!(toks(&long).len() <= MAX_TOKENS);
        let huge = "x".repeat(100_000);
        assert_eq!(toks(&huge).len(), 1);
    }
}
