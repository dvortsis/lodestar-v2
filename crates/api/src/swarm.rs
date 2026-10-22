//! Swarm health in API responses (§6, §10.5, §10.12), under the field
//! names of the UI contract (`docs/metrics.md` §7): counts on every hit,
//! and the full picture (per tracker, DHT estimate, history) on the
//! detail view.
//!
//! "Never scraped" is `null`, never 0 (§10.12: null ≠ zero); the DHT
//! figure is an estimate and says so.

use std::time::Duration;

use lodestar_store::{Col, HotColumns, NEVER, Store, SwarmRow, TorrentRow, swarm_flags, trend};
use serde_json::{Value, json};

/// What the API tells the rest of the process: interest in torrents (to
/// scrape them sooner) and the searches it served (Serve rollups,
/// §10.9.5). Implemented by the binary; a no-op without one.
pub trait ApiEvents: Send + Sync + std::fmt::Debug {
    /// Someone opened this torrent, or saw it at the top of a search.
    fn interest(&self, id: i64);
    /// A search ran: how long it took and how many matched (`None` if it
    /// failed).
    fn searched(&self, took: Duration, total: Option<usize>);
}

/// Hits marked as interesting per search: what's on screen first.
pub const INTEREST_TOP: usize = 10;

/// Seconds in a history day.
const DAY: i64 = 86_400;

/// The swarm fields of a torrent summary (`docs/metrics.md` §7.1), on
/// search hits and the detail view alike (blocking).
///
/// They come from the stored swarm row, which is never behind the hot
/// columns, so the six nullable counts are null together.
pub fn summary(
    store: &Store,
    hot: Option<&HotColumns>,
    row: &TorrentRow,
    v: &mut Value,
) -> lodestar_store::Result<Option<SwarmRow>> {
    let swarm = store.swarm(row.id)?;
    // A row with nothing checked and no DHT estimate is a first scrape
    // nobody answered: still "never scraped".
    let answered = swarm
        .as_ref()
        .filter(|r| r.trackers_checked > 0 || r.dht.is_some());
    v["seeders"] = json!(answered.map(|r| r.seeders));
    v["leechers"] = json!(answered.map(|r| r.leechers));
    v["completed"] = json!(answered.map(|r| r.completed));
    // With no tracker reporting, the DHT estimate stood in for the counts.
    v["seeders_approx"] =
        json!(answered.is_some_and(|r| r.trackers_reporting == 0 && r.dht.is_some()));
    v["trackers_reporting"] = json!(answered.map(|r| r.trackers_reporting));
    v["trackers_checked"] = json!(answered.map(|r| r.trackers_checked));
    v["last_scrape_at"] = json!(answered.map(|r| r.last_scrape_at));
    v["swarm_suspect"] = json!(answered.is_some_and(|r| r.flags & swarm_flags::SUSPECT != 0));
    // The hot column moves on every sighting; the row at most hourly.
    let seen = hot.map_or(NEVER, |h| h.get(Col::LastSeen, row.id as u64));
    v["last_seen_at"] = json!(if seen == NEVER {
        row.last_seen_at
    } else {
        i64::from(seen).max(row.last_seen_at)
    });
    Ok(answered.cloned())
}

/// The detail view's swarm section (`docs/metrics.md` §7.2), after
/// [`summary`] (blocking).
pub fn detail(
    store: &Store,
    swarm: Option<&SwarmRow>,
    id: i64,
    v: &mut Value,
) -> lodestar_store::Result<()> {
    v["dht_seeds"] = json!(swarm.and_then(|r| r.dht).map(|d| d.0));
    v["dht_peers"] = json!(swarm.and_then(|r| r.dht).map(|d| d.1));
    let urls: std::collections::HashMap<u16, String> = store
        .trackers()?
        .into_iter()
        .map(|t| (t.id, t.url))
        .collect();
    v["swarm_trackers"] = store
        .swarm_trackers(id)?
        .into_iter()
        .map(|s| {
            json!({
                "url": urls.get(&s.counts.tracker),
                "seeders": s.counts.seeders,
                "leechers": s.counts.leechers,
                "completed": s.counts.completed,
                "at": s.at,
            })
        })
        .collect();
    let history = store.history(id)?;
    // `at` is the start of the UTC day or week (epoch day / 7).
    let samples = |it: &mut dyn Iterator<Item = (u32, Option<(u16, u16)>)>, len: i64| {
        it.map(|(n, s)| {
            json!({
                "at": i64::from(n) * len,
                // Missing samples are gaps, not zeros (§10.5).
                "seeders": s.map(|s| s.0),
                "leechers": s.map(|s| s.1),
            })
        })
        .collect::<Vec<_>>()
    };
    v["swarm_history"] = json!(history.as_ref().map(|h| json!({
        "daily": samples(&mut h.daily(), DAY),
        "weekly": samples(&mut h.weekly(), 7 * DAY),
    })));
    v["swarm_trend"] = json!(history.as_ref().and_then(trend));
    Ok(())
}
