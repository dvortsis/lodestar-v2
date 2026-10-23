//! `lodestar rollup --repair`: recounts the Insights rollups that the
//! store's own data can rebuild (architecture §7 "Insights rollups"),
//! with the server stopped.
//!
//! It's offline, like `vacuum`: a recount racing the live writer's
//! pending increments would count them twice, so it takes the store lock
//! ([`StoreLock`]) and refuses (exit 3) while anything has the store open.
//! The server runs the same repair by itself, over the last 72 hours,
//! when it starts after an unclean stop.
//!
//! **What it rebuilds,** from `torrents` and `index_watermark_hour`, for
//! the discovery cohorts and hours in its window: Stored per cohort and
//! source, newly stored per hour, and everything the searchable-time
//! recorder writes (newly searchable per hour, Searchable per cohort, the
//! time-to-searchable histogram); and the imported total. **It only
//! raises counts:** deletions since lower a recount, and final cohorts
//! never change.
//!
//! **What no repair can rebuild:** counts from before storage (harvested,
//! peer found, fetched, losses, keys reported, drops), coverage, gauges
//! (the searchable count's hourly snapshots), and torrents indexed before
//! searchable time was tracked. After compaction, hourly detail older
//! than 30 days is gone: those days are repaired at daily grain.

use lodestar_store::{RepairOptions, Repaired};

use super::lock::StoreLock;
use super::util::{
    Failure, Outcome, failed, format_count, refuse_during_update_rollback, unix_now,
};
use crate::config::Config;

/// The window: the hourly 30 days by default, or the last `since_days`.
pub fn run(config: &Config, since_days: Option<u32>) -> Outcome {
    let r = repair(config, since_days)?;
    let days = since_days.unwrap_or(30);
    if !r.changed() {
        println!(
            "The Insights rollups of the last {days} days match the store; nothing changed \
             ({} torrents read).",
            format_count(r.scanned)
        );
        return Ok(());
    }
    println!(
        "Repaired the Insights rollups of the last {days} days ({} torrents read):",
        format_count(r.scanned)
    );
    for (n, what) in [
        (r.cohorts_stored, "discovery cohorts' stored counts raised"),
        (
            r.cohorts_searchable,
            "discovery cohorts' searchable counts raised",
        ),
        (r.histograms, "time-to-searchable histograms rebuilt"),
        (r.hours_stored, "hours or days of newly stored raised"),
        (
            r.hours_searchable,
            "hours or days of newly searchable raised",
        ),
    ] {
        if n > 0 {
            println!("  {} {what}", format_count(n));
        }
    }
    if let Some((before, after)) = r.imported {
        println!(
            "  imported total corrected from {} to {}",
            format_count(before.max(0) as u64),
            format_count(after.max(0) as u64)
        );
    }
    Ok(())
}

pub fn repair(config: &Config, since_days: Option<u32>) -> Outcome<Repaired> {
    refuse_during_update_rollback(&config.data_dir)?;
    let db = config.db_path();
    let lock = StoreLock::acquire(&db)?;
    let since_hour = unix_now().div_euclid(3600) - i64::from(since_days.unwrap_or(30)) * 24;
    let r = lodestar_store::repair_rollups(
        lock.conn(),
        RepairOptions {
            codes: crate::insights::REPAIR_CODES,
            since_hour,
            recount_imported: true,
        },
    )
    .map_err(|e| match e {
        lodestar_store::Error::Corrupt(_) => Failure::Failed(format!(
            "{} isn't upgraded to this version yet: start Lodestar once, stop it, then run \
             the repair",
            db.display()
        )),
        e => Failure::Failed(format!("repairing the rollups: {e}")),
    });
    let released = lock.release();
    let r = r?;
    released.map_err(failed("unlocking the store"))?;
    Ok(r)
}

/// `--since <N>d`: whole days, 1 to 7,320 (20 years).
pub fn parse_days(s: &str) -> Result<u32, String> {
    let n = s
        .strip_suffix('d')
        .ok_or_else(|| "give a number of days, like 90d".to_string())?;
    match n.parse::<u32>() {
        Ok(n @ 1..=7320) => Ok(n),
        _ => Err("give 1d to 7320d".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn days_parse() {
        assert_eq!(parse_days("90d"), Ok(90));
        assert_eq!(parse_days("1d"), Ok(1));
        assert_eq!(parse_days("7320d"), Ok(7320));
        for bad in ["0d", "7321d", "90", "d", "-3d", "3h", "3.5d", ""] {
            assert!(parse_days(bad).is_err(), "{bad}");
        }
    }
}
