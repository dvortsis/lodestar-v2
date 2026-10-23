//! Swarm health in API responses (§6, §10.5, §10.12), under the field
//! names of the UI contract (`docs/metrics.md` §7): counts on every hit,
//! and the full picture (per tracker, DHT estimate, history) on the
//! detail view.
//!
//! "Never scraped" is `null`, never 0 (§10.12: null ≠ zero); the DHT
//! figure is an estimate and says so.

use std::time::Duration;

use lodestar_store::{Col, HotColumns, NEVER, Store, SwarmRow, TorrentRow, swarm_flags, trend};

use crate::dto::torrents::{SwarmHistory, SwarmSample, SwarmTracker};

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
/// search hits and the detail view alike.
///
/// They come from the stored swarm row, which is never behind the hot
/// columns, so the six nullable counts are null together.
#[derive(Debug, Clone, Default)]
pub(crate) struct Summary {
    pub seeders: Option<u32>,
    pub leechers: Option<u32>,
    pub completed: Option<u32>,
    pub seeders_approx: bool,
    pub trackers_reporting: Option<u32>,
    pub trackers_checked: Option<u32>,
    pub last_scrape_at: Option<i64>,
    pub swarm_suspect: bool,
    pub last_seen_at: i64,
    /// The answered row, for [`detail`].
    pub row: Option<SwarmRow>,
}

/// Reads a torrent's [`Summary`] (blocking).
pub(crate) fn summary(
    store: &Store,
    hot: Option<&HotColumns>,
    row: &TorrentRow,
) -> lodestar_store::Result<Summary> {
    // A row with nothing checked and no DHT estimate is a first scrape
    // nobody answered: still "never scraped".
    let answered = store
        .swarm(row.id)?
        .filter(|r| r.trackers_checked > 0 || r.dht.is_some());
    // The hot column moves on every sighting; the row at most hourly.
    let seen = hot.map_or(NEVER, |h| h.get(Col::LastSeen, row.id as u64));
    let a = answered.as_ref();
    Ok(Summary {
        seeders: a.map(|r| r.seeders),
        leechers: a.map(|r| r.leechers),
        completed: a.map(|r| r.completed),
        // With no tracker reporting, the DHT estimate stood in.
        seeders_approx: a.is_some_and(|r| r.trackers_reporting == 0 && r.dht.is_some()),
        trackers_reporting: a.map(|r| u32::from(r.trackers_reporting)),
        trackers_checked: a.map(|r| u32::from(r.trackers_checked)),
        last_scrape_at: a.map(|r| r.last_scrape_at),
        swarm_suspect: a.is_some_and(|r| r.flags & swarm_flags::SUSPECT != 0),
        last_seen_at: if seen == NEVER {
            row.last_seen_at
        } else {
            i64::from(seen).max(row.last_seen_at)
        },
        row: answered,
    })
}

/// The detail view's swarm section (`docs/metrics.md` §7.2).
#[derive(Debug, Clone, Default)]
pub(crate) struct Detail {
    pub dht_seeds: Option<u32>,
    pub dht_peers: Option<u32>,
    pub swarm_trackers: Vec<SwarmTracker>,
    pub swarm_history: Option<SwarmHistory>,
    pub swarm_trend: Option<f64>,
}

/// Reads torrent `id`'s [`Detail`], given its [`Summary`] (blocking).
pub(crate) fn detail(store: &Store, summary: &Summary, id: i64) -> lodestar_store::Result<Detail> {
    let dht = summary.row.as_ref().and_then(|r| r.dht);
    let urls: std::collections::HashMap<u16, String> = store
        .trackers()?
        .into_iter()
        .map(|t| (t.id, t.url))
        .collect();
    let swarm_trackers = store
        .swarm_trackers(id)?
        .into_iter()
        // Tracker rows are never deleted; one missing is skipped rather
        // than shown without a name.
        .filter_map(|s| {
            Some(SwarmTracker {
                at: s.at,
                completed: s.counts.completed,
                leechers: s.counts.leechers,
                seeders: s.counts.seeders,
                url: urls.get(&s.counts.tracker)?.clone(),
            })
        })
        .collect();
    let history = store.history(id)?;
    // `at` is the start of the UTC day or week (epoch day / 7).
    let samples = |it: &mut dyn Iterator<Item = (u32, Option<(u16, u16)>)>, len: i64| {
        it.map(|(n, s)| SwarmSample {
            at: i64::from(n) * len,
            // Missing samples are gaps, not zeros (§10.5).
            seeders: s.map(|s| u32::from(s.0)),
            leechers: s.map(|s| u32::from(s.1)),
        })
        .collect()
    };
    Ok(Detail {
        dht_seeds: dht.map(|d| d.0),
        dht_peers: dht.map(|d| d.1),
        swarm_trackers,
        swarm_history: history.as_ref().map(|h| SwarmHistory {
            daily: samples(&mut h.daily(), DAY),
            weekly: samples(&mut h.weekly(), 7 * DAY),
        }),
        swarm_trend: history.as_ref().and_then(trend),
    })
}
