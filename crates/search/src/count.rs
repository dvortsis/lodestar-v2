//! Result counts (architecture §8 "Counts", `docs/metrics.md` §5): exact
//! up to [`EXACT_UP_TO`] matches, a segment-level estimate above that
//! (`total_approx`), and the exact count on demand.
//!
//! Counting is apart from ranking, so its cost is bounded whatever the
//! ranking does:
//!
//! 1. Every segment is sampled: one slice in each of [`STRATA`] equal
//!    strata of its doc-id space, at a fixed pseudo-random point in it
//!    and 1/[`SHARE`] of its size, is counted, and the
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
//! error has a standard deviation of about `1 / sqrt(matches / 16)`: 0.9%
//! at 200,000 matches, the least ever reported as estimated. Matches
//! packed into a narrow doc-id range (a recent date range on an old
//! segment) can be off by up to one stratum per edge of the range: the
//! edge's stratum counts its slice ×16, whether the slice falls inside the
//! range or not. That's about 3,900 documents per edge on a 1M-document
//! segment (a simulated worst case came to about 6,500 for both), small
//! against the 200,000 matches below which a count is never estimated.
//! The search bench checks every
//! query of at least 20,000 matches against four standard deviations
//! (`bench/src/bin/search-bench.rs`).

use std::ops::Range;

use tantivy::query::{EnableScoring, Weight};
use tantivy::{COLLECT_BLOCK_BUFFER_LEN, DocId, DocSet, Searcher, SegmentReader, TERMINATED};

use crate::cancel::{self, CHECK_EVERY, CancelToken};
use crate::filter;
use crate::index::{Compiled, Result};

/// Counts up to this many matches are always exact (§8).
pub const EXACT_UP_TO: usize = 100_000;
/// Slices per sampled segment.
pub(crate) const STRATA: u32 = 256;
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
    let Total {
        value: estimate,
        approx: sampled,
    } = estimate_with(searcher, compiled, cancel, policy.sample_above)?;
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

/// Step 1 alone: the sampled sum, `approx` when any segment was sampled
/// (whatever the threshold would decide).
pub(crate) fn estimate_with(
    searcher: &Searcher,
    compiled: &Compiled,
    cancel: &CancelToken,
    sample_above: u32,
) -> Result<Total> {
    let parts = per_segment(searcher, compiled, cancel, |w, r| {
        segment_estimate(w, r, compiled, cancel, sample_above)
    })?;
    Ok(Total {
        value: parts.iter().map(|p| p.0).sum::<f64>().round() as usize,
        approx: parts.iter().any(|p| p.1),
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
pub(crate) fn per_segment<R: Send>(
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
    let Some(plan) = slices(max, sample_above) else {
        return Ok((m.count_until(TERMINATED)? as f64, false));
    };
    let mut found = 0u64;
    for slice in &plan {
        m.seek(slice.start);
        found += m.count_until(slice.end)? as u64;
    }
    Ok((found as f64 * scale(max, &plan), true))
}

/// The doc-id ranges a segment of `max` documents is sampled in, one per
/// stratum (step 1 of the module docs), or `None` when it's counted
/// whole. The same segment always gets the same slices.
pub(crate) fn slices(max: u32, sample_above: u32) -> Option<Vec<Range<DocId>>> {
    if max <= sample_above {
        return None;
    }
    let stratum = max.div_ceil(STRATA);
    let chunk = stratum.div_ceil(SHARE).max(1);
    let mut out = Vec::with_capacity(STRATA as usize);
    for s in 0..STRATA {
        let base = s * stratum;
        if base >= max {
            break;
        }
        // Each slice sits at a fixed pseudo-random point of its stratum,
        // so a pattern repeating with the doc ids can't line up with
        // every slice (aliasing); the same segment always samples alike.
        let room = stratum.saturating_sub(chunk);
        let start = base + if room == 0 { 0 } else { jitter(s) % room };
        if start >= max {
            break;
        }
        out.push(start..(start + chunk).min(max));
    }
    Some(out)
}

/// What a count over `plan`'s slices is multiplied by to stand for the
/// whole segment: the documents in it over the documents covered.
pub(crate) fn scale(max: u32, plan: &[Range<DocId>]) -> f64 {
    let covered: u64 = plan.iter().map(|r| u64::from(r.end - r.start)).sum();
    f64::from(max) / covered.max(1) as f64
}

/// A fixed scramble of a slice number (SplitMix64's finalizer).
fn jitter(s: u32) -> u32 {
    let mut z = u64::from(s).wrapping_add(0x9e37_79b9_7f4a_7c15);
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    (z ^ (z >> 31)) as u32
}

/// Walks one segment's matches, applying the deletes and the collector
/// ranges ([`filter`]), checking the token as it goes.
pub(crate) struct Matcher<'a> {
    docs: Box<dyn tantivy::query::Scorer>,
    checks: Vec<filter::Check>,
    alive: Option<&'a tantivy::fastfield::AliveBitSet>,
    cancel: &'a CancelToken,
    since_check: u32,
}

impl<'a> Matcher<'a> {
    pub(crate) fn new(
        weight: &dyn Weight,
        reader: &'a SegmentReader,
        compiled: &Compiled,
        cancel: &'a CancelToken,
    ) -> tantivy::Result<Matcher<'a>> {
        Ok(Matcher {
            docs: weight.scorer(reader, 1.0)?,
            checks: filter::checks(&compiled.filters, reader, compiled.hot.as_ref())?,
            alive: reader.alive_bitset(),
            cancel,
            since_check: 0,
        })
    }

    fn passes(&self, doc: DocId) -> bool {
        self.alive.is_none_or(|a| a.is_alive(doc)) && self.checks.iter().all(|c| c.passes(doc))
    }

    pub(crate) fn seek(&mut self, target: DocId) {
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

impl Matcher<'_> {
    /// Hands the matches from the current document up to (not including)
    /// `end` to `f`, a block of up to [`COLLECT_BLOCK_BUFFER_LEN`] at a
    /// time, and returns how many there were.
    pub(crate) fn visit_until(
        &mut self,
        end: DocId,
        f: &mut impl FnMut(&[DocId]),
    ) -> tantivy::Result<usize> {
        let mut block = [0; COLLECT_BLOCK_BUFFER_LEN];
        let mut n = 0;
        let mut total = 0;
        if end == TERMINATED {
            // The whole rest, a block at a time.
            let mut buf = [0; COLLECT_BLOCK_BUFFER_LEN];
            loop {
                let got = self.docs.fill_buffer(&mut buf);
                n = 0;
                for &d in &buf[..got] {
                    if self.passes(d) {
                        block[n] = d;
                        n += 1;
                    }
                }
                f(&block[..n]);
                total += n;
                self.tick(got as u32)?;
                if got < buf.len() {
                    return Ok(total);
                }
            }
        }
        let mut doc = self.docs.doc();
        while doc < end {
            if self.passes(doc) {
                block[n] = doc;
                n += 1;
                if n == block.len() {
                    f(&block);
                    total += n;
                    n = 0;
                }
            }
            self.tick(1)?;
            doc = self.docs.advance();
        }
        f(&block[..n]);
        Ok(total + n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{delete, index_with, slow, tripwire};
    use std::sync::atomic::Ordering;
    use std::time::Duration;

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
                n.contains("Half") && 1000 + *i as u64 > 1500 && !id.is_multiple_of(7)
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
        // Cancelled on the 1,000th of 50,000 documents, it stops within
        // one check interval per segment, whatever the clock does.
        let (index, _) = index_with(50_000, 1);
        let s = index.searcher();
        let cancel = CancelToken::new();
        let (c, visits) = tripwire(1_000, &cancel);
        let r = exact(&s, &c, &cancel);
        assert!(matches!(r, Err(crate::Error::Cancelled)), "{r:?}");
        // Segments run in parallel, each checking on its own.
        let went = visits.load(Ordering::Relaxed);
        let bound = 1_000 + s.segment_readers().len() * CHECK_EVERY as usize;
        assert!(bound < 50_000, "the bound means something: {bound}");
        assert!(went <= bound, "visited {went} of 50,000, bound {bound}");
        // A budget stops it the same way, as out of time: 50,000
        // documents at 20 µs each take a second.
        let c = slow(&index, Duration::from_micros(20));
        let budget = CancelToken::new().with_budget(Duration::from_millis(20));
        assert!(matches!(
            exact(&s, &c, &budget),
            Err(crate::Error::OutOfTime)
        ));
    }
}
