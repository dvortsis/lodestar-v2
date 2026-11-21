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
        // At most three digits: the value fits, so it's folded directly.
        let num = if !text.is_empty() && text.len() <= 3 && text.bytes().all(|b| b.is_ascii_digit())
        {
            Some(text.bytes().fold(0u16, |n, b| n * 10 + u16::from(b - b'0')))
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
/// the Mongolian free variation selectors U+180B–180D and U+180F, the
/// combining grapheme joiner). The variation selectors, the Mongolian ones
/// and the combining grapheme joiner are combining marks too, which
/// [`plan_char`] drops before it reads this, so listing them changes no
/// reading: it keeps this exactly the Unicode set
/// (`docs/decisions/2026-11-21-X-filter-dicp-doc.md`).
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
            | '\u{180B}'..='\u{180F}'
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
/// (U+E0100–E01EF) and the Mongolian free variation selectors (U+180B–180D,
/// U+180F) are combining marks, dropped either way, so a text holding one
/// needs no second reading.
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

/// Character readings displaced from the cache during one metered read
/// ([`Tokenizer::char_spill`]): when a character finds its slot held by
/// another this read has used, the two trade places, so the slot holds
/// the latest and nothing this read has used is lost. The map's hash is
/// keyed at random (std's), so no choice of characters makes its
/// look-ups slow, and each look-up is charged the same
/// ([`cost::PROBED`]). Buffers are kept from read to read. Words have
/// their own store, [`WordSpill`].
///
/// Bounded by charges already paid: every entry was charged as new in
/// this read, at least [`cost::CHAR_PLAN`], so a read within the budget
/// keeps at most about `BUDGET / CHAR_PLAN` characters (checked in debug
/// builds). [`Tokenizer::meter`] empties it at the start of every read,
/// and the meter's guard (`Unmeter`) at its end, a panic included, so
/// nothing carries over between torrents.
#[derive(Debug)]
struct Spill<K, P> {
    map: std::collections::HashMap<K, P>,
    spare: Vec<P>,
}

impl<K, P> Default for Spill<K, P> {
    fn default() -> Self {
        Spill {
            map: std::collections::HashMap::new(),
            spare: Vec::new(),
        }
    }
}

/// The most plans [`Spill`] keeps between reads; a larger read's are
/// freed when the next starts.
const SPILL_KEPT: usize = 1_024;

impl<K: std::hash::Hash + Eq, P> Spill<K, P> {
    /// Empties it for a new read, keeping some buffers.
    fn clear(&mut self) {
        if self.map.is_empty() {
            return;
        }
        for (_, plan) in self.map.drain() {
            if self.spare.len() < SPILL_KEPT {
                self.spare.push(plan);
            }
        }
        if self.map.capacity() > 2 * SPILL_KEPT {
            self.map = std::collections::HashMap::new();
        }
    }
}

/// Tokens a [`WordSpill`] entry holds in place; a word with more keeps
/// the rest in a buffer of the store's.
const STASHED_TOKENS: usize = 6;

/// One word's plan, as [`WordSpill`] keeps it: in place, with no buffer
/// of its own unless it has more than [`STASHED_TOKENS`] tokens.
#[derive(Debug, Clone, Copy)]
struct Stashed {
    /// The word as written ([`Plan::key`]): at most
    /// [`MAX_PLANNED_BYTES`], since longer words aren't kept.
    key: [u8; MAX_PLANNED_BYTES],
    key_len: u8,
    empty: bool,
    advance: u32,
    letter: Option<char>,
    tokens: [(Token, bool); STASHED_TOKENS],
    /// How many tokens: past [`STASHED_TOKENS`], all of them are in
    /// [`WordSpill::long`]`[more]`.
    count: u32,
    more: u32,
    /// The next entry whose key hashes the same ([`NONE`] for none).
    next: u32,
    /// Its key's hash ([`WordSpill::hash`]).
    hash: u64,
}

/// No entry.
const NONE: u32 = u32::MAX;

const NO_TOKEN: (Token, bool) = (
    Token {
        pos: 0,
        hash: 0,
        num: None,
    },
    false,
);

/// Plans of words displaced from the cache during one metered read
/// ([`Tokenizer::plan_spill`]): when a word finds its slot held by
/// another this read has used, the two trade places, so the slot holds
/// the latest and nothing this read has used is lost. A word goes here
/// only where its place in [`Tokenizer::kept`] is taken too.
///
/// A read with many distinct words (a file number each, say) keeps most
/// of them here, so it costs no allocation per word: entries sit in one
/// array, copied in and out, and freed entries are reused within the
/// read. The index is keyed by a hash keyed at random (std's), so no
/// choice of words makes its look-ups slow, and each look-up is charged
/// the same ([`cost::PROBED`]); words whose hashes are equal are chained,
/// compared by their bytes.
///
/// Bounded by charges already paid: every entry was charged as new in
/// this read, at least [`cost::PLAN`], so a read within the budget keeps
/// at most about `BUDGET / PLAN` words, counting [`Tokenizer::kept`]'s
/// too (checked in debug builds). [`Tokenizer::meter`] empties it at the
/// start of every read, and the meter's guard (`Unmeter`) at its end, a
/// panic included, so nothing carries over between torrents. Up to
/// [`STASH_KEPT`] entries' room is kept between reads.
#[derive(Debug, Default)]
struct WordSpill {
    keys: std::hash::RandomState,
    index: crate::hash::IdMap<u32>,
    entries: Vec<Stashed>,
    free: Vec<u32>,
    /// Tokens of entries with more than [`STASHED_TOKENS`].
    long: Vec<Vec<(Token, bool)>>,
    long_free: Vec<u32>,
}

/// Entries whose room [`WordSpill`] keeps between reads; a larger read's
/// is freed when the next starts.
const STASH_KEPT: usize = 1_024;

impl WordSpill {
    fn is_empty(&self) -> bool {
        self.index.is_empty()
    }

    fn hash(&self, word: &[u8]) -> u64 {
        use std::hash::BuildHasher;
        self.keys.hash_one(word)
    }

    /// Empties it for a new read, keeping some room.
    fn clear(&mut self) {
        if self.entries.is_empty() {
            return;
        }
        self.index.clear();
        self.entries.clear();
        self.free.clear();
        self.long_free.clear();
        self.long_free.extend(0..self.long.len() as u32);
        if self.entries.capacity() > STASH_KEPT {
            self.entries.shrink_to(STASH_KEPT);
            self.index.shrink_to(STASH_KEPT);
            self.free.shrink_to(STASH_KEPT);
        }
        if self.long.len() > SPILL_KEPT {
            self.long.truncate(SPILL_KEPT);
            self.long.shrink_to(SPILL_KEPT);
            self.long_free.clear();
            self.long_free.extend(0..self.long.len() as u32);
        }
    }

    /// The entry holding `word`'s plan.
    fn find(&self, word: &str) -> Option<u32> {
        self.find_hashed(word, self.hash(word.as_bytes()))
    }

    fn find_hashed(&self, word: &str, h: u64) -> Option<u32> {
        let mut at = *self.index.get(&h)?;
        while at != NONE {
            let e = &self.entries[at as usize];
            if &e.key[..usize::from(e.key_len)] == word.as_bytes() {
                return Some(at);
            }
            at = e.next;
        }
        None
    }

    /// A free entry, its fields stale.
    fn alloc(&mut self) -> u32 {
        if let Some(at) = self.free.pop() {
            return at;
        }
        self.entries.push(Stashed {
            key: [0; MAX_PLANNED_BYTES],
            key_len: 0,
            empty: false,
            advance: 0,
            letter: None,
            tokens: [NO_TOKEN; STASHED_TOKENS],
            count: 0,
            more: NONE,
            next: NONE,
            hash: 0,
        });
        (self.entries.len() - 1) as u32
    }

    /// Keeps a copy of `plan`, a word this read has used.
    fn put(&mut self, plan: &Plan) {
        self.put_hashed(plan, self.hash(plan.key.as_bytes()));
    }

    fn put_hashed(&mut self, plan: &Plan, h: u64) {
        let key = plan.key.as_bytes();
        debug_assert!(key.len() <= MAX_PLANNED_BYTES, "only short words are kept");
        let more = if plan.tokens.len() <= STASHED_TOKENS {
            NONE
        } else {
            let more = match self.long_free.pop() {
                Some(m) => m,
                None => {
                    self.long.push(Vec::new());
                    (self.long.len() - 1) as u32
                }
            };
            let buf = &mut self.long[more as usize];
            buf.clear();
            buf.extend_from_slice(&plan.tokens);
            more
        };
        let at = self.alloc();
        let next = self.index.insert(h, at).unwrap_or(NONE);
        let e = &mut self.entries[at as usize];
        e.key[..key.len()].copy_from_slice(key);
        e.key_len = key.len() as u8;
        e.empty = plan.empty;
        e.advance = plan.advance;
        e.letter = plan.letter;
        e.count = plan.tokens.len() as u32;
        if more == NONE {
            e.tokens[..plan.tokens.len()].copy_from_slice(&plan.tokens);
        }
        e.more = more;
        e.next = next;
        e.hash = h;
    }

    /// Moves entry `at`'s plan into `plan`, reusing its buffers, as used
    /// by read `round`, and frees the entry.
    fn take(&mut self, at: u32, plan: &mut Plan, round: u32) {
        let e = &self.entries[at as usize];
        let (next, more, h) = (e.next, e.more, e.hash);
        let key = &e.key[..usize::from(e.key_len)];
        plan.key.clear();
        plan.key
            .push_str(std::str::from_utf8(key).expect("a key copied from a str"));
        plan.empty = e.empty;
        plan.advance = e.advance;
        plan.letter = e.letter;
        plan.round = round;
        plan.tokens.clear();
        if more == NONE {
            plan.tokens.extend_from_slice(&e.tokens[..e.count as usize]);
        } else {
            plan.tokens.extend_from_slice(&self.long[more as usize]);
            self.long_free.push(more);
        }
        // Unlink it from its hash's chain.
        let head = self.index.get_mut(&h).expect("an entry is indexed");
        if *head == at {
            if next == NONE {
                self.index.remove(&h);
            } else {
                *head = next;
            }
        } else {
            let mut prev = *head;
            while self.entries[prev as usize].next != at {
                prev = self.entries[prev as usize].next;
            }
            self.entries[prev as usize].next = next;
        }
        self.free.push(at);
    }
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
    /// Plans of words this metered read has used, displaced from their
    /// slot by another word of the read ([`Spill`]): so no word is
    /// planned, or charged as new, twice in one torrent whatever the
    /// cache's size. Each was charged as new, so the budget bounds how
    /// many.
    plan_spill: WordSpill,
    /// The first place a displaced word is kept ([`Tokenizer::trade`]):
    /// in either of its two places ([`places`]), an entry kept while its
    /// round is this read's. [`Tokenizer::plan_spill`] holds those whose
    /// places here are both taken.
    kept: Vec<Plan>,
    /// Words this read keeps, in [`Tokenizer::kept`] and
    /// [`Tokenizer::plan_spill`] together.
    kept_words: usize,
    /// The plan of a word too long to keep.
    long: Plan,
    /// A plan's buffers, for [`Tokenizer::trade`] to exchange a word
    /// with one in [`Tokenizer::plan_spill`].
    exchange: Plan,
    /// [`paths_apart`]'s folders, kept so their buffers carry over
    /// between torrents.
    folders: [Paused; FOLDERS],
    /// Reads [`mark`]s as word breaks, as the matching before did
    /// ([`Tokenizer::splitting`]).
    split: bool,
    /// Each character's reading in non-ASCII text, cached
    /// ([`CharPlan`]); empty until the first such text.
    chars: Vec<CharPlan>,
    /// Readings of characters displaced so, as
    /// [`Tokenizer::plan_spill`].
    char_spill: Spill<u32, CharPlan>,
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
    /// Each token of a word new to the read: its first look-ups, which
    /// later reads of the same word find cached.
    pub const PLANNED_TOKEN: u64 = 48;
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
    /// A word or character whose slot another of the same read holds,
    /// looked up among those displaced ([`super::Spill`]) and traded for
    /// it, on top of [`BYTE`] per byte of a word to hash and compare it.
    pub const PROBED: u64 = 90;
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
                let metered = self.limit != 0;
                let fresh = if self.chars[slot].key == c as u32 {
                    metered && self.chars[slot].round != self.round
                } else if metered && self.chars[slot].round == self.round {
                    // Another character this read uses holds the slot: they
                    // trade places, the other kept for the read.
                    self.work += cost::PROBED;
                    let (plan, fresh) = match self.char_spill.map.remove(&(c as u32)) {
                        Some(plan) => (plan, false),
                        None => {
                            let mut plan = self.char_spill.spare.pop().unwrap_or_default();
                            plan.ops.clear();
                            plan.decomposed = plan_char(c, &mut plan.ops) as u64;
                            plan.mark = mark(c);
                            plan.key = c as u32;
                            (plan, true)
                        }
                    };
                    let held = std::mem::replace(&mut self.chars[slot], plan);
                    self.char_spill.map.insert(held.key, held);
                    debug_assert!(
                        self.char_spill.map.len() as u64 * cost::CHAR_PLAN <= self.work,
                        "each kept character was charged as new"
                    );
                    fresh
                } else {
                    let plan = &mut self.chars[slot];
                    // The key last: a slot is never matched before its
                    // plan is made.
                    plan.key = u32::MAX;
                    plan.round = 0;
                    plan.decomposed = plan_char(c, &mut plan.ops) as u64;
                    plan.mark = mark(c);
                    plan.key = c as u32;
                    metered
                };
                // New to this read unless this read used it already.
                if fresh {
                    self.chars[slot].round = self.round;
                }
                self.saw_mark |= self.chars[slot].mark;
                let planned = cost::CHAR_PLAN + self.chars[slot].decomposed * cost::DECOMPOSED;
                if metered {
                    self.cold += planned;
                    if fresh {
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
        self.char_spill.clear();
        self.plan_spill.clear();
        self.kept_words = 0;
        if limit != 0 {
            // A new round: every slot of the meter's view reads as empty
            // (round 0 is never one).
            self.round = self.round.checked_add(1).unwrap_or_else(|| {
                self.plans.iter_mut().for_each(|p| p.round = 0);
                self.kept.iter_mut().for_each(|p| p.round = 0);
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
        if self.plans.len() < PLANS {
            self.plans.resize_with(PLANS, Plan::default);
        }
        let metered = self.limit != 0;
        // Long words (hashes, junk) aren't kept.
        let plan = if word.len() <= MAX_PLANNED_BYTES {
            let at = slot(word);
            if self.plans[at].key.as_bytes() != word.as_bytes() {
                if metered && self.plans[at].round == self.round {
                    // Another word this read uses holds the slot: they
                    // trade places, the other kept for the read.
                    self.trade(at, word);
                } else {
                    let plan = &mut self.plans[at];
                    // The key last: a plan is never found before it's
                    // made, even if making it was cut short.
                    plan.round = 0;
                    plan.key.clear();
                    plan.word.clear();
                    plan.word.push_str(word);
                    plan.word.make_ascii_lowercase();
                    make_plan(plan, &mut self.plain, &mut self.swapped);
                    plan.key.push_str(word);
                }
            }
            let plan = &mut self.plans[at];
            // Its key is this word: new to this read unless this read
            // used it already.
            if metered && plan.round != self.round {
                plan.round = self.round;
                self.work += cost::PLAN
                    + word.len() as u64 * cost::PLANNED_BYTE
                    + plan.tokens.len() as u64 * cost::PLANNED_TOKEN;
            }
            plan
        } else {
            let plan = &mut self.long;
            plan.word.clear();
            plan.word.push_str(word);
            plan.word.make_ascii_lowercase();
            make_plan(plan, &mut self.plain, &mut self.swapped);
            self.work += cost::PLAN
                + word.len() as u64 * cost::PLANNED_BYTE
                + plan.tokens.len() as u64 * cost::PLANNED_TOKEN;
            plan
        };
        if metered {
            self.cold += cost::PLAN
                + word.len() as u64 * cost::PLANNED_BYTE
                + plan.tokens.len() as u64 * cost::PLANNED_TOKEN;
        }
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

    /// [`Tokenizer::emit`]'s trade: `word` takes slot `at` from another
    /// word this read has used, which is kept for the read: in its place
    /// in [`Tokenizer::kept`], or in [`Tokenizer::plan_spill`] where that
    /// is taken. `word`'s plan comes back from where it was kept, or is
    /// made anew; one made anew is left for `emit` to charge, its round
    /// not yet this read's. Plans move by swapping in place, or are copied
    /// in and out of the store, so a trade allocates nothing once the
    /// buffers have grown, and hashes no more than the word's bytes unless
    /// both places are taken.
    #[cold]
    #[inline(never)]
    fn trade(&mut self, at: usize, word: &str) {
        self.work += cost::PROBED + word.len() as u64 * cost::BYTE;
        if self.kept.len() < KEPT {
            self.kept.resize_with(KEPT, Plan::default);
        }
        let round = self.round;
        let held = |p: &Plan| p.round == round && p.key == word;
        let w2 = places(word).into_iter().find(|&w| held(&self.kept[w]));
        if let Some(w2) = w2 {
            // Kept in one of its places: the two trade, and the one
            // displaced moves to one of its own.
            std::mem::swap(&mut self.plans[at], &mut self.kept[w2]);
            self.keep_from(w2);
            return;
        }
        let back = if self.plan_spill.is_empty() {
            None
        } else {
            self.plan_spill.find(word)
        };
        match back {
            Some(from) => {
                // Kept in the store: it comes back, and the one displaced
                // is kept in its place, or in the store where that is
                // taken.
                if let Some(to) = self.free_place(&self.plans[at].key) {
                    std::mem::swap(&mut self.plans[at], &mut self.kept[to]);
                    self.plan_spill.take(from, &mut self.plans[at], round);
                } else {
                    self.plan_spill.take(from, &mut self.exchange, round);
                    self.plan_spill.put(&self.plans[at]);
                    std::mem::swap(&mut self.plans[at], &mut self.exchange);
                }
            }
            None => {
                // New to this read: the one displaced is kept, and the
                // slot's buffer is planned for `word`.
                if let Some(h2) = self.free_place(&self.plans[at].key) {
                    std::mem::swap(&mut self.plans[at], &mut self.kept[h2]);
                } else {
                    self.plan_spill.put(&self.plans[at]);
                }
                self.kept_words += 1;
                let plan = &mut self.plans[at];
                plan.round = 0;
                plan.key.clear();
                plan.word.clear();
                plan.word.push_str(word);
                plan.word.make_ascii_lowercase();
                make_plan(plan, &mut self.plain, &mut self.swapped);
                plan.key.push_str(word);
            }
        }
        debug_assert!(
            self.kept_words as u64 * cost::PLAN <= self.work,
            "each kept word was charged as new"
        );
    }

    /// Moves the plan at `kept[from]`, a word this read keeps, to one of
    /// its own places, or to [`Tokenizer::plan_spill`] where both are
    /// taken; `kept[from]` is left free. It stays where it is if that's
    /// one of them.
    fn keep_from(&mut self, from: usize) {
        if places(&self.kept[from].key).contains(&from) {
            return;
        }
        if let Some(to) = self.free_place(&self.kept[from].key) {
            self.kept.swap(from, to);
        } else {
            self.plan_spill.put(&self.kept[from]);
        }
        self.kept[from].round = 0;
    }

    /// One of `key`'s places in [`Tokenizer::kept`] that this read isn't
    /// using, if either is free.
    fn free_place(&self, key: &str) -> Option<usize> {
        places(key)
            .into_iter()
            .find(|&p| self.kept[p].round != self.round)
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
        // Every byte of ASCII text is a character's start.
        let ascii = plain.is_ascii();
        for (i, b) in plain.bytes().enumerate() {
            if !ascii && !plain.is_char_boundary(i) {
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

/// Places in [`Tokenizer::kept`]: a power of two.
const KEPT: usize = 2048;

/// A word's two places in [`Tokenizer::kept`], from all its bytes, so
/// words that share a [`slot`] seldom share these too. A word is kept in
/// whichever is free, so the table fills well before words overflow to
/// [`Tokenizer::plan_spill`].
fn places(word: &str) -> [usize; 2] {
    let h = word.bytes().fold(0xcbf2_9ce4_8422_2325u64, |h, b| {
        (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
    });
    let h = crate::hash::fmix64(h);
    [h as usize % KEPT, (h >> 32) as usize % KEPT]
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

/// Runs `f` with this thread's tokenizer for the earlier reading
/// ([`Tokenizer::splitting`]), or a new one if it's in use, as
/// [`with_tokenizer`] does for the main one. Kept per thread, so a read
/// that trades through [`Tokenizer::kept`] doesn't allocate and free the
/// table each time. Its meter is stopped when `f` returns, a panic
/// included, so a meter `f` starts never reaches the next caller.
pub(crate) fn with_splitting<R>(f: impl FnOnce(&mut Tokenizer) -> R) -> R {
    thread_local! {
        static SPLITTING: std::cell::RefCell<Tokenizer> =
            std::cell::RefCell::new(Tokenizer::splitting());
    }
    let mut f = Some(f);
    let reused = SPLITTING.try_with(|t| {
        t.try_borrow_mut().ok().map(|mut t| {
            let tok = Unmeter(&mut t);
            (f.take().expect("called once"))(&mut *tok.0)
        })
    });
    match reused {
        Ok(Some(r)) => r,
        _ => (f.take().expect("not called"))(&mut Tokenizer::splitting()),
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
    /// The work the read took ([`Metered::work`]), zero where it's over.
    pub(crate) work: u64,
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

/// [`read_whole`] in the earlier reading ([`Tokenizer::splitting`]), with
/// the thread's tokenizer for it ([`with_splitting`]), metered against the
/// same budget. Paths read alike either way here: in one folder they share
/// its part.
pub(crate) fn read_split<'p>(
    name: &str,
    paths: impl IntoIterator<Item = &'p [String]>,
    out: &mut Vec<Token>,
    ends: &mut Vec<usize>,
) -> Metered {
    with_splitting(|tok| read_with(tok, name, paths, out, ends, &mut Vec::new()))
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

    /// A metered read of `name` and `paths`: its work, its cold work and
    /// its tokens, the tokenizer left unmetered after.
    fn metered(tok: &mut Tokenizer, name: &str, paths: &[Vec<String>]) -> (u64, u64, Vec<Token>) {
        let (mut out, mut ends, mut apart) = (Vec::new(), Vec::new(), Vec::new());
        tok.meter(crate::core::BUDGET);
        tok.text(name, &mut out);
        ends.push(out.len());
        paths_apart_with(
            tok,
            paths.iter().map(|p| &p[..]),
            &mut out,
            &mut ends,
            &mut apart,
        );
        let read = (tok.work, tok.cold, out);
        tok.meter(0);
        read
    }

    /// Random text from a small vocabulary, so its words and characters
    /// keep meeting in the tokenizer's caches: a name and some paths.
    fn colliding(seed: u64) -> (String, Vec<Vec<String>>) {
        let mut x = seed | 1;
        let mut next = move || {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x
        };
        let letters: Vec<char> = "abcdefghij0123"
            .chars()
            .chain((0..6).filter_map(|k| char::from_u32(0x4E00 + k * 2048)))
            .collect();
        let size = 200 + (next() % 3_000) as usize;
        let vocabulary: Vec<String> = (0..size)
            .map(|_| {
                let len = 1 + next() % 6;
                (0..len)
                    .map(|_| letters[next() as usize % letters.len()])
                    .collect()
            })
            .collect();
        let mut words = |n: usize| {
            let mut s = String::new();
            for _ in 0..n {
                s.push_str(&vocabulary[next() as usize % vocabulary.len()]);
                s.push(' ');
            }
            s
        };
        let name = words(400);
        let folders: Vec<String> = (0..3).map(|_| words(4)).collect();
        let paths = (0..40)
            .map(|i| vec![folders[i % 3].clone(), words(30)])
            .collect();
        (name, paths)
    }

    /// A read costs the same, and reads the same, on a tokenizer that
    /// earlier reads left in any state as on a new one: its caches and
    /// what it keeps between reads change how fast it reads, never what a
    /// read is charged. Both readings' tokenizers, the joined one and the
    /// earlier one ([`Tokenizer::splitting`], kept per thread by
    /// [`with_splitting`]), on texts with marks and without
    /// (X-filter-split-nits).
    #[test]
    fn a_read_costs_the_same_whatever_came_before() {
        // A mark after every `a`: the earlier reading ends a word there,
        // the joined one reads on and pays for it.
        let mark = |s: &str| s.replace('a', "a\u{0600}");
        let new = |earlier: bool| {
            if earlier {
                Tokenizer::splitting()
            } else {
                Tokenizer::default()
            }
        };
        let mut apart = 0;
        for (reading, earlier) in [("joined", false), ("earlier", true)] {
            for marks in [false, true] {
                let text = |seed: u64| {
                    let (name, paths) = colliding(seed);
                    if !marks {
                        return (name, paths);
                    }
                    let paths = paths
                        .iter()
                        .map(|p| p.iter().map(|s| mark(s)).collect())
                        .collect();
                    (mark(&name), paths)
                };
                for case in 0..48u64 {
                    let (name, paths) = text(case * 7 + 1);
                    let fresh = metered(&mut new(earlier), &name, &paths);
                    if marks && earlier {
                        let joined = metered(&mut Tokenizer::default(), &name, &paths);
                        apart += usize::from(joined.2 != fresh.2);
                    }
                    let mut used = new(earlier);
                    for earlier in 0..(1 + case % 4) {
                        let (n, p) = text(case * 131 + earlier * 17 + 3);
                        metered(&mut used, &n, &p);
                        // An unmetered read between them, too.
                        let mut out = Vec::new();
                        used.text(&n, &mut out);
                    }
                    let at = format!("{reading} reading, marks {marks}, case {case}");
                    assert_eq!(metered(&mut used, &name, &paths), fresh, "{at}");
                    // And again, right after the same read.
                    assert_eq!(metered(&mut used, &name, &paths), fresh, "{at}, again");
                }
            }
        }
        // The marks make the two readings differ, so the earlier one's own
        // state is what's tested.
        assert_eq!(apart, 48, "the readings differed in {apart} of 48 cases");
    }

    /// A metered read, whose words trade places through
    /// [`Tokenizer::kept`] and [`WordSpill`], reads exactly what an
    /// unmetered one does, which never trades: no plan is changed on the
    /// way in or out. The vocabularies run from a few hundred words to
    /// thousands, past both tables, and some words have more tokens than
    /// a [`WordSpill`] entry holds in place. What each read is charged is
    /// pinned too.
    #[test]
    fn a_metered_read_reads_what_an_unmetered_one_does() {
        let unmetered = |tok: &mut Tokenizer, name: &str, paths: &[Vec<String>]| {
            let (mut out, mut ends, mut apart) = (Vec::new(), Vec::new(), Vec::new());
            tok.text(name, &mut out);
            ends.push(out.len());
            paths_apart_with(
                tok,
                paths.iter().map(|p| &p[..]),
                &mut out,
                &mut ends,
                &mut apart,
            );
            out
        };
        let mut checked = 0;
        let mut charged = 0u64;
        let mut tok = Tokenizer::default();
        for case in 0..48u64 {
            let (name, mut paths) = colliding(case * 7 + 1);
            // Words of many parts, each with more tokens than an entry
            // holds in place, and distinct numbers past every table.
            paths.extend((0..3_000u64).map(|i| {
                vec![
                    format!("a{}b{}c{}d", i % 97, i % 89, i % 83),
                    format!("{}", i * 7 + case),
                ]
            }));
            let (work, cold, metered_out) = metered(&mut tok, &name, &paths);
            if work > crate::core::BUDGET {
                continue;
            }
            checked += 1;
            charged = charged.wrapping_mul(1_000_003).wrapping_add(work);
            charged = charged.wrapping_mul(1_000_003).wrapping_add(cold);
            let plain = unmetered(&mut Tokenizer::default(), &name, &paths);
            assert_eq!(metered_out, plain, "case {case}");
        }
        assert!(checked >= 24, "only {checked} cases fit the budget");
        // Every case's work and cold work, folded: the same on `main`
        // before the flat store (#910), so no word is lost from a table
        // and planned, or charged, again.
        assert_eq!(checked, 48);
        assert_eq!(charged, 17_323_027_761_506_347_612);
    }

    /// The earlier reading's tokenizer is kept per thread: a split read
    /// that trades reuses its tables, reads and is charged exactly as on
    /// a new tokenizer, and leaves it unmetered for the next caller, even
    /// one that started a meter of its own.
    #[test]
    fn the_splitting_tokenizer_is_kept_and_left_unmetered() {
        // Distinct numbers past the cache, so the read trades.
        let paths: Vec<Vec<String>> = (0..3_000u32)
            .map(|i| vec!["disc".into(), format!("{i}")])
            .collect();
        let read = |out: &mut Vec<Token>, ends: &mut Vec<usize>| {
            read_split("name", paths.iter().map(|p| &p[..]), out, ends)
        };
        let fresh = {
            let (mut out, mut ends) = (Vec::new(), Vec::new());
            let m = read_with(
                &mut Tokenizer::splitting(),
                "name",
                paths.iter().map(|p| &p[..]),
                &mut out,
                &mut ends,
                &mut Vec::new(),
            );
            (m.work, out, ends)
        };
        assert!(fresh.0.is_some(), "within the budget");
        let mut kept_at = None;
        for _ in 0..3 {
            let (mut out, mut ends) = (Vec::new(), Vec::new());
            let m = read(&mut out, &mut ends);
            assert_eq!((m.work, out, ends), fresh);
            let at = with_splitting(|t| {
                assert!(!t.kept.is_empty(), "the read traded");
                assert_eq!(t.limit, 0, "unmetered after a read");
                t.kept.as_ptr()
            });
            assert_eq!(*kept_at.get_or_insert(at), at, "the table is reused");
        }
        // A caller's own meter doesn't outlive its call.
        with_splitting(|t| t.meter(1));
        with_splitting(|t| {
            assert_eq!(t.limit, 0);
            let mut out = Vec::new();
            t.text("still read whole", &mut out);
            assert_eq!(out.len(), 3);
        });
    }

    fn plan_of(word: &str) -> Plan {
        let mut plan = Plan::default();
        plan.word.push_str(word);
        plan.word.make_ascii_lowercase();
        make_plan(&mut plan, &mut String::new(), &mut String::new());
        plan.key.push_str(word);
        plan
    }

    fn same_plan(a: &Plan, b: &Plan) -> bool {
        a.key == b.key
            && a.tokens == b.tokens
            && a.advance == b.advance
            && a.letter == b.letter
            && a.empty == b.empty
    }

    /// Words whose hashes are equal share a chain: each is found, and
    /// comes back whole, whether it's the chain's first, middle or last;
    /// the others stay found.
    #[test]
    fn a_word_spill_chain_keeps_each_word() {
        let words = ["alpha", "a1b2c3d4e5", "z"];
        for gone in 0..words.len() {
            let mut spill = WordSpill::default();
            for w in words {
                spill.put_hashed(&plan_of(w), 7);
            }
            let at = spill.find_hashed(words[gone], 7).expect("kept");
            let mut back = Plan::default();
            spill.take(at, &mut back, 3);
            assert!(same_plan(&back, &plan_of(words[gone])), "{}", words[gone]);
            assert_eq!(back.round, 3);
            for (i, w) in words.iter().enumerate() {
                assert_eq!(spill.find_hashed(w, 7).is_some(), i != gone, "{w}");
            }
            // Its entry is reused.
            spill.put_hashed(&plan_of("again"), 7);
            assert_eq!(spill.entries.len(), words.len());
            assert!(spill.find_hashed("again", 7).is_some());
        }
    }

    /// Plans with more tokens than an entry holds in place keep them all,
    /// and their buffer is reused after [`WordSpill::clear`].
    #[test]
    fn a_word_spill_keeps_long_plans_and_clears() {
        let mut spill = WordSpill::default();
        let long = plan_of("a1b2c3d4e5f6");
        assert!(long.tokens.len() > STASHED_TOKENS);
        let short = plan_of("hello");
        spill.put(&long);
        spill.put(&short);
        for p in [&long, &short] {
            let at = spill.find(&p.key).expect("kept");
            let mut back = plan_of("stale");
            spill.take(at, &mut back, 1);
            assert!(same_plan(&back, p), "{}", p.key);
        }
        assert!(spill.is_empty());
        spill.put(&long);
        spill.clear();
        assert!(spill.is_empty() && spill.find(&long.key).is_none());
        spill.put(&long);
        assert_eq!(spill.long.len(), 1, "the buffer is reused");
        // Room past what's kept between reads is freed.
        for i in 0..3 * STASH_KEPT {
            spill.put(&plan_of(&format!("w{i}")));
        }
        spill.clear();
        assert!(spill.entries.capacity() <= STASH_KEPT);
    }

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
                    | '\u{180B}'..='\u{180F}'
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

    /// The Mongolian free variation selectors (U+180B–180D, U+180F) are
    /// default-ignorable and combining marks: dropped in both readings, as
    /// the matching before dropped them, so they join a word, never split
    /// one, and ask for no second reading
    /// (docs/decisions/2026-11-21-X-filter-dicp-doc.md).
    #[test]
    fn mongolian_free_variation_selectors_read_as_nothing() {
        let alpha_beta = crate::rules::keyword_tokens("alpha beta");
        for c in ['\u{180B}', '\u{180C}', '\u{180D}', '\u{180F}'] {
            let at = c as u32;
            assert!(is_combining_mark(c), "U+{at:04X} is a combining mark");
            assert!(invisible(c), "U+{at:04X}");
            assert!(!newly_ignored(c) && !mark(c), "U+{at:04X}");
            let mut ops = Vec::new();
            plan_char(c, &mut ops);
            assert!(ops.is_empty(), "U+{at:04X} reads as nothing: {ops:?}");
            assert_eq!(toks(&format!("hel{c}lo")), toks("hello"), "U+{at:04X}");
            assert!(has(&format!("hel{c}lo"), "hello", 0), "U+{at:04X}");
            assert!(!has(&format!("hel{c}lo"), "hel", 0), "U+{at:04X}");
            assert!(has(&format!("alpha{c}beta"), "alphabeta", 0), "U+{at:04X}");
            let mut t = Vec::new();
            tokens(&format!("The.Al{c}pha.{c}Be{c}ta{c}.2019"), &mut t);
            assert!(crate::rules::in_a_row(&alpha_beta, &t), "U+{at:04X}");
            assert!(!splits([format!("na\u{0430}me{c}").as_str()]), "U+{at:04X}");
        }
        // U+180E, the vowel separator, is a format character, ignored as
        // it was.
        assert!(invisible('\u{180E}') && !is_combining_mark('\u{180E}'));
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
