//! Event-time counters for the Insights rollups (architecture §7),
//! accumulated in memory and flushed by the writer.
//!
//! Metric codes are persisted in `rollup_hour`: append only, never reuse.

use std::collections::HashMap;
use std::sync::Mutex;

use lodestar_store::{Counter, SourceId};

/// `rollup_hour.metric` codes.
pub mod metric {
    /// Keys reported by a source (dim = source, key = strategy).
    pub const HARVESTED: u32 = 1;
    /// Keys already stored, seen again (dim = source, key = strategy).
    pub const REDISCOVERED: u32 = 2;
    /// Keys dropped because the pipeline was full (dim = source).
    pub const DROPPED: u32 = 3;
    /// Torrents newly stored (dim = source that found them first).
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
