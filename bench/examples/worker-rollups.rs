//! Rollup storage per year with remote workers' rows (WK-9b; owner
//! §1.6.4: ≤ 50 MB a year), at the plan's fleet size of 8 workers.
//!
//! A year of B11a's wide hour (`reporting-overhead`'s `wide_hour`, the
//! busy crawler's bound), alone, with a realistic fleet's rows and with
//! the most `Admit` lets through, compacted as the recorder does (30 days
//! hourly, the rest daily) and vacuumed. The Insights pages' time with the
//! same rows is `crates/api/tests/insights.rs`
//! `a_year_of_rollups_with_eight_workers_answers_within_budget`.
//!
//! `cargo run --release -p lodestar-bench --example worker-rollups`

use std::path::Path;
use std::time::Duration;

use lodestar_pipeline::metric;
use lodestar_store::rusqlite::{Connection, params};
use lodestar_store::{CompactOptions, Store, StoreConfig};

/// The plan's fleet size (WK-15's gate).
const WORKERS: u32 = 8;

type Rows = Vec<(u32, u32, i64, i64)>;

/// `reporting-overhead`'s `wide_hour`, rollup rows only.
fn wide_hour() -> Rows {
    let mut r = Vec::new();
    for source in 0..2 {
        for strategy in 0..3 {
            r.push((1, source, strategy, 900));
            r.push((2, source, strategy, 300));
        }
        r.push((3, source, 0, 2));
        r.push((4, source, 0, 120));
    }
    for tracker in 0..8 {
        for outcome in 0..3 {
            r.push((5, tracker, outcome, 40));
        }
    }
    r.extend((0..17).map(|k| (9, 0, k, 3)));
    for t in 0..12 {
        r.push((10, t, 0, 10));
        r.push((10, t, 1, 2));
    }
    r.extend((80..110).map(|b| (11, 0, b, 4)));
    r.extend((0..2).map(|d| (12, d, 0, 3600)));
    for m in 6..=8 {
        r.extend((0..12).map(|k| (m, 0, k, 5)));
    }
    r.push((13, 0, 0, 1_000_000));
    r.push((14, 3, 0, 300));
    r.extend((1..=10).map(|reason| (14, 6, reason, 20)));
    r.extend((0..12).map(|t| (15, t, 0, 1)));
    r
}

/// Each worker's `BY_WORKER` row for each fleet row, and WK-15's lease
/// counts (`WORKER_LEASES`: granted, marginal, committed, lost) for the
/// built-in worker and each remote one.
fn by_worker(fleet: &Rows) -> Rows {
    let mut r = fleet.clone();
    for w in 0..WORKERS {
        for &(m, d, k, v) in fleet {
            r.push((
                metric::BY_WORKER,
                100 + w,
                metric::by_worker_key(m, d, k),
                v,
            ));
        }
    }
    for w in std::iter::once(0).chain((0..WORKERS).map(|w| 100 + w)) {
        r.extend((0..4).map(|k| (metric::WORKER_LEASES, w, k, 5)));
    }
    r
}

/// A real fleet: every worker counts the same 22 rows an hour (2 sources
/// × 3 strategies rediscovered, 2 dropped, 4 lanes × 3 scrape results, 2
/// DHT scrapes).
fn realistic(_hour: i64) -> Rows {
    let mut c = Vec::new();
    for s in 0..2 {
        for k in 0..3 {
            c.push((metric::REDISCOVERED, s, k, 30));
        }
        c.push((metric::DROPPED, s, 0, 2));
    }
    for lane in 0..4 {
        for result in 0..3 {
            c.push((metric::SCRAPED, lane, result, 9));
        }
    }
    c.extend((0..2).map(|k| (metric::DHT_SCRAPES, 0, k, 9)));
    by_worker(&c)
}

/// The cap's worst case: the fleet's full 32 rows every hour, a new set
/// each day (the most the daily cap lets rotate), each counted by every
/// worker. No two days share a row, so compaction folds nothing across
/// days.
fn worst(hour: i64) -> Rows {
    // Keys 3–255: the wide hour has 0–2 for its own rows.
    let key = 3 + hour.div_euclid(24).rem_euclid(253);
    let rows = metric::FLEET_ROWS_PER_DAY as u32;
    by_worker(
        &(0..rows)
            .map(|dim| (metric::REDISCOVERED, dim, key, 7))
            .collect(),
    )
}

fn year(db: &Path, extra: &dyn Fn(i64) -> Rows) {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    let now_h = now / 3600;
    let base = wide_hour();
    {
        let mut c = Connection::open(db).unwrap();
        let tx = c.transaction().unwrap();
        let mut r = tx
            .prepare(
                "INSERT INTO rollup_hour (hour, metric, dim, key, value) VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT DO UPDATE SET value = value + excluded.value",
            )
            .unwrap();
        let mut w = tx
            .prepare("INSERT OR REPLACE INTO index_watermark_hour (hour, last_id) VALUES (?1, ?2)")
            .unwrap();
        for h in now_h - 365 * 24..now_h {
            for &(m, d, k, v) in base.iter().chain(extra(h).iter()) {
                r.execute(params![h, m, d, k, v]).unwrap();
            }
            w.execute(params![h, h * 100]).unwrap();
        }
        drop((r, w));
        tx.commit().unwrap();
    }
    let mut store = Store::open(db, StoreConfig::default()).unwrap();
    let r = store
        .compact_rollups(CompactOptions {
            now,
            keep_days: 30,
            hold_from_day: None,
            budget: Duration::from_secs(36_000),
            kind: &metric::aggregation,
        })
        .unwrap();
    assert!(r.done, "{r:?}");
}

/// Bytes a vacuumed store with the year takes, less an empty one's.
fn storage(name: &str, extra: &dyn Fn(i64) -> Rows) -> f64 {
    let dir = tempfile::tempdir().unwrap();
    let size = |file: &str, fill: bool| -> u64 {
        let db = dir.path().join(file);
        drop(Store::open(&db, StoreConfig::default()).unwrap());
        if fill {
            year(&db, extra);
        }
        let c = Connection::open(&db).unwrap();
        if fill {
            let count = |sql: &str| -> i64 { c.query_row(sql, [], |r| r.get(0)).unwrap() };
            eprintln!(
                "{name}: {} rows, {} of them BY_WORKER",
                count(
                    "SELECT (SELECT count(*) FROM rollup_hour) + (SELECT count(*) FROM rollup_day)"
                ),
                count(
                    "SELECT (SELECT count(*) FROM rollup_hour WHERE metric = 17)
                          + (SELECT count(*) FROM rollup_day WHERE metric = 17)"
                ),
            );
        }
        let out = dir.path().join(format!("{file}.vacuumed"));
        c.execute("VACUUM INTO ?1", [out.to_string_lossy()])
            .unwrap();
        std::fs::metadata(out).unwrap().len()
    };
    let bytes = size("year.db", true).saturating_sub(size("empty.db", false));
    bytes as f64 / 1e6
}

fn main() {
    let none = |_: i64| Rows::new();
    for (name, extra) in [
        ("the coordinator alone", &none as &dyn Fn(i64) -> Rows),
        ("8 workers, realistic", &realistic),
        ("8 workers, the caps' worst case", &worst),
    ] {
        println!(
            "{name}: {:.2} MB a year (budget 50 MB)",
            storage(name, extra)
        );
    }
}
