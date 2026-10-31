//! Zero-result recovery (architecture §10.4 "No results"): what a query
//! would match with one of its top-level clauses removed, counted on the
//! snapshot the empty answer came from.
//!
//! The cost is bounded three ways: at most [`MAX_CLAUSES`] clauses, one
//! deadline shared by every count, and the same sampled estimate a total
//! uses (`count.rs`). A count the deadline cut off before any estimate
//! is `None`, never 0; one cut off during its exact pass keeps the
//! estimate, marked approximate.

use std::time::Duration;

use crate::cancel::CancelToken;
use crate::compile::has_stem_alternative;
use crate::count::{self, Policy, Total};
use crate::index::{Error, Result, STEM_FALLBACK_BELOW};
use crate::lease::Lease;
use crate::query::Node;

/// Clauses counted at most. The active-filter strip rarely holds more,
/// and a count of a narrowed query takes 1–3 ms, so eight fit in
/// [`RECOVERY_BUDGET`] with room for the search itself.
pub const MAX_CLAUSES: usize = 8;

/// The deadline every count of one response shares, inside §3's 30 ms
/// for a search.
pub const RECOVERY_BUDGET: Duration = Duration::from_millis(20);

/// The top-level clauses of `node` (the `of` of a root `And`, else the
/// root alone; `All` has none).
pub fn top_level(node: &Node) -> &[Node] {
    match node {
        Node::All => &[],
        Node::And { of } => of,
        n => std::slice::from_ref(n),
    }
}

/// `node` with top-level clause `index` removed: one left is unwrapped,
/// none is `All`. The UI's `rootWithout` does the same, so a clause's
/// index removes the clause that was counted.
pub fn without_clause(node: &Node, index: usize) -> Node {
    let mut kept: Vec<Node> = top_level(node)
        .iter()
        .enumerate()
        .filter(|(i, _)| *i != index)
        .map(|(_, n)| n.clone())
        .collect();
    match kept.len() {
        0 => Node::All,
        1 => kept.pop().expect("one"),
        _ => Node::And { of: kept },
    }
}

/// The removals worth counting: every top-level clause of a query that
/// has at least two (removing a query's only clause browses everything),
/// the first [`MAX_CLAUSES`] of them. Each is the clause's index and the
/// query without it.
pub fn removals(node: &Node) -> Vec<(usize, Node)> {
    let clauses = top_level(node);
    if clauses.len() < 2 {
        return Vec::new();
    }
    (0..clauses.len().min(MAX_CLAUSES))
        .map(|i| (i, without_clause(node, i)))
        .collect()
}

/// The total of each of `queries` on `lease`'s snapshot (at its `now` and
/// time zone, with the same stem fallback a search takes), all within
/// one `budget`. `None` for every query the deadline reached first.
/// Callers pass queries already restricted to what the user may see.
pub fn totals_on(
    lease: &Lease,
    queries: &[Node],
    cancel: &CancelToken,
    budget: Duration,
) -> Result<Vec<Option<Total>>> {
    totals_with(lease, queries, cancel, budget, Policy::default())
}

pub(crate) fn totals_with(
    lease: &Lease,
    queries: &[Node],
    cancel: &CancelToken,
    budget: Duration,
    policy: Policy,
) -> Result<Vec<Option<Total>>> {
    let deadline = cancel.with_budget(budget);
    let mut out = Vec::with_capacity(queries.len());
    for q in queries {
        match total(lease, q, &deadline, policy) {
            Ok(t) => out.push(Some(t)),
            // Out of time: this one and every later one are absent.
            Err(Error::OutOfTime) => break,
            Err(e) => return Err(e),
        }
    }
    out.resize(queries.len(), None);
    Ok(out)
}

/// One query's total, as a lease taken for it would count it.
fn total(lease: &Lease, node: &Node, deadline: &CancelToken, policy: Policy) -> Result<Total> {
    let index = lease.index();
    let compile =
        |stems| index.compile_cancelled(node, lease.now(), lease.utc_offset(), stems, deadline);
    let mut best = count_within(lease, &compile(false)?, deadline, policy)?;
    if best.value < STEM_FALLBACK_BELOW && has_stem_alternative(node) {
        let wider = count_within(lease, &compile(true)?, deadline, policy)?;
        if wider.value > best.value {
            best = wider;
        }
    }
    Ok(best)
}

/// [`count::total_with`], except that an exact pass the deadline cuts
/// short keeps its estimate (approximate) instead of failing.
fn count_within(
    lease: &Lease,
    compiled: &crate::index::Compiled,
    deadline: &CancelToken,
    policy: Policy,
) -> Result<Total> {
    let searcher = lease.searcher();
    let estimate = count::estimate_with(searcher, compiled, deadline, policy.sample_above)?;
    if !estimate.approx {
        return Ok(estimate);
    }
    if estimate.value > policy.exact_up_to.saturating_mul(2) {
        return Ok(estimate);
    }
    match count::exact(searcher, compiled, deadline) {
        Ok(value) => Ok(Total {
            value,
            approx: false,
        }),
        Err(Error::OutOfTime) => Ok(estimate),
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::index::Sort;
    use crate::parse;
    use crate::testing::index_with;

    const NOW: i64 = 1_700_000_100;

    fn lease(index: &Arc<crate::SearchIndex>, q: &str) -> Lease {
        Lease::new(
            index.clone(),
            &parse(q).unwrap(),
            NOW,
            Sort::Relevance,
            &CancelToken::new(),
        )
        .unwrap()
    }

    fn queries(q: &str) -> Vec<Node> {
        removals(&parse(q).unwrap())
            .into_iter()
            .map(|(_, n)| n)
            .collect()
    }

    #[test]
    fn a_clause_index_removes_that_clause() {
        let ast = parse("rare common size:>1500 -half").unwrap();
        let got = removals(&ast);
        assert_eq!(got.len(), 4);
        for (i, without) in &got {
            let mut want = top_level(&ast).to_vec();
            want.remove(*i);
            assert_eq!(top_level(without), &want[..], "clause {i}");
            // The text round-trips to the same query.
            assert_eq!(&parse(&without.to_query()).unwrap(), without);
        }
        // Two clauses: the one left is unwrapped.
        let ast = parse("rare common").unwrap();
        assert_eq!(without_clause(&ast, 0), parse("common").unwrap());
        // One clause, or none: nothing to remove.
        assert!(removals(&parse("rare").unwrap()).is_empty());
        assert!(removals(&Node::All).is_empty());
        // An `or` at the root is one clause.
        assert!(removals(&parse("rare OR common").unwrap()).is_empty());
    }

    #[test]
    fn at_most_eight_clauses_are_counted() {
        let q: Vec<String> = (0..12).map(|i| format!("w{i}")).collect();
        let got = removals(&parse(&q.join(" ")).unwrap());
        assert_eq!(got.len(), MAX_CLAUSES);
        assert_eq!(got.last().unwrap().0, MAX_CLAUSES - 1);
    }

    #[test]
    fn each_count_is_the_total_of_the_query_without_its_clause() {
        let (index, names) = index_with(2_000, 3);
        let index = Arc::new(index);
        let q = "rare nosuchword size:>1500";
        let l = lease(&index, q);
        assert_eq!(l.total().value, 0);
        let got = totals_on(
            &l,
            &queries(q),
            &CancelToken::new(),
            Duration::from_secs(10),
        )
        .unwrap();
        let count = |f: &dyn Fn(usize, &str) -> bool| {
            names.iter().enumerate().filter(|(i, n)| f(*i, n)).count()
        };
        let rare = |n: &str| n.contains("Rare");
        let big = |i: usize| 1000 + i as u64 > 1500;
        let want = [
            0,                                // without `rare`
            count(&|i, n| rare(n) && big(i)), // without `nosuchword`
            0,                                // without the size
        ];
        for (i, w) in want.iter().enumerate() {
            assert_eq!(
                got[i],
                Some(Total {
                    value: *w,
                    approx: false
                }),
                "clause {i}"
            );
        }
        assert!(want[1] > 0);
    }

    #[test]
    fn a_count_the_deadline_missed_is_absent_not_zero() {
        let (index, _) = index_with(2_000, 2);
        let index = Arc::new(index);
        let q = "rare nosuchword";
        let l = lease(&index, q);
        let got = totals_on(&l, &queries(q), &CancelToken::new(), Duration::ZERO).unwrap();
        assert_eq!(got, vec![None, None]);
        // A cancelled client is an error, not an absent count.
        let cancel = CancelToken::new();
        cancel.cancel();
        let r = totals_on(&l, &queries(q), &cancel, Duration::from_secs(10));
        assert!(matches!(r, Err(Error::Cancelled)), "{r:?}");
    }

    #[test]
    fn a_sampled_count_is_approximate() {
        // 40,000 torrents in one segment, sampled above 2,048 documents,
        // and "large" above 1,000 matches: `common` (36,000) is estimated.
        let (index, names) = index_with(40_000, 1);
        let index = Arc::new(index);
        let policy = Policy {
            exact_up_to: 1_000,
            sample_above: 2_048,
        };
        let q = "common nosuchword";
        let l = lease(&index, q);
        let got = totals_with(
            &l,
            &queries(q),
            &CancelToken::new(),
            Duration::from_secs(10),
            policy,
        )
        .unwrap();
        // Without `common`, nothing; without `nosuchword`, an estimate.
        assert_eq!(
            got[0],
            Some(Total {
                value: 0,
                approx: false
            })
        );
        let est = got[1].expect("counted");
        assert!(est.approx);
        let truth = names.iter().filter(|n| n.contains("Common")).count();
        let err = (est.value as f64 - truth as f64).abs() / truth as f64;
        assert!(err < 0.05, "{} against {truth}", est.value);
    }
}
