//! A Levenshtein automaton over bytes, for "did you mean" terms from the
//! term dictionaries (§8). Words reach it folded to ASCII, where bytes
//! are characters.

use tantivy_fst::Automaton;

/// Matches terms within `distance` edits of `word`.
#[derive(Debug, Clone)]
pub(crate) struct Lev<'a> {
    word: &'a [u8],
    distance: u8,
}

impl<'a> Lev<'a> {
    pub(crate) fn new(word: &'a str, distance: u8) -> Lev<'a> {
        Lev {
            word: word.as_bytes(),
            distance,
        }
    }
}

impl Automaton for Lev<'_> {
    /// The last row of the edit-distance table, or `None` once no
    /// continuation can come within the distance.
    type State = Option<Vec<u8>>;

    fn start(&self) -> Self::State {
        Some(
            (0..=self.word.len())
                .map(|i| i.min(usize::from(u8::MAX)) as u8)
                .collect(),
        )
    }

    fn is_match(&self, state: &Self::State) -> bool {
        state
            .as_ref()
            .and_then(|row| row.last())
            .is_some_and(|d| *d <= self.distance)
    }

    fn can_match(&self, state: &Self::State) -> bool {
        state.is_some()
    }

    fn accept(&self, state: &Self::State, byte: u8) -> Self::State {
        let row = state.as_ref()?;
        let mut next = Vec::with_capacity(row.len());
        next.push(row[0].saturating_add(1));
        for (i, c) in self.word.iter().enumerate() {
            let cost = u8::from(*c != byte);
            let d = row[i]
                .saturating_add(cost)
                .min(row[i + 1].saturating_add(1))
                .min(next[i].saturating_add(1));
            next.push(d);
        }
        next.iter()
            .min()
            .is_some_and(|d| *d <= self.distance)
            .then_some(next)
    }
}

/// The edits allowed for a word of `len` letters, as for `~fuzzy` terms
/// (§8): 1 under 6 letters, else 2; none under 3.
pub(crate) fn allowed(len: usize) -> Option<u8> {
    match len {
        0..3 => None,
        3..6 => Some(1),
        _ => Some(2),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::query::edit_distance;

    fn matches(word: &str, d: u8, term: &str) -> bool {
        let a = Lev::new(word, d);
        let mut s = a.start();
        for b in term.bytes() {
            s = a.accept(&s, b);
        }
        a.is_match(&s)
    }

    #[test]
    fn agrees_with_edit_distance() {
        let words = ["knight", "dark", "matrix", "abc", "a", ""];
        let terms = [
            "knigth", "night", "knights", "dart", "drak", "matrx", "mtrix", "abd", "xyz", "", "a",
            "kn",
        ];
        for w in words {
            for t in terms {
                for d in 0..=2u8 {
                    assert_eq!(
                        matches(w, d, t),
                        edit_distance(w, t) <= usize::from(d),
                        "{w} {t} {d}"
                    );
                }
            }
        }
    }

    #[test]
    fn edits_by_length() {
        assert_eq!(allowed(2), None);
        assert_eq!(allowed(3), Some(1));
        assert_eq!(allowed(5), Some(1));
        assert_eq!(allowed(6), Some(2));
    }
}
