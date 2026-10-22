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

/// The locked floor's rule ids (the filter's `CoreRule::floor`; a filter
/// test keeps them equal). An adult hold's purge never replaces their
/// tombstones.
pub const FLOOR_RULES: [&str; 3] = ["CS-1a", "CS-1b", "CS-8a"];

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

    /// Whether core rule `rule` is switched on, as stored: on unless it
    /// was switched off.
    pub fn core_switch(&self, rule: &str) -> Result<bool> {
        Ok(self
            .conn
            .query_row(
                "SELECT enabled FROM core_membership WHERE rule = ?1",
                [rule],
                |r| r.get(0),
            )
            .optional()?
            .unwrap_or(true))
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
                "SELECT hash FROM quarantine
                 WHERE expires_at <= ?1 AND substr(rule, -length(?2)) != ?2",
            )?;
            let adult = ADULT_HOLD;
            for row in stmt.query_map(params![now, adult], |r| r.get::<_, Vec<u8>>(0))? {
                expired.push(hash20(row?)?);
            }
            // The adult mark is the one hold filtering can't rebuild (the
            // correction went with the torrent), so it replaces another
            // rule's tombstone, except the floor's, which stays.
            tx.execute(
                "INSERT INTO tombstones (hash, rule, at)
                 SELECT hash, rule, at FROM quarantine
                 WHERE expires_at <= ?1 AND substr(rule, -length(?2)) = ?2
                 ON CONFLICT (hash) DO UPDATE SET rule = excluded.rule, at = excluded.at
                 WHERE tombstones.rule NOT IN (?3, ?4, ?5)",
                params![now, adult, FLOOR_RULES[0], FLOOR_RULES[1], FLOOR_RULES[2]],
            )?;
        }
        tx.execute("DELETE FROM quarantine WHERE expires_at <= ?1", [now])?;
        let hits = tx.execute("DELETE FROM filter_hits WHERE at < ?1", [hits_before])?;
        tx.commit()?;
        Ok((expired, hits))
    }

    /// Releases everything `rules` hold (quarantine and tombstones), so it
    /// can be fetched again: the rules stopped applying. Deletes in
    /// transactions of up to [`MARK_CHUNK`](crate::MARK_CHUNK) rows, so
    /// ingest isn't kept waiting, and calls `released` after each with the
    /// hashes no longer held by anything (another rule may still hold
    /// one). Returns how many were released.
    pub fn release_rules(
        &mut self,
        rules: &[String],
        mut released: impl FnMut(&[[u8; 20]]),
    ) -> Result<usize> {
        let mut total = 0;
        for rule in rules {
            for table in ["quarantine", "tombstones"] {
                loop {
                    let tx = self.conn.transaction()?;
                    let mut deleted = Vec::new();
                    {
                        let mut stmt = tx.prepare_cached(&format!(
                            "DELETE FROM {table} WHERE hash IN (
                                 SELECT hash FROM {table} WHERE rule IN (?1, ?2) LIMIT ?3
                             ) RETURNING hash"
                        ))?;
                        let rows = stmt.query_map(
                            params![rule, held_rule(rule, true), crate::MARK_CHUNK as i64],
                            |r| r.get::<_, Vec<u8>>(0),
                        )?;
                        for h in rows {
                            deleted.push(hash20(h?)?);
                        }
                    }
                    let mut free = Vec::with_capacity(deleted.len());
                    {
                        let mut held = tx.prepare_cached(
                            "SELECT 1 FROM quarantine WHERE hash = ?1
                             UNION ALL SELECT 1 FROM tombstones WHERE hash = ?1",
                        )?;
                        for h in &deleted {
                            if !held.exists([&h[..]])? {
                                free.push(*h);
                            }
                        }
                    }
                    tx.commit()?;
                    released(&free);
                    total += free.len();
                    if deleted.len() < crate::MARK_CHUNK {
                        break;
                    }
                }
            }
        }
        Ok(total)
    }

    /// How many hashes each rule holds: `(rule, quarantined, tombstoned)`.
    pub fn held_counts(&self) -> Result<Vec<(String, usize, usize)>> {
        let mut by_rule: std::collections::BTreeMap<String, (usize, usize)> = Default::default();
        for (i, table) in ["quarantine", "tombstones"].into_iter().enumerate() {
            let mut stmt = self
                .conn
                .prepare_cached(&format!("SELECT rule, count(*) FROM {table} GROUP BY rule"))?;
            let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?;
            for row in rows {
                let (rule, n) = row?;
                let e = by_rule.entry(split_rule(rule).0).or_default();
                if i == 0 {
                    e.0 += n as usize;
                } else {
                    e.1 += n as usize;
                }
            }
        }
        Ok(by_rule.into_iter().map(|(r, (q, t))| (r, q, t)).collect())
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
        self.tombstones_before(None, false, limit)
    }

    /// Tombstones, newest first, after the one at `before` (`(at, hash)`,
    /// exclusive): one page. With `adult`, only adult holds.
    pub fn tombstones_before(
        &self,
        before: Option<(i64, [u8; 20])>,
        adult: bool,
        limit: usize,
    ) -> Result<Vec<Tombstone>> {
        let (at, hash) = before.map_or((i64::MAX, None), |(at, h)| (at, Some(h.to_vec())));
        let mut stmt = self.conn.prepare_cached(
            "SELECT hash, rule, at FROM tombstones
             WHERE (?1 IS NULL OR at < ?2 OR (at = ?2 AND hash > ?1))
               AND (NOT ?4 OR substr(rule, -length(?5)) = ?5)
             ORDER BY at DESC, hash LIMIT ?3",
        )?;
        let rows = stmt.query_map(params![hash, at, limit as i64, adult, ADULT_HOLD], |r| {
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
                "SELECT 1 FROM quarantine WHERE hash = ?1 AND substr(rule, -length(?2)) = ?2
                 UNION ALL
                 SELECT 1 FROM tombstones WHERE hash = ?1 AND substr(rule, -length(?2)) = ?2",
                params![&hash[..], ADULT_HOLD],
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::StoreConfig;

    /// The adult-hold mark is a suffix on the stored rule id; reads strip
    /// it. Pinned because no migration records it (§20.7).
    #[test]
    fn the_adult_hold_mark_is_a_rule_suffix() {
        let mut s = Store::open_in_memory(StoreConfig::default()).unwrap();
        fn f(hash: &[[u8; 20]], held: Held, adult_hold: bool) -> Filtered<'_> {
            Filtered {
                hashes: hash,
                rule: "CS-5a",
                action: 4,
                held: Some(held),
                sealed: None,
                evidence: &[],
                at: 10,
                expires_at: 20,
                adult_hold,
            }
        }
        let tx = s.conn.transaction().unwrap();
        write(
            &tx,
            &[
                f(&[[1; 20]], Held::Quarantine, true),
                f(&[[2; 20]], Held::Tombstone, true),
                f(&[[3; 20]], Held::Quarantine, false),
            ],
        )
        .unwrap();
        tx.commit().unwrap();
        let stored = |table: &str, h: u8| -> String {
            s.conn
                .query_row(
                    &format!("SELECT rule FROM {table} WHERE hash = ?1"),
                    [&[h; 20][..]],
                    |r| r.get(0),
                )
                .unwrap()
        };
        assert_eq!(stored("quarantine", 1), "CS-5a+adult");
        assert_eq!(stored("tombstones", 2), "CS-5a+adult");
        assert_eq!(stored("quarantine", 3), "CS-5a");
        assert_eq!(split_rule("CS-5a+adult".into()), ("CS-5a".into(), true));
        assert_eq!(split_rule("U-12".into()), ("U-12".into(), false));
        let q = s.quarantined(10).unwrap();
        assert!(q.iter().all(|q| q.rule == "CS-5a"));
        assert!(s.adult_hold(&[1; 20]).unwrap() && s.adult_hold(&[2; 20]).unwrap());
        assert!(!s.adult_hold(&[3; 20]).unwrap());
        assert_eq!(s.held_counts().unwrap(), vec![("CS-5a".into(), 2, 1)]);
        let adult = s.tombstones_before(None, true, 10).unwrap();
        assert_eq!(adult.len(), 1);
        assert!(adult[0].adult_hold);
    }

    /// An adult hold's purge never replaces the floor's tombstone:
    /// switching that adult rule off later mustn't release a floor hold.
    #[test]
    fn the_purge_keeps_the_floors_tombstone() {
        let mut s = Store::open_in_memory(StoreConfig::default()).unwrap();
        let h = [[7u8; 20]];
        let row = |rule, held, adult_hold| Filtered {
            hashes: &h,
            rule,
            action: 4,
            held: Some(held),
            sealed: None,
            evidence: &[],
            at: 10,
            expires_at: 20,
            adult_hold,
        };
        let tx = s.conn.transaction().unwrap();
        write(
            &tx,
            &[
                row("CS-1a", Held::Tombstone, false),
                row("CS-5a", Held::Quarantine, true),
            ],
        )
        .unwrap();
        tx.commit().unwrap();
        let (released, _) = s.purge_filter(100, 0).unwrap();
        assert!(released.is_empty());
        let t = s.tombstones(10).unwrap();
        assert_eq!(t.len(), 1);
        assert_eq!((t[0].rule.as_str(), t[0].adult_hold), ("CS-1a", false));
        assert_eq!(s.release_rules(&["CS-5a".into()], |_| {}).unwrap(), 0);
        assert_eq!(s.tombstones(10).unwrap().len(), 1, "the floor's stays");
    }

    /// Any other rule's tombstone gives way to the adult mark, which
    /// filtering couldn't rebuild: removing that rule later doesn't free
    /// the hash, switching the Adult-only rule off does.
    #[test]
    fn the_purge_marks_another_rules_tombstone_adult() {
        for other in ["U-3", "L-2", "FK-4"] {
            let mut s = Store::open_in_memory(StoreConfig::default()).unwrap();
            let h = [[8u8; 20]];
            let row = |rule, held, adult_hold| Filtered {
                hashes: &h,
                rule,
                action: 4,
                held: Some(held),
                sealed: None,
                evidence: &[],
                at: 10,
                expires_at: 20,
                adult_hold,
            };
            let tx = s.conn.transaction().unwrap();
            write(
                &tx,
                &[
                    row(other, Held::Tombstone, false),
                    row("CS-5a", Held::Quarantine, true),
                ],
            )
            .unwrap();
            tx.commit().unwrap();
            assert!(s.purge_filter(100, 0).unwrap().0.is_empty());
            let t = s.tombstones(10).unwrap();
            assert_eq!(t.len(), 1, "{other}");
            assert_eq!(
                (t[0].rule.as_str(), t[0].adult_hold),
                ("CS-5a", true),
                "{other}"
            );
            assert_eq!(
                s.release_rules(&[other.into()], |_| {}).unwrap(),
                0,
                "{other}"
            );
            assert_eq!(
                s.release_rules(&["CS-5a".into()], |_| {}).unwrap(),
                1,
                "{other}"
            );
        }
    }

    /// Releasing a rule's holds runs in chunks and reports each, and a
    /// hash another rule still holds isn't reported as released.
    #[test]
    fn releasing_a_rule_runs_in_chunks() {
        let mut s = Store::open_in_memory(StoreConfig::default()).unwrap();
        let n = crate::MARK_CHUNK + 5;
        let hashes: Vec<[u8; 20]> = (0..n as u32)
            .map(|i| {
                let mut h = [0u8; 20];
                h[..4].copy_from_slice(&i.to_be_bytes());
                h
            })
            .collect();
        let row = |hashes, rule, held| Filtered {
            hashes,
            rule,
            action: 4,
            held: Some(held),
            sealed: None,
            evidence: &[],
            at: 10,
            expires_at: 20,
            adult_hold: false,
        };
        let tx = s.conn.transaction().unwrap();
        let mut rows: Vec<Filtered<'_>> = hashes
            .iter()
            .map(|h| row(std::slice::from_ref(h), "U-1", Held::Quarantine))
            .collect();
        rows.push(row(&hashes[..1], "FK-4", Held::Tombstone));
        write(&tx, &rows).unwrap();
        tx.commit().unwrap();
        let mut calls = 0;
        let mut freed = Vec::new();
        let total = s
            .release_rules(&["U-1".into()], |hs| {
                calls += 1;
                freed.extend_from_slice(hs);
            })
            .unwrap();
        assert_eq!(total, n - 1, "the pack still holds one");
        assert_eq!(freed.len(), n - 1);
        assert!(!freed.contains(&hashes[0]));
        assert!(calls >= 2, "{calls}");
        assert!(s.quarantined(10).unwrap().is_empty());
    }
}
