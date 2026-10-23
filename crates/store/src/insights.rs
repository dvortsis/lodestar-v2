//! Insights rollups (architecture §7): small additive counter tables the
//! pipeline flushes inside the writer's transaction. The store treats
//! metric, dimension, stage and reason codes as opaque; their meanings are
//! defined by the pipeline and are persisted, so append only.

use std::collections::HashMap;

use rusqlite::{Connection, params};

use crate::sources::SourceId;
use crate::{Result, Store};

/// An increment to `rollup_hour`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Counter {
    /// Unix time / 3600.
    pub hour: i64,
    pub metric: u32,
    pub dim: u32,
    pub key: i64,
    pub value: i64,
}

/// An increment to the discovery-cohort funnel (`cohort_hour`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CohortCount {
    /// Discovery hour.
    pub cohort: i64,
    pub source: SourceId,
    pub stage: u8,
    /// 0 for a stage reached; otherwise the terminal loss reason.
    pub reason: u8,
    pub count: i64,
}

/// A trend annotation (restart, upgrade, config change, …).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Event {
    pub at: i64,
    pub kind: u32,
    pub detail: String,
}

pub(crate) fn write(
    conn: &Connection,
    counters: &[Counter],
    cohorts: &[CohortCount],
    events: &[Event],
) -> Result<()> {
    if !counters.is_empty() {
        let mut stmt = conn.prepare_cached(
            "INSERT INTO rollup_hour (hour, metric, dim, key, value) VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT DO UPDATE SET value = value + excluded.value",
        )?;
        for c in counters {
            stmt.execute(params![c.hour, c.metric, c.dim, c.key, c.value])?;
        }
    }
    if !cohorts.is_empty() {
        let mut stmt = conn.prepare_cached(
            "INSERT INTO cohort_hour (cohort, source, stage, reason, count)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT DO UPDATE SET count = count + excluded.count",
        )?;
        for c in cohorts {
            stmt.execute(params![
                c.cohort,
                c.source.get(),
                c.stage,
                c.reason,
                c.count
            ])?;
        }
    }
    if !events.is_empty() {
        let mut stmt =
            conn.prepare_cached("INSERT INTO events (at, kind, detail) VALUES (?1, ?2, ?3)")?;
        for e in events {
            stmt.execute(params![e.at, e.kind, e.detail])?;
        }
    }
    Ok(())
}

impl Store {
    /// One metric's counters for hours in `from..to`.
    pub fn rollup(&self, metric: u32, from: i64, to: i64) -> Result<Vec<Counter>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT hour, dim, key, value FROM rollup_hour
             WHERE hour >= ?1 AND hour < ?2 AND metric = ?3
             ORDER BY hour, dim, key",
        )?;
        let rows = stmt.query_map(params![from, to, metric], |r| {
            Ok(Counter {
                hour: r.get(0)?,
                metric,
                dim: r.get(1)?,
                key: r.get(2)?,
                value: r.get(3)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// The funnel for cohorts in `from..to`.
    pub fn cohorts(&self, from: i64, to: i64) -> Result<Vec<CohortCount>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT cohort, source, stage, reason, count FROM cohort_hour
             WHERE cohort >= ?1 AND cohort < ?2
             ORDER BY cohort, source, stage, reason",
        )?;
        let rows = stmt.query_map(params![from, to], |r| {
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

    /// Events with `from <= at < to`, oldest first.
    pub fn events(&self, from: i64, to: i64) -> Result<Vec<Event>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT at, kind, detail FROM events WHERE at >= ?1 AND at < ?2 ORDER BY at, id",
        )?;
        let rows = stmt.query_map(params![from, to], |r| {
            Ok(Event {
                at: r.get(0)?,
                kind: r.get(1)?,
                detail: r.get(2)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// The newest event of one of `kinds`, if any. It walks the table
    /// back from the newest row and stops at the first match.
    pub fn last_event(&self, kinds: &[u32]) -> Result<Option<Event>> {
        if kinds.is_empty() {
            return Ok(None);
        }
        let marks = vec!["?"; kinds.len()].join(", ");
        let mut stmt = self.conn.prepare(&format!(
            "SELECT at, kind, detail FROM events WHERE kind IN ({marks}) ORDER BY id DESC LIMIT 1"
        ))?;
        let mut rows = stmt.query(rusqlite::params_from_iter(kinds))?;
        Ok(match rows.next()? {
            Some(r) => Some(Event {
                at: r.get(0)?,
                kind: r.get(1)?,
                detail: r.get(2)?,
            }),
            None => None,
        })
    }
}

/// The codes [`Store::record_searchable`] writes under. They're the
/// pipeline's (`lodestar_pipeline::metric`, `lodestar_sources::Stage`):
/// the store keeps treating codes as opaque.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SearchableCodes {
    /// `cohort_hour.stage` for "searchable".
    pub stage: u8,
    /// `rollup_hour.metric` for torrents newly searchable, by event time
    /// (dim = content type code or [`UNCLASSIFIED`], key = 1 for imports).
    pub newly: u32,
    /// `rollup_hour.metric` for the time-to-searchable histogram, by
    /// discovery cohort (hour = cohort, key = [`log_bucket`] of seconds).
    pub delay: u32,
}

/// The `dim` of a torrent with no content type yet.
pub const UNCLASSIFIED: u32 = 255;

/// What one [`Store::record_searchable`] counted: the ids in
/// `(after, last_id]`, made searchable in `hour`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Searchable {
    pub after: i64,
    pub last_id: i64,
    pub hour: i64,
    /// Torrents in the range (ids with no row, deleted or never a
    /// torrent, aren't counted).
    pub torrents: u64,
    /// Of those, imported ([`crate::flags::IMPORTED`]).
    pub imported: u64,
    /// The earliest discovery cohort counted, if any: a reader that keeps
    /// old hours' sums learns what changed.
    pub first_cohort: Option<i64>,
}

/// What [`Store::rollup_sums`] groups by; the other columns of the
/// returned [`Counter`]s are 0.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RollupBy {
    /// Nothing: one total (no row when there's nothing).
    Total,
    Hour,
    Dim,
    Key,
    HourDim,
    HourKey,
}

/// Log buckets with consecutive edges a factor 1.1 apart, so a value
/// read back as its bucket's midpoint is within 5% (architecture §10.9.3:
/// "bucket resolution ≤ 10%"). Bucket 0 holds values up to 0; bucket
/// `b ≥ 1` holds `[1.1^(b-1), 1.1^b)`.
pub fn log_bucket(value: i64) -> i64 {
    if value <= 0 {
        return 0;
    }
    // The float estimate, then corrected against the exact edges, so a
    // value on an edge always lands in the bucket it starts.
    let mut b = ((value as f64).ln() / LOG_STEP.ln()).floor() as i64 + 1;
    while b > 1 && (value as f64) < log_bucket_bounds(b).0 {
        b -= 1;
    }
    while (value as f64) >= log_bucket_bounds(b).1 {
        b += 1;
    }
    b
}

const LOG_STEP: f64 = 1.1;

/// `[lo, hi)` of a [`log_bucket`]; `(0, 0)` for bucket 0 or below.
pub fn log_bucket_bounds(bucket: i64) -> (f64, f64) {
    if bucket <= 0 {
        return (0.0, 0.0);
    }
    let lo = LOG_STEP.powi((bucket - 1) as i32);
    (lo, lo * LOG_STEP)
}

/// Raises `index_watermark_hour` to `last_id` under `tx`, by the rules of
/// [`Store::record_index_watermark`]: both columns only grow, an id at or
/// below the highest recorded changes nothing, and an hour before the
/// latest is counted in the latest. Returns the hour written and the
/// highest id recorded before, or `None` if nothing changed.
///
/// At most `cap` ids above the highest recorded are covered: the id
/// written is returned too.
fn raise_watermark(
    tx: &Connection,
    hour: i64,
    last_id: i64,
    cap: i64,
) -> Result<Option<(i64, i64, i64)>> {
    let (max_hour, max_id): (i64, i64) = tx.query_row(
        "SELECT coalesce(max(hour), 0), coalesce(max(last_id), 0) FROM index_watermark_hour",
        [],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    if last_id <= max_id {
        return Ok(None);
    }
    let last_id = last_id.min(max_id.saturating_add(cap));
    let hour = hour.max(max_hour).max(1);
    tx.execute(
        "INSERT INTO index_watermark_hour (hour, last_id) VALUES (?1, ?2)
         ON CONFLICT (hour) DO UPDATE SET last_id = excluded.last_id",
        params![hour, last_id],
    )?;
    Ok(Some((hour, max_id, last_id)))
}

/// Ids one `record_searchable` transaction covers at most, so a large
/// range (the first start after an upgrade, a swap) never holds the write
/// lock for long.
const SEARCHABLE_CHUNK: i64 = 20_000;

/// Insights' write hook and read path (B11a): searchable time and the
/// Searchable stage, and the indexed reads the overview is built from.
impl Store {
    /// Records that every torrent id up to `last_id` was searchable at
    /// `at` (unix seconds), in `BEGIN IMMEDIATE` transactions of at most
    /// 20,000 ids each (so a large range never holds the write lock for
    /// long), each of which:
    /// - `index_watermark_hour` is raised as by
    ///   [`Store::record_index_watermark`];
    /// - each torrent with an id above the highest id recorded before (a
    ///   primary-key range: one commit's worth) is counted in
    ///   `codes.newly` at the hour written, by content type, imports
    ///   apart;
    /// - unless imported (imports never went through discovery, §10.9.3),
    ///   it's counted at `codes.stage` in its discovery cohort
    ///   (`discovered_at` hour, `first_source`), and its delay from
    ///   `discovered_at` to `at` in that cohort's `codes.delay` histogram.
    ///
    /// The lower end of the range is read from the table, not passed in,
    /// so a repeated call, or one after a call that failed, neither counts
    /// an id twice nor skips one. Ids at or below hour 0's (indexed before
    /// tracking) are never counted. A torrent with no `first_source`
    /// (stored before M1) has no cohort and is left out of the funnel.
    /// Returns `None` when `last_id` adds nothing.
    pub fn record_searchable(
        &mut self,
        last_id: i64,
        at: i64,
        codes: SearchableCodes,
    ) -> Result<Option<Searchable>> {
        if last_id < 0 {
            return Err(crate::Error::Invalid("last_id must not be negative"));
        }
        let mut total: Option<Searchable> = None;
        while let Some(r) = self.record_searchable_chunk(last_id, at, codes)? {
            total = Some(match total {
                None => r,
                Some(t) => Searchable {
                    after: t.after,
                    last_id: r.last_id,
                    hour: r.hour,
                    torrents: t.torrents + r.torrents,
                    imported: t.imported + r.imported,
                    first_cohort: match (t.first_cohort, r.first_cohort) {
                        (Some(a), Some(b)) => Some(a.min(b)),
                        (a, b) => a.or(b),
                    },
                },
            });
            if r.last_id >= last_id {
                break;
            }
        }
        Ok(total)
    }

    /// One transaction of [`Store::record_searchable`]: at most
    /// [`SEARCHABLE_CHUNK`] ids.
    fn record_searchable_chunk(
        &mut self,
        last_id: i64,
        at: i64,
        codes: SearchableCodes,
    ) -> Result<Option<Searchable>> {
        let tx = crate::begin_write(&mut self.conn)?;
        let Some((hour, after, last_id)) =
            raise_watermark(&tx, at.div_euclid(3600), last_id, SEARCHABLE_CHUNK)?
        else {
            return Ok(None);
        };
        let mut newly: HashMap<(u32, i64), i64> = HashMap::new();
        let mut cohorts: HashMap<(i64, u8), i64> = HashMap::new();
        let mut delays: HashMap<(i64, i64), i64> = HashMap::new();
        let (mut torrents, mut imported) = (0u64, 0u64);
        {
            let mut stmt = tx.prepare_cached(
                "SELECT discovered_at, first_source, content_type, flags FROM torrents
                 WHERE id > ?1 AND id <= ?2",
            )?;
            let mut rows = stmt.query(params![after, last_id])?;
            while let Some(r) = rows.next()? {
                let discovered_at: i64 = r.get(0)?;
                let source: Option<i64> = r.get(1)?;
                let kind: Option<i64> = r.get(2)?;
                let is_import = r.get::<_, i64>(3)? & crate::flags::IMPORTED != 0;
                torrents += 1;
                let dim = kind
                    .and_then(|k| u32::try_from(k).ok())
                    .filter(|&k| k < UNCLASSIFIED)
                    .unwrap_or(UNCLASSIFIED);
                *newly.entry((dim, i64::from(is_import))).or_default() += 1;
                if is_import {
                    imported += 1;
                    continue;
                }
                let Some(source) = source else { continue };
                let cohort = discovered_at.div_euclid(3600);
                *cohorts
                    .entry((cohort, SourceId::from_sql(source)?.get()))
                    .or_default() += 1;
                *delays
                    .entry((cohort, log_bucket(at - discovered_at)))
                    .or_default() += 1;
            }
        }
        let mut counters: Vec<Counter> = newly
            .into_iter()
            .map(|((dim, key), value)| Counter {
                hour,
                metric: codes.newly,
                dim,
                key,
                value,
            })
            .chain(delays.into_iter().map(|((cohort, key), value)| Counter {
                hour: cohort,
                metric: codes.delay,
                dim: 0,
                key,
                value,
            }))
            .collect();
        counters.sort_by_key(|c| (c.hour, c.metric, c.dim, c.key));
        let mut cohort_rows = Vec::with_capacity(cohorts.len());
        for ((cohort, source), count) in cohorts {
            cohort_rows.push(CohortCount {
                cohort,
                source: SourceId::from_sql(i64::from(source))?,
                stage: codes.stage,
                reason: 0,
                count,
            });
        }
        cohort_rows.sort_by_key(|c| (c.cohort, c.source));
        let first_cohort = cohort_rows.first().map(|c| c.cohort);
        write(&tx, &counters, &cohort_rows, &[])?;
        tx.commit()?;
        Ok(Some(Searchable {
            after,
            last_id,
            hour,
            torrents,
            imported,
            first_cohort,
        }))
    }

    /// Adds `counters` to `rollup_hour` in one transaction, outside the
    /// writer's batches (Insights' coverage counters).
    pub fn add_counters(&mut self, counters: &[Counter]) -> Result<()> {
        if counters.is_empty() {
            return Ok(());
        }
        let tx = crate::begin_write(&mut self.conn)?;
        write(&tx, counters, &[], &[])?;
        tx.commit()?;
        Ok(())
    }

    /// Runs `f` inside one read transaction, so every query in it sees
    /// the same snapshot. Inside a transaction already, it just runs `f`.
    pub fn read_snapshot<T>(&self, f: impl FnOnce(&Store) -> Result<T>) -> Result<T> {
        if !self.conn.is_autocommit() {
            return f(self);
        }
        self.conn.execute_batch("BEGIN DEFERRED")?;
        let out = f(self);
        // A read transaction has nothing to keep: end it either way.
        let end = self
            .conn
            .execute_batch(if out.is_ok() { "COMMIT" } else { "ROLLBACK" });
        let out = out?;
        end?;
        Ok(out)
    }

    /// Sums of one metric over hours in `from..to`, grouped `by`, in
    /// order. One primary-key seek per hour (`hour = ? AND metric = ?`),
    /// so the cost follows the hours and that metric's rows, not every
    /// metric's.
    pub fn rollup_sums(
        &self,
        metric: u32,
        from: i64,
        to: i64,
        by: RollupBy,
    ) -> Result<Vec<Counter>> {
        let (cols, group) = match by {
            RollupBy::Total => ("0, 0, 0", ""),
            RollupBy::Hour => ("r.hour, 0, 0", "GROUP BY r.hour ORDER BY r.hour"),
            RollupBy::Dim => ("0, r.dim, 0", "GROUP BY r.dim ORDER BY r.dim"),
            RollupBy::Key => ("0, 0, r.key", "GROUP BY r.key ORDER BY r.key"),
            RollupBy::HourDim => (
                "r.hour, r.dim, 0",
                "GROUP BY r.hour, r.dim ORDER BY r.hour, r.dim",
            ),
            RollupBy::HourKey => (
                "r.hour, 0, r.key",
                "GROUP BY r.hour, r.key ORDER BY r.hour, r.key",
            ),
        };
        // `CROSS JOIN` keeps the hour list as the outer loop.
        let sql = format!(
            "WITH RECURSIVE h(x) AS (
                 SELECT ?1 WHERE ?1 < ?2
                 UNION ALL SELECT x + 1 FROM h WHERE x + 1 < ?2)
             SELECT {cols}, sum(r.value) FROM h CROSS JOIN rollup_hour r
             WHERE r.hour = h.x AND r.metric = ?3 {group}"
        );
        let mut stmt = self.conn.prepare_cached(&sql)?;
        let rows = stmt.query_map(params![from, to, metric], |r| {
            Ok(Counter {
                hour: r.get(0)?,
                metric,
                dim: r.get(1)?,
                key: r.get(2)?,
                value: r.get::<_, Option<i64>>(3)?.unwrap_or(0),
            })
        })?;
        let out: Vec<Counter> = rows.collect::<rusqlite::Result<_>>()?;
        // An aggregate without GROUP BY returns one row even over nothing.
        Ok(if by == RollupBy::Total {
            out.into_iter().filter(|c| c.value != 0).collect()
        } else {
            out
        })
    }

    /// The funnel for cohorts in `from..to`, summed over buckets of
    /// `grain` hours (aligned to multiples of `grain`; 24 gives UTC days):
    /// each row's `cohort` is its bucket's first hour.
    pub fn cohort_sums(&self, from: i64, to: i64, grain: i64) -> Result<Vec<CohortCount>> {
        let grain = grain.max(1);
        let mut stmt = self.conn.prepare_cached(
            "SELECT (cohort / ?3) * ?3 AS b, source, stage, reason, sum(count) FROM cohort_hour
             WHERE cohort >= ?1 AND cohort < ?2
             GROUP BY b, source, stage, reason ORDER BY b, source, stage, reason",
        )?;
        let rows = stmt.query_map(params![from.max(0), to, grain], |r| {
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

    /// The earliest hour any rollup or cohort row holds, if any: the
    /// start of the "all" range. Two primary-key ends.
    pub fn first_rollup_hour(&self) -> Result<Option<i64>> {
        Ok(self.conn.query_row(
            "SELECT min(coalesce((SELECT min(hour) FROM rollup_hour WHERE hour > 0), 1 << 62),
                        coalesce((SELECT min(cohort) FROM cohort_hour WHERE cohort > 0), 1 << 62))",
            [],
            |r| r.get::<_, i64>(0),
        )?)
        .map(|h| (h < 1 << 62).then_some(h))
    }

    /// The newest event of one of `kinds` before `at`, if any (the state
    /// a range starts in: paused or running). It walks the events back
    /// from `at`.
    pub fn last_event_before(&self, kinds: &[u32], at: i64) -> Result<Option<Event>> {
        if kinds.is_empty() {
            return Ok(None);
        }
        let marks = vec!["?"; kinds.len()].join(", ");
        let mut stmt = self.conn.prepare(&format!(
            "SELECT at, kind, detail FROM events WHERE at < ? AND kind IN ({marks})
             ORDER BY at DESC, id DESC LIMIT 1"
        ))?;
        let params = std::iter::once(at).chain(kinds.iter().map(|&k| i64::from(k)));
        let mut rows = stmt.query(rusqlite::params_from_iter(params))?;
        Ok(match rows.next()? {
            Some(r) => Some(Event {
                at: r.get(0)?,
                kind: r.get(1)?,
                detail: r.get(2)?,
            }),
            None => None,
        })
    }

    /// How many torrents up to the highest id `index_watermark_hour`
    /// records (searchable) have `flag` set. A scan of `torrents`: for a
    /// count taken once at start (Insights' imported total), never on the
    /// read path.
    pub fn count_flagged_searchable(&self, flag: i64) -> Result<u64> {
        Ok(self.conn.query_row(
            "SELECT count(*) FROM torrents WHERE flags & ?1 != 0
             AND id <= (SELECT coalesce(max(last_id), 0) FROM index_watermark_hour)",
            [flag],
            |r| r.get::<_, i64>(0),
        )? as u64)
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;
    use crate::flags;

    const CODES: SearchableCodes = SearchableCodes {
        stage: 5,
        newly: 10,
        delay: 11,
    };
    const H: i64 = 490_000;

    fn store() -> Store {
        Store::open_in_memory(crate::StoreConfig::default()).unwrap()
    }

    /// A torrent row with what `record_searchable` reads.
    fn torrent(
        s: &Store,
        id: i64,
        discovered_at: i64,
        source: Option<i64>,
        kind: Option<i64>,
        fl: i64,
    ) {
        s.conn
            .execute(
                "INSERT INTO torrents (id, info_hash, name, size, piece_len, piece_count,
                     file_count, info_size, flags, discovered_at, metadata_at, last_seen_at,
                     first_source, content_type)
                 VALUES (?1, ?2, 'x', 0, 0, 0, 0, 0, ?3, ?4, ?4, ?4, ?5, ?6)",
                params![
                    id,
                    id.to_be_bytes().repeat(3)[..20].to_vec(),
                    fl,
                    discovered_at,
                    source,
                    kind
                ],
            )
            .unwrap();
    }

    fn searchable_cohorts(s: &Store) -> Vec<(i64, i64, i64)> {
        let mut stmt = s
            .conn
            .prepare(
                "SELECT cohort, source, count FROM cohort_hour WHERE stage = 5 AND reason = 0
                 ORDER BY cohort, source",
            )
            .unwrap();
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap()
    }

    /// The independent count: every non-imported torrent with a source
    /// above hour 0's id and up to the watermark, by cohort and source.
    fn counted_by_sql(s: &Store) -> Vec<(i64, i64, i64)> {
        let mut stmt = s
            .conn
            .prepare(
                "SELECT discovered_at / 3600, first_source, count(*) FROM torrents
                 WHERE flags & ?1 = 0 AND first_source IS NOT NULL
                   AND id > (SELECT last_id FROM index_watermark_hour WHERE hour = 0)
                   AND id <= (SELECT max(last_id) FROM index_watermark_hour)
                 GROUP BY 1, 2 ORDER BY 1, 2",
            )
            .unwrap();
        stmt.query_map([flags::IMPORTED], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap()
    }

    /// Newly searchable per hour, against each id's first covering
    /// watermark row (architecture §10.17).
    fn newly_by_sql(s: &Store) -> Vec<(i64, i64)> {
        let mut stmt = s
            .conn
            .prepare(
                "SELECT (SELECT hour FROM index_watermark_hour w WHERE w.last_id >= t.id
                         ORDER BY w.last_id LIMIT 1) AS h, count(*)
                 FROM torrents t WHERE h > 0 GROUP BY h ORDER BY h",
            )
            .unwrap();
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap()
    }

    fn newly_rolled(s: &Store) -> Vec<(i64, i64)> {
        s.rollup_sums(CODES.newly, H - 10, H + 100, RollupBy::Hour)
            .unwrap()
            .into_iter()
            .map(|c| (c.hour, c.value))
            .collect()
    }

    #[test]
    fn searchable_counts_reconcile_and_repeat_nothing() {
        let mut s = store();
        // Ids 1–2 were indexed before tracking (hour 0).
        torrent(&s, 1, H * 3600, Some(0), Some(0), 0);
        torrent(&s, 2, H * 3600, Some(0), Some(0), 0);
        s.conn
            .execute(
                "UPDATE index_watermark_hour SET last_id = 2 WHERE hour = 0",
                [],
            )
            .unwrap();
        torrent(&s, 3, H * 3600 + 10, Some(0), Some(0), 0);
        torrent(&s, 4, (H + 1) * 3600, Some(1), None, 0);
        torrent(&s, 5, (H + 1) * 3600, Some(1), Some(3), flags::IMPORTED);
        torrent(&s, 6, (H + 1) * 3600, None, Some(3), 0);
        // Id 7 is missing (an unverified item's id, or deleted).
        torrent(&s, 8, (H + 2) * 3600, Some(0), Some(11), 0);

        let at = (H + 2) * 3600 + 100;
        let r = s.record_searchable(6, at, CODES).unwrap().unwrap();
        assert_eq!(
            r,
            Searchable {
                after: 2,
                last_id: 6,
                hour: H + 2,
                torrents: 4,
                imported: 1,
                first_cohort: Some(H),
            }
        );
        // The same range again, or a lower one: nothing.
        assert_eq!(s.record_searchable(6, at, CODES).unwrap(), None);
        assert_eq!(s.record_searchable(4, at + 5, CODES).unwrap(), None);
        let before = (searchable_cohorts(&s), newly_rolled(&s));
        assert_eq!(s.record_searchable(6, at + 9, CODES).unwrap(), None);
        assert_eq!((searchable_cohorts(&s), newly_rolled(&s)), before);
        // A clock set back counts in the latest hour.
        let r = s.record_searchable(8, at - 7200, CODES).unwrap().unwrap();
        assert_eq!((r.after, r.hour, r.torrents), (6, H + 2, 1));

        assert_eq!(searchable_cohorts(&s), counted_by_sql(&s));
        assert_eq!(
            searchable_cohorts(&s),
            [(H, 0, 1), (H + 1, 1, 1), (H + 2, 0, 1)]
        );
        assert_eq!(newly_rolled(&s), newly_by_sql(&s));
        // By content type: unclassified apart, imports under key 1.
        let by_dim: Vec<_> = s
            .rollup_sums(CODES.newly, H - 10, H + 100, RollupBy::Dim)
            .unwrap()
            .into_iter()
            .map(|c| (c.dim, c.value))
            .collect();
        assert_eq!(by_dim, [(0, 1), (3, 2), (11, 1), (UNCLASSIFIED, 1)]);
        let imports: i64 = s
            .rollup_sums(CODES.newly, H - 10, H + 100, RollupBy::Key)
            .unwrap()
            .iter()
            .filter(|c| c.key == 1)
            .map(|c| c.value)
            .sum();
        assert_eq!(imports, 1);
        // Delays per cohort: id 3 waited 2 h less 10 s… plus 100 s.
        let delays = s.rollup_sums(CODES.delay, H, H + 1, RollupBy::Key).unwrap();
        assert_eq!(delays.len(), 1);
        let (lo, hi) = log_bucket_bounds(delays[0].key);
        assert!((lo..hi).contains(&((2 * 3600 + 90) as f64)));
        assert_eq!(s.index_watermarks(1, H + 10).unwrap(), [(0, 2), (H + 2, 8)]);
        assert!(s.record_searchable(-1, at, CODES).is_err());
    }

    /// A range wider than one transaction's chunk is counted in full.
    #[test]
    fn a_wide_range_is_recorded_in_chunks() {
        let mut s = store();
        let n = SEARCHABLE_CHUNK * 2 + 7;
        let tx = s.conn.transaction().unwrap();
        for id in 1..=n {
            tx.execute(
                "INSERT INTO torrents (id, info_hash, name, size, piece_len, piece_count,
                     file_count, info_size, flags, discovered_at, metadata_at, last_seen_at,
                     first_source, content_type)
                 VALUES (?1, ?2, 'x', 0, 0, 0, 0, 0, 0, ?3, ?3, ?3, 0, 0)",
                params![id, id.to_be_bytes().repeat(3)[..20].to_vec(), H * 3600],
            )
            .unwrap();
        }
        tx.commit().unwrap();
        let r = s
            .record_searchable(n, H * 3600 + 60, CODES)
            .unwrap()
            .unwrap();
        assert_eq!((r.after, r.last_id, r.torrents), (0, n, n as u64));
        assert_eq!(r.first_cohort, Some(H));
        assert_eq!(searchable_cohorts(&s), [(H, 0, n)]);
        assert_eq!(s.index_watermarks(H, H).unwrap(), [(0, 0), (H, n)]);
    }

    #[test]
    fn searchable_counts_follow_many_commits() {
        let mut s = store();
        let mut id = 0;
        for commit in 0..40i64 {
            for k in 0..(commit % 7) {
                id += 1;
                let disc = (H + commit / 3) * 3600 + k * 60;
                torrent(
                    &s,
                    id,
                    disc,
                    Some(k % 3),
                    Some(k % 12),
                    if k == 5 { flags::IMPORTED } else { 0 },
                );
            }
            s.record_searchable(id, (H + commit / 2) * 3600 + 30, CODES)
                .unwrap();
        }
        assert_eq!(searchable_cohorts(&s), counted_by_sql(&s));
        assert_eq!(newly_rolled(&s), newly_by_sql(&s));
        let total: i64 = newly_rolled(&s).iter().map(|r| r.1).sum();
        assert_eq!(total, id);
    }

    #[derive(Debug, Clone)]
    enum Op {
        Raise(i64, i64),
    }

    proptest! {
        /// `record_searchable`'s watermark upsert keeps
        /// `record_index_watermark`'s rules exactly.
        #[test]
        fn watermark_rules_match(ops in proptest::collection::vec(
            (1i64..20, 0i64..60).prop_map(|(h, id)| Op::Raise(h, id)), 0..30)
        ) {
            let (mut a, mut b) = (store(), store());
            for Op::Raise(hour, id) in ops {
                let wrote = a.record_index_watermark(hour, id).unwrap();
                let r = b.record_searchable(id, hour * 3600 + 59, CODES).unwrap();
                prop_assert_eq!(wrote, r.is_some());
            }
            prop_assert_eq!(a.index_watermarks(0, 100).unwrap(), b.index_watermarks(0, 100).unwrap());
        }

        /// Every value falls inside its bucket, buckets never go down,
        /// and a bucket's midpoint is within 5% of anything in it.
        #[test]
        fn log_buckets_hold_their_values(v in 1i64..(1 << 40)) {
            let b = log_bucket(v);
            let (lo, hi) = log_bucket_bounds(b);
            prop_assert!(lo <= v as f64 && (v as f64) < hi, "{} in [{}, {})", v, lo, hi);
            prop_assert!(log_bucket(v + 1) >= b);
            let mid = (lo * hi).sqrt();
            prop_assert!(((mid - v as f64) / v as f64).abs() <= 0.05);
        }
    }

    #[test]
    fn log_bucket_edges() {
        assert_eq!(log_bucket(0), 0);
        assert_eq!(log_bucket(-5), 0);
        assert_eq!(log_bucket(1), 1);
        assert_eq!(log_bucket_bounds(0), (0.0, 0.0));
        assert!(log_bucket(i64::MAX) > log_bucket(1 << 40));
    }

    #[test]
    fn sums_group_as_asked() {
        let mut s = store();
        let c = |hour, metric, dim, key, value| Counter {
            hour,
            metric,
            dim,
            key,
            value,
        };
        s.add_counters(&[
            c(H, 4, 0, 0, 3),
            c(H, 4, 1, 0, 2),
            c(H + 2, 4, 0, 1, 5),
            c(H + 2, 9, 0, 0, 100),
            c(H + 5, 4, 0, 0, 7),
        ])
        .unwrap();
        s.add_counters(&[c(H, 4, 0, 0, 1)]).unwrap();
        let pick = |by| -> Vec<(i64, u32, i64, i64)> {
            s.rollup_sums(4, H, H + 5, by)
                .unwrap()
                .into_iter()
                .map(|c| (c.hour, c.dim, c.key, c.value))
                .collect()
        };
        assert_eq!(pick(RollupBy::Total), [(0, 0, 0, 11)]);
        assert_eq!(pick(RollupBy::Hour), [(H, 0, 0, 6), (H + 2, 0, 0, 5)]);
        assert_eq!(pick(RollupBy::Dim), [(0, 0, 0, 9), (0, 1, 0, 2)]);
        assert_eq!(pick(RollupBy::Key), [(0, 0, 0, 6), (0, 0, 1, 5)]);
        assert_eq!(
            pick(RollupBy::HourDim),
            [(H, 0, 0, 4), (H, 1, 0, 2), (H + 2, 0, 0, 5)]
        );
        assert_eq!(pick(RollupBy::HourKey), [(H, 0, 0, 6), (H + 2, 0, 1, 5)]);
        assert!(
            s.rollup_sums(4, H + 6, H + 9, RollupBy::Total)
                .unwrap()
                .is_empty()
        );
        assert!(
            s.rollup_sums(4, H + 9, H, RollupBy::Hour)
                .unwrap()
                .is_empty()
        );
        assert_eq!(s.first_rollup_hour().unwrap(), Some(H));
    }

    #[test]
    fn cohorts_sum_into_buckets() {
        let mut s = store();
        let src = SourceId::from_sql(0).unwrap();
        let row = |cohort, stage, reason, count| CohortCount {
            cohort,
            source: src,
            stage,
            reason,
            count,
        };
        let day = H / 24 * 24;
        s.write(&crate::WriteBatch {
            cohorts: &[
                row(day + 1, 1, 0, 10),
                row(day + 23, 1, 0, 5),
                row(day + 24, 1, 0, 2),
                row(day + 2, 6, 3, 1),
            ],
            ..crate::WriteBatch::default()
        })
        .unwrap();
        let got: Vec<_> = s
            .cohort_sums(day, day + 48, 24)
            .unwrap()
            .into_iter()
            .map(|c| (c.cohort, c.stage, c.reason, c.count))
            .collect();
        assert_eq!(got, [(day, 1, 0, 15), (day, 6, 3, 1), (day + 24, 1, 0, 2)]);
        assert_eq!(s.cohort_sums(day, day + 48, 1).unwrap().len(), 4);
        assert_eq!(s.first_rollup_hour().unwrap(), Some(day + 1));
        assert_eq!(store().first_rollup_hour().unwrap(), None);
    }

    #[test]
    fn events_before_a_time() {
        let mut s = store();
        let e = |at, kind| Event {
            at,
            kind,
            detail: String::new(),
        };
        s.write(&crate::WriteBatch {
            events: &[e(10, 3), e(20, 1), e(30, 4), e(40, 3)],
            ..crate::WriteBatch::default()
        })
        .unwrap();
        let at = |t| {
            s.last_event_before(&[3, 4], t)
                .unwrap()
                .map(|e| (e.at, e.kind))
        };
        assert_eq!(at(10), None);
        assert_eq!(at(25), Some((10, 3)));
        assert_eq!(at(31), Some((30, 4)));
        assert_eq!(at(1000), Some((40, 3)));
        assert_eq!(s.last_event_before(&[], 1000).unwrap(), None);
    }

    #[test]
    fn a_snapshot_reads_one_state() {
        let s = store();
        torrent(&s, 1, 0, None, None, flags::IMPORTED);
        torrent(&s, 2, 0, None, None, 0);
        let n = |s: &Store| {
            s.read_snapshot(|s| s.read_snapshot(|s| s.count_flagged_searchable(flags::IMPORTED)))
                .unwrap()
        };
        assert_eq!(n(&s), 0, "nothing searchable yet");
        s.conn
            .execute("UPDATE index_watermark_hour SET last_id = 2", [])
            .unwrap();
        assert_eq!(n(&s), 1);
        assert!(s.conn.is_autocommit(), "the transaction ended");
        let failed: Result<()> = s.read_snapshot(|_| Err(crate::Error::Invalid("x")));
        assert!(failed.is_err());
        assert!(s.conn.is_autocommit());
    }

    /// Every read the overview makes is an indexed seek, never a scan of
    /// a table (§10.9: ≤ 50 ms whatever the index size).
    #[test]
    fn overview_reads_use_indexes() {
        let s = store();
        let plan = |sql: &str, params: &[i64]| -> String {
            let mut stmt = s
                .conn
                .prepare(&format!("EXPLAIN QUERY PLAN {sql}"))
                .unwrap();
            let rows = stmt
                .query_map(rusqlite::params_from_iter(params), |r| {
                    r.get::<_, String>(3)
                })
                .unwrap();
            rows.collect::<rusqlite::Result<Vec<_>>>()
                .unwrap()
                .join("\n")
        };
        let rollup = "WITH RECURSIVE h(x) AS (SELECT ?1 WHERE ?1 < ?2 UNION ALL SELECT x + 1 FROM h WHERE x + 1 < ?2)
             SELECT r.hour, r.dim, 0, sum(r.value) FROM h CROSS JOIN rollup_hour r
             WHERE r.hour = h.x AND r.metric = ?3 GROUP BY r.hour, r.dim";
        let cohorts =
            "SELECT (cohort / ?3) * ?3 AS b, source, stage, reason, sum(count) FROM cohort_hour
             WHERE cohort >= ?1 AND cohort < ?2 GROUP BY b, source, stage, reason";
        let events =
            "SELECT at, kind, detail FROM events WHERE at >= ?1 AND at < ?2 ORDER BY at, id";
        let before = "SELECT at, kind, detail FROM events WHERE at < ? AND kind IN (?, ?)
             ORDER BY at DESC, id DESC LIMIT 1";
        let watermarks =
            "SELECT hour, last_id FROM index_watermark_hour WHERE hour BETWEEN ?1 AND ?2";
        let first =
            "SELECT min(coalesce((SELECT min(hour) FROM rollup_hour WHERE hour > 0), 1 << 62),
                        coalesce((SELECT min(cohort) FROM cohort_hour WHERE cohort > 0), 1 << 62))";
        for (sql, params, want) in [
            (
                rollup,
                &[1, 2, 3][..],
                "SEARCH r USING PRIMARY KEY (hour=? AND metric=?)",
            ),
            (
                cohorts,
                &[1, 2, 24][..],
                "SEARCH cohort_hour USING PRIMARY KEY (cohort>? AND cohort<?)",
            ),
            (
                events,
                &[1, 2][..],
                "SEARCH events USING INDEX events_at (at>? AND at<?)",
            ),
            (
                before,
                &[1, 3, 4][..],
                "SEARCH events USING INDEX events_at (at<?)",
            ),
            (
                watermarks,
                &[1, 2][..],
                "SEARCH index_watermark_hour USING INTEGER PRIMARY KEY (rowid>? AND rowid<?)",
            ),
            (
                first,
                &[][..],
                "SEARCH rollup_hour USING PRIMARY KEY (hour>?)",
            ),
        ] {
            let p = plan(sql, params);
            assert!(p.contains(want), "{sql}\n{p}");
            for table in [
                "rollup_hour",
                "cohort_hour",
                "events",
                "torrents",
                "index_watermark_hour",
            ] {
                assert!(!p.contains(&format!("SCAN {table}")), "{sql}\n{p}");
            }
        }
        // The SQL above is the store's own.
        let src = include_str!("insights.rs");
        for frag in [
            "SELECT (cohort / ?3) * ?3 AS b",
            "ORDER BY at DESC, id DESC LIMIT 1",
            "CROSS JOIN rollup_hour r",
        ] {
            assert!(src.contains(frag));
        }
    }
}
