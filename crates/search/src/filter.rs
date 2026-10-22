//! Range filters checked in the collector (architecture §8).
//!
//! A fast-field range inside a Tantivy intersection is a docset that
//! scans or seeks the column for every candidate. When the query has
//! another clause to drive it, reading the candidate's value straight
//! from the column is several times cheaper, so top-level ranges are
//! lifted out of the query and checked here.
//!
//! Hot columns (§7: seeders, trackers, the DHT estimate, last seen) and
//! times of day are always checked this way: they aren't in the index,
//! so the check reads the document's torrent id and looks the value up.
//! Where such a clause has to select by itself (under `|` or `-`),
//! [`CheckQuery`] runs the same check over a whole segment.

use std::sync::Arc;

use lodestar_store::{Col, HotColumns, NEVER};
use tantivy::collector::{Collector, SegmentCollector};
use tantivy::columnar::Column;
use tantivy::query::{
    BitSetDocSet, ConstScorer, EnableScoring, Explanation, Query, Scorer, Weight,
};
use tantivy::{BitSet, DocId, Score, SegmentOrdinal, SegmentReader};

use crate::files::IdSet;

/// An inclusive range on a fast field.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum FastRange {
    U64 {
        field: String,
        lo: u64,
        hi: u64,
        /// The value of a document without one (category shares are only
        /// written when non-zero); `None` fails it.
        missing: Option<u64>,
    },
    I64 {
        field: &'static str,
        lo: i64,
        hi: i64,
    },
    /// The torrent id is one of these (sorted): a `file:(…)` clause.
    Ids { ids: Arc<IdSet> },
    /// A hot column, read by torrent id. "Never scraped" (and a search
    /// without hot columns) matches no range.
    Hot { col: Col, lo: u64, hi: u64 },
    /// Discovered at a time of day: minutes after midnight in the zone
    /// `offset` seconds east of UTC, inclusive, wrapping when `from > to`.
    TimeOfDay { from: u16, to: u16, offset: i32 },
    /// Passes what the inner check fails: a negated hot-column or
    /// time-of-day clause, checked on the candidates alone instead of
    /// scanning a segment for what to exclude.
    Not(Box<FastRange>),
    /// Passes when any inner check does: an OR of checked clauses.
    Any(Vec<FastRange>),
    /// Passes when every inner check does.
    All(Vec<FastRange>),
}

/// Wraps a collector, passing on only documents inside every range.
pub(crate) struct PostFilter<'a, C> {
    pub ranges: &'a [FastRange],
    pub hot: Option<&'a Arc<HotColumns>>,
    pub inner: C,
}

pub(crate) enum Check {
    /// No column: no document in the segment has the field.
    U64(Option<Column<u64>>, u64, u64, Option<u64>),
    I64(Column<i64>, i64, i64),
    Ids(Column<u64>, Arc<IdSet>),
    /// The segment's id column, and the hot columns (`None`: nothing
    /// passes).
    Hot(Column<u64>, Option<Arc<HotColumns>>, Col, u64, u64),
    /// The `discovered` column, the minutes and the zone offset.
    Time(Column<i64>, u16, u16, i32),
    Not(Box<Check>),
    Any(Vec<Check>),
    All(Vec<Check>),
}

/// Whether `minute` falls in `from..=to`, wrapping past midnight.
pub fn in_time(minute: u16, from: u16, to: u16) -> bool {
    if from <= to {
        from <= minute && minute <= to
    } else {
        minute >= from || minute <= to
    }
}

/// The minute of the day of unix time `t`, in the zone `offset` seconds
/// east of UTC.
pub fn minute_of_day(t: i64, offset: i32) -> u16 {
    (t.saturating_add(i64::from(offset)).rem_euclid(86_400) / 60) as u16
}

impl Check {
    pub(crate) fn passes(&self, doc: DocId) -> bool {
        match self {
            Check::U64(col, lo, hi, missing) => col
                .as_ref()
                .and_then(|c| c.first(doc))
                .or(*missing)
                .is_some_and(|v| *lo <= v && v <= *hi),
            Check::I64(col, lo, hi) => col.first(doc).is_some_and(|v| *lo <= v && v <= *hi),
            Check::Ids(col, ids) => col.first(doc).is_some_and(|id| ids.contains(id)),
            Check::Hot(ids, hot, c, lo, hi) => hot.as_ref().is_some_and(|hot| {
                ids.first(doc).is_some_and(|id| {
                    let v = hot.get(*c, id);
                    v != NEVER && *lo <= u64::from(v) && u64::from(v) <= *hi
                })
            }),
            Check::Time(col, from, to, offset) => col
                .first(doc)
                .is_some_and(|t| in_time(minute_of_day(t, *offset), *from, *to)),
            Check::Not(inner) => !inner.passes(doc),
            Check::Any(of) => of.iter().any(|c| c.passes(doc)),
            Check::All(of) => of.iter().all(|c| c.passes(doc)),
        }
    }
}

/// The column checks for `ranges` in one segment.
pub(crate) fn checks(
    ranges: &[FastRange],
    reader: &SegmentReader,
    hot: Option<&Arc<HotColumns>>,
) -> tantivy::Result<Vec<Check>> {
    ranges.iter().map(|r| check(r, reader, hot)).collect()
}

fn check(
    r: &FastRange,
    reader: &SegmentReader,
    hot: Option<&Arc<HotColumns>>,
) -> tantivy::Result<Check> {
    let fast = reader.fast_fields();
    Ok(match r {
        FastRange::U64 {
            field,
            lo,
            hi,
            missing,
        } => Check::U64(fast.column_opt(field)?, *lo, *hi, *missing),
        FastRange::I64 { field, lo, hi } => Check::I64(fast.i64(field)?, *lo, *hi),
        FastRange::Ids { ids } => Check::Ids(fast.u64("id")?, ids.clone()),
        FastRange::Hot { col, lo, hi } => Check::Hot(fast.u64("id")?, hot.cloned(), *col, *lo, *hi),
        FastRange::TimeOfDay { from, to, offset } => {
            Check::Time(fast.i64("discovered")?, *from, *to, *offset)
        }
        FastRange::Not(inner) => Check::Not(Box::new(check(inner, reader, hot)?)),
        FastRange::Any(of) => Check::Any(checks(of, reader, hot)?),
        FastRange::All(of) => Check::All(checks(of, reader, hot)?),
    })
}

pub(crate) struct PostFilterSegment<S> {
    checks: Vec<Check>,
    inner: S,
}

impl<C: Collector> Collector for PostFilter<'_, C> {
    type Fruit = C::Fruit;
    type Child = PostFilterSegment<C::Child>;

    fn for_segment(
        &self,
        ord: SegmentOrdinal,
        reader: &SegmentReader,
    ) -> tantivy::Result<Self::Child> {
        Ok(PostFilterSegment {
            checks: checks(self.ranges, reader, self.hot)?,
            inner: self.inner.for_segment(ord, reader)?,
        })
    }

    fn requires_scoring(&self) -> bool {
        self.inner.requires_scoring()
    }

    fn merge_fruits(
        &self,
        fruits: Vec<<C::Child as SegmentCollector>::Fruit>,
    ) -> tantivy::Result<C::Fruit> {
        self.inner.merge_fruits(fruits)
    }
}

impl<S: SegmentCollector> SegmentCollector for PostFilterSegment<S> {
    type Fruit = S::Fruit;

    fn collect(&mut self, doc: DocId, score: Score) {
        if self.checks.iter().all(|c| c.passes(doc)) {
            self.inner.collect(doc, score);
        }
    }

    fn harvest(self) -> S::Fruit {
        self.inner.harvest()
    }
}

/// A check as a query of its own, for a hot-column range or a time of day
/// that has to select by itself (under `|` or `-`). It reads each segment
/// once into a bitset, like the file clause's `IdsQuery`, and scores a
/// constant.
#[derive(Debug, Clone)]
pub(crate) struct CheckQuery {
    pub range: FastRange,
    pub hot: Option<Arc<HotColumns>>,
}

impl Query for CheckQuery {
    fn weight(&self, _: EnableScoring<'_>) -> tantivy::Result<Box<dyn Weight>> {
        Ok(Box::new(self.clone()))
    }
}

impl CheckQuery {
    fn matches(&self, reader: &SegmentReader) -> tantivy::Result<BitSet> {
        let check = checks(std::slice::from_ref(&self.range), reader, self.hot.as_ref())?
            .pop()
            .expect("one check");
        let mut set = BitSet::with_max_value(reader.max_doc());
        for doc in 0..reader.max_doc() {
            if check.passes(doc) {
                set.insert(doc);
            }
        }
        Ok(set)
    }
}

impl Weight for CheckQuery {
    fn scorer(&self, reader: &SegmentReader, boost: Score) -> tantivy::Result<Box<dyn Scorer>> {
        let set = self.matches(reader)?;
        Ok(Box::new(ConstScorer::new(BitSetDocSet::from(set), boost)))
    }

    fn explain(&self, reader: &SegmentReader, doc: DocId) -> tantivy::Result<Explanation> {
        if doc < reader.max_doc() && self.matches(reader)?.contains(doc) {
            Ok(Explanation::new("column check", 1.0))
        } else {
            Err(tantivy::TantivyError::InvalidArgument(format!(
                "document {doc} doesn't match"
            )))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn time_of_day_wraps_past_midnight() {
        assert!(in_time(600, 540, 1079));
        assert!(!in_time(1080, 540, 1079));
        // 22:00..02:00
        for m in [1320, 1439, 0, 120] {
            assert!(in_time(m, 1320, 120), "{m}");
        }
        for m in [121, 600, 1319] {
            assert!(!in_time(m, 1320, 120), "{m}");
        }
    }

    #[test]
    fn minutes_follow_the_zone() {
        // 2026-01-01T23:30:00Z
        let t = 1_767_310_200;
        assert_eq!(minute_of_day(t, 0), 23 * 60 + 30);
        assert_eq!(minute_of_day(t, 3600), 30, "UTC+1 is past midnight");
        assert_eq!(minute_of_day(t, -5 * 3600), 18 * 60 + 30);
        assert_eq!(minute_of_day(-1, 0), 1439);
    }
}
