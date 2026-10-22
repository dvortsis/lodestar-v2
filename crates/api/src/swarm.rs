//! Swarm health in API responses (§6, §10.5, §10.12): counts on every hit
//! from the hot columns, and the full picture (per tracker, DHT estimate,
//! history) on the detail view.
//!
//! "Never scraped" is `null`, never 0 (§10.12: null ≠ zero); the DHT
//! figure is an estimate and says so.

use std::time::Duration;

use lodestar_store::{Col, HotColumns, NEVER, Store, swarm_flags, trend};
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

fn known(v: u32) -> Value {
    if v == NEVER { Value::Null } else { json!(v) }
}

/// A hit's swarm numbers from the hot columns.
pub fn hit(hot: Option<&HotColumns>, id: i64) -> Value {
    let Some(hot) = hot else {
        return Value::Null;
    };
    let id = id as u64;
    json!({
        "seeders": known(hot.get(Col::Seeders, id)),
        "leechers": known(hot.get(Col::Leechers, id)),
        "trackers": known(hot.get(Col::Trackers, id)),
        "dht": known(hot.get(Col::Dht, id)),
        "last_seen_at": known(hot.get(Col::LastSeen, id)),
    })
}

/// A torrent's swarm section for the detail view (blocking).
pub fn detail(
    store: &Store,
    hot: Option<&HotColumns>,
    id: i64,
    last_seen_at: i64,
) -> lodestar_store::Result<Value> {
    let row = store.swarm(id)?;
    // A row with nothing checked and no DHT estimate is a first scrape
    // nobody answered: still "never scraped" for display.
    let answered = row
        .as_ref()
        .filter(|r| r.trackers_checked > 0 || r.dht.is_some());
    let urls: std::collections::HashMap<u16, String> = store
        .trackers()?
        .into_iter()
        .map(|t| (t.id, t.url))
        .collect();
    let trackers: Vec<Value> = store
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
    let samples = |it: &mut dyn Iterator<Item = (u32, Option<(u16, u16)>)>| -> Vec<Value> {
        it.map(|(n, s)| {
            json!({
                "n": n,
                // Missing samples are gaps, not zeros (§10.5).
                "seeders": s.map(|s| s.0),
                "leechers": s.map(|s| s.1),
            })
        })
        .collect()
    };
    let hot_seen = hot.map_or(NEVER, |h| h.get(Col::LastSeen, id as u64));
    Ok(json!({
        "scraped_at": answered.map(|r| r.last_scrape_at),
        "next_scrape_at": row.as_ref().map(|r| r.next_scrape_at),
        "seeders": answered.map(|r| r.seeders),
        "leechers": answered.map(|r| r.leechers),
        "completed": answered.map(|r| r.completed),
        "trackers_reporting": answered.map(|r| r.trackers_reporting),
        "trackers_checked": answered.map(|r| r.trackers_checked),
        "dht_estimate": answered.and_then(|r| r.dht).map(|(seeds, peers)| json!({
            "seeds": seeds,
            "peers": peers,
        })),
        "suspect": answered.is_some_and(|r| r.flags & swarm_flags::SUSPECT != 0),
        "trackers": trackers,
        "history": history.as_ref().map(|h| json!({
            "daily": samples(&mut h.daily()),
            "weekly": samples(&mut h.weekly()),
        })),
        "trend": history.as_ref().and_then(trend),
        "last_seen_at": if hot_seen == NEVER { last_seen_at } else { i64::from(hot_seen).max(last_seen_at) },
    }))
}
