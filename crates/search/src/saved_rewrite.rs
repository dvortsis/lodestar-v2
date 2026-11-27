//! Rewrites a stored `verified:yes,no` to `verified:yes,imported,no`
//! (X-saved-verified-rewrite, `docs/decisions/2026-11-27-X-saved-verified-rewrite.md`).
//!
//! Before X-import-verified-no, "Show unverified" sent `verified:yes,no`,
//! and imports matched it because they were indexed `yes`. Imports are
//! now `imported`, so a saved search or alert holding that list would
//! leave them out without notice. The list meant "verified and
//! unverified", so it becomes all three values.
//!
//! Only a `verified:` node whose values are exactly {yes, no}, in any order
//! and at any depth, is rewritten, under a negation too: `-verified:yes,no`
//! matched nothing and still does (runner, 2026-11-27). A lone
//! `verified:yes` was only ever typed (the UI never wrote it) and now means
//! "hashed here", as intended; `verified:no`, `-verified:no` and a list
//! that already has `imported` keep their meaning.

use crate::query::{Node, Verified};

/// Rewrites every `verified:yes,no` in `node`, under a `Not` too;
/// whether any was. The caller stores the result's [`Node::to_query`]
/// beside its AST (the search crate doesn't read JSON; the app does).
pub fn rewrite_verified_yes_no(node: &mut Node) -> bool {
    match node {
        Node::Verified { any } if is_yes_no(any) => {
            *any = vec![Verified::Yes, Verified::Imported, Verified::No];
            true
        }
        Node::And { of } | Node::Or { of } => {
            // Every child, not just up to the first rewritten one.
            of.iter_mut()
                .fold(false, |done, n| rewrite_verified_yes_no(n) | done)
        }
        Node::Not { of } | Node::File { of } => rewrite_verified_yes_no(of),
        _ => false,
    }
}

/// Exactly {yes, no}, repeats allowed.
fn is_yes_no(any: &[Verified]) -> bool {
    any.contains(&Verified::Yes)
        && any.contains(&Verified::No)
        && any
            .iter()
            .all(|v| matches!(v, Verified::Yes | Verified::No))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse;

    /// The rewritten query's canonical text, which reads back as the
    /// rewritten AST.
    fn rewritten(q: &str) -> Option<String> {
        let mut node = parse(q).unwrap();
        rewrite_verified_yes_no(&mut node).then(|| {
            let text = node.to_query();
            assert_eq!(parse(&text).unwrap(), node);
            text
        })
    }

    #[test]
    fn yes_and_no_in_any_order_at_any_depth_become_all_three() {
        let all = "verified:yes,imported,no";
        assert_eq!(rewritten("verified:yes,no").as_deref(), Some(all));
        assert_eq!(rewritten("verified:no,yes").as_deref(), Some(all));
        assert_eq!(rewritten("verified:yes,no,yes").as_deref(), Some(all));
        assert_eq!(
            rewritten("movie verified:yes,no").unwrap(),
            parse(&format!("movie {all}")).unwrap().to_query()
        );
        assert_eq!(
            rewritten("movie (show | verified:no,yes) verified:yes,no").unwrap(),
            parse(&format!("movie (show | {all}) {all}"))
                .unwrap()
                .to_query()
        );
    }

    #[test]
    fn other_lists_negations_and_queries_without_one_are_left_alone() {
        for q in [
            "verified:yes",
            "verified:no",
            "verified:imported",
            "verified:yes,imported",
            "verified:yes,imported,no",
            "verified:no,imported",
            "-verified:no",
            "movie size:>1GiB",
            "",
        ] {
            assert_eq!(rewritten(q), None, "{q}");
        }
    }

    #[test]
    fn a_negated_yes_no_is_rewritten_so_it_still_matches_nothing() {
        let all = "verified:yes,imported,no";
        assert_eq!(rewritten("-verified:yes,no").unwrap(), format!("-{all}"));
        assert_eq!(
            rewritten("movie -(show verified:no,yes)").unwrap(),
            parse(&format!("movie -(show {all})")).unwrap().to_query()
        );
    }

    #[test]
    fn the_rest_of_the_query_is_kept() {
        let text = rewritten("movie res:2160p size:>1GiB -cam verified:yes,no").unwrap();
        let mut expected = parse("movie res:2160p size:>1GiB -cam verified:yes,no").unwrap();
        let Node::And { of } = &mut expected else {
            panic!("an and");
        };
        *of.last_mut().unwrap() = Node::Verified {
            any: vec![Verified::Yes, Verified::Imported, Verified::No],
        };
        assert_eq!(parse(&text).unwrap(), expected);
    }
}
