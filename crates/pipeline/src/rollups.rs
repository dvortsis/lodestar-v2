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
