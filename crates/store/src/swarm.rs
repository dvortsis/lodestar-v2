//! Swarm health rows (architecture §7, §16 M4): scrape results per
//! torrent, per-tracker detail, history and the tracker list.

use lodestar_metainfo::InfoHash;
use rusqlite::{Connection, OptionalExtension, params};

use crate::history::History;
use crate::hot::{Col, HotColumns, NEVER, SwarmHot};
use crate::{Error, Result, Store, flags};

/// Bits of `swarm.flags`. Persisted: append only.
pub mod swarm_flags {
    /// One tracker reports a swarm that the DHT and every other tracker
    /// don't see: the fake-seeding signature (§8).
    pub const SUSPECT: u32 = 1;
}

/// One tracker's answer for a torrent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TrackerCounts {
    pub tracker: u16,
    pub seeders: u32,
    pub leechers: u32,
    pub completed: u32,
}

impl TrackerCounts {
    /// The opentracker rule (§15): zeros mean "unknown here".
    pub fn is_reporting(&self) -> bool {
        self.seeders > 0 || self.leechers > 0 || self.completed > 0
    }
}

/// One scrape round's result for a torrent, as the scheduler aggregated
/// it. The store keeps what it's given; which number counts as "seeders"
/// is the scheduler's rule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SwarmUpdate {
    pub torrent_id: i64,
    /// Unix seconds of the scrape.
    pub at: i64,
    pub seeders: u32,
    pub leechers: u32,
    pub completed: u32,
    pub trackers_reporting: u16,
    /// Trackers that answered this round (the "m" of "n of m").
    pub trackers_checked: u16,
    /// The DHT estimate (BEP 33): (seeds, peers).
    pub dht: Option<(u32, u32)>,
    /// Per-tracker answers; only reporting ones are stored.
    pub per_tracker: Vec<TrackerCounts>,
    pub next_scrape_at: i64,
    /// [`swarm_flags`].
    pub flags: u32,
    /// A user is interested (viewed, favorited, saved-search hit): keep
    /// history even for a zero swarm.
    pub interest: bool,
}

impl SwarmUpdate {
    fn is_live(&self) -> bool {
        self.seeders > 0
            || self.leechers > 0
            || self.completed > 0
            || self.trackers_reporting > 0
            || self.dht.is_some_and(|(s, p)| s > 0 || p > 0)
    }

    /// The hot-column values this update sets.
    pub fn hot(&self) -> SwarmHot {
        SwarmHot {
            seeders: self.seeders,
            leechers: self.leechers,
            trackers: u32::from(self.trackers_reporting),
            dht: self.dht.map_or(NEVER, |(s, p)| s.saturating_add(p)),
        }
    }
}

/// A torrent's `swarm` row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SwarmRow {
    pub torrent_id: i64,
    pub seeders: u32,
    pub leechers: u32,
    pub completed: u32,
    pub trackers_reporting: u16,
    pub trackers_checked: u16,
    pub dht: Option<(u32, u32)>,
    pub last_scrape_at: i64,
    pub next_scrape_at: i64,
    pub flags: u32,
}

/// A tracker's last report for a torrent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TrackerSample {
    pub counts: TrackerCounts,
    /// Unix seconds of the scrape.
    pub at: i64,
}

/// `trackers.state` values. Persisted: append only.
pub mod tracker_state {
    pub const OK: i64 = 0;
    /// Failing; retried with backoff.
    pub const BACKOFF: i64 = 1;
    /// Unreachable for long enough to stop trying for a while.
    pub const DOWN: i64 = 2;
    /// Refused us (rate limit or ban message).
    pub const BANNED: i64 = 3;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrackerRow {
    pub id: u16,
    /// Canonical URL (`lodestar_tracker_scrape::TrackerUrl::as_str`).
    pub url: String,
    pub added_at: i64,
    pub enabled: bool,
    /// [`tracker_state`].
    pub state: i64,
    pub state_at: Option<i64>,
    pub message: Option<String>,
}

// Per-tracker blob, format 1: version u8 | at varint | then per tracker:
// id u16 LE | seeders varint | leechers varint | completed varint.
// Every entry comes from the same scrape round, so `at` is stored once.
const TRACKERS_VERSION: u8 = 1;

fn put_varint(out: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        out.push(v as u8 | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

fn get_varint(b: &[u8], pos: &mut usize) -> Option<u64> {
    let mut v = 0u64;
    for shift in (0..64).step_by(7) {
        let byte = *b.get(*pos)?;
        *pos += 1;
        v |= u64::from(byte & 0x7f).checked_shl(shift)?;
        if byte & 0x80 == 0 {
            // Canonical only: no trailing zero groups, no overflow.
            if shift > 0 && byte == 0 {
                return None;
            }
            if shift == 63 && byte > 1 {
                return None;
            }
            return Some(v);
        }
    }
    None
}

pub(crate) fn encode_trackers(at: i64, entries: &[TrackerCounts]) -> Vec<u8> {
    let mut out = Vec::with_capacity(2 + 8 * entries.len());
    out.push(TRACKERS_VERSION);
    put_varint(&mut out, at.max(0) as u64);
    for e in entries {
        out.extend_from_slice(&e.tracker.to_le_bytes());
        for v in [e.seeders, e.leechers, e.completed] {
            put_varint(&mut out, u64::from(v));
        }
    }
    out
}

/// Strict: anything the encoder wouldn't write is an error.
pub(crate) fn decode_trackers(b: &[u8]) -> Option<Vec<TrackerSample>> {
    if b.first() != Some(&TRACKERS_VERSION) {
        return None;
    }
    let mut pos = 1;
    let at = i64::try_from(get_varint(b, &mut pos)?).ok()?;
    let mut out = Vec::new();
    while pos < b.len() {
        let id = u16::from_le_bytes(b.get(pos..pos + 2)?.try_into().ok()?);
        pos += 2;
        let mut v = [0u32; 3];
        for x in &mut v {
            *x = u32::try_from(get_varint(b, &mut pos)?).ok()?;
        }
        out.push(TrackerSample {
            counts: TrackerCounts {
                tracker: id,
                seeders: v[0],
                leechers: v[1],
                completed: v[2],
            },
            at,
        });
    }
    Some(out)
}

/// Applies one update inside the writer's transaction. Returns `false`
/// when the torrent no longer exists.
pub(crate) fn apply(tx: &Connection, u: &SwarmUpdate) -> Result<bool> {
    let exists = tx
        .prepare_cached("SELECT 1 FROM torrents WHERE id = ?1")?
        .query_row([u.torrent_id], |_| Ok(()))
        .optional()?
        .is_some();
    if !exists {
        return Ok(false);
    }
    tx.prepare_cached(
        "INSERT INTO swarm (torrent_id, seeders, leechers, completed, trackers_reporting,
             trackers_checked, dht_seeds, dht_peers, last_scrape_at, next_scrape_at, flags)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
         ON CONFLICT (torrent_id) DO UPDATE SET seeders = excluded.seeders,
             leechers = excluded.leechers, completed = excluded.completed,
             trackers_reporting = excluded.trackers_reporting,
             trackers_checked = excluded.trackers_checked, dht_seeds = excluded.dht_seeds,
             dht_peers = excluded.dht_peers, last_scrape_at = excluded.last_scrape_at,
             next_scrape_at = excluded.next_scrape_at, flags = excluded.flags",
    )?
    .execute(params![
        u.torrent_id,
        u.seeders,
        u.leechers,
        u.completed,
        u.trackers_reporting,
        u.trackers_checked,
        u.dht.map(|d| d.0),
        u.dht.map(|d| d.1),
        u.at,
        u.next_scrape_at,
        u.flags,
    ])?;
    let reporting: Vec<TrackerCounts> = u
        .per_tracker
        .iter()
        .filter(|t| t.is_reporting())
        .copied()
        .collect();
    if reporting.is_empty() {
        tx.prepare_cached("DELETE FROM swarm_trackers WHERE torrent_id = ?1")?
            .execute([u.torrent_id])?;
    } else {
        tx.prepare_cached(
            "INSERT INTO swarm_trackers (torrent_id, data) VALUES (?1, ?2)
             ON CONFLICT (torrent_id) DO UPDATE SET data = excluded.data",
        )?
        .execute(params![u.torrent_id, encode_trackers(u.at, &reporting)])?;
    }
    // History is kept for swarms that were ever live or that someone looked
    // at; once kept, it follows the swarm down to zero, which is the point.
    let stored: Option<Vec<u8>> = tx
        .prepare_cached("SELECT data FROM swarm_history WHERE torrent_id = ?1")?
        .query_row([u.torrent_id], |r| r.get(0))
        .optional()?;
    if stored.is_some() || u.is_live() || u.interest {
        // A corrupt ring is replaced rather than failing the batch: it's
        // derived data, and the next scrapes refill it.
        let mut h = stored
            .and_then(|b| History::decode(&b).ok())
            .unwrap_or_default();
        h.record(u.at, u.seeders, u.leechers);
        tx.prepare_cached(
            "INSERT INTO swarm_history (torrent_id, data) VALUES (?1, ?2)
             ON CONFLICT (torrent_id) DO UPDATE SET data = excluded.data",
        )?
        .execute(params![u.torrent_id, h.encode()])?;
    }
    Ok(true)
}

/// Torrents a scrape must never reach: a public tracker or DHT scrape
/// would leak the hash (§18.7).
const NEVER_SCRAPED: i64 = flags::PRIVATE | flags::PRIVATE_SOURCE;
/// Rows per statement when loading hot columns.
const LOAD_CHUNK: i64 = 10_000;

fn hash20(b: Vec<u8>) -> Result<InfoHash> {
    Ok(InfoHash(
        b.try_into()
            .map_err(|_| Error::Corrupt("info_hash length"))?,
    ))
}

fn u32_col(r: &rusqlite::Row<'_>, i: usize) -> rusqlite::Result<u32> {
    let v: i64 = r.get(i)?;
    u32::try_from(v).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(i, rusqlite::types::Type::Integer, Box::new(e))
    })
}

impl Store {
    /// Scraped torrents due by `now`, most overdue first: `(id, hash,
    /// flags)`. Private torrents are never returned.
    pub fn due_for_scrape(&self, now: i64, limit: usize) -> Result<Vec<(i64, InfoHash, i64)>> {
        let rows: Vec<(i64, Vec<u8>, i64)> = self
            .connection()
            .prepare_cached(
                "SELECT s.torrent_id, t.info_hash, t.flags FROM swarm s
                 JOIN torrents t ON t.id = s.torrent_id
                 WHERE s.next_scrape_at <= ?1 AND t.flags & ?2 = 0
                 ORDER BY s.next_scrape_at LIMIT ?3",
            )?
            .query_map(params![now, NEVER_SCRAPED, limit as i64], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })?
            .collect::<rusqlite::Result<_>>()?;
        rows.into_iter()
            .map(|(id, h, f)| Ok((id, hash20(h)?, f)))
            .collect()
    }

    /// Never-scraped torrents (no `swarm` row) with ids above `after`, in
    /// id order: the scheduler's "new" lane follows them with a watermark.
    /// Private torrents are skipped.
    pub fn unscraped_after(&self, after: i64, limit: usize) -> Result<Vec<(i64, InfoHash, i64)>> {
        let rows: Vec<(i64, Vec<u8>, i64)> = self
            .connection()
            .prepare_cached(
                "SELECT t.id, t.info_hash, t.flags FROM torrents t
                 WHERE t.id > ?1 AND t.flags & ?2 = 0
                   AND NOT EXISTS (SELECT 1 FROM swarm s WHERE s.torrent_id = t.id)
                 ORDER BY t.id LIMIT ?3",
            )?
            .query_map(params![after, NEVER_SCRAPED, limit as i64], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })?
            .collect::<rusqlite::Result<_>>()?;
        rows.into_iter()
            .map(|(id, h, f)| Ok((id, hash20(h)?, f)))
            .collect()
    }

    pub fn swarm(&self, id: i64) -> Result<Option<SwarmRow>> {
        Ok(self
            .connection()
            .prepare_cached(
                "SELECT torrent_id, seeders, leechers, completed, trackers_reporting,
                        trackers_checked, dht_seeds, dht_peers, last_scrape_at,
                        next_scrape_at, flags
                 FROM swarm WHERE torrent_id = ?1",
            )?
            .query_row([id], |r| {
                let seeds: Option<i64> = r.get(6)?;
                let peers: Option<i64> = r.get(7)?;
                Ok(SwarmRow {
                    torrent_id: r.get(0)?,
                    seeders: u32_col(r, 1)?,
                    leechers: u32_col(r, 2)?,
                    completed: u32_col(r, 3)?,
                    trackers_reporting: r.get(4)?,
                    trackers_checked: r.get(5)?,
                    dht: match (seeds, peers) {
                        (Some(_), Some(_)) => Some((u32_col(r, 6)?, u32_col(r, 7)?)),
                        _ => None,
                    },
                    last_scrape_at: r.get(8)?,
                    next_scrape_at: r.get(9)?,
                    flags: u32_col(r, 10)?,
                })
            })
            .optional()?)
    }

    /// The reporting trackers from the last scrape; empty when none
    /// reported.
    pub fn swarm_trackers(&self, id: i64) -> Result<Vec<TrackerSample>> {
        let blob: Option<Vec<u8>> = self
            .connection()
            .prepare_cached("SELECT data FROM swarm_trackers WHERE torrent_id = ?1")?
            .query_row([id], |r| r.get(0))
            .optional()?;
        match blob {
            None => Ok(Vec::new()),
            Some(b) => decode_trackers(&b).ok_or(Error::Corrupt("swarm_trackers blob")),
        }
    }

    pub fn history(&self, id: i64) -> Result<Option<History>> {
        let blob: Option<Vec<u8>> = self
            .connection()
            .prepare_cached("SELECT data FROM swarm_history WHERE torrent_id = ?1")?
            .query_row([id], |r| r.get(0))
            .optional()?;
        blob.map(|b| History::decode(&b).map_err(|_| Error::Corrupt("swarm_history blob")))
            .transpose()
    }

    /// Every tracker, enabled or not, by id.
    pub fn trackers(&self) -> Result<Vec<TrackerRow>> {
        Ok(self
            .connection()
            .prepare_cached(
                "SELECT id, url, added_at, enabled, state, state_at, message
                 FROM trackers ORDER BY id",
            )?
            .query_map([], |r| {
                Ok(TrackerRow {
                    id: r.get(0)?,
                    url: r.get(1)?,
                    added_at: r.get(2)?,
                    enabled: r.get(3)?,
                    state: r.get(4)?,
                    state_at: r.get(5)?,
                    message: r.get(6)?,
                })
            })?
            .collect::<rusqlite::Result<_>>()?)
    }

    /// The tracker's id, adding it (enabled) if it's new. `url` should be
    /// canonical, so one tracker never gets two ids.
    pub fn upsert_tracker(&mut self, url: &str, now: i64) -> Result<u16> {
        let conn = self.connection();
        if let Some(id) = conn
            .prepare_cached("SELECT id FROM trackers WHERE url = ?1")?
            .query_row([url], |r| r.get(0))
            .optional()?
        {
            return Ok(id);
        }
        let id: i64 = conn
            .prepare_cached("INSERT INTO trackers (url, added_at) VALUES (?1, ?2) RETURNING id")?
            .query_row(params![url, now], |r| r.get(0))
            .map_err(|e| match e {
                rusqlite::Error::SqliteFailure(f, _)
                    if f.code == rusqlite::ErrorCode::ConstraintViolation =>
                {
                    Error::Corrupt("all 65,535 tracker ids are used")
                }
                e => Error::Sqlite(e),
            })?;
        u16::try_from(id).map_err(|_| Error::Corrupt("tracker id out of range"))
    }

    /// Records a tracker's health ([`tracker_state`]) and its last message.
    pub fn set_tracker_state(
        &mut self,
        id: u16,
        state: i64,
        at: i64,
        message: Option<&str>,
    ) -> Result<()> {
        self.connection()
            .prepare_cached(
                "UPDATE trackers SET state = ?2, state_at = ?3, message = ?4 WHERE id = ?1",
            )?
            .execute(params![id, state, at, message])?;
        Ok(())
    }

    pub fn set_tracker_enabled(&mut self, id: u16, enabled: bool) -> Result<()> {
        self.connection()
            .prepare_cached("UPDATE trackers SET enabled = ?2 WHERE id = ?1")?
            .execute(params![id, enabled])?;
        Ok(())
    }

    /// Fills hot columns from SQLite after a start without a valid
    /// snapshot: swarm columns from `swarm`, last-seen from `torrents`.
    /// Reads in id-ordered chunks, one short statement each, so the WAL is
    /// never pinned by a long read. Returns the swarm rows loaded.
    pub fn load_hot(&self, hot: &HotColumns) -> Result<u64> {
        let conn = self.connection();
        let mut loaded = 0u64;
        let mut after = 0i64;
        loop {
            let rows: Vec<(i64, u32, u32, u32, Option<u32>)> = conn
                .prepare_cached(
                    "SELECT torrent_id, seeders, leechers, trackers_reporting,
                            dht_seeds + dht_peers
                     FROM swarm WHERE torrent_id > ?1 ORDER BY torrent_id LIMIT ?2",
                )?
                .query_map(params![after, LOAD_CHUNK], |r| {
                    let dht: Option<i64> = r.get(4)?;
                    Ok((
                        r.get(0)?,
                        u32_col(r, 1)?,
                        u32_col(r, 2)?,
                        u32_col(r, 3)?,
                        dht.map(|v| u32::try_from(v).unwrap_or(NEVER - 1)),
                    ))
                })?
                .collect::<rusqlite::Result<_>>()?;
            let Some(&(last, ..)) = rows.last() else {
                break;
            };
            after = last;
            for (id, seeders, leechers, trackers, dht) in rows {
                let Ok(id) = u64::try_from(id) else { continue };
                hot.set_swarm(
                    id,
                    SwarmHot {
                        seeders,
                        leechers,
                        trackers,
                        dht: dht.unwrap_or(0),
                    },
                );
                if dht.is_none() {
                    hot.set(Col::Dht, id, NEVER);
                }
                loaded += 1;
            }
        }
        let mut after = 0i64;
        loop {
            let rows: Vec<(i64, i64)> = conn
                .prepare_cached(
                    "SELECT id, last_seen_at FROM torrents WHERE id > ?1 ORDER BY id LIMIT ?2",
                )?
                .query_map(params![after, LOAD_CHUNK], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect::<rusqlite::Result<_>>()?;
            let Some(&(last, _)) = rows.last() else {
                break;
            };
            after = last;
            for (id, seen) in rows {
                if let Ok(id) = u64::try_from(id) {
                    hot.set(
                        Col::LastSeen,
                        id,
                        seen.clamp(0, i64::from(NEVER - 1)) as u32,
                    );
                }
            }
        }
        hot.changed();
        Ok(loaded)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tracker_blobs_round_trip_and_reject_noise() {
        let e = [
            TrackerCounts {
                tracker: 1,
                seeders: 0,
                leechers: 3,
                completed: 300,
            },
            TrackerCounts {
                tracker: 65_535,
                seeders: u32::MAX,
                leechers: 1,
                completed: 0,
            },
        ];
        let b = encode_trackers(1_700_000_000, &e);
        let back = decode_trackers(&b).unwrap();
        assert_eq!(
            back.iter().map(|s| s.counts).collect::<Vec<_>>(),
            e.to_vec()
        );
        assert!(back.iter().all(|s| s.at == 1_700_000_000));
        assert_eq!(decode_trackers(&encode_trackers(5, &[])).unwrap(), vec![]);
        for bad in [&b[..b.len() - 1], &[2u8, 0][..], &[], &[1, 0x80, 0x00]] {
            assert!(decode_trackers(bad).is_none(), "{bad:?}");
        }
    }
}
