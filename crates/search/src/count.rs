//! Result counts (architecture §8 "Counts", `docs/metrics.md` §5): exact
//! up to [`EXACT_UP_TO`] matches, a segment-level estimate above that
//! (`total_approx`), and the exact count on demand.
//!
//! Counting is apart from ranking, so its cost is bounded whatever the
//! ranking does:
//!
//! 1. Every segment is sampled: [`STRATA`] evenly spaced slices of its
//!    doc-id space, each 1/[`SHARE`] of its stratum, are counted, and the
//!    segment's count is the sample's scaled by the slots it covered.
//!    Segments of at most [`SAMPLE_ABOVE`] documents are counted whole.
//!    Doc ids follow discovery order, so slices spread over the whole
//!    space keep time-correlated queries (`discovered:<7d`) honest, which
//!    a prefix sample wouldn't.
//! 2. When nothing was sampled, that sum is exact. When it's at most twice
//!    [`EXACT_UP_TO`], the count could be under the threshold, so it's
//!    counted exactly (at most a few hundred thousand matches, without
//!    scoring). Otherwise the estimate stands, marked approximate.
//!
//! **Tolerance:** the sample is 1/16 of every large segment. For a query
//! whose matches spread over the doc-id space, the estimate's relative
//! error is about `1 / sqrt(matches / 16)`: under 1% above 200,000
//! matches. Matches packed into a narrow doc-id range (a recent date
//! range on an old segment) can be off by up to one slice per edge of
//! the range, which on a 1M-document segment is about 1,000 documents.
//! The search bench gates the estimate at 5% on every class it estimates
//! (`bench/src/bin/search-bench.rs`).

use tantivy::query::{EnableScoring, Weight};
use tantivy::{COLLECT_BLOCK_BUFFER_LEN, DocId, DocSet, Searcher, SegmentReader, TERMINATED};

use crate::cancel::{self, CHECK_EVERY, CancelToken};
use crate::filter;
use crate::index::{Compiled, Result};

/// Counts up to this many matches are always exact (§8).
pub const EXACT_UP_TO: usize = 100_000;
/// Slices per sampled segment.
pub(crate) const STRATA: u32 = 64;
/// Each slice covers 1/`SHARE` of its stratum.
pub(crate) const SHARE: u32 = 16;
/// Segments up to this size are counted whole rather than sampled.
pub(crate) const SAMPLE_ABOVE: u32 = 64 * 1024;

/// A result count.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Total {
    pub value: usize,
    /// An estimate (shown as "≈", `docs/metrics.md` §1) rather than an
    /// exact count.
    pub approx: bool,
}

/// How [`total`] decides; tests use a lower threshold and smaller
/// segments than the real ones.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Policy {
    pub exact_up_to: usize,
    pub sample_above: u32,
}

impl Default for Policy {
    fn default() -> Self {
        Policy {
            exact_up_to: EXACT_UP_TO,
            sample_above: SAMPLE_ABOVE,
        }
    }
}

/// The count to show for `compiled` in `searcher`: exact up to the
/// threshold, estimated above it.
pub(crate) fn total(
    searcher: &Searcher,
    compiled: &Compiled,
    cancel: &CancelToken,
) -> Result<Total> {
    total_with(searcher, compiled, cancel, Policy::default())
}

pub(crate) fn total_with(
    searcher: &Searcher,
    compiled: &Compiled,
    cancel: &CancelToken,
    policy: Policy,
) -> Result<Total> {
    let parts = per_segment(searcher, compiled, cancel, |w, r| {
        segment_estimate(w, r, compiled, cancel, policy.sample_above)
    })?;
    let sampled = parts.iter().any(|p| p.1);
    let estimate = parts.iter().map(|p| p.0).sum::<f64>().round() as usize;
    if !sampled {
        return Ok(Total {
            value: estimate,
            approx: false,
        });
    }
    if estimate <= policy.exact_up_to.saturating_mul(2) {
        return Ok(Total {
            value: exact(searcher, compiled, cancel)?,
            approx: false,
        });
    }
    Ok(Total {
        value: estimate,
        approx: true,
    })
}

/// Every match counted, segments in parallel. Stops with the token's
/// error once it does (a budget, for the count on demand).
pub(crate) fn exact(
    searcher: &Searcher,
    compiled: &Compiled,
    cancel: &CancelToken,
) -> Result<usize> {
    let counts = per_segment(searcher, compiled, cancel, |w, r| {
        let mut m = Matcher::new(w, r, compiled, cancel)?;
        m.count_until(TERMINATED)
    })?;
    Ok(counts.into_iter().sum())
}

/// Runs `f` on every segment through the index's executor.
fn per_segment<R: Send>(
    searcher: &Searcher,
    compiled: &Compiled,
    cancel: &CancelToken,
    f: impl Fn(&dyn Weight, &SegmentReader) -> tantivy::Result<R> + Sync,
) -> Result<Vec<R>> {
    cancel.check()?;
    let weight = compiled
        .query
        .weight(EnableScoring::disabled_from_searcher(searcher))?;
    let run = searcher.index().search_executor().map(
        |reader: &SegmentReader| {
            if cancel.is_stopped() {
                return Err(cancel::interrupted());
            }
            f(weight.as_ref(), reader)
        },
        searcher.segment_readers().iter(),
    );
    run.map_err(|e| cancel::stopped_or(cancel, e))
}

/// One segment's count, and whether it was sampled.
fn segment_estimate(
    weight: &dyn Weight,
    reader: &SegmentReader,
    compiled: &Compiled,
    cancel: &CancelToken,
    sample_above: u32,
) -> tantivy::Result<(f64, bool)> {
    let max = reader.max_doc();
    let mut m = Matcher::new(weight, reader, compiled, cancel)?;
    if max <= sample_above {
        return Ok((m.count_until(TERMINATED)? as f64, false));
    }
    let stratum = max.div_ceil(STRATA);
    let chunk = stratum.div_ceil(SHARE).max(1);
    let (mut found, mut covered) = (0u64, 0u64);
    for s in 0..STRATA {
        let start = s * stratum;
        if start >= max {
            break;
        }
        let end = (start + chunk).min(max);
        m.seek(start);
        found += m.count_until(end)? as u64;
        covered += u64::from(end - start);
    }
    Ok((found as f64 * f64::from(max) / covered as f64, true))
}

/// Walks one segment's matches, applying the deletes and the collector
/// ranges ([`filter`]), checking the token as it goes.
struct Matcher<'a> {
    docs: Box<dyn tantivy::query::Scorer>,
    checks: Vec<filter::Check>,
    alive: Option<&'a tantivy::fastfield::AliveBitSet>,
    cancel: &'a CancelToken,
    since_check: u32,
}

impl<'a> Matcher<'a> {
    fn new(
        weight: &dyn Weight,
        reader: &'a SegmentReader,
        compiled: &Compiled,
        cancel: &'a CancelToken,
    ) -> tantivy::Result<Matcher<'a>> {
        Ok(Matcher {
            docs: weight.scorer(reader, 1.0)?,
            checks: filter::checks(&compiled.filters, reader)?,
            alive: reader.alive_bitset(),
            cancel,
            since_check: 0,
        })
    }

    fn passes(&self, doc: DocId) -> bool {
        self.alive.is_none_or(|a| a.is_alive(doc)) && self.checks.iter().all(|c| c.passes(doc))
    }

    fn seek(&mut self, target: DocId) {
        if self.docs.doc() < target {
            self.docs.seek(target);
        }
    }

    fn tick(&mut self, n: u32) -> tantivy::Result<()> {
        self.since_check += n;
        if self.since_check >= CHECK_EVERY {
            self.since_check = 0;
            if self.cancel.is_stopped() {
                return Err(cancel::interrupted());
            }
        }
        Ok(())
    }

    /// Matches from the current document up to (not including) `end`.
    fn count_until(&mut self, end: DocId) -> tantivy::Result<usize> {
        let mut n = 0;
        if end == TERMINATED {
            // The whole rest, a block at a time.
            let mut buf = [0; COLLECT_BLOCK_BUFFER_LEN];
            loop {
                let got = self.docs.fill_buffer(&mut buf);
                n += buf[..got].iter().filter(|&&d| self.passes(d)).count();
                self.tick(got as u32)?;
                if got < buf.len() {
                    return Ok(n);
                }
            }
        }
        let mut doc = self.docs.doc();
        while doc < end {
            if self.passes(doc) {
                n += 1;
            }
            self.tick(1)?;
            doc = self.docs.advance();
        }
        Ok(n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{delete, index_with, slow};
    use std::time::{Duration, Instant};

    const NOW: i64 = 1_700_000_100;

    fn compiled(index: &crate::SearchIndex, q: &str) -> Compiled {
        index.query(&crate::parse(q).unwrap(), NOW)
    }

    /// Exact counts by brute force over the names.
    fn truth(names: &[String], pred: impl Fn(&str) -> bool) -> usize {
        names.iter().filter(|n| pred(n)).count()
    }

    #[test]
    fn small_counts_are_exact_and_large_ones_estimated_within_tolerance() {
        // 40,000 torrents in one segment: sampled with a 2,048-document
        // threshold, and "large" above 1,000 matches.
        let (index, names) = index_with(40_000, 1);
        let s = index.searcher();
        let policy = Policy {
            exact_up_to: 1_000,
            sample_above: 2_048,
        };
        let cancel = CancelToken::new();
        // `rare` is in 1 name of 100: 400 matches, under twice the
        // threshold, so it's counted exactly.
        let rare = total_with(&s, &compiled(&index, "rare"), &cancel, policy).unwrap();
        assert_eq!(
            rare,
            Total {
                value: truth(&names, |n| n.contains("Rare")),
                approx: false
            }
        );
        for q in ["", "common", "half", "-half", "recent"] {
            let c = compiled(&index, q);
            let t = total_with(&s, &c, &cancel, policy).unwrap();
            let want = exact(&s, &c, &cancel).unwrap();
            assert!(t.approx, "{q}: {t:?}");
            let err = (t.value as f64 - want as f64).abs() / want as f64;
            assert!(
                err < 0.05,
                "{q}: estimate {} vs exact {want} ({err:.3})",
                t.value
            );
        }
        // The exact count is the true one.
        let c = compiled(&index, "half");
        assert_eq!(
            exact(&s, &c, &cancel).unwrap(),
            truth(&names, |n| n.contains("Half"))
        );
        // Below the sampling size everything is exact, however many match.
        let whole = Policy {
            exact_up_to: 1_000,
            sample_above: u32::MAX,
        };
        let all = total_with(&s, &compiled(&index, ""), &cancel, whole).unwrap();
        assert_eq!(
            all,
            Total {
                value: 40_000,
                approx: false
            }
        );
    }

    #[test]
    fn deletes_and_collector_ranges_count() {
        let (index, names) = index_with(5_000, 3);
        // Every seventh torrent deleted.
        delete(&index, (1..=5_000).filter(|id| id % 7 == 0));
        let s = index.searcher();
        let cancel = CancelToken::new();
        // A range lifted into the collector (size).
        let c = compiled(&index, "half size:>1500");
        let want = names
            .iter()
            .enumerate()
            .filter(|(i, n)| {
                let id = *i as u64 + 1;
                n.contains("Half") && 1000 + *i as u64 > 1500 && id % 7 != 0
            })
            .count();
        assert_eq!(exact(&s, &c, &cancel).unwrap(), want);
        assert_eq!(
            total(&s, &c, &cancel).unwrap(),
            Total {
                value: want,
                approx: false
            }
        );
    }

    #[test]
    fn a_cancelled_count_stops_early() {
        // Every document takes 20 µs to visit: a full count of 50,000
        // takes a second.
        let (index, _) = index_with(50_000, 1);
        let s = index.searcher();
        let c = slow(&index, Duration::from_micros(20));
        let cancel = CancelToken::new();
        let stopper = {
            let cancel = cancel.clone();
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(30));
                cancel.cancel();
            })
        };
        let start = Instant::now();
        let r = exact(&s, &c, &cancel);
        let took = start.elapsed();
        stopper.join().unwrap();
        assert!(matches!(r, Err(crate::Error::Cancelled)), "{r:?}");
        // At most one check interval (4,096 × 20 µs ≈ 82 ms) past the
        // cancel, far short of the full second.
        assert!(took < Duration::from_millis(400), "took {took:?}");
        // A budget stops it the same way, as out of time.
        let budget = CancelToken::new().with_budget(Duration::from_millis(20));
        assert!(matches!(
            exact(&s, &c, &budget),
            Err(crate::Error::OutOfTime)
        ));
    }
}
