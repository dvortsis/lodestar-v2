//! The tokenizer as it was before it planned words ahead (§20.4), kept
//! verbatim as the reference: stored fixtures and shipped lists are
//! hashes of its tokens, so the faster one must produce exactly the same
//! tokens for any input.

use lodestar_filter::normalize::{Token, Tokenizer};
use proptest::prelude::*;

#[allow(clippy::all, dead_code)]
mod reference {
    use unicode_normalization::UnicodeNormalization;
    use unicode_normalization::char::is_combining_mark;

    use lodestar_filter::normalize::Token;
    use lodestar_filter::term_hash;

    /// Tokens kept per text (a name or one file name).
    pub const MAX_TOKENS: usize = 128;
    /// Words longer than this (in chars) are kept whole, not split or
    /// re-read: they're hashes or junk.
    const MAX_WORD_CHARS: usize = 32;
    /// Single letters in a row that read as a spelled-out word.
    const MIN_SPELLED: usize = 3;

    fn token(pos: u32, text: &str) -> Token {
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
                out.push(token(self.pos, &self.plain));
                self.pos += 1;
            } else {
                // Parts at letter/digit boundaries.
                let mut from = 0;
                let mut prev: Option<bool> = None;
                for (i, c) in self.plain.char_indices() {
                    let digit = c.is_ascii_digit();
                    if prev.is_some_and(|p| p != digit) {
                        if self.room(out) {
                            out.push(token(self.pos, &self.plain[from..i]));
                        }
                        self.pos += 1;
                        split += 1;
                        from = i;
                    }
                    prev = Some(digit);
                }
                if self.room(out) {
                    out.push(token(self.pos, &self.plain[from..]));
                }
                self.pos += 1;
                if split > 1 && self.room(out) {
                    out.push(token(first, &self.plain));
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
                        out.push(token(first, &self.swapped));
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
                out.push(token(self.spelled_at, &self.spelled));
            }
            self.spelled.clear();
        }
    }

    /// Appends the tokens of `text` to `out`, positions starting at 0.
    pub fn tokens(text: &str, out: &mut Vec<Token>) {
        Tokenizer::default().text(text, out);
    }
}

fn ours(text: &str) -> Vec<Token> {
    let mut out = Vec::new();
    Tokenizer::default().text(text, &mut out);
    out
}

fn theirs(text: &str) -> Vec<Token> {
    let mut out = Vec::new();
    reference::Tokenizer::default().text(text, &mut out);
    out
}

/// Text built to hit every path: leet digits, `@` and `$`, spelled-out
/// letters, letter/digit splits, look-alikes, invisible characters, long
/// words and the token cap.
fn texty() -> impl Strategy<Value = String> {
    let piece = prop_oneof![
        "[a-zA-Z]{1,8}",
        "[0-9]{1,4}",
        "[a-z0-9@$]{1,10}",
        "[A-Za-z]",
        "h3ll0|he\\$\\$@|16yo|S01E02|x264|1080p|@@|\\$",
        "[a-z]{30,40}",
        "\\PC{1,4}",
        "\u{0435}|\u{043E}|\u{200B}|\u{00AD}|ｈｅｌｌｏ|é|ß",
    ];
    let sep = prop::sample::select(vec![" ", ".", "-", "_", "", "/", "+", "  "]);
    prop::collection::vec((piece, sep), 0..80).prop_map(|v| {
        v.into_iter()
            .map(|(p, s)| format!("{p}{s}"))
            .collect::<String>()
    })
}

/// A path segment: [`texty`], single letters that a spelled-out run
/// carries over a `/`, a segment long enough to reach the token cap, or
/// none.
fn segment() -> impl Strategy<Value = String> {
    prop_oneof![
        4 => texty(),
        2 => "([a-zA-Z]\\.){0,3}[a-zA-Z]",
        1 => "[a-z]{1,3}( [a-z]{1,3}){60,70}",
        1 => Just(String::new()),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(2000))]

    #[test]
    fn same_tokens_as_the_reference(text in texty()) {
        prop_assert_eq!(ours(&text), theirs(&text));
    }

    #[test]
    fn same_tokens_for_any_text(text in "\\PC{0,200}") {
        prop_assert_eq!(ours(&text), theirs(&text));
    }

    /// One tokenizer reused across texts, as the core keeps one per
    /// thread, and paths continued after a folder.
    #[test]
    fn same_tokens_when_reused(texts in prop::collection::vec(texty(), 1..6)) {
        let mut tok = Tokenizer::default();
        let mut reference = reference::Tokenizer::default();
        for t in &texts {
            let (mut a, mut b) = (Vec::new(), Vec::new());
            tok.text(t, &mut a);
            reference.text(t, &mut b);
            prop_assert_eq!(&a, &b);
            let (mut a, mut b) = (Vec::new(), Vec::new());
            tok.path(&["folder one", t.as_str()], &mut a);
            reference.path(&["folder one", t.as_str()], &mut b);
            prop_assert_eq!(&a, &b);
            let end = tok.pos();
            prop_assert_eq!(end, reference.pos());
            tok.continue_path(&[t.as_str()], &mut a, 0, end);
            reference.continue_path(&[t.as_str()], &mut b, 0, end);
            prop_assert_eq!(&a, &b);
        }
    }

    /// Paths read in one pass, each folder once, give every path the
    /// tokens of its joined text: single letters and look-alikes on
    /// either side of a `/`, more folders than are remembered, paths of
    /// one segment and of none, and texts past the token cap.
    #[test]
    fn paths_read_as_their_joined_text(
        folders in prop::collection::vec(prop::collection::vec(segment(), 0..4), 1..8),
        files in prop::collection::vec((any::<prop::sample::Index>(), segment()), 0..24),
    ) {
        let paths: Vec<Vec<String>> = files
            .into_iter()
            .map(|(at, own)| {
                let mut p = folders[at.index(folders.len())].clone();
                if !own.is_empty() || p.is_empty() {
                    p.push(own);
                }
                p
            })
            .collect();
        let (mut out, mut ends) = (Vec::new(), Vec::new());
        lodestar_filter::normalize::paths(paths.iter().map(|p| &p[..]), &mut out, &mut ends);
        prop_assert_eq!(ends.len(), paths.len());
        let mut from = 0;
        for (p, &to) in paths.iter().zip(&ends) {
            prop_assert_eq!(&out[from..to], &theirs(&p.join("/"))[..], "{:?}", p);
            from = to;
        }
    }
}
