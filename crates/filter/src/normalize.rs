//! Normalization for matching (§20.4): text in, hashed tokens out.
//!
//! - Compatibility decomposition, diacritics removed, lowercase
//!   (full-width `ｐ` → `p`, `é` → `e`).
//! - Look-alike letters mapped to Latin (Cyrillic `р` → `p`, Greek `ο` →
//!   `o`): the common confusables, not the whole UTS #39 table.
//! - Characters that render as nothing (zero-width and other invisible
//!   format characters, tag characters, blank fillers) removed, so they
//!   can't split a word. The format characters that show, and the line and
//!   paragraph separators, are removed too; the earlier reading
//!   ([`Tokenizer::splitting`]) keeps them as word breaks.
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

/// Tokens kept per text (a name or one file path), and per torrent when
/// the core reads one whole ([`read_whole`]): a bound on memory. A
/// metered read that reaches it is past the core's budget, so the core
/// holds the torrent (CS-0a).
pub const MAX_TOKENS: usize = 1 << 19;
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
/// [`visible_mark`] ones; U+2065, U+FFF0–FFF8 and U+E0000–E0FFF (the
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
    ) || (lodestar_metainfo::format_char(c) && !visible_mark(c))
}

/// What the joined reading drops: the [`invisible`] characters and the
/// [`visible_mark`]s.
#[inline]
fn ignored(c: char) -> bool {
    invisible(c) || visible_mark(c)
}

/// The characters the joined reading ignores that the matching before
/// read as word breaks: the [`newly_ignored`] ones and the
/// [`visible_mark`]s. A text holding one is read both ways
/// (`docs/decisions/2026-11-02-F-normalize-invisible.md`,
/// `docs/decisions/2026-11-02-F-visible-format-two-readings.md`), and the
/// stricter verdict stands, so no text matches less than it did.
#[inline]
fn mark(c: char) -> bool {
    newly_ignored(c) || visible_mark(c)
}

/// The [`invisible`] characters that the matching before
/// F-normalize-invisible read as word breaks. Variation selectors
/// (U+E0100–E01EF) are combining marks, dropped either way.
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

/// Whether some text may read differently with [`mark`]s as word breaks:
/// only non-ASCII text that holds one.
pub(crate) fn splits<'t>(texts: impl IntoIterator<Item = &'t str>) -> bool {
    texts
        .into_iter()
        .any(|t| !t.is_ascii() && t.chars().any(mark))
}

/// Format characters that show, and the line and paragraph separators:
/// the prepended concatenation marks (Arabic number signs, Syriac
/// abbreviation, Kaithi number signs), the interlinear annotation marks,
/// the Egyptian hieroglyph controls, U+2028 and U+2029. None is
/// default-ignorable: they render as a glyph, a box or a line break.
///
/// Read only as word breaks, one inside a word splits it past a term
/// (U+0605 and U+08E2 sit above the following letters, so the split hardly
/// shows); read only as nothing, one would join two words a reader sees
/// apart, so a rule for the second would miss. So the joined reading
/// ignores them, the earlier reading keeps them as breaks, and the
/// stricter verdict stands
/// (`docs/decisions/2026-11-02-F-visible-format-two-readings.md`).
fn visible_mark(c: char) -> bool {
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
            | '\u{2028}'
            | '\u{2029}'
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
    /// The metered read that last used it ([`Tokenizer::round`]).
    round: u32,
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
    /// Reads [`mark`]s as word breaks, as the matching before did
    /// ([`Tokenizer::splitting`]).
    split: bool,
    /// Each character's reading in non-ASCII text, cached
    /// ([`CharPlan`]); empty until the first such text.
    chars: Vec<CharPlan>,
    /// The work done since [`Tokenizer::meter`], in units of the core's
    /// budget ([`crate::core::BUDGET`]).
    work: u64,
    /// Where reading stops: past it the rest of a text is skipped. Zero
    /// for no limit.
    limit: u64,
    /// What the caches would have cost this read had every word and
    /// character missed them, since [`Tokenizer::meter`]: charged again
    /// for a text the core reads a second time, whose caches may be cold.
    cold: u64,
    /// The metered read under way ([`Tokenizer::meter`]). Each cached word
    /// and character notes the round that last used it, so the meter
    /// charges as new whatever this read hasn't used yet in that slot, as
    /// if the caches had started it empty: the same torrent costs the
    /// same whatever the thread read before, and never less than it does.
    round: u32,
    /// A [`mark`] was read since [`Tokenizer::meter`]: the text reads
    /// differently in the earlier reading ([`splits`]).
    saw_mark: bool,
}

/// Characters [`Tokenizer`] caches the reading of: a script's block fits
/// without two characters sharing a slot.
const CHARS: usize = 2048;

/// What one character of non-ASCII text does to the word being read, once
/// decomposed, mapped and lowercased ([`plan_char`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Op {
    /// A letter or digit: it goes on the word.
    Push(char),
    /// `@` or `$`: it goes on a word already started, or ends one.
    At(char),
    /// Ends the word.
    End,
    /// A [`mark`]: a word break in the earlier reading, nothing in the
    /// joined one.
    Mark,
}

impl Op {
    fn of(c: char) -> Op {
        if c.is_alphanumeric() {
            Op::Push(c)
        } else if matches!(c, '@' | '$') {
            Op::At(c)
        } else {
            Op::End
        }
    }
}

/// One character's [`Op`]s, cached by the character.
#[derive(Debug, Clone)]
struct CharPlan {
    /// The character, or `u32::MAX` for an empty slot.
    key: u32,
    ops: Vec<Op>,
    /// Characters in its decomposition.
    decomposed: u64,
    /// A [`mark`]: the earlier reading reads it as a word break.
    mark: bool,
    /// The metered read that last used it ([`Tokenizer::round`]).
    round: u32,
}

impl Default for CharPlan {
    fn default() -> CharPlan {
        CharPlan {
            key: u32::MAX,
            ops: Vec::new(),
            decomposed: 0,
            mark: false,
            round: 0,
        }
    }
}

/// `c`'s [`Op`]s into `ops`, and how many characters its compatibility
/// decomposition has. Decomposing a text reorders only runs of combining
/// marks, which are dropped, so a text's reading is its characters' in a
/// row (`every_reordered_character_is_a_combining_mark`).
fn plan_char(c: char, ops: &mut Vec<Op>) -> usize {
    ops.clear();
    let mut n = 0;
    for d in std::iter::once(c).nfkd() {
        n += 1;
        if is_combining_mark(d) {
            continue;
        }
        if ignored(d) {
            if mark(d) {
                ops.push(Op::Mark);
            }
            continue;
        }
        let d = match confusable(d) {
            'ß' => 's',
            d => d,
        };
        if d.is_alphanumeric() {
            for l in d.to_lowercase() {
                ops.push(Op::of(confusable(l)));
            }
        } else {
            ops.push(Op::of(d));
        }
    }
    n
}

/// Units of work ([`Tokenizer::meter`]), each about a nanosecond on the
/// reference runner, measured on the costliest input of each kind
/// (`budget_worst_case` in `tests/props.rs`).
mod cost {
    /// A byte of ASCII text.
    pub const BYTE: u64 = 1;
    /// A word whose plan is cached.
    pub const WORD: u64 = 14;
    /// A word planned anew, plus [`PLANNED_BYTE`] per byte.
    pub const PLAN: u64 = 160;
    pub const PLANNED_BYTE: u64 = 4;
    /// A spelled-out run's token.
    pub const SPELLED: u64 = 40;
    /// A character of non-ASCII text whose reading is cached, plus
    /// [`OP`] per [`super::Op`].
    pub const CHAR: u64 = 6;
    pub const OP: u64 = 2;
    /// A character read anew, plus [`DECOMPOSED`] per character of its
    /// decomposition.
    pub const CHAR_PLAN: u64 = 220;
    pub const DECOMPOSED: u64 = 90;
    /// A token copied or looked at again.
    pub const TOKEN: u64 = 6;
    /// A [`super::mark`] in the joined reading: what ending a word there
    /// costs the earlier reading.
    pub const MARK: u64 = WORD + PLAN + SPELLED;
    /// A file path, plus [`SEGMENT`] per segment and a unit per
    /// [`COMPARED`] bytes of it (its folder is compared with the ones
    /// remembered).
    pub const FILE: u64 = 40;
    pub const SEGMENT: u64 = 8;
    pub const COMPARED: u64 = 16;
}

/// A [`Tokenizer::meter`] limit no read reaches by its work: the read is
/// over only where a text reaches [`MAX_TOKENS`]. For the earlier reading
/// ([`Tokenizer::splitting`]), whose work the joined reading's meter
/// bounds but whose tokens it doesn't.
pub(crate) const CUT_ONLY: u64 = u64::MAX / 2;

/// Units of the core's work budget a file costs, however short its path.
pub(crate) const FILE_UNITS: u64 = cost::FILE;

/// The least a metered read of one path costs ([`read_whole`]): the file,
/// each segment, its bytes compared with the folders remembered, and its
/// name read (a byte costs at least a unit). Its folders may be read
/// once for many files, so they count only as compared.
pub(crate) fn least_path_cost(path: &[String]) -> u64 {
    let bytes: usize = path.iter().map(String::len).sum();
    let name = path.last().map_or(0, String::len);
    cost::FILE
        + path.len() as u64 * cost::SEGMENT
        + bytes as u64 / cost::COMPARED
        + name as u64 * cost::BYTE
}

impl Tokenizer {
    /// A tokenizer that reads [`mark`]s as word breaks: the earlier
    /// reading, kept for texts that hold one ([`splits`]).
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
        if self.over() {
            return;
        }
        if text.is_ascii() {
            self.work += text.len() as u64 * cost::BYTE;
            // A word starts at a letter or digit (the word is empty there,
            // so `@` and `$` can't start one) and takes `@` and `$` after.
            // Anything else ends it, as an [`Op::End`] does in other text.
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
                if self.over() {
                    return;
                }
                i = j;
            }
        } else {
            if self.chars.is_empty() {
                self.chars.resize_with(CHARS, CharPlan::default);
            }
            for c in text.chars() {
                let slot = c as usize % CHARS;
                if self.chars[slot].key != c as u32 {
                    let plan = &mut self.chars[slot];
                    // The key last: a slot is never matched before its
                    // plan is made.
                    plan.key = u32::MAX;
                    plan.round = 0;
                    plan.decomposed = plan_char(c, &mut plan.ops) as u64;
                    plan.mark = mark(c);
                    plan.key = c as u32;
                }
                self.saw_mark |= self.chars[slot].mark;
                let planned = cost::CHAR_PLAN + self.chars[slot].decomposed * cost::DECOMPOSED;
                if self.limit != 0 {
                    self.cold += planned;
                    if self.chars[slot].round != self.round {
                        // The key is this character: new to this read here.
                        self.chars[slot].round = self.round;
                        self.work += planned;
                    }
                }
                let n = self.chars[slot].ops.len();
                self.work += cost::CHAR + n as u64 * cost::OP;
                for i in 0..n {
                    match self.chars[slot].ops[i] {
                        Op::Push(c) => self.word.push(c),
                        Op::At(c) if !self.word.is_empty() => self.word.push(c),
                        Op::At(_) | Op::End => self.end_word(out),
                        Op::Mark if self.split => self.end_word(out),
                        // The earlier reading ends a word here, so the
                        // joined reading pays for that too: its meter
                        // bounds both ([`cost::MARK`]).
                        Op::Mark => self.work += cost::MARK,
                    }
                }
                if self.over() {
                    return;
                }
            }
        }
        self.end_word(out);
    }

    /// Starts counting the work done ([`cost`]), from zero, and stops
    /// reading once it passes `limit` (zero for no limit): every text
    /// after reads as nothing, and [`Tokenizer::over`] says so.
    pub(crate) fn meter(&mut self, limit: u64) {
        self.work = 0;
        self.cold = 0;
        self.limit = limit;
        self.saw_mark = false;
        if limit != 0 {
            // A new round: every slot of the meter's view reads as empty
            // (round 0 is never one).
            self.round = self.round.checked_add(1).unwrap_or_else(|| {
                self.plans.iter_mut().for_each(|p| p.round = 0);
                self.chars.iter_mut().for_each(|c| c.round = 0);
                1
            });
        }
    }

    /// Whether the work passed the limit [`Tokenizer::meter`] set.
    pub(crate) fn over(&self) -> bool {
        self.limit != 0 && self.work > self.limit
    }

    /// A token was dropped at [`MAX_TOKENS`]: a metered read is past its
    /// limit, so it's never taken for the whole text.
    fn cut(&mut self) {
        if self.limit != 0 {
            self.work = self.work.max(self.limit + 1);
        }
    }

    /// Counts `tokens` tokens copied or looked at again outside the
    /// tokenizer, for the same limit.
    pub(crate) fn charge(&mut self, tokens: usize) {
        self.work += tokens as u64 * cost::TOKEN;
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
        if word.is_empty() || self.over() {
            return;
        }
        if !self.room(out) {
            self.cut();
            return;
        }
        self.work += cost::WORD;
        if self.limit != 0 {
            self.cold += cost::PLAN + word.len() as u64 * cost::PLANNED_BYTE;
        }
        if self.plans.len() < PLANS {
            self.plans.resize_with(PLANS, Plan::default);
        }
        // Long words (hashes, junk) aren't kept.
        let plan = if word.len() <= MAX_PLANNED_BYTES {
            let at = slot(word);
            let plan = &mut self.plans[at];
            if plan.key.as_bytes() != word.as_bytes() {
                // The key last: a plan is never found before it's made,
                // even if making it was cut short.
                plan.round = 0;
                plan.key.clear();
                plan.word.clear();
                plan.word.push_str(word);
                plan.word.make_ascii_lowercase();
                make_plan(plan, &mut self.plain, &mut self.swapped);
                plan.key.push_str(word);
            }
            // Its key is this word: new to this read in this slot unless
            // this read used it last.
            if self.limit != 0 && plan.round != self.round {
                plan.round = self.round;
                self.work += cost::PLAN + word.len() as u64 * cost::PLANNED_BYTE;
            }
            plan
        } else {
            self.work += cost::PLAN + word.len() as u64 * cost::PLANNED_BYTE;
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
        // Each token is looked at again once read (its flags, the
        // signals).
        self.work += plan.tokens.len() as u64 * cost::TOKEN;
        out.reserve(plan.tokens.len());
        let mut cut = false;
        for (t, guarded) in &plan.tokens {
            if !guarded || out.len() - self.start < MAX_TOKENS {
                out.push(Token {
                    pos: first + t.pos,
                    ..*t
                });
            } else {
                cut = true;
            }
        }
        // `Tokenizer::cut`, on the fields: the plan is still borrowed.
        if cut && self.limit != 0 {
            self.work = self.work.max(self.limit + 1);
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
        self.work += cost::SPELLED + self.spelled.len() as u64 * cost::BYTE;
        if self.spelled.chars().count() >= MIN_SPELLED {
            if self.room(out) {
                out.push(Token::new(self.spelled_at, &self.spelled));
            } else {
                self.cut();
            }
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
    /// The meter's work for reading the folder, charged again for each
    /// path that the core reads a second time with its segments apart.
    cost: u64,
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
        let (before, cold) = (self.work, self.cold);
        self.begin(0, 0);
        into.apart = true;
        for d in dirs {
            self.feed(d, &mut into.tokens);
            into.apart &= self.spelled.is_empty();
        }
        let bytes: usize = dirs.iter().map(String::len).sum();
        // What this read cost, and its caches again as if cold: the core's
        // second read may miss them.
        into.cost = (self.work - before + (self.cold - cold)).max(bytes as u64 * cost::BYTE);
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
        self.charge(paused.tokens.len());
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
    /// The read passed the core's work budget ([`read_whole`]): its
    /// tokens stop short.
    pub(crate) over: bool,
    /// A text holds a [`mark`] ([`Metered::marks`]).
    pub(crate) marks: bool,
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
    with_tokenizer(|tok| paths_apart_with(tok, paths, out, ends, apart));
}

/// What a metered read of a torrent found ([`read_whole`], [`read_split`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Metered {
    /// The work it took, or `None` once that passed the core's budget.
    pub(crate) work: Option<u64>,
    /// A text held a [`mark`], so it reads differently in the earlier
    /// reading ([`splits`], without reading the text again).
    pub(crate) marks: bool,
}

/// Stops metering `tok` when dropped, so a panic mid-read never leaves
/// the thread's tokenizer metered (and every later read of it empty).
struct Unmeter<'t>(&'t mut Tokenizer);

impl Drop for Unmeter<'_> {
    fn drop(&mut self) {
        self.0.meter(0);
    }
}

/// A torrent's name and every path read as [`paths_apart`] reads them,
/// metered against the core's work budget ([`crate::core::BUDGET`]),
/// the rest read as nothing once past it (an empty text for each path
/// left, so the ends still line up with the files).
pub(crate) fn read_whole<'p>(
    name: &str,
    paths: impl IntoIterator<Item = &'p [String]>,
    out: &mut Vec<Token>,
    ends: &mut Vec<usize>,
    apart: &mut Vec<bool>,
) -> Metered {
    with_tokenizer(|tok| read_with(tok, name, paths, out, ends, apart))
}

/// [`read_whole`] in the earlier reading ([`Tokenizer::splitting`]), its
/// own tokenizer, metered against the same budget. Paths read alike
/// either way here: in one folder they share its part.
pub(crate) fn read_split<'p>(
    name: &str,
    paths: impl IntoIterator<Item = &'p [String]>,
    out: &mut Vec<Token>,
    ends: &mut Vec<usize>,
) -> Metered {
    let mut tok = Tokenizer::splitting();
    read_with(&mut tok, name, paths, out, ends, &mut Vec::new())
}

fn read_with<'p>(
    tok: &mut Tokenizer,
    name: &str,
    paths: impl IntoIterator<Item = &'p [String]>,
    out: &mut Vec<Token>,
    ends: &mut Vec<usize>,
    apart: &mut Vec<bool>,
) -> Metered {
    tok.meter(crate::core::BUDGET);
    let tok = Unmeter(tok);
    // Paths are read in turn, so the whole read is cut at the cap too.
    tok.0.text(name, out);
    ends.push(out.len());
    paths_apart_with(tok.0, paths, out, ends, apart);
    Metered {
        work: (!tok.0.over()).then_some(tok.0.work),
        marks: tok.0.saw_mark,
    }
}

fn paths_apart_with<'p>(
    tok: &mut Tokenizer,
    paths: impl IntoIterator<Item = &'p [String]>,
    out: &mut Vec<Token>,
    ends: &mut Vec<usize>,
    apart: &mut Vec<bool>,
) {
    {
        let mut paused = std::mem::take(&mut tok.folders);
        let mut dirs_of: [&[String]; FOLDERS] = [&[]; FOLDERS];
        let (mut known, mut next) = (0, 0);
        for path in paths {
            if out.len() >= MAX_TOKENS {
                tok.cut();
            }
            if tok.over() {
                apart.push(true);
                ends.push(out.len());
                continue;
            }
            let bytes: usize = path.iter().map(String::len).sum();
            tok.work +=
                cost::FILE + path.len() as u64 * cost::SEGMENT + bytes as u64 / cost::COMPARED;
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
                    let (before, cold) = (tok.work, tok.cold);
                    tok.resume(&paused[slot], name, out);
                    if !paused[slot].apart {
                        // The core reads this path again with its segments
                        // apart (`Read::path`), so the meter counts that
                        // read too: its folder in full, its name as this
                        // read of it cost and its caches again as if cold
                        // (the second read may miss them), its least, and
                        // its tokens looked at. A read that can't fit the
                        // text's cap is past the budget.
                        let again = out.len() - ends.last().copied().unwrap_or(0);
                        if again + path.len() >= MAX_TOKENS {
                            tok.cut();
                        }
                        tok.work += paused[slot].cost
                            + (tok.work - before)
                            + (tok.cold - cold)
                            + least_path_cost(path)
                            + again as u64 * cost::TOKEN;
                    }
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
    }
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

    /// Decomposing a text reorders only runs of characters with a
    /// combining class, so reading a text a character at a time
    /// ([`plan_char`]) gives what decomposing it whole does, as long as
    /// every such character is dropped: a combining mark.
    #[test]
    fn every_reordered_character_is_a_combining_mark() {
        use unicode_normalization::char::canonical_combining_class;
        for c in (0..=0x10FFFF).filter_map(char::from_u32) {
            if canonical_combining_class(c) != 0 {
                assert!(is_combining_mark(c), "U+{:04X}", c as u32);
            }
        }
    }

    #[test]
    fn bounded() {
        // Four tokens a word: past the cap, cut at it.
        let long = "a1 ".repeat(MAX_TOKENS / 2);
        assert_eq!(toks(&long).len(), MAX_TOKENS);
        let huge = "x".repeat(100_000);
        assert_eq!(toks(&huge).len(), 1);
    }

    /// Every character the joined reading ignores ([`ignored`]).
    fn all_ignored() -> Vec<char> {
        (0..=0x10FFFF)
            .filter_map(char::from_u32)
            .filter(|&c| ignored(c))
            .collect()
    }

    /// The format characters that show and the line and paragraph
    /// separators, at the ends of each range: word breaks in the earlier
    /// reading, nothing in the joined one.
    const VISIBLE: [char; 15] = [
        '\u{0600}',
        '\u{0605}',
        '\u{06DD}',
        '\u{070F}',
        '\u{0890}',
        '\u{0891}',
        '\u{08E2}',
        '\u{110BD}',
        '\u{110CD}',
        '\u{FFF9}',
        '\u{FFFB}',
        '\u{13430}',
        '\u{1343F}',
        '\u{2028}',
        '\u{2029}',
    ];

    /// `invisible` is exactly Unicode's default-ignorable characters,
    /// written out here on their own; every Cf character is either ignored
    /// or a known visible one; [`newly_ignored`] is exactly what it
    /// ignores that the matching before didn't; and [`mark`] is exactly
    /// what the joined reading ignores that the matching before didn't.
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
                assert_eq!(mark(c), ignored(c) && !before(c), "U+{at:04X}");
            }
            if lodestar_metainfo::format_char(c) {
                assert!(invisible(c) || visible_mark(c), "U+{at:04X} is Cf");
            }
            let visible = matches!(
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
                    | '\u{2028}'..='\u{2029}'
            );
            assert_eq!(visible_mark(c), visible, "U+{at:04X}");
            assert!(!(visible && invisible(c)), "U+{at:04X}");
        }
        let hello = toks("hello");
        for c in all_ignored() {
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
    /// paragraph separators, join a word in the joined reading and break
    /// it in the earlier one, as they did
    /// (docs/decisions/2026-11-02-F-visible-format-two-readings.md).
    #[test]
    fn visible_marks_read_both_ways() {
        let split = |text: &str| {
            let mut out = Vec::new();
            Tokenizer::splitting().text(text, &mut out);
            out
        };
        for c in VISIBLE {
            let at = c as u32;
            let text = format!("alpha{c}beta");
            assert!(splits([text.as_str()]), "U+{at:04X}");
            assert_eq!(toks(&text), toks("alphabeta"), "U+{at:04X}");
            assert_eq!(split(&text), toks("alpha beta"), "U+{at:04X}");
            let bad = format!("ba{c}d");
            assert_eq!(toks(&bad), toks("bad"), "U+{at:04X}");
            assert_eq!(split(&bad), toks("ba d"), "U+{at:04X}");
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

    /// A segment for the reads below: words, characters each reading
    /// reads differently, single letters and other characters.
    const SEGMENT: &str =
        "([a-z0-9]{1,6}|[a-z]|\u{2028}|\u{0605}|\u{FFF9}|\u{200B}|\\PC|[ .]){0,12}";

    proptest::proptest! {
        /// The core's check before reading never counts more than the
        /// read does, so nothing within the budget is held unread.
        #[test]
        fn the_least_cost_is_never_more_than_the_read(
            name in SEGMENT,
            folders in proptest::collection::vec(proptest::collection::vec(SEGMENT, 0..4), 1..7),
            files in proptest::collection::vec((proptest::prelude::any::<proptest::sample::Index>(), SEGMENT), 0..24),
        ) {
            let paths: Vec<Vec<String>> = files
                .into_iter()
                .map(|(at, own)| {
                    let mut p = folders[at.index(folders.len())].clone();
                    if !own.is_empty() || !p.is_empty() {
                        p.push(own);
                    }
                    p
                })
                .collect();
            let least = name.len() as u64 + paths.iter().map(|p| least_path_cost(p)).sum::<u64>();
            let (mut out, mut ends, mut apart) = (Vec::new(), Vec::new(), Vec::new());
            let read = read_whole(&name, paths.iter().map(|p| &p[..]), &mut out, &mut ends, &mut apart);
            proptest::prop_assert!(least <= read.work.unwrap(), "{} > {:?}", least, read.work);
        }

        /// The read notes a character the earlier reading reads as a word
        /// break exactly where [`splits`] finds one, and the earlier
        /// reading of every path, folders shared, is each path's own.
        #[test]
        fn the_reads_note_marks_and_read_each_path_whole(
            name in SEGMENT,
            folders in proptest::collection::vec(proptest::collection::vec(SEGMENT, 0..3), 1..6),
            files in proptest::collection::vec((proptest::prelude::any::<proptest::sample::Index>(), SEGMENT), 0..12),
        ) {
            let paths: Vec<Vec<String>> = files
                .into_iter()
                .map(|(at, own)| {
                    let mut p = folders[at.index(folders.len())].clone();
                    p.push(own);
                    p
                })
                .collect();
            let (mut out, mut ends, mut apart) = (Vec::new(), Vec::new(), Vec::new());
            let read = read_whole(&name, paths.iter().map(|p| &p[..]), &mut out, &mut ends, &mut apart);
            let texts = std::iter::once(&name[..]).chain(paths.iter().flatten().map(|s| &s[..]));
            proptest::prop_assert_eq!(read.marks, splits(texts));
            proptest::prop_assert!(read.work.is_some());
            let (mut split, mut split_ends) = (Vec::new(), Vec::new());
            proptest::prop_assert!(read_split(&name, paths.iter().map(|p| &p[..]), &mut split, &mut split_ends).work.is_some());
            let mut each = Vec::new();
            Tokenizer::splitting().text(&name, &mut each);
            proptest::prop_assert_eq!(&split[..split_ends[0]], &each[..]);
            for (i, p) in paths.iter().enumerate() {
                let mut each = Vec::new();
                Tokenizer::splitting().text(&p.join("/"), &mut each);
                proptest::prop_assert_eq!(&split[split_ends[i]..split_ends[i + 1]], &each[..], "{:?}", p);
            }
        }
    }

    proptest::proptest! {
        /// Inserting any character the joined reading ignores, anywhere in a
        /// text, never changes its tokens, so never whether a term rule
        /// that reads them matches (a core or pack term, a keyword, the
        /// core's windows).
        #[test]
        fn an_invisible_character_never_changes_a_match(
            text in "([a-e]|[a-e]{3,6}|[0-9]{1,3}|[@$]|é|ｂ|\u{0435}|e\u{301}|[ ./_-]){0,12}",
            rule in "[a-e]{1,6}( [a-e]{1,6}){0,2}",
            inserts in proptest::collection::vec((proptest::sample::select(all_ignored()), 0usize..64), 1..4),
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
        /// each mark a space: the matching before.
        #[test]
        fn the_splitting_reader_reads_as_before(
            text in "([a-e]|[a-e]{3,6}|[0-9]{1,3}|[@$]|é|ｂ|e\u{301}|[ ./_-]|\u{200B}|\u{2065}|\u{FFF0}|\u{1BCA0}|\u{1D173}|\u{E0001}|\u{E0041}|\u{E0FFF}|\u{0600}|\u{0605}|\u{08E2}|\u{110BD}|\u{FFF9}|\u{13430}|\u{2028}|\u{2029}){0,16}",
        ) {
            let mut split = Vec::new();
            Tokenizer::splitting().text(&text, &mut split);
            let spaced: String = text
                .chars()
                .map(|c| if mark(c) { ' ' } else { c })
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
