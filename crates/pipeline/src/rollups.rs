//! Event-time counters for the Insights rollups (architecture §7),
//! accumulated in memory and flushed by the writer.
//!
//! Metric codes are persisted in `rollup_hour`: append only, never reuse.

use std::collections::HashMap;
use std::sync::Mutex;

use lodestar_store::{CohortCount, Counter, SourceId};

/// `rollup_hour.metric` codes.
pub mod metric {
    use lodestar_store::Aggregation;

    /// Keys reported by a source (dim = source, key = strategy).
    pub const HARVESTED: u32 = 1;
    /// Keys already stored, seen again (dim = source, key = strategy).
    pub const REDISCOVERED: u32 = 2;
    /// Keys dropped because the pipeline was full (dim = source).
    pub const DROPPED: u32 = 3;
    /// Torrents newly stored (dim = source that found them first), at the
    /// hour of their `metadata_at` (the fetch, seconds before the commit).
    pub const STORED: u32 = 4;
    // 5–9 are taken by #48 (M4's scrape and search counters).
    /// Torrents that became searchable, by event time: the hour of the
    /// `index_watermark_hour` row that first covers them (dim = content
    /// type code, or `lodestar_store::UNCLASSIFIED`; key = 1 for imports,
    /// else 0). Written by `Store::record_searchable` (B11a).
    pub const NEWLY_SEARCHABLE: u32 = 10;
    /// First harvest → searchable, as a `lodestar_store::log_bucket`
    /// histogram of seconds. **By discovery cohort:** `hour` is the
    /// cohort, not the event (dim = 0, key = bucket). Imports aren't
    /// counted.
    pub const TIME_TO_SEARCHABLE: u32 = 11;
    /// Seconds each hour was measured, so a quiet hour reads 0 and an
    /// unmeasured one (paused, not running) a gap (key = 0; dim =
    /// [`coverage::INDEX`] or [`coverage::CRAWL`]).
    pub const COVERAGE: u32 = 12;
    /// Searchable torrents (the index's document count), a gauge: the
    /// last snapshot in each hour, every 30 s (dim = 0, key = 0). Insights'
    /// Compare reads it as the count at a period's end.
    pub const SEARCHABLE: u32 = 13;

    /// Funnel transitions by **event time**: each stage reached (dim =
    /// `lodestar_sources::Stage` code, key 0) or loss (dim = `Stage::Lost`,
    /// key = `Loss` code), at the hour the writer flushed it (within its
    /// 5 s cadence). `stages.fetch` reads dim `Stage::Fetched`. Hours
    /// before `insights_state.funnel_events_since_hour` weren't counted.
    pub const FUNNEL_EVENTS: u32 = 14;
    /// Searchable torrents deleted (a filter relabel, `delete_torrents`,
    /// retention), a lifecycle event (§10.9.3), at the hour of the delete
    /// (dim = content type, or `UNCLASSIFIED`; key 0). Written by the
    /// store's `torrents_removed` trigger (V11), which has this code as a
    /// literal.
    pub const REMOVED: u32 = 15;

    /// How each code folds into a day (compaction, repair and day
    /// reads). **Every code has a kind;** a code with none is never
    /// compacted, so a gauge can't be summed by accident. Codes 5–9 are
    /// #48's (scrape and search counters, all additive).
    pub fn aggregation(code: u32) -> Option<Aggregation> {
        AGGREGATION
            .iter()
            .find(|(c, _)| *c == code)
            .map(|&(_, a)| a)
    }

    /// The highest code defined.
    pub const LAST_CODE: u32 = REMOVED;

    const AGGREGATION: [(u32, Aggregation); 15] = [
        (HARVESTED, Aggregation::Sum),
        (REDISCOVERED, Aggregation::Sum),
        (DROPPED, Aggregation::Sum),
        (STORED, Aggregation::Sum),
        (5, Aggregation::Sum),
        (6, Aggregation::Sum),
        (7, Aggregation::Sum),
        (8, Aggregation::Sum),
        (9, Aggregation::Sum),
        (NEWLY_SEARCHABLE, Aggregation::Sum),
        (TIME_TO_SEARCHABLE, Aggregation::Sum),
        (COVERAGE, Aggregation::Sum),
        (SEARCHABLE, Aggregation::Last),
        (FUNNEL_EVENTS, Aggregation::Sum),
        (REMOVED, Aggregation::Sum),
    ];

    /// [`COVERAGE`] dims.
    pub mod coverage {
        /// Searchable time was being recorded: the server ran with
        /// search on and the crawler not paused (a paused stretch is a
        /// gap on the trend, §10.9.3).
        pub const INDEX: u32 = 0;
        /// The crawler ran, not paused.
        pub const CRAWL: u32 = 1;
    }
}

#[derive(Debug, Default)]
pub struct Rollups {
    counts: Mutex<HashMap<(i64, u32, u32, i64), i64>>,
}

impl Rollups {
    pub fn add(&self, at: i64, metric: u32, source: SourceId, key: i64, n: i64) {
        let k = (at.div_euclid(3600), metric, u32::from(source.get()), key);
        *self
            .counts
            .lock()
            .expect("rollups lock")
            .entry(k)
            .or_insert(0) += n;
    }

    pub fn is_empty(&self) -> bool {
        self.counts.lock().expect("rollups lock").is_empty()
    }

    pub fn drain(&self) -> Vec<Counter> {
        let counts = std::mem::take(&mut *self.counts.lock().expect("rollups lock"));
        let mut out: Vec<Counter> = counts
            .into_iter()
            .map(|((hour, metric, dim, key), value)| Counter {
                hour,
                metric,
                dim,
                key,
                value,
            })
            .collect();
        out.sort_by_key(|c| (c.hour, c.metric, c.dim, c.key));
        out
    }
}

/// The funnel rows a flush writes, again by event time
/// ([`metric::FUNNEL_EVENTS`] at `now`'s hour).
pub fn funnel_events(cohorts: &[CohortCount], now: i64) -> Vec<Counter> {
    let mut by: HashMap<(u32, i64), i64> = HashMap::new();
    for c in cohorts {
        *by.entry((u32::from(c.stage), i64::from(c.reason)))
            .or_default() += c.count;
    }
    let mut out: Vec<Counter> = by
        .into_iter()
        .map(|((dim, key), value)| Counter {
            hour: now.div_euclid(3600),
            metric: metric::FUNNEL_EVENTS,
            dim,
            key,
            value,
        })
        .collect();
    out.sort_by_key(|c| (c.dim, c.key));
    out
}

#[cfg(test)]
mod tests {
    use lodestar_store::Aggregation;

    use super::*;

    /// Every code from 1 to the highest has a kind; only the gauge is
    /// `Last`.
    #[test]
    fn every_code_has_an_aggregation() {
        for code in 1..=metric::LAST_CODE {
            let kind = metric::aggregation(code);
            assert!(kind.is_some(), "metric {code} has no aggregation kind");
            assert_eq!(
                kind == Some(Aggregation::Last),
                code == metric::SEARCHABLE,
                "metric {code}"
            );
        }
        assert_eq!(metric::aggregation(0), None);
        assert_eq!(metric::aggregation(metric::LAST_CODE + 1), None);
    }

    #[test]
    fn funnel_rows_become_event_time_counts() {
        let src = SourceId::new(1).unwrap();
        let row = |cohort, stage, reason, count| CohortCount {
            cohort,
            source: src,
            stage,
            reason,
            count,
        };
        let now = 500_000 * 3600 + 17;
        let got: Vec<_> =
            funnel_events(&[row(10, 3, 0, 2), row(99, 3, 0, 5), row(10, 6, 2, 1)], now)
                .into_iter()
                .map(|c| (c.hour, c.metric, c.dim, c.key, c.value))
                .collect();
        assert_eq!(
            got,
            [
                (500_000, metric::FUNNEL_EVENTS, 3, 0, 7),
                (500_000, metric::FUNNEL_EVENTS, 6, 2, 1)
            ]
        );
        assert!(funnel_events(&[], now).is_empty());
    }
}
