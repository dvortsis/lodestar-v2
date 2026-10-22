//! Content-filtering state (architecture §20.7): CS-1 hashes and CS-8
//! signatures, core switches, tombstones, quarantine and filter hits.
//!
//! The store keeps plain values: rule ids as text, actions and origins as
//! small integers. Their meaning belongs to the filter engine.

use lodestar_metainfo::InfoHash;
use rusqlite::{OptionalExtension, Transaction, params};

use crate::{Error, Result, Store};

/// How a filtered torrent is held back.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Held {
    /// Dropped; the tombstone stops it being fetched again.
    Tombstone,
    /// Quarantined until it's released, confirmed or expires.
    Quarantine,
}

/// Everything filtering loads at start.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FilterState {
    /// `(hash, origin)`.
    pub cs_hashes: Vec<([u8; 20], u8)>,
    pub cs_sigs: Vec<u64>,
    /// `(rule, enabled)` for rules whose switch was ever set.
    pub switches: Vec<(String, bool)>,
    /// Tombstoned and quarantined hashes.
    pub held: Vec<([u8; 20], Held)>,
}

/// A torrent a filter rule acted on, written with the batch it came in.
#[derive(Debug, Clone, Copy)]
pub struct Filtered<'a> {
    /// Every swarm hash of the torrent.
    pub hashes: &'a [[u8; 20]],
    pub rule: &'a str,
    /// The engine's action code, for `filter_hits`, plus [`SHADOW`] for
    /// what a rule in shadow mode would have done.
    pub action: u8,
    pub held: Option<Held>,
    /// The sealed name, for quarantine.
    pub sealed: Option<&'a [u8]>,
    /// Evidence codes (no names).
    pub evidence: &'a [u8],
    pub at: i64,
    /// Quarantine expiry.
    pub expires_at: i64,
    /// Held only because your correction or classification rule made it
    /// adult: when its quarantine expires, it becomes a tombstone (hash
    /// and rule, no name) instead of being released (§20.9).
    pub adult_hold: bool,
}

/// A stored torrent filtering judged again (§20.9), for
/// [`Store::set_enrichments_at`]: the decision as ingest records it, and
/// the `torrents.flags` marks of one it keeps.
#[derive(Debug, Clone, Copy)]
pub struct Refiled<'a> {
    pub id: i64,
    pub filtered: Filtered<'a>,
    pub flags: i64,
}

/// One filter decision (`filter_hits`): no names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FilterHit {
    pub hash: [u8; 20],
    pub rule: String,
    pub action: u8,
    pub at: i64,
    pub evidence: Vec<u8>,
}

/// Marks a filter hit as shadow mode: logged, not applied (§20.3).
pub const SHADOW: u8 = 0x80;

/// Appended to the stored rule id of an adult hold
/// ([`Filtered::adult_hold`]) in `quarantine` and `tombstones`. Reads
/// strip it, so callers see plain rule ids.
const ADULT_HOLD: &str = "+adult";

/// The stored rule id of a held hash.
fn held_rule(rule: &str, adult_hold: bool) -> String {
    if adult_hold {
        format!("{rule}{ADULT_HOLD}")
    } else {
        rule.to_owned()
    }
}

/// A stored rule id as the rule and whether it's an adult hold.
fn split_rule(mut stored: String) -> (String, bool) {
    match stored.strip_suffix(ADULT_HOLD).map(str::len) {
        Some(n) => {
            stored.truncate(n);
            (stored, true)
        }
        None => (stored, false),
    }
}

/// One quarantined torrent, as the review queue lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Quarantined {
    pub hash: [u8; 20],
    pub rule: String,
    pub sealed: Option<Vec<u8>>,
    pub at: i64,
    pub expires_at: i64,
    /// It becomes a tombstone at `expires_at` ([`Filtered::adult_hold`]).
    pub adult_hold: bool,
}

/// One tombstone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tombstone {
    pub hash: [u8; 20],
    pub rule: String,
    pub at: i64,
    /// Left by an adult hold's quarantine ([`Filtered::adult_hold`]).
    pub adult_hold: bool,
}

fn hash20(v: Vec<u8>) -> Result<[u8; 20]> {
    v.try_into()
        .map_err(|_| Error::Corrupt("hash is not 20 bytes"))
}

pub(crate) fn write(tx: &Transaction<'_>, filtered: &[Filtered<'_>]) -> Result<()> {
    if filtered.is_empty() {
        return Ok(());
    }
    let mut hit = tx.prepare_cached(
        "INSERT INTO filter_hits (hash, rule, action, at, evidence) VALUES (?1, ?2, ?3, ?4, ?5)",
    )?;
    let mut tomb = tx
        .prepare_cached("INSERT OR REPLACE INTO tombstones (hash, rule, at) VALUES (?1, ?2, ?3)")?;
    let mut quarantine = tx.prepare_cached(
        "INSERT OR REPLACE INTO quarantine (hash, rule, sealed, at, expires_at)
         VALUES (?1, ?2, ?3, ?4, ?5)",
    )?;
    for f in filtered {
        let Some(first) = f.hashes.first() else {
            continue;
        };
        hit.execute(params![&first[..], f.rule, f.action, f.at, f.evidence])?;
        let rule = held_rule(f.rule, f.adult_hold);
        for h in f.hashes {
            match f.held {
                Some(Held::Tombstone) => {
                    tomb.execute(params![&h[..], rule, f.at])?;
                }
                Some(Held::Quarantine) => {
                    quarantine.execute(params![&h[..], rule, f.sealed, f.at, f.expires_at])?;
                }
                None => {}
            }
        }
    }
    Ok(())
}

impl Store {
    pub fn filter_state(&self) -> Result<FilterState> {
        let c = &self.conn;
        let mut s = FilterState::default();
        let mut stmt = c.prepare("SELECT hash, origin FROM cs_hashes")?;
        for row in stmt.query_map([], |r| Ok((r.get::<_, Vec<u8>>(0)?, r.get::<_, u8>(1)?)))? {
            let (h, o) = row?;
            s.cs_hashes.push((hash20(h)?, o));
        }
        let mut stmt = c.prepare("SELECT sig FROM cs_sigs")?;
        for row in stmt.query_map([], |r| r.get::<_, i64>(0))? {
            s.cs_sigs.push(row? as u64);
        }
        let mut stmt = c.prepare("SELECT rule, enabled FROM core_membership")?;
        for row in stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))? {
            s.switches.push(row?);
        }
        for (sql, held) in [
            ("SELECT hash FROM tombstones", Held::Tombstone),
            ("SELECT hash FROM quarantine", Held::Quarantine),
        ] {
            let mut stmt = c.prepare(sql)?;
            for row in stmt.query_map([], |r| r.get::<_, Vec<u8>>(0))? {
                s.held.push((hash20(row?)?, held));
            }
        }
        Ok(s)
    }

    /// Adds CS-1 hashes. Your own confirmation outranks an import of the
    /// same hash (origin 2).
    pub fn add_cs_hashes(&mut self, hashes: &[[u8; 20]], origin: u8, at: i64) -> Result<()> {
        let tx = self.conn.transaction()?;
        {
            let mut stmt = tx.prepare_cached(
                "INSERT INTO cs_hashes (hash, origin, added_at) VALUES (?1, ?2, ?3)
                 ON CONFLICT (hash) DO UPDATE SET origin = excluded.origin
                 WHERE cs_hashes.origin = 2",
            )?;
            for h in hashes {
                stmt.execute(params![&h[..], origin, at])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    pub fn remove_cs_hashes(&mut self, hashes: &[[u8; 20]]) -> Result<usize> {
        let tx = self.conn.transaction()?;
        let mut n = 0;
        {
            let mut stmt = tx.prepare_cached("DELETE FROM cs_hashes WHERE hash = ?1")?;
            for h in hashes {
                n += stmt.execute([&h[..]])?;
            }
        }
        tx.commit()?;
        Ok(n)
    }

    pub fn add_cs_sig(&mut self, sig: u64, at: i64) -> Result<()> {
        self.conn.execute(
            "INSERT OR IGNORE INTO cs_sigs (sig, added_at) VALUES (?1, ?2)",
            params![sig as i64, at],
        )?;
        Ok(())
    }

    pub fn remove_cs_sig(&mut self, sig: u64) -> Result<bool> {
        Ok(self
            .conn
            .execute("DELETE FROM cs_sigs WHERE sig = ?1", [sig as i64])?
            > 0)
    }

    pub fn set_core_switch(&mut self, rule: &str, enabled: bool) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO core_membership (rule, enabled) VALUES (?1, ?2)",
            params![rule, enabled],
        )?;
        Ok(())
    }

    /// Releases a held hash (removes its tombstone or quarantine row), so
    /// it can be fetched again. Returns whether it was held.
    pub fn release(&mut self, hash: &[u8; 20]) -> Result<bool> {
        let tx = self.conn.transaction()?;
        let n = tx.execute("DELETE FROM tombstones WHERE hash = ?1", [&hash[..]])?
            + tx.execute("DELETE FROM quarantine WHERE hash = ?1", [&hash[..]])?;
        tx.commit()?;
        Ok(n > 0)
    }

    /// Deletes stored torrents with any of these swarm hashes, and logs
    /// them so the search indexer drops their documents (§20.5.2: abuse
    /// material reported or confirmed after it was stored). Returns the
    /// deleted ids. Ids are never reused, so hot-column slots left behind
    /// are never read again.
    pub fn delete_torrents(&mut self, hashes: &[[u8; 20]]) -> Result<Vec<i64>> {
        let tx = self.conn.transaction()?;
        let mut ids = Vec::new();
        for h in hashes {
            if let Some(id) = crate::find_any(&tx, std::iter::once(InfoHash(*h)))? {
                if !ids.contains(&id) {
                    ids.push(id);
                }
            }
        }
        for id in &ids {
            tx.execute("DELETE FROM torrents WHERE id = ?1", [id])?;
            tx.execute("INSERT INTO reindex_log (torrent_id) VALUES (?1)", [id])?;
        }
        tx.commit()?;
        Ok(ids)
    }

    /// The quarantine, newest first.
    pub fn quarantined(&self, limit: usize) -> Result<Vec<Quarantined>> {
        self.quarantined_before(None, limit)
    }

    /// The quarantine, newest first, after the row at `before`
    /// (`(at, hash)`, exclusive): one page of the review queue.
    pub fn quarantined_before(
        &self,
        before: Option<(i64, [u8; 20])>,
        limit: usize,
    ) -> Result<Vec<Quarantined>> {
        let (at, hash) = before.map_or((i64::MAX, None), |(at, h)| (at, Some(h.to_vec())));
        let mut stmt = self.conn.prepare_cached(
            "SELECT hash, rule, sealed, at, expires_at FROM quarantine
             WHERE ?1 IS NULL OR at < ?2 OR (at = ?2 AND hash > ?1)
             ORDER BY at DESC, hash LIMIT ?3",
        )?;
        let rows = stmt.query_map(params![hash, at, limit as i64], |r| {
            Ok((
                r.get::<_, Vec<u8>>(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (hash, rule, sealed, at, expires_at) = row?;
            let (rule, adult_hold) = split_rule(rule);
            out.push(Quarantined {
                hash: hash20(hash)?,
                rule,
                sealed,
                at,
                expires_at,
                adult_hold,
            });
        }
        Ok(out)
    }

    /// Ends expired quarantine and removes filter hits older than
    /// `hits_before`. An expired quarantine is released (its hash becomes
    /// fetchable again), except an adult hold, which becomes a tombstone:
    /// its sealed name goes, its hash stays refused (§20.9). Returns the
    /// released hashes and how many hits were removed.
    pub fn purge_filter(&mut self, now: i64, hits_before: i64) -> Result<(Vec<[u8; 20]>, usize)> {
        let tx = self.conn.transaction()?;
        let mut expired = Vec::new();
        {
            let mut stmt = tx.prepare(
                "SELECT hash FROM quarantine WHERE expires_at <= ?1 AND rule NOT LIKE ?2",
            )?;
            let adult = format!("%{ADULT_HOLD}");
            for row in stmt.query_map(params![now, adult], |r| r.get::<_, Vec<u8>>(0))? {
                expired.push(hash20(row?)?);
            }
            tx.execute(
                "INSERT OR REPLACE INTO tombstones (hash, rule, at)
                 SELECT hash, rule, at FROM quarantine WHERE expires_at <= ?1 AND rule LIKE ?2",
                params![now, adult],
            )?;
        }
        tx.execute("DELETE FROM quarantine WHERE expires_at <= ?1", [now])?;
        let hits = tx.execute("DELETE FROM filter_hits WHERE at < ?1", [hits_before])?;
        tx.commit()?;
        Ok((expired, hits))
    }

    /// Releases everything `rules` hold (quarantine and tombstones), so it
    /// can be fetched again: the rules stopped applying. Returns the
    /// released hashes.
    pub fn release_rules(&mut self, rules: &[String]) -> Result<Vec<[u8; 20]>> {
        let tx = self.conn.transaction()?;
        let mut out = Vec::new();
        for rule in rules {
            for table in ["quarantine", "tombstones"] {
                let mut stmt = tx.prepare(&format!(
                    "DELETE FROM {table} WHERE rule = ?1 OR rule = ?2 RETURNING hash"
                ))?;
                let rows = stmt.query_map(params![rule, held_rule(rule, true)], |r| {
                    r.get::<_, Vec<u8>>(0)
                })?;
                for h in rows {
                    out.push(hash20(h?)?);
                }
            }
        }
        tx.commit()?;
        Ok(out)
    }

    /// Filter decisions since `since`, per rule and action code.
    pub fn hit_counts(&self, since: i64) -> Result<Vec<(String, u8, i64)>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT rule, action, count(*) FROM filter_hits WHERE at >= ?1
             GROUP BY rule, action ORDER BY rule, action",
        )?;
        let rows = stmt.query_map([since], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Tombstones, newest first.
    pub fn tombstones(&self, limit: usize) -> Result<Vec<Tombstone>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT hash, rule, at FROM tombstones ORDER BY at DESC, hash LIMIT ?1",
        )?;
        let rows = stmt.query_map([limit as i64], |r| {
            Ok((r.get::<_, Vec<u8>>(0)?, r.get(1)?, r.get(2)?))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (h, rule, at) = row?;
            let (rule, adult_hold) = split_rule(rule);
            out.push(Tombstone {
                hash: hash20(h)?,
                rule,
                at,
                adult_hold,
            });
        }
        Ok(out)
    }

    /// Whether `hash` is held (quarantined or tombstoned) as an adult hold.
    pub fn adult_hold(&self, hash: &[u8; 20]) -> Result<bool> {
        Ok(self
            .conn
            .query_row(
                "SELECT 1 FROM quarantine WHERE hash = ?1 AND rule LIKE ?2
                 UNION ALL SELECT 1 FROM tombstones WHERE hash = ?1 AND rule LIKE ?2",
                params![&hash[..], format!("%{ADULT_HOLD}")],
                |_| Ok(()),
            )
            .optional()?
            .is_some())
    }

    /// Filter hits since `since`, oldest first.
    pub fn filter_hits(&self, since: i64) -> Result<Vec<FilterHit>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT hash, rule, action, at, evidence FROM filter_hits WHERE at >= ?1 ORDER BY id",
        )?;
        let rows = stmt.query_map([since], |r| {
            Ok((
                r.get::<_, Vec<u8>>(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (h, rule, action, at, evidence) = row?;
            out.push(FilterHit {
                hash: hash20(h)?,
                rule,
                action,
                at,
                evidence,
            });
        }
        Ok(out)
    }
}
