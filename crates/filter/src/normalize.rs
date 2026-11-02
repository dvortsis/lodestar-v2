//! Normalization for matching (§20.4): text in, hashed tokens out.
//!
//! - Compatibility decomposition, diacritics removed, lowercase
//!   (full-width `ｐ` → `p`, `é` → `e`).
//! - Look-alike letters mapped to Latin (Cyrillic `р` → `p`, Greek `ο` →
//!   `o`): the common confusables, not the whole UTS #39 table.
//! - Characters that render as nothing (zero-width and other invisible
//!   format characters, tag characters, blank fillers) removed, so they
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

/// Invisible characters that would otherwise split or disguise a word:
/// those that render as nothing (Unicode's Default_Ignorable_Code_Point,
/// which browsers and HarfBuzz hide). That is every format character
/// (category Cf, [`lodestar_metainfo::format_char`]) except the
/// [`visible_format`] ones; U+2065, U+FFF0–FFF8 and U+E0000–E0FFF (the
/// tags and plane 14's unassigned code points); and fillers that show as
/// nothing (Hangul fillers, Khmer inherent vowels, variation selectors,
/// the combining grapheme joiner).
#[inline]
fn invisible(c: char) -> bool {
    // Nothing below the soft hyphen is invisible.
    if c < '\u{00AD}' {
        return false;
    }
    matches!(
        c,
        '\u{034F}'
            | '\u{115F}'
            | '\u{1160}'
            | '\u{17B4}'
            | '\u{17B5}'
            | '\u{3164}'
            | '\u{FE00}'..='\u{FE0F}'
            | '\u{2065}'
            | '\u{FFA0}'
            | '\u{FFF0}'..='\u{FFF8}'
            | '\u{E0000}'..='\u{E0FFF}'
    ) || (lodestar_metainfo::format_char(c) && !visible_format(c))
}

/// The characters [`invisible`] ignores that the matching before
/// F-normalize-invisible read as word breaks. A text holding one is read
/// both ways (`docs/decisions/2026-11-02-F-normalize-invisible.md`): a
/// term hits if it hits in either reading, and an entry that releases
/// applies only if it matches in both, so no text matches less than it
/// did. Variation selectors (U+E0100–E01EF) are combining marks, dropped
/// either way.
fn newly_ignored(c: char) -> bool {
    matches!(
        c,
        '\u{2065}'
            | '\u{FFF0}'..='\u{FFF8}'
            | '\u{1BCA0}'..='\u{1BCA3}'
            | '\u{1D173}'..='\u{1D17A}'
            | '\u{E0000}'..='\u{E0FFF}'
    )
}

/// Whether some text reads differently with [`newly_ignored`] characters
/// as word breaks: only non-ASCII text that holds one.
pub(crate) fn splits<'t>(texts: impl IntoIterator<Item = &'t str>) -> bool {
    texts
        .into_iter()
        .any(|t| !t.is_ascii() && t.chars().any(newly_ignored))
}

/// Format characters that show: the prepended concatenation marks (Arabic
/// number signs, Syriac abbreviation, Kaithi number signs), the
/// interlinear annotation marks and the Egyptian hieroglyph controls.
/// None is default-ignorable, so they render as a glyph or a box and
/// split a word visibly, like punctuation: they stay word breaks. Ignoring
/// one would join two words a reader sees apart, so a rule for the second
/// would miss.
fn visible_format(c: char) -> bool {
    matches!(
        c,
        '\u{0600}'..='\u{0605}'
            | '\u{06DD}'
            | '\u{070F}'
            | '\u{0890}'..='\u{0891}'
            | '\u{08E2}'
            | '\u{110BD}'
            | '\u{110CD}'
            | '\u{FFF9}'..='\u{FFFB}'
            | '\u{13430}'..='\u{1343F}'
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

/// ASCII bytes that start a word ([`START`]: letters and digits) or only
/// continue one (`@` and `$`).
const WORD: [u8; 256] = {
    let mut t = [0u8; 256];
    let mut c = 0;
    while c < 256 {
        let b = c as u8;
        t[c] = if b.is_ascii_alphanumeric() {
            START
        } else if b == b'@' || b == b'$' {
            1
        } else {
            0
        };
        c += 1;
    }
    t
};
const START: u8 = 2;

/// Plans cached per tokenizer.
const PLANS: usize = 512;
/// Words longer than this aren't planned ahead.
const MAX_PLANNED_BYTES: usize = 64;

/// One word's tokens, positions relative to its first, and whether each
/// waits for room (the cap) when it's emitted.
#[derive(Debug, Default)]
struct Plan {
    /// The word as it was written: the cache key.
    key: String,
    /// The word in lowercase.
    word: String,
    /// Nothing but `@` and `$`: no tokens, and spelled-out runs go on.
    empty: bool,
    tokens: Vec<(Token, bool)>,
    advance: u32,
    /// A single letter, which may be part of a spelled-out word.
    letter: Option<char>,
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
    plans: Vec<Plan>,
    /// The plan of a word too long to keep.
    long: Plan,
    /// [`paths_apart`]'s folders, kept so their buffers carry over
    /// between torrents.
    folders: [Paused; FOLDERS],
    /// Reads [`newly_ignored`] characters as word breaks, as the matching
    /// before them did ([`Tokenizer::splitting`]).
    split: bool,
}

impl Tokenizer {
    /// A tokenizer that reads [`newly_ignored`] characters as word breaks:
    /// the earlier reading, kept for texts that hold one ([`splits`]).
    pub(crate) fn splitting() -> Tokenizer {
        Tokenizer {
            split: true,
            ..Tokenizer::default()
        }
    }

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
            // A word starts at a letter or digit (the word is empty there,
            // so `@` and `$` can't start one) and takes `@` and `$` after.
            // Anything else ends it, as `push_char` does one char at a time.
            let bytes = text.as_bytes();
            let mut i = 0;
            while i < bytes.len() {
                if WORD[bytes[i] as usize] != START {
                    i += 1;
                    continue;
                }
                let mut j = i + 1;
                while j < bytes.len() && WORD[bytes[j] as usize] != 0 {
                    j += 1;
                }
                self.emit(&text[i..j], out);
                i = j;
            }
        } else {
            for c in text.nfkd() {
                if is_combining_mark(c) {
                    continue;
                }
                if invisible(c) {
                    if self.split && newly_ignored(c) {
                        self.end_word(out);
                    }
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
        let word = std::mem::take(&mut self.word);
        self.emit(&word, out);
        self.word = word;
        self.word.clear();
    }

    /// [`Tokenizer::end_word`] for `word`, lowercase or ASCII in any case.
    /// Words repeat across a torrent's file names, so each word's tokens
    /// are planned once and replayed.
    fn emit(&mut self, word: &str, out: &mut Vec<Token>) {
        if word.is_empty() || !self.room(out) {
            return;
        }
        if self.plans.len() < PLANS {
            self.plans.resize_with(PLANS, Plan::default);
        }
        // Long words (hashes, junk) aren't kept.
        let plan = if word.len() <= MAX_PLANNED_BYTES {
            let plan = &mut self.plans[slot(word)];
            if plan.key.as_bytes() != word.as_bytes() {
                // The key last: a plan is never found before it's made,
                // even if making it was cut short.
                plan.key.clear();
                plan.word.clear();
                plan.word.push_str(word);
                plan.word.make_ascii_lowercase();
                make_plan(plan, &mut self.plain, &mut self.swapped);
                plan.key.push_str(word);
            }
            plan
        } else {
            let plan = &mut self.long;
            plan.word.clear();
            plan.word.push_str(word);
            plan.word.make_ascii_lowercase();
            make_plan(plan, &mut self.plain, &mut self.swapped);
            plan
        };
        if plan.empty {
            return;
        }
        let first = self.pos;
        out.reserve(plan.tokens.len());
        for (t, guarded) in &plan.tokens {
            if !guarded || out.len() - self.start < MAX_TOKENS {
                out.push(Token {
                    pos: first + t.pos,
                    ..*t
                });
            }
        }
        self.pos += plan.advance;
        match plan.letter {
            Some(c) => {
                if self.spelled.is_empty() {
                    self.spelled_at = first;
                }
                self.spelled.push(c);
            }
            None => self.flush_spelled(out),
        }
    }

    #[inline]
    fn flush_spelled(&mut self, out: &mut Vec<Token>) {
        if !self.spelled.is_empty() {
            self.end_spelled(out);
        }
    }

    #[cold]
    fn end_spelled(&mut self, out: &mut Vec<Token>) {
        if self.spelled.chars().count() >= MIN_SPELLED && self.room(out) {
            out.push(Token::new(self.spelled_at, &self.spelled));
        }
        self.spelled.clear();
    }
}

/// Plans `plan.word`'s tokens, positions relative to its first. `plain`
/// and `swapped` are scratch space.
fn make_plan(plan: &mut Plan, plain: &mut String, swapped: &mut String) {
    plan.tokens.clear();
    plan.advance = 0;
    plan.letter = None;
    let raw = &plan.word;
    // `@`, `$` and the letter-for-number digits are ASCII, so bytes
    // find them in any text.
    let special = raw.bytes().any(|b| matches!(b, b'@' | b'$'));
    plain.clear();
    if special {
        plain.extend(raw.chars().filter(|c| !matches!(c, '@' | '$')));
    } else {
        plain.push_str(raw);
    }
    plan.empty = plain.is_empty();
    if plan.empty {
        return;
    }
    let long = plain.len() > MAX_WORD_CHARS
        && (plain.is_ascii() || plain.chars().count() > MAX_WORD_CHARS);
    let mut pos = 0;
    if long {
        plan.tokens.push((Token::new(pos, plain), false));
        pos += 1;
    } else {
        // Parts at letter/digit boundaries. A digit is one byte and no
        // other char's bytes look like one, so the boundaries fall on
        // the same byte offsets either way.
        let mut from = 0;
        let mut prev: Option<bool> = None;
        let mut split = 1;
        for (i, b) in plain.bytes().enumerate() {
            if !plain.is_char_boundary(i) {
                continue;
            }
            let digit = b.is_ascii_digit();
            if prev.is_some_and(|p| p != digit) {
                plan.tokens.push((Token::new(pos, &plain[from..i]), true));
                pos += 1;
                split += 1;
                from = i;
            }
            prev = Some(digit);
        }
        plan.tokens.push((Token::new(pos, &plain[from..]), true));
        pos += 1;
        if split > 1 {
            plan.tokens.push((Token::new(0, plain), true));
        }
        let swappable = raw
            .bytes()
            .any(|b| matches!(b, b'0' | b'1' | b'3' | b'4' | b'5' | b'7' | b'@' | b'$'));
        let has_letter = swappable
            && if raw.is_ascii() {
                raw.bytes().any(|b| b.is_ascii_alphabetic())
            } else {
                raw.chars().any(char::is_alphabetic)
            };
        if has_letter && swappable {
            swapped.clear();
            swapped.extend(raw.chars().map(|c| leet(c).unwrap_or(c)));
            if *swapped != *plain && swapped.chars().all(char::is_alphabetic) {
                plan.tokens.push((Token::new(0, swapped), true));
            }
        }
    }
    plan.advance = pos;
    let mut letters = plain.chars();
    if let (Some(c), None) = (letters.next(), letters.next())
        && c.is_alphabetic()
    {
        plan.letter = Some(c);
    }
}

/// A word's slot in the plan cache: cheap, since a hit compares the
/// whole word.
fn slot(word: &str) -> usize {
    let b = word.as_bytes();
    let n = b.len();
    let at = |i: usize| u64::from(b[i]);
    let mix = (n as u64)
        ^ (at(0) << 8)
        ^ (at(n / 2) << 16)
        ^ (at(n - 1) << 24)
        ^ (at(n.saturating_sub(2)) << 32);
    (crate::hash::fmix64(mix) as usize) % PLANS
}

/// Runs `f` with this thread's tokenizer, or a new one if it's in use.
///
/// One tokenizer is kept per thread, for the core and keyword entries
/// alike: a new one sets up its word plans on its first word and frees
/// them after, which cost more than the words did, and the two read the
/// same words (§20.8). A tokenizer's tokens never depend on what it read
/// before (`same_tokens_when_reused`).
pub(crate) fn with_tokenizer<R>(f: impl FnOnce(&mut Tokenizer) -> R) -> R {
    thread_local! {
        static TOKENIZER: std::cell::RefCell<Tokenizer> = Default::default();
    }
    let mut f = Some(f);
    let reused = TOKENIZER.try_with(|t| {
        t.try_borrow_mut()
            .ok()
            .map(|mut t| (f.take().expect("called once"))(&mut t))
    });
    match reused {
        Ok(Some(r)) => r,
        // A call inside `f` (the thread's tokenizer is borrowed) or during
        // thread teardown gets a new tokenizer, silently: the same tokens,
        // only without the cached plans, so a nested call is slow, never
        // wrong. Callers keep their calls apart (`Doc::normalized` reads the
        // name and then the paths, one call each).
        _ => (f.take().expect("not called"))(&mut Tokenizer::default()),
    }
}

/// Appends the tokens of `text` to `out`, positions starting at 0.
pub fn tokens(text: &str, out: &mut Vec<Token>) {
    with_tokenizer(|t| t.text(text, out));
}

/// A text read up to just after a `/`, so texts that start with it go
/// on from there ([`Tokenizer::resume`]).
#[derive(Debug, Default)]
struct Paused {
    tokens: Vec<Token>,
    pos: u32,
    spelled: String,
    spelled_at: u32,
    /// No spelled-out run was open at any of its `/`s, so a path in the
    /// folder gets the same tokens with its segments read apart
    /// ([`Tokenizer::path`]), as the core reads them.
    apart: bool,
}

impl Tokenizer {
    /// Reads `dirs` joined by `/` with a `/` after, the start of a text,
    /// into `into`. It ends with a `/`, where no word goes on, so the
    /// state between words is all there is to keep: the position and a
    /// spelled-out run, which goes on over a `/` in one text. A `/` only
    /// ends a word, as the end of a feed does, so each folder is fed on
    /// its own, and whether a run is open at each `/` noted.
    fn pause(&mut self, dirs: &[String], into: &mut Paused) {
        into.tokens.clear();
        self.begin(0, 0);
        into.apart = true;
        for d in dirs {
            self.feed(d, &mut into.tokens);
            into.apart &= self.spelled.is_empty();
        }
        into.pos = self.pos;
        into.spelled.clear();
        into.spelled.push_str(&self.spelled);
        into.spelled_at = self.spelled_at;
    }

    /// Appends the tokens of the text `paused`'s prefix and then `rest`,
    /// as [`Tokenizer::text`] gives them for the two joined. Compatibility
    /// decomposition reads the two alike: it reorders only runs of
    /// combining marks, and a `/` ends any run.
    fn resume(&mut self, paused: &Paused, rest: &str, out: &mut Vec<Token>) {
        let start = out.len();
        out.extend_from_slice(&paused.tokens);
        self.begin(start, paused.pos);
        self.spelled.push_str(&paused.spelled);
        self.spelled_at = paused.spelled_at;
        self.segment(rest, out);
    }
}

/// Folders [`paths`] remembers: a season's episodes and subtitles
/// alternate between two.
const FOLDERS: usize = 4;

/// Appends the tokens of each path, its segments joined by `/`, as
/// [`tokens`] gives them for that text, and the end of each in `out`
/// to `ends`. Paths in one folder read its part once.
pub fn paths<'p>(
    paths: impl IntoIterator<Item = &'p [String]>,
    out: &mut Vec<Token>,
    ends: &mut Vec<usize>,
) {
    paths_apart(paths, out, ends, &mut Vec::new());
}

/// A torrent's name and paths in tokens, read once for keyword entries
/// ([`paths_apart`]), for the core to reuse where it would read the same.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Read<'a> {
    /// The name's, then each path's, in one buffer.
    pub(crate) tokens: &'a [Token],
    /// Where the name and each path end.
    pub(crate) ends: &'a [usize],
    /// Whether each path reads the same with its segments apart.
    pub(crate) apart: &'a [bool],
}

impl<'a> Read<'a> {
    /// The name's tokens, as [`Tokenizer::text`] gives them.
    pub(crate) fn name(self) -> &'a [Token] {
        &self.tokens[..self.ends[0]]
    }

    /// Path `i`'s tokens, if they're what [`Tokenizer::path`] gives for
    /// its segments.
    pub(crate) fn path(self, i: usize) -> Option<&'a [Token]> {
        (self.apart.get(i) == Some(&true)).then(|| &self.tokens[self.ends[i]..self.ends[i + 1]])
    }
}

/// [`paths`], and for each path whether its segments read apart
/// ([`Tokenizer::path`]) give the same tokens: unless a spelled-out run
/// is open at one of its `/`s, which goes on over it in one text and
/// ends at it read apart.
pub(crate) fn paths_apart<'p>(
    paths: impl IntoIterator<Item = &'p [String]>,
    out: &mut Vec<Token>,
    ends: &mut Vec<usize>,
    apart: &mut Vec<bool>,
) {
    with_tokenizer(|tok| {
        let mut paused = std::mem::take(&mut tok.folders);
        let mut dirs_of: [&[String]; FOLDERS] = [&[]; FOLDERS];
        let (mut known, mut next) = (0, 0);
        for path in paths {
            match path.split_last() {
                Some((name, dirs)) if !dirs.is_empty() => {
                    let slot = match dirs_of[..known].iter().position(|d| *d == dirs) {
                        Some(slot) => slot,
                        None => {
                            let slot = if known < FOLDERS {
                                known += 1;
                                known - 1
                            } else {
                                let slot = next;
                                next = (next + 1) % FOLDERS;
                                slot
                            };
                            dirs_of[slot] = dirs;
                            tok.pause(dirs, &mut paused[slot]);
                            slot
                        }
                    };
                    tok.resume(&paused[slot], name, out);
                    apart.push(paused[slot].apart);
                }
                // One segment is its own text; none is an empty one.
                Some((name, _)) => {
                    tok.text(name, out);
                    apart.push(true);
                }
                None => apart.push(true),
            }
            ends.push(out.len());
        }
        tok.folders = paused;
    });
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

    /// Every character [`invisible`] ignores.
    fn all_invisible() -> Vec<char> {
        (0..=0x10FFFF)
            .filter_map(char::from_u32)
            .filter(|&c| invisible(c))
            .collect()
    }

    /// The format characters that show, which stay word breaks.
    const VISIBLE: [char; 8] = [
        '\u{0600}',
        '\u{0605}',
        '\u{06DD}',
        '\u{070F}',
        '\u{0890}',
        '\u{08E2}',
        '\u{110BD}',
        '\u{110CD}',
    ];

    /// `invisible` is exactly Unicode's default-ignorable characters,
    /// written out here on their own; every Cf character is either ignored
    /// or a known visible one; and [`newly_ignored`] is exactly what it
    /// ignores that the matching before didn't.
    #[test]
    fn invisible_is_exactly_the_default_ignorables() {
        let expected = |c: char| {
            matches!(
                c,
                '\u{00AD}'
                    | '\u{034F}'
                    | '\u{061C}'
                    | '\u{115F}'..='\u{1160}'
                    | '\u{17B4}'..='\u{17B5}'
                    | '\u{180E}'
                    | '\u{200B}'..='\u{200F}'
                    | '\u{202A}'..='\u{202E}'
                    | '\u{2060}'..='\u{206F}'
                    | '\u{3164}'
                    | '\u{FE00}'..='\u{FE0F}'
                    | '\u{FEFF}'
                    | '\u{FFA0}'
                    | '\u{FFF0}'..='\u{FFF8}'
                    | '\u{1BCA0}'..='\u{1BCA3}'
                    | '\u{1D173}'..='\u{1D17A}'
                    | '\u{E0000}'..='\u{E0FFF}'
            )
        };
        // What the matching before ignored, verbatim.
        let before = |c: char| {
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
        };
        for c in (0..=0x10FFFF).filter_map(char::from_u32) {
            let at = c as u32;
            assert_eq!(invisible(c), expected(c), "U+{at:04X}");
            // The splitting reader breaks words exactly where the
            // matching before did.
            if !is_combining_mark(c) {
                let newly = invisible(c) && !before(c);
                assert_eq!(newly_ignored(c), newly, "U+{at:04X}");
            }
            if lodestar_metainfo::format_char(c) {
                assert!(invisible(c) || visible_format(c), "U+{at:04X} is Cf");
            }
        }
        let hello = toks("hello");
        for c in all_invisible() {
            assert_eq!(toks(&format!("hel{c}lo")), hello, "U+{:04X}", c as u32);
        }
    }

    /// Golden cases: the ranges `invisible` didn't cover before join a
    /// word (docs/decisions/2026-11-02-F-normalize-invisible.md).
    #[test]
    fn newly_ignored_ranges_join_a_word() {
        let alpha_beta = crate::rules::keyword_tokens("alpha beta");
        for c in [
            '\u{2065}',
            '\u{FFF0}',
            '\u{FFF8}',
            '\u{1BCA0}',
            '\u{1BCA3}',
            '\u{1D173}',
            '\u{1D17A}',
            '\u{E0000}',
            '\u{E0001}',
            '\u{E0002}',
            '\u{E0020}',
            '\u{E007F}',
            '\u{E0080}',
            '\u{E00FF}',
            '\u{E01F0}',
            '\u{E0FFF}',
        ] {
            let at = c as u32;
            assert!(has(&format!("hel{c}lo"), "hello", 0), "U+{at:04X}");
            assert!(!has(&format!("hel{c}lo"), "hel", 0), "U+{at:04X}");
            let mut t = Vec::new();
            tokens(&format!("The.Al{c}pha.{c}Be{c}ta{c}.2019"), &mut t);
            assert!(crate::rules::in_a_row(&alpha_beta, &t), "U+{at:04X}");
            // Between two words, as between any two letters, it reads as
            // nothing: what shows is one word.
            assert!(has(&format!("alpha{c}beta"), "alphabeta", 0), "U+{at:04X}");
        }
    }

    /// Golden cases: the format characters that show, and the line and
    /// paragraph separators, still break words, as they did.
    #[test]
    fn visible_breaks_still_split() {
        for c in VISIBLE
            .into_iter()
            .chain(['\u{0891}', '\u{FFF9}', '\u{FFFB}', '\u{13430}', '\u{1343F}'])
            .chain(['\u{2028}', '\u{2029}'])
        {
            let at = c as u32;
            let text = format!("alpha{c}beta");
            assert!(has(&text, "alpha", 0), "U+{at:04X}");
            assert!(has(&text, "beta", 1), "U+{at:04X}");
            assert!(!has(&text, "alphabeta", 0), "U+{at:04X}");
            assert_eq!(toks(&text), toks("alpha beta"), "U+{at:04X}");
        }
    }

    fn read_apart(paths: &[Vec<String>]) -> (Vec<Token>, Vec<usize>, Vec<bool>) {
        let (mut out, mut ends, mut apart) = (Vec::new(), vec![0], Vec::new());
        paths_apart(
            paths.iter().map(|p| &p[..]),
            &mut out,
            &mut ends,
            &mut apart,
        );
        (out, ends, apart)
    }

    /// A spelled-out run open at a `/` reads differently whole, and is
    /// marked so; one that ends before it isn't.
    #[test]
    fn a_run_over_a_slash_is_marked() {
        let path = |p: &str| p.split('/').map(String::from).collect::<Vec<_>>();
        let (_, _, apart) = read_apart(&[path("l/e/x/a.mkv"), path("lexa/x.mkv"), path("a.b.c/d")]);
        assert_eq!(apart, [false, true, false]);
        let mut apart_read = Vec::new();
        Tokenizer::default().path(&path("l/e/x/a.mkv"), &mut apart_read);
        let (whole, ends, _) = read_apart(&[path("l/e/x/a.mkv")]);
        assert_ne!(&whole[ends[0]..ends[1]], &apart_read[..]);
    }

    proptest::proptest! {
        /// Inserting any character `invisible` ignores, anywhere in a
        /// text, never changes its tokens, so never whether a term rule
        /// that reads them matches (a core or pack term, a keyword, the
        /// core's windows).
        #[test]
        fn an_invisible_character_never_changes_a_match(
            text in "([a-e]|[a-e]{3,6}|[0-9]{1,3}|[@$]|é|ｂ|\u{0435}|e\u{301}|[ ./_-]){0,12}",
            rule in "[a-e]{1,6}( [a-e]{1,6}){0,2}",
            inserts in proptest::collection::vec((proptest::sample::select(all_invisible()), 0usize..64), 1..4),
        ) {
            let mut spliced: Vec<char> = text.chars().collect();
            for (c, at) in &inserts {
                spliced.insert(at % (spliced.len() + 1), *c);
            }
            let spliced: String = spliced.into_iter().collect();
            let (plain, hidden) = (toks(&text), toks(&spliced));
            proptest::prop_assert_eq!(&plain, &hidden);
            let words = crate::rules::keyword_tokens(&rule);
            proptest::prop_assert_eq!(
                crate::rules::in_a_row(&words, &plain),
                crate::rules::in_a_row(&words, &hidden)
            );
        }

        /// The splitting reader gives exactly the tokens of the text with
        /// each newly ignored character a space: the matching before.
        #[test]
        fn the_splitting_reader_reads_as_before(
            text in "([a-e]|[a-e]{3,6}|[0-9]{1,3}|[@$]|é|ｂ|e\u{301}|[ ./_-]|\u{200B}|\u{2065}|\u{FFF0}|\u{1BCA0}|\u{1D173}|\u{E0001}|\u{E0041}|\u{E0FFF}){0,16}",
        ) {
            let mut split = Vec::new();
            Tokenizer::splitting().text(&text, &mut split);
            let spaced: String = text
                .chars()
                .map(|c| if newly_ignored(c) { ' ' } else { c })
                .collect();
            proptest::prop_assert_eq!(split, toks(&spaced));
            proptest::prop_assert_eq!(splits([text.as_str()]), text != spaced);
        }

        /// Wherever a path is marked, its segments read apart give the
        /// tokens of its joined text: letters on both sides of a `/`,
        /// look-alikes, combining marks, folders shared and forgotten.
        #[test]
        fn marked_paths_read_the_same_apart(
            segs in proptest::collection::vec("([a-c]|[a-c]{4}|1|ⓐ|Ａ|e\u{301}|[@$]|x264)([ ./-]([a-c]|[a-c]{4}|1|ⓐ|Ａ|e\u{301}|[@$]|x264)){0,3}", 1..6),
            layout in proptest::collection::vec((0usize..6, 0usize..6, 0usize..4), 0..14),
        ) {
            let paths: Vec<Vec<String>> = layout
                .iter()
                .map(|&(a, b, depth)| {
                    let mut p: Vec<String> = (0..depth).map(|d| segs[(a + d) % segs.len()].clone()).collect();
                    p.push(segs[b % segs.len()].clone());
                    p
                })
                .collect();
            let (whole, ends, apart) = read_apart(&paths);
            proptest::prop_assert_eq!(apart.len(), paths.len());
            for (i, p) in paths.iter().enumerate() {
                let mut alone = Vec::new();
                Tokenizer::default().path(p, &mut alone);
                if apart[i] {
                    proptest::prop_assert_eq!(&whole[ends[i]..ends[i + 1]], &alone[..], "{:?}", p);
                }
            }
        }
    }
}
