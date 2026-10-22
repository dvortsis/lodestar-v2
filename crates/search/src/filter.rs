//! Range filters checked in the collector (architecture §8).
//!
//! A fast-field range inside a Tantivy intersection is a docset that
//! scans or seeks the column for every candidate. When the query has
//! another clause to drive it, reading the candidate's value straight
//! from the column is several times cheaper, so top-level ranges are
//! lifted out of the query and checked here.

use tantivy::collector::{Collector, SegmentCollector};
use tantivy::columnar::Column;
use tantivy::{DocId, Score, SegmentOrdinal, SegmentReader};

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
}

/// Wraps a collector, passing on only documents inside every range.
pub(crate) struct PostFilter<'a, C> {
    pub ranges: &'a [FastRange],
    pub inner: C,
}

pub(crate) enum Check {
    /// No column: no document in the segment has the field.
    U64(Option<Column<u64>>, u64, u64, Option<u64>),
    I64(Column<i64>, i64, i64),
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
        }
    }
}

/// The column checks for `ranges` in one segment.
pub(crate) fn checks(ranges: &[FastRange], reader: &SegmentReader) -> tantivy::Result<Vec<Check>> {
    let fast = reader.fast_fields();
    ranges
        .iter()
        .map(|r| {
            Ok(match r {
                FastRange::U64 {
                    field,
                    lo,
                    hi,
                    missing,
                } => Check::U64(fast.column_opt(field)?, *lo, *hi, *missing),
                FastRange::I64 { field, lo, hi } => Check::I64(fast.i64(field)?, *lo, *hi),
            })
        })
        .collect()
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
            checks: checks(self.ranges, reader)?,
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
