//! The phrase and NEAR search before positions were indexed (§20.8),
//! kept as the reference the indexed one is property-tested against.
//! Each scanned the text for every position it looked at, so a name
//! repeating a word thousands of times cost the square of its length.

use std::borrow::Cow;
use std::cell::{Cell, OnceCell};

use super::{Operand, Term};
use crate::fasttok::{self, Text};

/// The terms at consecutive positions.
pub(super) fn sequence(field: &Text, words: &[Term]) -> bool {
    let Some(first) = words.first() else {
        return false;
    };
    let at = |pos: u32, w: &Term| {
        field
            .toks
            .iter()
            .any(|t| t.position == pos && field.is(t, w.hash, &w.text))
    };
    field
        .toks
        .iter()
        .filter(|t| field.is(t, first.hash, &first.text))
        .any(|start| {
            words
                .iter()
                .zip(0..)
                .skip(1)
                .all(|(w, i)| at(start.position + i, w))
        })
}

/// Every word within a window of `slop` extra positions, in any order:
/// for some place of the first word, every word has a place within the
/// window. A word of several parts takes up their span, and distances
/// run between the nearest ends of two spans (§16), so `1080p NEAR/1 x`
/// allows one token between `p` and `x`, as a single word would.
///
/// Names are untrusted and can repeat a word hundreds of times, so the
/// other words are looked for only in the window around each place of
/// the first, found by binary search on position: the cost grows with
/// the text, not its square.
pub(super) fn has_near(field: &Text, words: &[Operand], slop: u32) -> bool {
    let Some((first, rest)) = words.split_first() else {
        return false;
    };
    let span = words.len() as u32 + slop;
    // Words of one token each, the usual case: each place of the first
    // word against every token, as cheap as it gets for an ordinary name.
    // Past a few places, the windowed search below takes over.
    if words
        .iter()
        .all(|w| w.whole.is_empty() && w.parts.len() == 1)
    {
        fn places<'a>(field: &'a Text, w: &'a Operand) -> impl Iterator<Item = u32> + 'a {
            let w = &w.parts[0];
            field
                .toks
                .iter()
                .filter(move |t| field.is(t, w.hash, &w.text))
                .map(|t| t.position)
        }
        let mut found = 0;
        for p0 in places(field, first) {
            found += 1;
            if found > SCANS {
                break;
            }
            if rest
                .iter()
                .all(|w| places(field, w).any(|p| p.abs_diff(p0) < span))
            {
                return true;
            }
        }
        if found <= SCANS {
            return false;
        }
    }
    let near = Near {
        field,
        order: OnceCell::new(),
        scans: Cell::new(SCANS),
    };
    // The first word's places in any order, then each other word only in
    // the window around one.
    field
        .toks
        .iter()
        .filter(|t| starts(first, t.hash))
        .filter_map(|t| near.place(first, t))
        .any(|(a0, a1)| {
            rest.iter().all(|w| {
                // A place of `w` ends within `span - 1` of the first
                // word's place, so it starts no earlier than this.
                let width = w.parts.len().max(1) as u32 - 1;
                let lo = a0.saturating_sub(span - 1 + width);
                let hi = a1.saturating_add(span - 1);
                near.any_between(lo, hi, |t| {
                    starts(w, t.hash) && near.place(w, t).is_some_and(|b| gap((a0, a1), b) < span)
                })
            })
        })
}

/// Plain scans of a text's tokens before [`Near`] sorts them by position
/// and searches: as cheap for an ordinary name, and a hostile one that
/// repeats a word hundreds of times then costs in proportion to its
/// length, not its square.
const SCANS: u32 = 8;

/// One text, for finding NEAR words' places.
struct Near<'a> {
    field: &'a Text,
    /// The tokens in position order, made when the scans run out: the
    /// tokenizer emits a compound after its word's later parts, so such
    /// texts are sorted.
    order: OnceCell<Cow<'a, [fasttok::Tok]>>,
    /// Plain scans left.
    scans: Cell<u32>,
}

impl Near<'_> {
    /// Whether `f` holds for a token at positions `lo..=hi`.
    #[inline]
    fn any_between(&self, lo: u32, hi: u32, f: impl FnMut(&fasttok::Tok) -> bool) -> bool {
        let toks = &self.field.toks;
        if let Some(left) = self.scans.get().checked_sub(1) {
            self.scans.set(left);
            return toks
                .iter()
                .filter(|t| (lo..=hi).contains(&t.position))
                .any(f);
        }
        let sorted = self.order.get_or_init(|| {
            if toks.is_sorted_by_key(|t| t.position) {
                Cow::Borrowed(&toks[..])
            } else {
                let mut v = toks.clone();
                v.sort_by_key(|t| t.position);
                Cow::Owned(v)
            }
        });
        let from = sorted.partition_point(|t| t.position < lo);
        let to = sorted.partition_point(|t| t.position <= hi);
        sorted[from..to.max(from)].iter().any(f)
    }

    /// The place of NEAR word `w` starting at token `t`, as first and
    /// last positions: its parts in a row from `t`, or `t` itself as a
    /// whole form.
    #[inline]
    fn place(&self, w: &Operand, t: &fasttok::Tok) -> Option<(u32, u32)> {
        let field = self.field;
        if let Some((first, rest)) = w.parts.split_first() {
            if t.hash == first.hash
                && field.text(t) == &*first.text
                && rest.iter().zip(1..).all(|(x, i)| {
                    let pos = t.position + i;
                    self.any_between(pos, pos, |t| field.is(t, x.hash, &x.text))
                })
            {
                return Some((t.position, t.position + rest.len() as u32));
            }
        }
        (!w.whole.is_empty() && w.whole.iter().any(|x| field.is(t, x.hash, &x.text)))
            .then_some((t.position, t.position))
    }
}

/// Whether a token with this hash could start a place of `w`: most
/// tokens can't, and this is the whole cost for them.
#[inline]
fn starts(w: &Operand, hash: u64) -> bool {
    w.parts.first().is_some_and(|f| f.hash == hash) || w.whole.iter().any(|x| x.hash == hash)
}

/// The positions between two spans' nearest ends; 0 where they overlap.
fn gap((a0, a1): (u32, u32), (b0, b1): (u32, u32)) -> u32 {
    b0.saturating_sub(a1).max(a0.saturating_sub(b1))
}
