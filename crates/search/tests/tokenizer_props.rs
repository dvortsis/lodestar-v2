//! Tokenizer invariants on arbitrary input (the fuzz target checks the same
//! on raw bytes).

use lodestar_search::tokenizer::{MAX_TOKEN_BYTES, TokKind, tokenize, tokenize_into};
use proptest::prelude::*;

proptest! {
    #![proptest_config(ProptestConfig::with_cases(2000))]

    #[test]
    fn bounded_and_well_formed(text in "\\PC{0,80}|[A-Za-z0-9 ._+\\-\\[\\]()]{0,80}|[千と尋の한국어a-z0-9.]{0,40}") {
        let mut out = Vec::new();
        tokenize_into(&text, 0, &mut out, 64);
        prop_assert!(out.len() <= 64);
        let mut last_part = None;
        for t in &out {
            prop_assert!(!t.text.is_empty() && t.text.len() <= MAX_TOKEN_BYTES);
            prop_assert!(text.is_char_boundary(t.from) && text.is_char_boundary(t.to) && t.from < t.to);
            if t.kind == TokKind::Part {
                // Parts advance one position at a time, in order.
                if let Some(p) = last_part {
                    prop_assert!(t.position > p);
                }
                last_part = Some(t.position);
            }
        }
    }

    /// A whole word, typed in any case, is always a token of the name:
    /// either its only part or the compound of its parts.
    #[test]
    fn whole_words_are_findable(word in "[A-Za-z0-9]{1,24}") {
        let all: Vec<String> = tokenize(&word).into_iter().map(|t| t.text).collect();
        let lower = word.to_lowercase();
        prop_assert!(all.contains(&lower), "{lower} not in {all:?}");
        let typed: Vec<String> = tokenize(&lower).into_iter().map(|t| t.text).collect();
        prop_assert!(typed.contains(&lower));
    }
}
