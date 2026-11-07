//! Event-time counters for the Insights rollups (architecture §7),
//! accumulated in memory and flushed by the writer.
//!
//! Metric codes are persisted in `rollup_hour`: append only, never reuse.

use std::collections::HashMap;
use std::sync::Mutex;

use lodestar_cluster::WorkerId;
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
    /// Scrape jobs sent to a tracker (dim = tracker id, key = outcome:
    /// ok, timeout, banned, error, skipped, late).
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
    /// Late results committed (plan Q7, WK-9), **by discovery cohort**:
    /// `hour` is the cohort (dim = the source that found it first, key 0).
    /// Each is a key whose first lease ended with a terminal outcome and
    /// whose late result then ended a second lease, so the funnel
    /// reconciles as harvested = Σ terminal − late results + in flight.
    pub const LATE_RESULTS: u32 = 16;
    /// Counters remote workers sent, by worker (WK-9b, §19.7): dim = the
    /// worker id (from its authenticated session, never its payload), key
    /// = [`by_worker_key`] of the counter's own metric, dim and key. The
    /// same counts are in their own metric's row too, so the fleet's
    /// totals read as before; this row only breaks them down.
    pub const BY_WORKER: u32 = 17;

    /// A [`BY_WORKER`] key: the counter's metric, dim and key, each in
    /// `0..=255` ([`super::Admit`] admits nothing else).
    pub const fn by_worker_key(metric: u32, dim: u32, key: i64) -> i64 {
        ((metric as i64) << 16) | ((dim as i64) << 8) | key
    }

    /// The metrics a remote worker may send, with their dim and key
    /// domains (inclusive). Everything else is the coordinator's own:
    /// harvests are counted at grant and stores at commit (§19.3), and
    /// search, scrape scheduling, coverage, lifecycle, late results and
    /// the per-worker rows themselves are coordinator work. Strategy codes
    /// are a plugin's own `u8`.
    pub const FROM_WORKERS: [(u32, (u32, u32), (i64, i64)); 4] = [
        // dim = source, key = strategy.
        (REDISCOVERED, (0, 63), (0, 255)),
        // dim = source, key 0.
        (DROPPED, (0, 63), (0, 0)),
        // dim = lane, key = result.
        (SCRAPED, (0, 3), (0, 2)),
        // dim 0, key = with or without an estimate.
        (DHT_SCRAPES, (0, 0), (0, 1)),
    ];

    /// How each code folds into a day (compaction, repair and day
    /// reads). **Every code has a kind;** a code with none is never
    /// compacted, so a gauge can't be summed by accident.
    pub fn aggregation(code: u32) -> Option<Aggregation> {
        AGGREGATION
            .iter()
            .find(|(c, _)| *c == code)
            .map(|&(_, a)| a)
    }

    /// The highest code defined.
    pub const LAST_CODE: u32 = BY_WORKER;

    const AGGREGATION: [(u32, Aggregation); 17] = [
        (HARVESTED, Aggregation::Sum),
        (REDISCOVERED, Aggregation::Sum),
        (DROPPED, Aggregation::Sum),
        (STORED, Aggregation::Sum),
        (SCRAPE_REQUESTS, Aggregation::Sum),
        (SCRAPED, Aggregation::Sum),
        (DHT_SCRAPES, Aggregation::Sum),
        (SEARCHES, Aggregation::Sum),
        (SEARCH_LATENCY, Aggregation::Sum),
        (NEWLY_SEARCHABLE, Aggregation::Sum),
        (TIME_TO_SEARCHABLE, Aggregation::Sum),
        (COVERAGE, Aggregation::Sum),
        (SEARCHABLE, Aggregation::Last),
        (FUNNEL_EVENTS, Aggregation::Sum),
        (REMOVED, Aggregation::Sum),
        (LATE_RESULTS, Aggregation::Sum),
        (BY_WORKER, Aggregation::Sum),
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

/// Hours behind the coordinator's clock a worker's counter may name: a
/// spool holds a day (owner §2.17), plus the hour under way.
pub const WORKER_HOURS_BACK: i64 = 24;

/// Distinct counter rows one worker may add to in one hour. Far above what
/// a worker counts (a few sources × strategies), and what bounds the
/// per-worker rows against the rollups budget (owner §1.6.4).
pub const WORKER_ROWS_PER_HOUR: usize = 64;

/// Distinct (metric, dim, key) one worker may count in one UTC day. The
/// nightly compaction folds a day's hours into one row per key, so this is
/// what bounds a worker's rows after 30 days: with [`WORKER_ROWS_PER_HOUR`],
/// at most 128 × 30 hourly-held days + 128 × 335 daily rows a year, about
/// 47,000 rows (≈ 1 MB at 21 bytes a row) per worker for its own rows and
/// as much again for the fleet's (`docs/decisions/2026-11-07-WK-9b.md`).
pub const WORKER_KEYS_PER_DAY: usize = 128;

/// The most one worker may add to one row in one hour. Far above any real
/// count, and low enough that no number of workers, batches or hours can
/// take a `rollup_hour` value (a STRICT INTEGER) out of range.
pub const WORKER_ROW_CAP: i64 = 1 << 32;

/// What a remote worker's counters may do to the rollups (WK-9b; the
/// WK-6 decisions' "bound what counters can do"):
/// - only [`metric::FROM_WORKERS`], each with its own dim and key domain;
/// - only hours in [`WORKER_HOURS_BACK`] up to now, on the coordinator's
///   clock after correcting for the worker's offset;
/// - at most [`WORKER_ROWS_PER_HOUR`] rows per worker per hour and
///   [`WORKER_KEYS_PER_DAY`] keys per worker per day, and at most
///   [`WORKER_ROW_CAP`] added to each row (anything over is cut, never
///   wrapped).
#[derive(Debug, Default)]
pub struct Admit {
    /// What each (worker, hour, metric, dim, key) took so far, for hours
    /// still in the window.
    taken: HashMap<(WorkerId, i64), HashMap<(u32, u32, i64), i64>>,
    /// The keys each worker counted on each UTC day still in the window.
    days: HashMap<(WorkerId, i64), std::collections::HashSet<(u32, u32, i64)>>,
}

impl Admit {
    /// The counter as it may be added (its hour corrected, its value cut
    /// to the row's cap), or `None` if it may not.
    pub fn admit(
        &mut self,
        worker: WorkerId,
        c: Counter,
        offset_ms: i64,
        now: i64,
    ) -> Option<Counter> {
        let (_, (d0, d1), (k0, k1)) = metric::FROM_WORKERS
            .iter()
            .find(|(m, _, _)| *m == c.metric)
            .copied()?;
        if !(d0..=d1).contains(&c.dim) || !(k0..=k1).contains(&c.key) || c.value <= 0 {
            return None;
        }
        // The worker's hour started at its own clock's hour mark; on ours
        // it started `offset` earlier.
        let start_ms = i128::from(c.hour) * 3_600_000 - i128::from(offset_ms);
        let hour = i64::try_from(start_ms.div_euclid(3_600_000)).ok()?;
        let now_hour = now.div_euclid(3600);
        if hour > now_hour || hour < now_hour - WORKER_HOURS_BACK {
            return None;
        }
        let oldest = now_hour - WORKER_HOURS_BACK;
        self.taken.retain(|(_, h), _| *h >= oldest);
        self.days.retain(|(_, d), _| *d >= oldest.div_euclid(24));
        let row = (c.metric, c.dim, c.key);
        let day = self.days.entry((worker, hour.div_euclid(24))).or_default();
        if !day.contains(&row) && day.len() >= WORKER_KEYS_PER_DAY {
            return None;
        }
        let rows = self.taken.entry((worker, hour)).or_default();
        if !rows.contains_key(&row) && rows.len() >= WORKER_ROWS_PER_HOUR {
            return None;
        }
        day.insert(row);
        let took = rows.entry(row).or_insert(0);
        let value = c.value.min(WORKER_ROW_CAP - *took);
        if value <= 0 {
            return None;
        }
        *took += value;
        Some(Counter { hour, value, ..c })
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

    fn counter(hour: i64, metric: u32, dim: u32, key: i64, value: i64) -> Counter {
        Counter {
            hour,
            metric,
            dim,
            key,
            value,
        }
    }

    const W: WorkerId = WorkerId(3);
    const NOW: i64 = 500_000 * 3600 + 1800;
    const HOUR: i64 = 500_000;

    /// Requirement 2: only the metrics a worker counts, in their own dim
    /// and key domains; the coordinator's own (harvests, stores, search,
    /// gauges) are refused whatever the value.
    #[test]
    fn worker_counters_stay_in_their_domains() {
        let mut a = Admit::default();
        let mut ok = |m, d, k| a.admit(W, counter(HOUR, m, d, k, 5), 0, NOW).is_some();
        assert!(ok(metric::REDISCOVERED, 63, 255));
        assert!(ok(metric::DROPPED, 1, 0));
        assert!(ok(metric::SCRAPED, 3, 2));
        assert!(ok(metric::DHT_SCRAPES, 0, 1));
        assert!(!ok(metric::REDISCOVERED, 64, 0), "no such source");
        assert!(!ok(metric::REDISCOVERED, 1, 256), "no such strategy");
        assert!(!ok(metric::DROPPED, 1, 1));
        assert!(!ok(metric::SCRAPED, 4, 0));
        assert!(!ok(metric::DHT_SCRAPES, 1, 0));
        for m in [
            metric::HARVESTED,
            metric::STORED,
            metric::SCRAPE_REQUESTS,
            metric::SEARCHES,
            metric::SEARCH_LATENCY,
            metric::NEWLY_SEARCHABLE,
            metric::TIME_TO_SEARCHABLE,
            metric::COVERAGE,
            metric::SEARCHABLE,
            metric::FUNNEL_EVENTS,
            metric::REMOVED,
            metric::BY_WORKER,
            metric::LATE_RESULTS,
            99,
        ] {
            assert!(!ok(m, 0, 0), "metric {m} is the coordinator's");
        }
        assert!(
            a.admit(W, counter(HOUR, metric::DROPPED, 1, 0, 0), 0, NOW)
                .is_none()
        );
        assert!(
            a.admit(W, counter(HOUR, metric::DROPPED, 1, 0, -4), 0, NOW)
                .is_none()
        );
        // The per-worker key holds the three, apart.
        assert_eq!(
            metric::by_worker_key(metric::REDISCOVERED, 63, 255),
            (2 << 16) | (63 << 8) | 255
        );
    }

    /// Requirement 2: hours are corrected by the worker's offset and kept
    /// to a day back from the coordinator's own hour, never ahead.
    #[test]
    fn worker_hours_are_corrected_and_windowed() {
        let mut a = Admit::default();
        let mut hour = |h: i64, offset_ms: i64| {
            a.admit(W, counter(h, metric::DROPPED, 1, 0, 1), offset_ms, NOW)
                .map(|c| c.hour)
        };
        assert_eq!(hour(HOUR, 0), Some(HOUR));
        // Its clock is two hours fast: its hour is our hour before last.
        assert_eq!(hour(HOUR + 2, 2 * 3_600_000), Some(HOUR));
        assert_eq!(hour(HOUR + 1, 0), None, "ahead of us");
        assert_eq!(
            hour(HOUR - WORKER_HOURS_BACK, 0),
            Some(HOUR - WORKER_HOURS_BACK)
        );
        assert_eq!(
            hour(HOUR - WORKER_HOURS_BACK - 1, 0),
            None,
            "older than a spool"
        );
        assert_eq!(hour(0, 0), None);
        assert_eq!(hour(i64::from(i32::MAX), i64::MIN), None);
        assert_eq!(hour(HOUR, i64::MAX), None);
    }

    /// Requirement 2: one row takes at most [`WORKER_ROW_CAP`] from one
    /// worker in one hour, across any number of batches, so no sum leaves
    /// SQLite's integer range; and a worker adds at most
    /// [`WORKER_ROWS_PER_HOUR`] rows an hour.
    #[test]
    fn worker_rows_are_capped() {
        let mut a = Admit::default();
        let big = 1 << 40; // the codec's largest delta
        let mut total = 0;
        for _ in 0..10_000 {
            if let Some(c) = a.admit(W, counter(HOUR, metric::DROPPED, 1, 0, big), 0, NOW) {
                total += c.value;
            }
        }
        assert_eq!(total, WORKER_ROW_CAP);
        // Another worker has its own cap; the fleet's row stays far inside
        // i64 for any plausible fleet.
        assert!(
            a.admit(WorkerId(4), counter(HOUR, metric::DROPPED, 1, 0, 9), 0, NOW)
                .is_some()
        );
        assert!(i64::MAX / WORKER_ROW_CAP > 1 << 30);
        let mut rows = 0;
        for source in 0..64 {
            for strategy in 0..4 {
                let c = counter(HOUR - 1, metric::REDISCOVERED, source, strategy, 1);
                rows += usize::from(a.admit(W, c, 0, NOW).is_some());
            }
        }
        assert_eq!(rows, WORKER_ROWS_PER_HOUR);
        // Over a day, new keys stop at the daily cap; known ones still add.
        let mut day = Admit::default();
        let first = (HOUR.div_euclid(24) * 24).max(HOUR - WORKER_HOURS_BACK);
        let mut keys = std::collections::HashSet::new();
        for h in first..=HOUR {
            for strategy in 0..32 {
                let c = counter(h, metric::REDISCOVERED, 1, (h * 32 + strategy) % 256, 1);
                if let Some(c) = day.admit(W, c, 0, NOW) {
                    keys.insert(c.key);
                }
            }
        }
        assert_eq!(
            keys.len(),
            WORKER_KEYS_PER_DAY,
            "{} hours",
            HOUR - first + 1
        );
        assert!(
            day.admit(
                W,
                counter(HOUR, metric::REDISCOVERED, 1, (HOUR * 32) % 256, 1),
                0,
                NOW
            )
            .is_some()
        );
        // Hours that left the window are forgotten.
        let later = NOW + (WORKER_HOURS_BACK + 2) * 3600;
        assert!(
            a.admit(
                W,
                counter(later.div_euclid(3600), metric::DROPPED, 1, 0, 1),
                0,
                later
            )
            .is_some()
        );
        assert_eq!(a.taken.len(), 1);
    }
}
