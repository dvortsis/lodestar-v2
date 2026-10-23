//! Insights at daily grain (B11b, architecture §7 "Insights rollups"):
//! the nightly compaction that folds old hourly rows into `rollup_day` and
//! `cohort_day`, the reads over both grains, the state they share
//! (`insights_state`), and `lodestar rollup --repair`'s recount.
//!
//! - **Compaction** folds one whole UTC day per `BEGIN IMMEDIATE`
//!   transaction: the day's rows go in at daily grain (a `Sum` metric's
//!   values add, a `Last` metric keeps its latest hour's rows), its hourly
//!   rows are deleted, and `compacted_before_day` moves past it, all in
//!   that transaction. A crash leaves each day either fully hourly or
//!   fully daily, and a second run finds nothing to do. A metric with no
//!   aggregation kind is never folded: its hours stay.
//! - **Readers** take `compacted_before_day` in the same read transaction
//!   as the rows ([`Store::insights_state`] inside
//!   [`Store::read_snapshot`]), so a compaction running beside them never
//!   double-counts or drops a day.
//! - **Repair** recounts from `torrents` and `index_watermark_hour` what
//!   they hold, and only ever raises a stored count.
//!
//! Codes stay opaque here: the caller says which metric is which.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::time::{Duration, Instant};

use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};

use crate::insights::{CohortCount, Counter, RollupBy, SEARCHABLE_CHUNK, SearchableCodes};
use crate::insights::{UNCLASSIFIED, log_bucket};
use crate::sources::SourceId;
use crate::{Result, Store};

/// How a metric's hourly rows fold into a day.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Aggregation {
    /// Counters and histograms: the day's value is the hours' sum.
    Sum,
    /// Gauges (a snapshot written with replace semantics): the day keeps
    /// the rows of its latest hour.
    Last,
}

/// `insights_state`: what compaction, the readers and repair share.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InsightsState {
    /// UTC days below this are daily (`rollup_day`, `cohort_day`).
    pub compacted_before_day: i64,
    /// When compaction last finished a run (unix seconds, 0 = never).
    pub last_compaction_at: i64,
    /// The first hour fetches are counted by event time: earlier buckets
    /// are unknown (`null`), never 0.
    pub funnel_events_since_hour: i64,
    /// Searchable imported torrents.
    pub imported: i64,
}

pub(crate) fn state(conn: &Connection) -> Result<InsightsState> {
    let mut stmt = conn.prepare_cached("SELECT key, value FROM insights_state")?;
    let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?;
    let mut s = InsightsState {
        compacted_before_day: 0,
        last_compaction_at: 0,
        funnel_events_since_hour: 0,
        imported: 0,
    };
    for row in rows {
        let (k, v) = row?;
        match k.as_str() {
            "compacted_before_day" => s.compacted_before_day = v,
            "last_compaction_at" => s.last_compaction_at = v,
            "funnel_events_since_hour" => s.funnel_events_since_hour = v,
            "imported" => s.imported = v,
            _ => {}
        }
    }
    Ok(s)
}

fn set_state(conn: &Connection, key: &str, value: i64) -> Result<()> {
    conn.prepare_cached("UPDATE insights_state SET value = ?2 WHERE key = ?1")?
        .execute(params![key, value])?;
    Ok(())
}

/// What one [`Store::compact_rollups`] call is allowed to do.
#[derive(Clone, Copy)]
pub struct CompactOptions<'a> {
    /// Unix seconds now.
    pub now: i64,
    /// Whole UTC days that ended more than this many days ago are folded
    /// (30, §7).
    pub keep_days: i64,
    /// A cohort day at or after this one may still be written to (keys
    /// restored after a long downtime keep their cohort): nothing from
    /// this day on is folded.
    pub hold_from_day: Option<i64>,
    /// The call stops after this long, between days; the next call goes
    /// on.
    pub budget: Duration,
    /// Each metric's kind; `None` keeps its hours.
    pub kind: &'a dyn Fn(u32) -> Option<Aggregation>,
}

impl std::fmt::Debug for CompactOptions<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CompactOptions")
            .field("now", &self.now)
            .field("keep_days", &self.keep_days)
            .field("hold_from_day", &self.hold_from_day)
            .field("budget", &self.budget)
            .finish_non_exhaustive()
    }
}

/// What a [`Store::compact_rollups`] call did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Compacted {
    /// Days folded past the old boundary.
    pub days: u64,
    /// Days below the boundary that still had hourly rows (a straggler:
    /// a clock set back, say), folded the same way.
    pub stragglers: u64,
    /// Hourly rows deleted.
    pub rows: u64,
    /// Metrics found with no aggregation kind, whose hours were kept.
    pub unknown_metrics: Vec<u32>,
    /// Every due day is folded; `false` when the budget ran out first.
    pub done: bool,
    pub compacted_before_day: i64,
    /// Pages the database's free list gained (what an
    /// `incremental_vacuum` after the run may give back).
    pub freed_pages: u64,
}

/// One day's hourly rows into the day tables, and their deletion, inside
/// `tx`. Returns the hourly rows deleted.
fn fold_day(
    tx: &Connection,
    day: i64,
    kind: &dyn Fn(u32) -> Option<Aggregation>,
    unknown: &mut BTreeSet<u32>,
) -> Result<u64> {
    let (lo, hi) = (day * 24, day * 24 + 24);
    let metrics: Vec<u32> = {
        let mut stmt = tx.prepare_cached(
            "SELECT DISTINCT metric FROM rollup_hour WHERE hour >= ?1 AND hour < ?2",
        )?;
        let rows = stmt.query_map(params![lo, hi], |r| r.get::<_, u32>(0))?;
        rows.collect::<rusqlite::Result<_>>()?
    };
    let mut deleted = 0u64;
    for metric in metrics {
        match kind(metric) {
            Some(Aggregation::Sum) => {
                tx.prepare_cached(
                    "INSERT INTO rollup_day (day, metric, dim, key, value)
                     SELECT ?1, metric, dim, key, sum(value) FROM rollup_hour
                     WHERE hour >= ?2 AND hour < ?3 AND metric = ?4 GROUP BY dim, key
                     ON CONFLICT DO UPDATE SET value = value + excluded.value",
                )?
                .execute(params![day, lo, hi, metric])?;
            }
            Some(Aggregation::Last) => {
                // The latest hour's rows; a straggler's replace the day's.
                tx.prepare_cached(
                    "INSERT INTO rollup_day (day, metric, dim, key, value)
                     SELECT ?1, metric, dim, key, value FROM rollup_hour
                     WHERE metric = ?4 AND hour = (
                         SELECT max(hour) FROM rollup_hour
                         WHERE hour >= ?2 AND hour < ?3 AND metric = ?4)
                     ON CONFLICT DO UPDATE SET value = excluded.value",
                )?
                .execute(params![day, lo, hi, metric])?;
            }
            None => {
                unknown.insert(metric);
                continue;
            }
        }
        deleted += tx
            .prepare_cached(
                "DELETE FROM rollup_hour WHERE hour >= ?1 AND hour < ?2 AND metric = ?3",
            )?
            .execute(params![lo, hi, metric])? as u64;
    }
    tx.prepare_cached(
        "INSERT INTO cohort_day (day, source, stage, reason, count)
         SELECT ?1, source, stage, reason, sum(count) FROM cohort_hour
         WHERE cohort >= ?2 AND cohort < ?3 GROUP BY source, stage, reason
         ON CONFLICT DO UPDATE SET count = count + excluded.count",
    )?
    .execute(params![day, lo, hi])?;
    deleted += tx
        .prepare_cached("DELETE FROM cohort_hour WHERE cohort >= ?1 AND cohort < ?2")?
        .execute(params![lo, hi])? as u64;
    Ok(deleted)
}

fn freelist(conn: &Connection) -> Result<u64> {
    let n: i64 = conn.pragma_query_value(None, "freelist_count", |r| r.get(0))?;
    Ok(n.max(0) as u64)
}

/// The earliest day at or after `from` with an hourly row of any kind,
/// before `to`: two primary-key seeks.
fn next_day(conn: &Connection, from: i64, to: i64) -> Result<Option<i64>> {
    let (lo, hi) = (from * 24, to * 24);
    let h: Option<i64> = conn
        .prepare_cached(
            "SELECT min(coalesce((SELECT min(hour) FROM rollup_hour WHERE hour >= ?1 AND hour < ?2),
                                 1 << 62),
                        coalesce((SELECT min(cohort) FROM cohort_hour
                                  WHERE cohort >= ?1 AND cohort < ?2), 1 << 62))",
        )?
        .query_row(params![lo, hi], |r| r.get(0))?;
    Ok(h.filter(|&h| h < 1 << 62).map(|h| h.div_euclid(24)))
}

/// Days below `before` that still have hourly rows, oldest first: the
/// rows of metrics with no kind (which stay) and stragglers. Ranges of
/// the primary keys below the boundary, which hold only those.
fn days_below(
    conn: &Connection,
    before: i64,
    kind: &dyn Fn(u32) -> Option<Aggregation>,
) -> Result<Vec<i64>> {
    let mut days = BTreeSet::new();
    let mut stmt = conn.prepare_cached(
        "SELECT DISTINCT hour / 24, metric FROM rollup_hour WHERE hour >= 0 AND hour < ?1",
    )?;
    for row in stmt.query_map([before * 24], |r| {
        Ok((r.get::<_, i64>(0)?, r.get::<_, u32>(1)?))
    })? {
        let (d, metric) = row?;
        // A metric with no kind stays hourly: its day isn't revisited.
        if kind(metric).is_some() {
            days.insert(d);
        }
    }
    let mut stmt = conn.prepare_cached(
        "SELECT DISTINCT cohort / 24 FROM cohort_hour WHERE cohort >= 0 AND cohort < ?1",
    )?;
    for d in stmt.query_map([before * 24], |r| r.get::<_, i64>(0))? {
        days.insert(d?);
    }
    Ok(days.into_iter().collect())
}

impl Store {
    /// The Insights state (`compacted_before_day` and the rest). Read it
    /// inside the same [`Store::read_snapshot`] as the rows it decides on.
    pub fn insights_state(&self) -> Result<InsightsState> {
        state(&self.conn)
    }

    /// Folds hourly rollups of whole UTC days that ended more than
    /// `keep_days` days ago into `rollup_day` and `cohort_day`, one day per
    /// `BEGIN IMMEDIATE` transaction (the module comment has the rules),
    /// oldest first, stragglers below the boundary included. It stops
    /// between days once `budget` is spent; call it again to go on. When
    /// every due day is folded, `last_compaction_at` becomes `now` in the
    /// last transaction.
    pub fn compact_rollups(&mut self, opts: CompactOptions<'_>) -> Result<Compacted> {
        let started = Instant::now();
        let free_before = freelist(&self.conn)?;
        let mut target = opts.now.div_euclid(86_400) - opts.keep_days;
        if let Some(hold) = opts.hold_from_day {
            target = target.min(hold);
        }
        let mut unknown = BTreeSet::new();
        let mut out = Compacted::default();
        // Stragglers first: they sit below the boundary, where readers
        // look only at daily rows.
        let before = state(&self.conn)?.compacted_before_day;
        // Each call folds at least one day before it looks at the budget,
        // so a run always gets on.
        let mut worked = false;
        for day in days_below(&self.conn, before, opts.kind)? {
            if worked && started.elapsed() >= opts.budget {
                out.compacted_before_day = before;
                out.unknown_metrics = unknown.into_iter().collect();
                out.freed_pages = freelist(&self.conn)?.saturating_sub(free_before);
                return Ok(out);
            }
            let tx = crate::begin_write(&mut self.conn)?;
            let rows = fold_day(&tx, day, opts.kind, &mut unknown)?;
            tx.commit()?;
            if rows > 0 {
                out.stragglers += 1;
                out.rows += rows;
                worked = true;
            }
        }
        loop {
            let tx = crate::begin_write(&mut self.conn)?;
            let st = state(&tx)?;
            let next = if st.compacted_before_day < target {
                next_day(&tx, st.compacted_before_day, target)?
            } else {
                None
            };
            match next {
                Some(day) => {
                    out.rows += fold_day(&tx, day, opts.kind, &mut unknown)?;
                    out.days += 1;
                    set_state(&tx, "compacted_before_day", day + 1)?;
                    out.compacted_before_day = day + 1;
                    tx.commit()?;
                    worked = true;
                }
                None => {
                    // Nothing hourly is due: the boundary moves to the
                    // target, and the run is done.
                    let cb = st.compacted_before_day.max(target);
                    set_state(&tx, "compacted_before_day", cb)?;
                    set_state(&tx, "last_compaction_at", opts.now)?;
                    tx.commit()?;
                    out.compacted_before_day = cb;
                    out.done = true;
                    break;
                }
            }
            if worked && started.elapsed() >= opts.budget {
                break;
            }
        }
        out.unknown_metrics = unknown.into_iter().collect();
        out.freed_pages = freelist(&self.conn)?.saturating_sub(free_before);
        Ok(out)
    }

    /// Returns at most `pages` free pages to the file system
    /// (`PRAGMA incremental_vacuum`): after a compaction, bounded to what
    /// it freed.
    pub fn incremental_vacuum(&mut self, pages: u64) -> Result<()> {
        if pages == 0 {
            return Ok(());
        }
        let n = i64::try_from(pages).unwrap_or(i64::MAX);
        let mut stmt = self
            .conn
            .prepare(&format!("PRAGMA incremental_vacuum({n})"))?;
        let mut rows = stmt.query([])?;
        while rows.next()?.is_some() {}
        Ok(())
    }

    /// Sums of one metric over UTC days `from_day..to_day` of
    /// `rollup_day`, grouped `by` as [`Store::rollup_sums`] groups hours:
    /// `RollupBy::Hour` groups by day, and each returned row's `hour` is
    /// its day's first hour. One primary-key seek per day.
    pub fn rollup_day_sums(
        &self,
        metric: u32,
        from_day: i64,
        to_day: i64,
        by: RollupBy,
    ) -> Result<Vec<Counter>> {
        let (cols, group) = match by {
            RollupBy::Total => ("0, 0, 0", ""),
            RollupBy::Hour => ("r.day * 24, 0, 0", "GROUP BY r.day ORDER BY r.day"),
            RollupBy::Dim => ("0, r.dim, 0", "GROUP BY r.dim ORDER BY r.dim"),
            RollupBy::Key => ("0, 0, r.key", "GROUP BY r.key ORDER BY r.key"),
            RollupBy::HourDim => (
                "r.day * 24, r.dim, 0",
                "GROUP BY r.day, r.dim ORDER BY r.day, r.dim",
            ),
            RollupBy::HourKey => (
                "r.day * 24, 0, r.key",
                "GROUP BY r.day, r.key ORDER BY r.day, r.key",
            ),
            RollupBy::All => (
                "r.day * 24, r.dim, r.key",
                "GROUP BY r.day, r.dim, r.key ORDER BY r.day, r.dim, r.key",
            ),
        };
        let sql = format!(
            "WITH RECURSIVE d(x) AS (
                 SELECT ?1 WHERE ?1 < ?2
                 UNION ALL SELECT x + 1 FROM d WHERE x + 1 < ?2)
             SELECT {cols}, sum(r.value) FROM d CROSS JOIN rollup_day r
             WHERE r.day = d.x AND r.metric = ?3 {group}"
        );
        let mut stmt = self.conn.prepare_cached(&sql)?;
        let rows = stmt.query_map(params![from_day, to_day, metric], |r| {
            Ok(Counter {
                hour: r.get(0)?,
                metric,
                dim: r.get(1)?,
                key: r.get(2)?,
                value: r.get::<_, Option<i64>>(3)?.unwrap_or(0),
            })
        })?;
        let out: Vec<Counter> = rows.collect::<rusqlite::Result<_>>()?;
        Ok(if by == RollupBy::Total {
            out.into_iter().filter(|c| c.value != 0).collect()
        } else {
            out
        })
    }

    /// Sums of one metric over hours `from..to` by a range scan of
    /// `rollup_hour`, grouped `by` UTC day (`RollupBy::Hour` and the
    /// other hour groupings give each day's first hour). For the sparse
    /// hours below `compacted_before_day`, where only stragglers wait for
    /// the next compaction (and metrics with no kind): a `Sum` metric's
    /// day total is its `rollup_day` row plus these. Over dense hours use
    /// [`Store::rollup_sums`].
    pub fn rollup_straggler_sums(
        &self,
        metric: u32,
        from: i64,
        to: i64,
        by: RollupBy,
    ) -> Result<Vec<Counter>> {
        let (cols, group) = match by {
            RollupBy::Total => ("0, 0, 0", ""),
            RollupBy::Hour => ("(hour / 24) * 24 AS d, 0, 0", "GROUP BY d ORDER BY d"),
            RollupBy::Dim => ("0, dim, 0", "GROUP BY dim ORDER BY dim"),
            RollupBy::Key => ("0, 0, key", "GROUP BY key ORDER BY key"),
            RollupBy::HourDim => (
                "(hour / 24) * 24 AS d, dim, 0",
                "GROUP BY d, dim ORDER BY d, dim",
            ),
            RollupBy::HourKey => (
                "(hour / 24) * 24 AS d, 0, key",
                "GROUP BY d, key ORDER BY d, key",
            ),
            RollupBy::All => (
                "(hour / 24) * 24 AS d, dim, key",
                "GROUP BY d, dim, key ORDER BY d, dim, key",
            ),
        };
        let sql = format!(
            "SELECT {cols}, sum(value) FROM rollup_hour
             WHERE hour >= ?1 AND hour < ?2 AND metric = ?3 {group}"
        );
        let mut stmt = self.conn.prepare_cached(&sql)?;
        let rows = stmt.query_map(params![from.max(0), to, metric], |r| {
            Ok(Counter {
                hour: r.get(0)?,
                metric,
                dim: r.get(1)?,
                key: r.get(2)?,
                value: r.get::<_, Option<i64>>(3)?.unwrap_or(0),
            })
        })?;
        let out: Vec<Counter> = rows.collect::<rusqlite::Result<_>>()?;
        Ok(if by == RollupBy::Total {
            out.into_iter().filter(|c| c.value != 0).collect()
        } else {
            out
        })
    }

    /// The daily funnel for UTC days `from_day..to_day`; each row's
    /// `cohort` is its day's first hour, as [`Store::cohort_sums`] with a
    /// grain of 24.
    pub fn cohort_day_sums(&self, from_day: i64, to_day: i64) -> Result<Vec<CohortCount>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT day * 24, source, stage, reason, count FROM cohort_day
             WHERE day >= ?1 AND day < ?2 ORDER BY day, source, stage, reason",
        )?;
        let rows = stmt.query_map(params![from_day, to_day], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, u8>(2)?,
                r.get::<_, u8>(3)?,
                r.get::<_, i64>(4)?,
            ))
        })?;
        rows.map(|row| {
            let (cohort, source, stage, reason, count) = row?;
            Ok(CohortCount {
                cohort,
                source: SourceId::from_sql(source)?,
                stage,
                reason,
                count,
            })
        })
        .collect()
    }

    /// Counts that repair can rebuild, recounted and raised (see
    /// [`repair_rollups`]), on this store's connection.
    pub fn repair_rollups(&mut self, opts: RepairOptions) -> Result<Repaired> {
        repair(&self.conn, opts)
    }

    /// When the store last saw activity (unix seconds): the end of the
    /// newest hour any rollup row was written for (coverage is written
    /// every 30 s while the server runs), or the newest torrent's
    /// `metadata_at`, whichever is later. Two primary-key ends. `None` on
    /// an empty store.
    pub fn last_activity(&self) -> Result<Option<i64>> {
        Ok(self.conn.query_row(
            "SELECT max(coalesce((SELECT (max(hour) + 1) * 3600 FROM rollup_hour), -1),
                        coalesce((SELECT metadata_at FROM torrents ORDER BY id DESC LIMIT 1), -1))",
            [],
            |r| r.get::<_, i64>(0),
        )?)
        .map(|t| (t >= 0).then_some(t))
    }

    /// Whether the last start/stop event of `kinds` (`start`, `stop`) is a
    /// start: the server didn't stop cleanly.
    pub fn stopped_uncleanly(&self, start: u32, stop: u32) -> Result<bool> {
        Ok(self
            .last_event(&[start, stop])?
            .is_some_and(|e| e.kind == start))
    }
}

/// The codes repair writes under (the pipeline's).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RepairCodes {
    /// Searchable stage, newly searchable and the delay histogram, as
    /// [`Store::record_searchable`] writes them.
    pub searchable: SearchableCodes,
    /// `cohort_hour.stage` for "stored".
    pub stored_stage: u8,
    /// `rollup_hour.metric` for torrents newly stored, by event time
    /// (dim = first source).
    pub stored: u32,
}

/// What [`repair_rollups`] recounts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RepairOptions {
    pub codes: RepairCodes,
    /// Cohorts and hours from this hour on are recounted (hours in
    /// compacted days at daily grain, from the start of the day).
    pub since_hour: i64,
    /// Also recount the imported total (one scan of `torrents`).
    pub recount_imported: bool,
}

/// What a repair changed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Repaired {
    /// Torrent rows read.
    pub scanned: u64,
    /// Cohort cells (cohort, source, stage) raised: stored, searchable.
    pub cohorts_stored: u64,
    pub cohorts_searchable: u64,
    /// Cohorts whose delay histogram was replaced with the recount.
    pub histograms: u64,
    /// Hours (or days) whose total was raised: stored, newly searchable.
    pub hours_stored: u64,
    pub hours_searchable: u64,
    /// The imported total before and after, when recounted and changed.
    pub imported: Option<(i64, i64)>,
}

impl Repaired {
    pub fn changed(&self) -> bool {
        self.cohorts_stored
            + self.cohorts_searchable
            + self.histograms
            + self.hours_stored
            + self.hours_searchable
            > 0
            || self.imported.is_some()
    }
}

/// [`Store::repair_rollups`] on a connection of its own (the offline
/// command's, under its exclusive lock); the schema must be this build's.
///
/// - Rebuilds from `torrents`, for discovery cohorts and hours from
///   `since_hour` on: cohort Stored (`discovered_at` hour, `first_source`,
///   imports excluded), newly stored by `metadata_at` hour (dim =
///   `first_source`, imports excluded) and, from `index_watermark_hour`'s
///   id ranges, everything [`Store::record_searchable`] writes: newly
///   searchable at the watermark row's hour, cohort Searchable, and the
///   delay histogram, taking the watermark hour's midpoint as the time.
/// - **Raise only:** a cohort cell is written only where the recount is
///   above what's stored; an hour's rows of a metric are replaced only
///   where the recount's total is above the stored total; a cohort's
///   histogram is replaced only when one of its Searchable cells is
///   raised (with the recount's midpoint delays). Deletions since lower a
///   recount, and final cohorts never change, so no total is ever
///   lowered (a cell inside a replaced hour follows the current
///   classification).
/// - Reads `torrents` in id chunks of 20,000, each in its own read
///   transaction, then writes in one `BEGIN IMMEDIATE` transaction.
pub fn repair_rollups(conn: &Connection, opts: RepairOptions) -> Result<Repaired> {
    crate::schema::check_current(conn)?;
    repair(conn, opts)
}

/// A recount key: (bucket hour, dim, key) for metrics; (cohort hour,
/// source) for cohorts.
#[derive(Debug, Default)]
struct Recount {
    cohort_stored: HashMap<(i64, u8), i64>,
    cohort_searchable: HashMap<(i64, u8), i64>,
    delays: HashMap<(i64, i64), i64>,
    stored: HashMap<(i64, u32), i64>,
    newly: HashMap<(i64, u32, i64), i64>,
}

/// The first torrent id whose `metadata_at` is at or after `t`, by a
/// binary search on the primary key (ids follow storage order closely;
/// the caller leaves slack for the rest). `None` when there is none.
fn first_id_stored_since(conn: &Connection, t: i64) -> Result<Option<i64>> {
    let (lo, hi): (Option<i64>, Option<i64>) =
        conn.query_row("SELECT min(id), max(id) FROM torrents", [], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })?;
    let (Some(mut lo), Some(mut hi)) = (lo, hi) else {
        return Ok(None);
    };
    let mut probe = conn.prepare_cached(
        "SELECT id, metadata_at FROM torrents WHERE id >= ?1 ORDER BY id LIMIT 1",
    )?;
    let at = |probe: &mut rusqlite::CachedStatement<'_>, id: i64| -> Result<(i64, i64)> {
        Ok(probe.query_row([id], |r| Ok((r.get(0)?, r.get(1)?)))?)
    };
    if at(&mut probe, hi)?.1 < t {
        return Ok(None);
    }
    // Invariant: the row at or after `hi` has metadata_at >= t.
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        let (id, m) = at(&mut probe, mid)?;
        if m >= t {
            hi = mid.min(id);
        } else {
            lo = id + 1;
        }
    }
    Ok(Some(at(&mut probe, lo)?.0))
}

/// Watermark rows `(hour, last_id)`, in order.
fn watermarks(conn: &Connection) -> Result<Vec<(i64, i64)>> {
    let mut stmt = conn.prepare("SELECT hour, last_id FROM index_watermark_hour ORDER BY hour")?;
    let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

fn repair(conn: &Connection, opts: RepairOptions) -> Result<Repaired> {
    let codes = opts.codes;
    let mut out = Repaired::default();
    // Everything the scan needs, in one snapshot.
    let (wm, start_id, max_id) = {
        let tx = Transaction::new_unchecked(conn, TransactionBehavior::Deferred)?;
        let wm = watermarks(&tx)?;
        // Slack: `metadata_at` follows id order only roughly.
        let first = first_id_stored_since(&tx, opts.since_hour * 3600 - 3600)?;
        // Ids made searchable from `since_hour` on start after the last
        // row before it.
        let wm_lo = wm
            .iter()
            .rev()
            .find(|(h, _)| *h < opts.since_hour)
            .map_or(0, |&(_, id)| id)
            + 1;
        let wm_any = wm.iter().any(|&(h, _)| h >= opts.since_hour);
        let max_id: Option<i64> = tx.query_row("SELECT max(id) FROM torrents", [], |r| r.get(0))?;
        tx.commit()?;
        let start = match (first, wm_any) {
            (Some(f), true) => Some(f.min(wm_lo)),
            (Some(f), false) => Some(f),
            (None, true) => Some(wm_lo),
            (None, false) => None,
        };
        (wm, start, max_id.unwrap_or(0))
    };
    let hour0 = wm.iter().find(|(h, _)| *h == 0).map_or(0, |&(_, id)| id);
    // The hour an id became searchable: the first row whose last_id
    // reaches it; `None` when it isn't, or when that's hour 0 (unknown).
    let searchable_hour = |id: i64| -> Option<i64> {
        let i = wm.partition_point(|&(_, last)| last < id);
        wm.get(i).map(|&(h, _)| h).filter(|&h| h > 0)
    };
    let mut rc = Recount::default();
    if let Some(mut from) = start_id {
        while from <= max_id {
            let to = from.saturating_add(SEARCHABLE_CHUNK);
            let tx = Transaction::new_unchecked(conn, TransactionBehavior::Deferred)?;
            {
                let mut stmt = tx.prepare_cached(
                    "SELECT id, discovered_at, metadata_at, first_source, content_type, flags
                     FROM torrents WHERE id >= ?1 AND id < ?2",
                )?;
                let mut rows = stmt.query(params![from, to])?;
                while let Some(r) = rows.next()? {
                    out.scanned += 1;
                    let id: i64 = r.get(0)?;
                    let discovered_at: i64 = r.get(1)?;
                    let metadata_at: i64 = r.get(2)?;
                    let source = r
                        .get::<_, Option<i64>>(3)?
                        .map(SourceId::from_sql)
                        .transpose()?
                        .map(SourceId::get);
                    let kind: Option<i64> = r.get(4)?;
                    let import = r.get::<_, i64>(5)? & crate::flags::IMPORTED != 0;
                    let cohort = discovered_at.div_euclid(3600);
                    let searchable = (id > hour0).then(|| searchable_hour(id)).flatten();
                    if let (false, Some(src)) = (import, source) {
                        if cohort >= opts.since_hour {
                            *rc.cohort_stored.entry((cohort, src)).or_default() += 1;
                        }
                        let stored_h = metadata_at.div_euclid(3600);
                        if stored_h >= opts.since_hour {
                            *rc.stored.entry((stored_h, u32::from(src))).or_default() += 1;
                        }
                        if let (Some(h), true) = (searchable, cohort >= opts.since_hour) {
                            *rc.cohort_searchable.entry((cohort, src)).or_default() += 1;
                            // The hour's midpoint, but never before it
                            // was stored.
                            let delay = (h * 3600 + 1800).max(metadata_at) - discovered_at;
                            *rc.delays.entry((cohort, log_bucket(delay))).or_default() += 1;
                        }
                    }
                    if let Some(h) = searchable.filter(|&h| h >= opts.since_hour) {
                        let dim = kind
                            .and_then(|k| u32::try_from(k).ok())
                            .filter(|&k| k < UNCLASSIFIED)
                            .unwrap_or(UNCLASSIFIED);
                        *rc.newly.entry((h, dim, i64::from(import))).or_default() += 1;
                    }
                }
            }
            tx.commit()?;
            from = to;
        }
    }
    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
    let st = state(&tx)?;
    let boundary = st.compacted_before_day * 24;
    let grain = |h: i64| -> Grain {
        if h < boundary {
            Grain::Day(h.div_euclid(24))
        } else {
            Grain::Hour(h)
        }
    };
    // Hours of a partial compacted day aren't compared: the recount
    // covers whole days only from the day after `since_hour`'s.
    let whole = |h: i64| match grain(h) {
        Grain::Day(d) => d * 24 >= opts.since_hour,
        Grain::Hour(_) => true,
    };
    let stage = codes.searchable.stage;
    let mut raised_searchable: BTreeSet<Grain> = BTreeSet::new();
    for (cells, st_code, counter) in [
        (
            &rc.cohort_stored,
            codes.stored_stage,
            &mut out.cohorts_stored,
        ),
        (&rc.cohort_searchable, stage, &mut out.cohorts_searchable),
    ] {
        let mut by: BTreeMap<(Grain, u8), i64> = BTreeMap::new();
        for (&(c, src), &n) in cells.iter().filter(|((c, _), _)| whole(*c)) {
            *by.entry((grain(c), src)).or_default() += n;
        }
        for ((g, src), n) in by {
            if n > cohort_cell(&tx, g, src, st_code)? {
                set_cohort_cell(&tx, g, src, st_code, n)?;
                *counter += 1;
                if st_code == stage {
                    raised_searchable.insert(g);
                }
            }
        }
    }
    // Histograms, only beside a raised count.
    let mut hist: BTreeMap<Grain, BTreeMap<i64, i64>> = BTreeMap::new();
    for (&(c, b), &n) in &rc.delays {
        if whole(c) {
            *hist.entry(grain(c)).or_default().entry(b).or_default() += n;
        }
    }
    for g in &raised_searchable {
        replace_metric(
            &tx,
            *g,
            codes.searchable.delay,
            hist.get(g).into_iter().flatten().map(|(&b, &n)| (0, b, n)),
        )?;
        out.histograms += 1;
    }
    let mut stored: BTreeMap<Grain, BTreeMap<(u32, i64), i64>> = BTreeMap::new();
    for (&(h, dim), &n) in rc.stored.iter().filter(|((h, _), _)| whole(*h)) {
        *stored
            .entry(grain(h))
            .or_default()
            .entry((dim, 0))
            .or_default() += n;
    }
    let mut newly: BTreeMap<Grain, BTreeMap<(u32, i64), i64>> = BTreeMap::new();
    for (&(h, dim, key), &n) in rc.newly.iter().filter(|((h, _, _), _)| whole(*h)) {
        *newly
            .entry(grain(h))
            .or_default()
            .entry((dim, key))
            .or_default() += n;
    }
    for (by, metric, counter) in [
        (&stored, codes.stored, &mut out.hours_stored),
        (&newly, codes.searchable.newly, &mut out.hours_searchable),
    ] {
        for (g, cells) in by {
            let total: i64 = cells.values().sum();
            if total > metric_total(&tx, *g, metric)? {
                replace_metric(&tx, *g, metric, cells.iter().map(|(&(d, k), &n)| (d, k, n)))?;
                *counter += 1;
            }
        }
    }
    if opts.recount_imported {
        let n: i64 = tx.query_row(
            "SELECT count(*) FROM torrents WHERE flags & ?1 != 0
             AND id <= (SELECT coalesce(max(last_id), 0) FROM index_watermark_hour)",
            [crate::flags::IMPORTED],
            |r| r.get(0),
        )?;
        if n != st.imported {
            set_state(&tx, "imported", n)?;
            out.imported = Some((st.imported, n));
        }
    }
    tx.commit()?;
    Ok(out)
}

/// Where a recount lands: an hourly bucket, or a compacted day.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Grain {
    Hour(i64),
    Day(i64),
}

fn cohort_cell(conn: &Connection, g: Grain, source: u8, stage: u8) -> Result<i64> {
    let (sql, at) = match g {
        Grain::Hour(h) => (
            "SELECT count FROM cohort_hour WHERE cohort = ?1 AND source = ?2 AND stage = ?3
             AND reason = 0",
            h,
        ),
        // The day's row plus its hourly stragglers, still to be folded.
        Grain::Day(d) => (
            "SELECT coalesce((SELECT count FROM cohort_day WHERE day = ?1 AND source = ?2
                                AND stage = ?3 AND reason = 0), 0)
                  + coalesce((SELECT sum(count) FROM cohort_hour
                              WHERE cohort >= ?1 * 24 AND cohort < ?1 * 24 + 24
                                AND source = ?2 AND stage = ?3 AND reason = 0), 0)",
            d,
        ),
    };
    Ok(conn
        .prepare_cached(sql)?
        .query_row(params![at, source, stage], |r| r.get::<_, Option<i64>>(0))
        .optional()?
        .flatten()
        .unwrap_or(0))
}

fn set_cohort_cell(conn: &Connection, g: Grain, source: u8, stage: u8, n: i64) -> Result<()> {
    let (sql, at) = match g {
        Grain::Hour(h) => (
            "INSERT INTO cohort_hour (cohort, source, stage, reason, count) VALUES (?1, ?2, ?3, 0, ?4)
             ON CONFLICT DO UPDATE SET count = excluded.count",
            h,
        ),
        Grain::Day(d) => {
            // The recount covers the stragglers: they go, so the next
            // compaction can't add them again.
            conn.prepare_cached(
                "DELETE FROM cohort_hour WHERE cohort >= ?1 * 24 AND cohort < ?1 * 24 + 24
                 AND source = ?2 AND stage = ?3 AND reason = 0",
            )?
            .execute(params![d, source, stage])?;
            (
                "INSERT INTO cohort_day (day, source, stage, reason, count)
                 VALUES (?1, ?2, ?3, 0, ?4)
                 ON CONFLICT DO UPDATE SET count = excluded.count",
                d,
            )
        }
    };
    conn.prepare_cached(sql)?
        .execute(params![at, source, stage, n])?;
    Ok(())
}

fn metric_total(conn: &Connection, g: Grain, metric: u32) -> Result<i64> {
    let (sql, at) = match g {
        Grain::Hour(h) => (
            "SELECT coalesce(sum(value), 0) FROM rollup_hour WHERE hour = ?1 AND metric = ?2",
            h,
        ),
        Grain::Day(d) => (
            "SELECT coalesce((SELECT sum(value) FROM rollup_day WHERE day = ?1 AND metric = ?2), 0)
                  + coalesce((SELECT sum(value) FROM rollup_hour
                              WHERE hour >= ?1 * 24 AND hour < ?1 * 24 + 24 AND metric = ?2), 0)",
            d,
        ),
    };
    Ok(conn
        .prepare_cached(sql)?
        .query_row(params![at, metric], |r| r.get(0))?)
}

/// Replaces one bucket's rows of `metric` with `cells` (dim, key, value).
fn replace_metric(
    conn: &Connection,
    g: Grain,
    metric: u32,
    cells: impl Iterator<Item = (u32, i64, i64)>,
) -> Result<()> {
    let (del, ins, at) = match g {
        Grain::Hour(h) => (
            "DELETE FROM rollup_hour WHERE hour = ?1 AND metric = ?2",
            "INSERT INTO rollup_hour (hour, metric, dim, key, value) VALUES (?1, ?2, ?3, ?4, ?5)",
            h,
        ),
        Grain::Day(d) => {
            // Stragglers too: the recount covers them.
            conn.prepare_cached(
                "DELETE FROM rollup_hour WHERE hour >= ?1 * 24 AND hour < ?1 * 24 + 24
                 AND metric = ?2",
            )?
            .execute(params![d, metric])?;
            (
                "DELETE FROM rollup_day WHERE day = ?1 AND metric = ?2",
                "INSERT INTO rollup_day (day, metric, dim, key, value)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                d,
            )
        }
    };
    conn.prepare_cached(del)?.execute(params![at, metric])?;
    let mut stmt = conn.prepare_cached(ins)?;
    for (dim, key, value) in cells {
        stmt.execute(params![at, metric, dim, key, value])?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::insights::Event;
    use crate::{Filtered, Held, Refiled, StoreConfig, flags};

    const H: i64 = 490_008; // a UTC day's first hour (490_008 / 24 = 20_417)
    const DAY: i64 = H / 24;
    const SUM: u32 = 4;
    const GAUGE: u32 = 13;
    const UNKNOWN: u32 = 99;
    const SEARCHABLE: SearchableCodes = SearchableCodes {
        stage: 5,
        newly: 10,
        delay: 11,
    };
    const CODES: RepairCodes = RepairCodes {
        searchable: SEARCHABLE,
        stored_stage: 4,
        stored: 4,
    };

    fn kind(metric: u32) -> Option<Aggregation> {
        match metric {
            GAUGE => Some(Aggregation::Last),
            UNKNOWN => None,
            _ => Some(Aggregation::Sum),
        }
    }

    fn store() -> Store {
        Store::open_in_memory(StoreConfig::default()).unwrap()
    }

    fn opts(now: i64) -> CompactOptions<'static> {
        CompactOptions {
            now,
            keep_days: 30,
            hold_from_day: None,
            budget: Duration::from_secs(3600),
            kind: &kind,
        }
    }

    fn c(hour: i64, metric: u32, dim: u32, key: i64, value: i64) -> Counter {
        Counter {
            hour,
            metric,
            dim,
            key,
            value,
        }
    }

    fn cohort(cohort: i64, stage: u8, reason: u8, count: i64) -> CohortCount {
        CohortCount {
            cohort,
            source: SourceId::from_sql(0).unwrap(),
            stage,
            reason,
            count,
        }
    }

    /// `days` days of hourly rows from `H`: a counter in two dims, a gauge
    /// that changes every hour, an unknown metric, and cohort rows.
    fn fill(s: &mut Store, days: i64) {
        let mut counters = Vec::new();
        let mut gauges = Vec::new();
        let mut cohorts = Vec::new();
        for h in H..H + days * 24 {
            counters.push(c(h, SUM, 0, 0, 1 + h % 3));
            counters.push(c(h, SUM, 1, 2, 5));
            counters.push(c(h, UNKNOWN, 0, 0, 1));
            gauges.push(c(h, GAUGE, 0, 0, h - H));
            cohorts.push(cohort(h, 1, 0, 10));
            cohorts.push(cohort(h, 6, 3, 2));
        }
        s.add_counters(&counters).unwrap();
        s.set_gauges(&gauges).unwrap();
        s.write(&crate::WriteBatch {
            cohorts: &cohorts,
            ..crate::WriteBatch::default()
        })
        .unwrap();
    }

    /// `(day, metric, dim, key, value)` and `(day, stage, reason, count)`.
    type Dump = (Vec<(i64, i64, i64, i64, i64)>, Vec<(i64, i64, i64, i64)>);

    /// Everything stored, at a day's grain: what a reader sums.
    fn dump(s: &Store) -> Dump {
        let q = |sql: &str| -> Vec<Vec<i64>> {
            let mut stmt = s.conn.prepare(sql).unwrap();
            let n = stmt.column_count();
            stmt.query_map([], |r| (0..n).map(|i| r.get(i)).collect())
                .unwrap()
                .collect::<rusqlite::Result<_>>()
                .unwrap()
        };
        let rollups = q("SELECT d, metric, dim, key, sum(v) FROM (
                 SELECT day AS d, metric, dim, key, value AS v FROM rollup_day WHERE metric != 13
                 UNION ALL SELECT hour / 24, metric, dim, key, value FROM rollup_hour
                 WHERE metric != 13)
             GROUP BY d, metric, dim, key ORDER BY 1, 2, 3, 4")
        .into_iter()
        .map(|r| (r[0], r[1], r[2], r[3], r[4]))
        .collect();
        let cohorts = q("SELECT d, stage, reason, sum(n) FROM (
                 SELECT day AS d, stage, reason, count AS n FROM cohort_day
                 UNION ALL SELECT cohort / 24, stage, reason, count FROM cohort_hour)
             GROUP BY d, stage, reason ORDER BY 1, 2, 3")
        .into_iter()
        .map(|r| (r[0], r[1], r[2], r[3]))
        .collect();
        (rollups, cohorts)
    }

    fn rows(s: &Store, table: &str) -> i64 {
        s.conn
            .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
            .unwrap()
    }

    /// Sums `metric` over hours `from..to` as a reader does: daily rows
    /// and stragglers below the boundary, hourly rows above it, all in
    /// one snapshot.
    fn read_total(s: &Store, metric: u32, from: i64, to: i64) -> i64 {
        s.read_snapshot(|s| {
            let b = s.insights_state()?.compacted_before_day * 24;
            let split = b.clamp(from, to);
            let mut n = 0;
            for c in s.rollup_day_sums(metric, from / 24, split / 24, RollupBy::Total)? {
                n += c.value;
            }
            for c in s.rollup_straggler_sums(metric, from, split, RollupBy::Total)? {
                n += c.value;
            }
            for c in s.rollup_sums(metric, split, to, RollupBy::Total)? {
                n += c.value;
            }
            Ok(n)
        })
        .unwrap()
    }

    #[test]
    fn a_day_folds_sums_keeps_the_last_gauge_and_leaves_unknown_codes() {
        let mut s = store();
        fill(&mut s, 3);
        let before = dump(&s);
        let total = read_total(&s, SUM, H, H + 72);
        // Day DAY ended 30 days before `now`'s day; DAY + 1 didn't yet.
        let now = (DAY + 31) * 86_400 + 5;
        let r = s.compact_rollups(opts(now)).unwrap();
        assert!(r.done);
        assert_eq!(
            (r.days, r.stragglers, r.compacted_before_day),
            (1, 0, DAY + 1)
        );
        assert_eq!(r.unknown_metrics, [UNKNOWN]);
        assert_eq!(dump(&s), before, "compaction loses nothing");
        assert_eq!(read_total(&s, SUM, H, H + 72), total);
        // The gauge's day is its last hour's snapshot, never a sum.
        let g = s
            .rollup_day_sums(GAUGE, DAY, DAY + 1, RollupBy::Total)
            .unwrap();
        assert_eq!(g[0].value, 23);
        // The unknown metric keeps its hours; the rest of the day is gone.
        let left: Vec<u32> = {
            let mut stmt = s
                .conn
                .prepare("SELECT DISTINCT metric FROM rollup_hour WHERE hour < ?1")
                .unwrap();
            stmt.query_map([H + 24], |r| r.get(0))
                .unwrap()
                .collect::<rusqlite::Result<_>>()
                .unwrap()
        };
        assert_eq!(left, [UNKNOWN]);
        let st = s.insights_state().unwrap();
        assert_eq!(
            (st.compacted_before_day, st.last_compaction_at),
            (DAY + 1, now)
        );
        // Again: nothing more to do, and nothing changes.
        let again = s.compact_rollups(opts(now)).unwrap();
        assert_eq!((again.days, again.stragglers, again.rows), (0, 0, 0));
        assert_eq!(dump(&s), before);
    }

    /// A run cut short after each day (a crash between days, or the
    /// budget) ends with the same tables as one run.
    #[test]
    fn day_by_day_runs_equal_one_run() {
        let now = (DAY + 40) * 86_400;
        let mut one = store();
        fill(&mut one, 12);
        one.compact_rollups(opts(now)).unwrap();
        let mut steps = store();
        fill(&mut steps, 12);
        let mut calls = 0;
        loop {
            calls += 1;
            let r = steps
                .compact_rollups(CompactOptions {
                    budget: Duration::ZERO,
                    ..opts(now)
                })
                .unwrap();
            // One day per call: the state is always a whole number of days.
            let st = steps.insights_state().unwrap();
            assert_eq!(r.compacted_before_day, st.compacted_before_day);
            if r.done {
                break;
            }
            assert_eq!(r.days, 1);
        }
        assert_eq!(calls, 11, "10 days due, then the call that finds none");
        assert_eq!(dump(&steps), dump(&one));
        let q = |s: &Store| {
            (
                rows(s, "rollup_hour"),
                rows(s, "rollup_day"),
                rows(s, "cohort_day"),
            )
        };
        assert_eq!(q(&steps), q(&one));
        assert_eq!(
            steps.insights_state().unwrap().compacted_before_day,
            DAY + 10
        );
    }

    #[test]
    fn the_seal_floor_holds_a_day_back() {
        let mut s = store();
        fill(&mut s, 5);
        let now = (DAY + 40) * 86_400;
        let r = s
            .compact_rollups(CompactOptions {
                hold_from_day: Some(DAY + 2),
                ..opts(now)
            })
            .unwrap();
        assert!(r.done);
        assert_eq!(r.compacted_before_day, DAY + 2);
        let cohort_hours: i64 = s
            .conn
            .query_row("SELECT min(cohort) FROM cohort_hour", [], |r| r.get(0))
            .unwrap();
        assert_eq!(cohort_hours, H + 48, "days from the floor on stay hourly");
    }

    #[test]
    fn stragglers_below_the_boundary_are_read_then_folded() {
        let mut s = store();
        fill(&mut s, 2);
        let now = (DAY + 40) * 86_400;
        s.compact_rollups(opts(now)).unwrap();
        let total = read_total(&s, SUM, H, H + 48);
        // A late write into a compacted day (a clock set back, say).
        s.add_counters(&[c(H + 5, SUM, 0, 0, 7)]).unwrap();
        s.write(&crate::WriteBatch {
            cohorts: &[cohort(H + 30, 4, 0, 3)],
            ..crate::WriteBatch::default()
        })
        .unwrap();
        assert_eq!(read_total(&s, SUM, H, H + 48), total + 7, "read at once");
        let before = dump(&s);
        let r = s.compact_rollups(opts(now + 86_400)).unwrap();
        assert_eq!(r.stragglers, 2);
        assert_eq!(dump(&s), before);
        assert_eq!(read_total(&s, SUM, H, H + 48), total + 7);
        assert_eq!(
            s.conn
                .query_row(
                    "SELECT count(*) FROM rollup_hour WHERE metric != ?1",
                    [UNKNOWN],
                    |r| r.get::<_, i64>(0)
                )
                .unwrap(),
            0
        );
    }

    /// A reader on its own connection, reading between every day a
    /// compaction folds, always sees the same totals: the boundary and
    /// the rows come from one snapshot.
    #[test]
    fn a_read_beside_a_compaction_never_double_counts() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("i.db");
        let mut w = Store::open(&path, StoreConfig::default()).unwrap();
        fill(&mut w, 8);
        let r = Store::open_reader(&path, StoreConfig::default()).unwrap();
        let total = read_total(&r, SUM, H, H + 8 * 24);
        let now = (DAY + 40) * 86_400;
        loop {
            // A read transaction held open across a fold sees the state
            // before it, consistently.
            let held = r
                .read_snapshot(|rs| {
                    let a = read_total(rs, SUM, H, H + 8 * 24);
                    let done = w
                        .compact_rollups(CompactOptions {
                            budget: Duration::ZERO,
                            ..opts(now)
                        })
                        .unwrap()
                        .done;
                    Ok((a, read_total(rs, SUM, H, H + 8 * 24), done))
                })
                .unwrap();
            assert_eq!((held.0, held.1), (total, total));
            assert_eq!(read_total(&r, SUM, H, H + 8 * 24), total);
            if held.2 {
                break;
            }
        }
        assert_eq!(r.insights_state().unwrap().compacted_before_day, DAY + 10);
    }

    /// A year of B11a's ~110 rollup rows and ~28 cohort rows an hour,
    /// compacted on its first run: the WAL stays bounded and the tables
    /// fit the 50 MB a year budget (§1.6.4).
    #[test]
    fn a_year_compacts_with_a_bounded_wal_and_fits_the_budget() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("y.db");
        let mut s = Store::open(&path, StoreConfig::default()).unwrap();
        let hours = 365 * 24;
        let src = |n: i64| SourceId::from_sql(n).unwrap();
        for chunk in (0..hours).step_by(24 * 30) {
            let mut counters = Vec::new();
            let mut cohorts = Vec::new();
            for h in (H + chunk)..(H + (chunk + 24 * 30).min(hours)) {
                for metric in [1u32, 2, 4, 10, 11, 12] {
                    for k in 0..18 {
                        counters.push(c(h, metric, (k % 6) as u32, k / 6, 1 + k));
                    }
                }
                for (stage, reason) in [(1, 0), (2, 0), (3, 0), (4, 0), (5, 0), (6, 1), (6, 2)] {
                    for s in 0..4 {
                        cohorts.push(CohortCount {
                            source: src(s),
                            ..cohort(h, stage, reason, 3)
                        });
                    }
                }
            }
            s.add_counters(&counters).unwrap();
            s.write(&crate::WriteBatch {
                cohorts: &cohorts,
                ..crate::WriteBatch::default()
            })
            .unwrap();
        }
        s.checkpoint().unwrap();
        let wal = dir.path().join("y.db-wal");
        let before = dump(&s);
        let now = (DAY + 365 + 1) * 86_400;
        let mut max_wal = 0;
        let mut freed = 0;
        // As the recorder runs it: budgeted steps, then the freed pages
        // handed back 256 at a time.
        loop {
            let r = s
                .compact_rollups(CompactOptions {
                    budget: Duration::from_millis(200),
                    ..opts(now)
                })
                .unwrap();
            max_wal = max_wal.max(std::fs::metadata(&wal).map_or(0, |m| m.len()));
            freed += r.freed_pages;
            if r.done {
                break;
            }
        }
        assert!(freed > 0);
        while freed > 0 {
            let n = freed.min(256);
            s.incremental_vacuum(n).unwrap();
            freed -= n;
            max_wal = max_wal.max(std::fs::metadata(&wal).map_or(0, |m| m.len()));
        }
        assert_eq!(dump(&s), before);
        // SQLite checkpoints every 1,000 pages (16 MB at 16 KB pages);
        // one day's transaction is a few hundred KB.
        assert!(max_wal < 40 << 20, "WAL grew to {max_wal} bytes");
        // The store holds nothing but these rollups, so its file is
        // their size (dbstat isn't in the bundled build).
        s.checkpoint().unwrap();
        let file = std::fs::metadata(&path).unwrap().len();
        assert!(file < 50 << 20, "a compacted year takes {file} bytes");
    }

    /// A torrent row with what repair reads.
    #[allow(clippy::too_many_arguments)]
    fn torrent(
        s: &Store,
        id: i64,
        discovered_at: i64,
        metadata_at: i64,
        source: Option<i64>,
        kind: Option<i64>,
        fl: i64,
    ) {
        s.conn
            .execute(
                "INSERT INTO torrents (id, info_hash, name, size, piece_len, piece_count,
                     file_count, info_size, flags, discovered_at, metadata_at, last_seen_at,
                     first_source, content_type)
                 VALUES (?1, ?2, 'x', 0, 0, 0, 0, 0, ?3, ?4, ?5, ?5, ?6, ?7)",
                params![
                    id,
                    hash(id).to_vec(),
                    fl,
                    discovered_at,
                    metadata_at,
                    source,
                    kind
                ],
            )
            .unwrap();
    }

    fn hash(id: i64) -> [u8; 20] {
        id.to_be_bytes().repeat(3)[..20].try_into().unwrap()
    }

    /// Cohort cells of `stage` as `(cohort, source, count)`, hourly.
    fn cells(s: &Store, stage: u8) -> Vec<(i64, i64, i64)> {
        let mut stmt = s
            .conn
            .prepare(
                "SELECT cohort, source, count FROM cohort_hour WHERE stage = ?1 AND reason = 0
                 ORDER BY 1, 2",
            )
            .unwrap();
        stmt.query_map([stage], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap()
    }

    /// Independent `COUNT(*)`s (§10.17): non-imported torrents with a
    /// source by cohort, and those searchable (above hour 0's id, up to
    /// the watermark).
    fn counted(s: &Store, searchable: bool) -> Vec<(i64, i64, i64)> {
        let mut stmt = s
            .conn
            .prepare(
                "SELECT discovered_at / 3600, first_source, count(*) FROM torrents
                 WHERE flags & 32 = 0 AND first_source IS NOT NULL
                   AND (?1 = 0 OR (id > (SELECT last_id FROM index_watermark_hour WHERE hour = 0)
                   AND id <= (SELECT max(last_id) FROM index_watermark_hour)))
                 GROUP BY 1, 2 ORDER BY 1, 2",
            )
            .unwrap();
        stmt.query_map([searchable], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap()
    }

    fn metric_by_hour(s: &Store, metric: u32) -> Vec<(i64, i64)> {
        s.rollup_sums(metric, H - 100, H + 200, RollupBy::Hour)
            .unwrap()
            .into_iter()
            .map(|c| (c.hour, c.value))
            .collect()
    }

    /// Torrents over a few hours as the pipeline would record them, with
    /// the writer's STORED counts and the recorder's searchable ranges.
    fn ingest(s: &mut Store, n: i64) {
        let mut stored = Vec::new();
        for id in 1..=n {
            let disc = H * 3600 + id * 300;
            let meta = disc + 1200;
            let fl = if id % 9 == 0 { flags::IMPORTED } else { 0 };
            torrent(s, id, disc, meta, Some(id % 3), Some(id % 12), fl);
            if fl == 0 {
                stored.push(c(meta.div_euclid(3600), 4, (id % 3) as u32, 0, 1));
                s.write(&crate::WriteBatch {
                    cohorts: &[CohortCount {
                        source: SourceId::from_sql(id % 3).unwrap(),
                        ..cohort(disc.div_euclid(3600), 4, 0, 1)
                    }],
                    ..crate::WriteBatch::default()
                })
                .unwrap();
            }
            if id % 7 == 0 {
                s.record_searchable(id, meta + 600, SEARCHABLE).unwrap();
            }
        }
        s.add_counters(&stored).unwrap();
        s.record_searchable(n, H * 3600 + n * 300 + 7200, SEARCHABLE)
            .unwrap();
    }

    fn repair_opts(since_hour: i64) -> RepairOptions {
        RepairOptions {
            codes: CODES,
            since_hour,
            recount_imported: true,
        }
    }

    #[test]
    fn repair_restores_what_a_crash_lost_and_never_lowers() {
        let mut s = store();
        ingest(&mut s, 60);
        let good = (
            cells(&s, 4),
            cells(&s, 5),
            metric_by_hour(&s, 4),
            metric_by_hour(&s, 10),
        );
        assert_eq!(good.0, counted(&s, false));
        assert_eq!(good.1, counted(&s, true));
        // The crash: the last flush's rows never landed.
        s.conn
            .execute_batch(&format!(
                "DELETE FROM cohort_hour WHERE cohort >= {h};
                 DELETE FROM rollup_hour WHERE hour >= {h} AND metric IN (4, 10, 11);
                 UPDATE insights_state SET value = 0 WHERE key = 'imported';",
                h = H + 3
            ))
            .unwrap();
        let r = s.repair_rollups(repair_opts(H - 24)).unwrap();
        assert!(r.changed());
        assert!(r.cohorts_stored > 0 && r.cohorts_searchable > 0 && r.histograms > 0);
        assert_eq!(r.imported, Some((0, 6)));
        assert_eq!(cells(&s, 4), counted(&s, false));
        assert_eq!(cells(&s, 5), counted(&s, true));
        assert_eq!(metric_by_hour(&s, 4), good.2);
        assert_eq!(metric_by_hour(&s, 10), good.3);
        // Every repaired cohort's histogram holds its searchable count.
        for (h, _, _) in cells(&s, 5) {
            let n: i64 = s
                .rollup_sums(11, h, h + 1, RollupBy::Total)
                .unwrap()
                .iter()
                .map(|c| c.value)
                .sum();
            let want: i64 = cells(&s, 5).iter().filter(|c| c.0 == h).map(|c| c.2).sum();
            assert_eq!(n, want, "cohort {h}");
        }
        // A second run changes nothing.
        let again = s.repair_rollups(repair_opts(H - 24)).unwrap();
        assert!(!again.changed(), "{again:?}");
        // A deleted torrent lowers no count.
        let before = (cells(&s, 4), cells(&s, 5), metric_by_hour(&s, 10));
        s.delete_torrents(&[hash(10), hash(11)]).unwrap();
        let after = s.repair_rollups(repair_opts(H - 24)).unwrap();
        assert!(!after.changed(), "{after:?}");
        assert_eq!((cells(&s, 4), cells(&s, 5), metric_by_hour(&s, 10)), before);
    }

    #[test]
    fn repair_reaches_compacted_days_at_day_grain() {
        let mut s = store();
        ingest(&mut s, 40);
        let now = (DAY + 40) * 86_400;
        s.compact_rollups(opts(now)).unwrap();
        let day_cell = |s: &Store, stage: u8| -> i64 {
            s.conn
                .query_row(
                    "SELECT coalesce(sum(count), 0) FROM cohort_day WHERE day = ?1 AND stage = ?2",
                    params![DAY, stage],
                    |r| r.get(0),
                )
                .unwrap()
        };
        let good = (day_cell(&s, 4), day_cell(&s, 5));
        s.conn
            .execute_batch(&format!(
                "UPDATE cohort_day SET count = 0 WHERE day = {DAY};
                 DELETE FROM rollup_day WHERE day = {DAY} AND metric = 10;"
            ))
            .unwrap();
        // The hourly default doesn't reach it.
        let hourly = s.repair_rollups(repair_opts(DAY * 24 + 24)).unwrap();
        assert_eq!(hourly.cohorts_stored + hourly.cohorts_searchable, 0);
        let r = s.repair_rollups(repair_opts(H)).unwrap();
        assert!(r.hours_searchable > 0 && r.cohorts_stored > 0, "{r:?}");
        assert_eq!((day_cell(&s, 4), day_cell(&s, 5)), good);
        assert_eq!(
            s.rollup_day_sums(10, DAY, DAY + 1, RollupBy::Total)
                .unwrap()[0]
                .value,
            40
        );
        assert!(!s.repair_rollups(repair_opts(H)).unwrap().changed());
        // A day short by 5 with a straggler of 2 still to fold: repair
        // absorbs the straggler, so the next compaction can't add it twice.
        s.conn
            .execute_batch(&format!(
                "UPDATE cohort_day SET count = count - 5 WHERE day = {DAY} AND stage = 4
                     AND source = 1;
                 INSERT INTO cohort_hour (cohort, source, stage, reason, count)
                     VALUES ({h}, 1, 4, 0, 2);",
                h = H + 3
            ))
            .unwrap();
        assert!(s.repair_rollups(repair_opts(H)).unwrap().cohorts_stored > 0);
        assert_eq!(day_cell(&s, 4), good.0);
        s.compact_rollups(opts(now + 86_400)).unwrap();
        assert_eq!(day_cell(&s, 4), good.0, "the straggler isn't counted twice");
    }

    #[test]
    fn the_first_id_stored_since_is_found_by_bisection() {
        let s = store();
        assert_eq!(first_id_stored_since(&s.conn, 0).unwrap(), None);
        for (i, id) in [3, 4, 9, 20, 21, 50].into_iter().enumerate() {
            torrent(&s, id, 0, 100 * i as i64, None, None, 0);
        }
        let f = |t| first_id_stored_since(&s.conn, t).unwrap();
        assert_eq!(f(-5), Some(3));
        assert_eq!(f(0), Some(3));
        assert_eq!(f(1), Some(4));
        assert_eq!(f(150), Some(9));
        assert_eq!(f(300), Some(20));
        assert_eq!(f(500), Some(50));
        assert_eq!(f(501), None);
    }

    #[test]
    fn an_unclean_stop_is_a_start_with_no_stop_after_it() {
        let mut s = store();
        assert!(!s.stopped_uncleanly(1, 2).unwrap(), "a new store");
        let e = |at, kind| Event {
            at,
            kind,
            detail: String::new(),
        };
        for ev in [e(1, 1), e(2, 3)] {
            s.write(&crate::WriteBatch {
                events: &[ev],
                ..crate::WriteBatch::default()
            })
            .unwrap();
        }
        assert!(s.stopped_uncleanly(1, 2).unwrap());
        s.write(&crate::WriteBatch {
            events: &[e(3, 2)],
            ..crate::WriteBatch::default()
        })
        .unwrap();
        assert!(!s.stopped_uncleanly(1, 2).unwrap());
    }

    #[test]
    fn the_last_activity_is_the_newest_rollup_hour_or_torrent() {
        let mut s = store();
        assert_eq!(s.last_activity().unwrap(), None);
        torrent(&s, 5, 0, H * 3600 + 10, None, None, 0);
        assert_eq!(s.last_activity().unwrap(), Some(H * 3600 + 10));
        s.add_counters(&[c(H + 3, 12, 1, 0, 30)]).unwrap();
        assert_eq!(s.last_activity().unwrap(), Some((H + 4) * 3600));
    }

    fn imported(s: &Store) -> i64 {
        s.insights_state().unwrap().imported
    }

    /// `removed` over every hour, by a range scan (the trigger writes at
    /// the current hour).
    fn removed_all(s: &Store) -> Vec<(u32, i64)> {
        let mut stmt = s
            .conn
            .prepare("SELECT dim, sum(value) FROM rollup_hour WHERE metric = 15 GROUP BY dim")
            .unwrap();
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap()
    }

    #[test]
    fn deleting_a_searchable_torrent_is_a_removal_by_any_path() {
        let mut s = store();
        torrent(&s, 1, H * 3600, H * 3600, Some(0), Some(3), flags::IMPORTED);
        torrent(&s, 2, H * 3600, H * 3600, Some(0), None, 0);
        torrent(&s, 3, H * 3600, H * 3600, Some(0), Some(1), flags::IMPORTED);
        torrent(&s, 4, H * 3600, H * 3600, Some(0), Some(2), flags::IMPORTED);
        s.record_searchable(3, H * 3600 + 60, SEARCHABLE).unwrap();
        let flagged = |s: &Store| s.count_flagged_searchable(flags::IMPORTED).unwrap() as i64;
        assert_eq!((imported(&s), flagged(&s)), (2, 2));
        // Id 4 isn't searchable yet: deleting it changes neither.
        s.delete_torrents(&[hash(4)]).unwrap();
        assert_eq!(removed_all(&s), []);
        assert_eq!((imported(&s), flagged(&s)), (2, 2));
        // `delete_torrents`: an import.
        s.delete_torrents(&[hash(1)]).unwrap();
        assert_eq!(removed_all(&s), [(3, 1)]);
        assert_eq!((imported(&s), flagged(&s)), (1, 1));
        // A filter relabel that holds it back (`set_enrichments_at`).
        for id in [2, 3] {
            let hashes = [hash(id)];
            let refiled = Refiled {
                id,
                filtered: Filtered {
                    hashes: &hashes,
                    rule: "U-1",
                    action: 4,
                    held: Some(Held::Quarantine),
                    sealed: Some(b"sealed"),
                    evidence: &[],
                    at: 10,
                    expires_at: 20,
                    adult_hold: false,
                },
                flags: 0,
            };
            let g = s.class_gen().unwrap();
            s.set_enrichments_at(g, &[], &[refiled]).unwrap();
        }
        assert_eq!(removed_all(&s), [(1, 1), (3, 1), (UNCLASSIFIED, 1)]);
        assert_eq!((imported(&s), flagged(&s)), (0, 0));
    }

    /// Every read over the day tables and every compaction statement is
    /// a primary-key search, never a scan (§10.9: ≤ 50 ms whatever the
    /// index size).
    #[test]
    fn day_reads_and_folds_use_the_keys() {
        let mut s = store();
        fill(&mut s, 40);
        s.compact_rollups(opts((DAY + 50) * 86_400)).unwrap();
        let plan = |sql: &str, n: usize| -> String {
            let mut stmt = s
                .conn
                .prepare(&format!("EXPLAIN QUERY PLAN {sql}"))
                .unwrap();
            let params: Vec<i64> = (1..=n as i64).collect();
            stmt.query_map(rusqlite::params_from_iter(params), |r| {
                r.get::<_, String>(3)
            })
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap()
            .join("\n")
        };
        let src = include_str!("insights_days.rs");
        for (sql, n, want) in [
            (
                "WITH RECURSIVE d(x) AS (SELECT ?1 WHERE ?1 < ?2 UNION ALL SELECT x + 1 FROM d WHERE x + 1 < ?2)
                 SELECT r.day * 24, r.dim, 0, sum(r.value) FROM d CROSS JOIN rollup_day r
                 WHERE r.day = d.x AND r.metric = ?3 GROUP BY r.day, r.dim ORDER BY r.day, r.dim",
                3,
                "SEARCH r USING PRIMARY KEY (day=? AND metric=?)",
            ),
            (
                "WITH RECURSIVE d(x) AS (SELECT ?1 WHERE ?1 < ?2 UNION ALL SELECT x + 1 FROM d WHERE x + 1 < ?2)
                 SELECT r.day * 24, r.dim, r.key, sum(r.value) FROM d CROSS JOIN rollup_day r
                 WHERE r.day = d.x AND r.metric = ?3 GROUP BY r.day, r.dim, r.key
                 ORDER BY r.day, r.dim, r.key",
                3,
                "SEARCH r USING PRIMARY KEY (day=? AND metric=?)",
            ),
            (
                "SELECT (hour / 24) * 24 AS d, dim, key, sum(value) FROM rollup_hour
                 WHERE hour >= ?1 AND hour < ?2 AND metric = ?3 GROUP BY d, dim, key
                 ORDER BY d, dim, key",
                3,
                "SEARCH rollup_hour USING PRIMARY KEY (hour>? AND hour<?)",
            ),
            (
                "SELECT day * 24, source, stage, reason, count FROM cohort_day
                 WHERE day >= ?1 AND day < ?2 ORDER BY day, source, stage, reason",
                2,
                "SEARCH cohort_day USING PRIMARY KEY (day>? AND day<?)",
            ),
            (
                "SELECT (hour / 24) * 24 AS d, dim, 0, sum(value) FROM rollup_hour
                 WHERE hour >= ?1 AND hour < ?2 AND metric = ?3 GROUP BY d, dim ORDER BY d, dim",
                3,
                "SEARCH rollup_hour USING PRIMARY KEY (hour>? AND hour<?)",
            ),
            (
                "SELECT key, value FROM insights_state",
                0,
                "insights_state",
            ),
            (
                "SELECT DISTINCT metric FROM rollup_hour WHERE hour >= ?1 AND hour < ?2",
                2,
                "SEARCH rollup_hour USING PRIMARY KEY (hour>? AND hour<?)",
            ),
            (
                "DELETE FROM rollup_hour WHERE hour >= ?1 AND hour < ?2 AND metric = ?3",
                3,
                "SEARCH rollup_hour USING PRIMARY KEY (hour>? AND hour<?)",
            ),
            (
                "DELETE FROM cohort_hour WHERE cohort >= ?1 AND cohort < ?2",
                2,
                "SEARCH cohort_hour USING PRIMARY KEY (cohort>? AND cohort<?)",
            ),
            (
                "SELECT ?1, metric, dim, key, value FROM rollup_hour
                 WHERE metric = ?4 AND hour = (
                     SELECT max(hour) FROM rollup_hour
                     WHERE hour >= ?2 AND hour < ?3 AND metric = ?4)",
                4,
                "SEARCH rollup_hour USING PRIMARY KEY (hour=? AND metric=?)",
            ),
            (
                "SELECT DISTINCT hour / 24 FROM rollup_hour WHERE hour >= 0 AND hour < ?1",
                1,
                "SEARCH rollup_hour USING PRIMARY KEY (hour>? AND hour<?)",
            ),
            (
                "SELECT id, discovered_at, metadata_at, first_source, content_type, flags
                 FROM torrents WHERE id >= ?1 AND id < ?2",
                2,
                "SEARCH torrents USING INTEGER PRIMARY KEY (rowid>? AND rowid<?)",
            ),
            (
                "SELECT id, metadata_at FROM torrents WHERE id >= ?1 ORDER BY id LIMIT 1",
                1,
                "SEARCH torrents USING INTEGER PRIMARY KEY (rowid>?)",
            ),
        ] {
            let p = plan(sql, n);
            assert!(p.contains(want), "{sql}\n{p}");
            for t in ["rollup_hour", "rollup_day", "cohort_hour", "cohort_day", "torrents"] {
                assert!(!p.contains(&format!("SCAN {t}")), "{sql}\n{p}");
            }
        }
        // The fragments above are the module's own SQL: each is there
        // once in the code and once in this list.
        for frag in [
            "FROM d CROSS JOIN rollup_day r",
            "SELECT day * 24, source, stage, reason, count FROM cohort_day",
            "FROM torrents WHERE id >= ?1 AND id < ?2",
            "SELECT DISTINCT hour / 24 FROM rollup_hour WHERE hour >= 0 AND hour < ?1",
        ] {
            assert!(src.matches(frag).count() >= 2, "{frag}");
        }
    }

    /// The trigger's cost on a bulk delete (the brief's measurement):
    /// `cargo test -p lodestar-store --release -- --ignored trigger_cost
    /// --nocapture`.
    #[test]
    #[ignore]
    fn trigger_cost() {
        let run = |with: bool, cached: bool| -> Duration {
            let dir = tempfile::tempdir().unwrap();
            let mut s = Store::open(dir.path().join("t.db"), StoreConfig::default()).unwrap();
            if !with {
                s.conn
                    .execute_batch("DROP TRIGGER torrents_removed")
                    .unwrap();
            }
            let n = 100_000;
            let tx = s.conn.transaction().unwrap();
            for id in 1..=n {
                tx.execute(
                    "INSERT INTO torrents (id, info_hash, name, size, piece_len, piece_count,
                         file_count, info_size, flags, discovered_at, metadata_at, last_seen_at,
                         first_source, content_type)
                     VALUES (?1, ?2, 'x', 0, 0, 0, 0, 0, ?3, 0, 0, 0, 0, ?4)",
                    params![id, hash(id).to_vec(), (id % 5 == 0) as i64 * 32, id % 12],
                )
                .unwrap();
            }
            tx.commit().unwrap();
            s.record_index_watermark(1, n).unwrap();
            let hashes: Vec<[u8; 20]> = (1..=n).map(hash).collect();
            let t = Instant::now();
            if cached {
                let tx = s.conn.transaction().unwrap();
                {
                    let mut del = tx
                        .prepare_cached("DELETE FROM torrents WHERE id = ?1")
                        .unwrap();
                    for id in 1..=n {
                        del.execute([id]).unwrap();
                    }
                }
                tx.commit().unwrap();
            } else {
                s.delete_torrents(&hashes).unwrap();
            }
            t.elapsed()
        };
        for cached in [false, true] {
            let (mut without, mut with) = (Vec::new(), Vec::new());
            for _ in 0..5 {
                without.push(run(false, cached));
                with.push(run(true, cached));
            }
            without.sort();
            with.sort();
            println!(
                "{} of 100k, median of 5: without {:?}, with the trigger {:?}",
                if cached {
                    "a cached DELETE"
                } else {
                    "delete_torrents"
                },
                without[2],
                with[2]
            );
        }
    }
}
