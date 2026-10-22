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
    /// Scrape jobs sent to a tracker (dim = tracker id, key = outcome:
    /// ok, timeout, banned, error, skipped).
    pub const SCRAPE_REQUESTS: u32 = 5;
    /// Torrents scraped (dim = lane: interest, new, rediscovered, due;
    /// key = result: alive, dead, unreached).
    pub const SCRAPED: u32 = 6;
    /// DHT scrapes (key = 0 with an estimate, 1 without).
    pub const DHT_SCRAPES: u32 = 7;
    /// Searches served (key = 0 with results, 1 with none, 2 failed).
    pub const SEARCHES: u32 = 8;
    /// Search latency as a log histogram (key = [`latency_bucket`]).
    pub const SEARCH_LATENCY: u32 = 9;

    /// Bucket 0 is under 1 ms; bucket `b` is `[2^(b-1), 2^b)` ms, up to
    /// bucket 16 (about a minute and over).
    pub fn latency_bucket(took: std::time::Duration) -> i64 {
        let ms = took.as_millis();
        if ms == 0 {
            0
        } else {
            i64::from(128 - ms.leading_zeros()).min(16)
        }
    }
}

#[derive(Debug, Default)]
pub struct Rollups {
    counts: Mutex<HashMap<(i64, u32, u32, i64), i64>>,
}

impl Rollups {
    pub fn add(&self, at: i64, metric: u32, source: SourceId, key: i64, n: i64) {
        self.add_dim(at, metric, u32::from(source.get()), key, n);
    }

    /// Like [`Rollups::add`], for metrics whose dimension isn't a source.
    pub fn add_dim(&self, at: i64, metric: u32, dim: u32, key: i64, n: i64) {
        let k = (at.div_euclid(3600), metric, dim, key);
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
