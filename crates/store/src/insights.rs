//! Insights rollups (architecture §7): small additive counter tables the
//! pipeline flushes inside the writer's transaction. The store treats
//! metric, dimension, stage and reason codes as opaque; their meanings are
//! defined by the pipeline and are persisted, so append only.

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
}
