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

/// Hashes per query of [`Store::latest_hits`]: under SQLite's variable
/// limit.
pub const LATEST_HITS_CHUNK: usize = 500;

/// How long a WAL scrub's TRUNCATE checkpoint may wait for readers and
/// writers on other connections (see [`Store::scrub_wal`]).
const SCRUB_WAIT: std::time::Duration = std::time::Duration::from_millis(100);

/// The locked floor's rule ids (the filter's `CoreRule::floor`; a filter
/// test keeps them equal). An adult hold's purge never replaces their
/// tombstones.
pub const FLOOR_RULES: [&str; 3] = ["CS-1a", "CS-1b", "CS-8a"];

/// The stored rule of a floor drop a remote worker reported (WK-10d),
/// followed by the worker's id: `worker-floor:7`. Such a row is a floor
/// hold everywhere ([`is_floor_hold`]): an adult hold's purge never
/// replaces it, a rollback keeps it over a weaker hold, and no release
/// removes it. The only way out is WK-13's purge-by-worker, which deletes
/// a removed worker's rows by rule.
pub const WORKER_FLOOR_PREFIX: &str = "worker-floor:";

/// Whether a stored tombstone rule is a floor hold: the locked floor's
/// rules, or a worker's floor report.
pub fn is_floor_hold(rule: &str) -> bool {
    FLOOR_RULES.contains(&rule) || rule.starts_with(WORKER_FLOOR_PREFIX)
}

/// The SQL guards, built here only so every one agrees. The values are
/// constants, written in place.
///
/// A worker's floor report over the column `col`: the exact,
/// case-sensitive prefix, as `str::starts_with` (not `LIKE`, which ignores
/// ASCII case).
pub(crate) fn worker_floor_sql(col: &str) -> String {
    let n = WORKER_FLOOR_PREFIX.len();
    format!("(substr({col}, 1, {n}) = '{WORKER_FLOOR_PREFIX}')")
}

/// The locked floor's own rules over `col`.
pub(crate) fn local_floor_sql(col: &str) -> String {
    let [a, b, c] = FLOOR_RULES;
    format!("({col} IN ('{a}', '{b}', '{c}'))")
}

/// [`is_floor_hold`] over `col`.
pub(crate) fn floor_hold_sql(col: &str) -> String {
    format!("({} OR {})", local_floor_sql(col), worker_floor_sql(col))
}

/// A release's tombstone delete: never a worker's floor report. (The
/// locked floor's own rows are released as before; their rules refuse the
/// key again when it's next seen, which nothing here can do for a
/// worker's report.)
fn release_tombstone_sql() -> String {
    format!(
        "DELETE FROM tombstones WHERE hash = ?1 AND NOT {}",
        worker_floor_sql("rule")
    )
}

/// Appended to the stored rule id of an adult hold
/// ([`Filtered::adult_hold`]) in `quarantine` and `tombstones`. Reads
/// strip it, so callers see plain rule ids.
const ADULT_HOLD: &str = "+adult";

/// The hashes one rule holds: quarantined, then tombstoned.
pub type RuleHolds = (Vec<[u8; 20]>, Vec<[u8; 20]>);

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

/// One quarantine row ([`Store::quarantine_between`]), for releasing it
/// only if it's unchanged ([`Store::release_rows`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HoldRow {
    pub hash: [u8; 20],
    pub rule: String,
    pub at: i64,
    pub sealed: Option<Vec<u8>>,
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
    // As before, a later hold replaces an earlier one (and the insert
    // triggers fire), except over a worker's floor report, which only a
    // local floor rule may take over (#604's review): otherwise a weaker
    // rule there could later be released and free the hash.
    let mut tomb = tx.prepare_cached(&format!(
        "INSERT OR REPLACE INTO tombstones (hash, rule, at)
         SELECT ?1, ?2, ?3 WHERE {} OR NOT EXISTS (
             SELECT 1 FROM tombstones WHERE hash = ?1 AND {}
         )",
        local_floor_sql("?2"),
        worker_floor_sql("rule")
    ))?;
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

    /// Records a remote worker's floor drops (CS-1a, CS-1b, CS-8a or a
    /// hash list, reported as `Lost(Blocked)`) as tombstones under
    /// `worker-floor:<worker>` (WK-10d), so the keys are never claimed,
    /// fetched or stored again. A hash already tombstoned keeps its row (a
    /// local floor row is never renamed). Returns how many rows were added.
    pub fn tombstone_from_worker(
        &mut self,
        hashes: &[[u8; 20]],
        worker: u32,
        at: i64,
    ) -> Result<usize> {
        if hashes.is_empty() {
            return Ok(0);
        }
        let rule = format!("{WORKER_FLOOR_PREFIX}{worker}");
        let tx = crate::begin_write(&mut self.conn)?;
        let mut added = 0;
        {
            let mut ins = tx.prepare_cached(
                "INSERT INTO tombstones (hash, rule, at) VALUES (?1, ?2, ?3)
                 ON CONFLICT (hash) DO NOTHING",
            )?;
            for h in hashes {
                added += ins.execute(params![&h[..], rule, at])?;
            }
        }
        tx.commit()?;
        Ok(added)
    }

    /// Releases a held hash (removes its tombstone or quarantine row), so
    /// it can be fetched again. Returns whether a row was removed. A
    /// worker's floor report is never removed, so a hash with one stays
    /// held even when its quarantine row goes and this returns `true`
    /// ([`Store::held_among`] tells).
    pub fn release(&mut self, hash: &[u8; 20]) -> Result<bool> {
        let tx = self.conn.transaction()?;
        let n = tx.execute(&release_tombstone_sql(), [&hash[..]])?
            + tx.execute("DELETE FROM quarantine WHERE hash = ?1", [&hash[..]])?;
        tx.commit()?;
        if n > 0 {
            self.scrub_wal();
        }
        Ok(n > 0)
    }

    /// Deletes stored torrents with any of these swarm hashes, and logs
    /// them so the search indexer drops their documents (§20.5.2: abuse
    /// material reported or confirmed after it was stored). Returns the
    /// deleted ids. Ids are never reused, so hot-column slots left behind
    /// are never read again.
    pub fn delete_torrents(&mut self, hashes: &[[u8; 20]]) -> Result<Vec<i64>> {
        let tx = crate::begin_write(&mut self.conn)?;
        let mut ids = Vec::new();
        for h in hashes {
            if let Some(id) = crate::find_any(&tx, std::iter::once(InfoHash(*h)))?
                && !ids.contains(&id)
            {
                ids.push(id);
            }
        }
        {
            // Cached: the delete carries Insights' trigger, which SQLite
            // would otherwise compile again for every row.
            let mut delete = tx.prepare_cached("DELETE FROM torrents WHERE id = ?1")?;
            let mut log = tx.prepare_cached("INSERT INTO reindex_log (torrent_id) VALUES (?1)")?;
            for id in &ids {
                delete.execute([id])?;
                log.execute([id])?;
            }
        }
        tx.commit()?;
        if !ids.is_empty() {
            self.scrub_wal();
        }
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
    /// its sealed name goes, its hash stays refused (§20.9). A hash that's
    /// also tombstoned isn't released: the tombstone still refuses it.
    /// Returns the released hashes and how many hits were removed.
    pub fn purge_filter(&mut self, now: i64, hits_before: i64) -> Result<(Vec<[u8; 20]>, usize)> {
        let tx = crate::begin_write(&mut self.conn)?;
        let mut expired = Vec::new();
        {
            let mut stmt = tx.prepare(
                "SELECT hash FROM quarantine
                 WHERE expires_at <= ?1 AND substr(rule, -length(?2)) != ?2
                   AND hash NOT IN (SELECT hash FROM tombstones)",
            )?;
            let adult = ADULT_HOLD;
            for row in stmt.query_map(params![now, adult], |r| r.get::<_, Vec<u8>>(0))? {
                expired.push(hash20(row?)?);
            }
            // The adult mark is the one hold filtering can't rebuild (the
            // correction went with the torrent), so it replaces another
            // rule's tombstone, except a floor hold, which stays.
            tx.execute(
                &format!(
                    "INSERT INTO tombstones (hash, rule, at)
                     SELECT hash, rule, at FROM quarantine
                     WHERE expires_at <= ?1 AND substr(rule, -length(?2)) = ?2
                     ON CONFLICT (hash) DO UPDATE SET rule = excluded.rule, at = excluded.at
                     WHERE NOT {}",
                    floor_hold_sql("tombstones.rule")
                ),
                params![now, adult],
            )?;
        }
        tx.execute("DELETE FROM quarantine WHERE expires_at <= ?1", [now])?;
        let hits = tx.execute("DELETE FROM filter_hits WHERE at < ?1", [hits_before])?;
        tx.commit()?;
        // Every purge scrubs (hourly on the writer), not only one that
        // deleted something: it retries a scrub that was busy on any
        // connection, since they share the WAL.
        self.scrub_wal();
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
        let mut deleted_any = false;
        // A worker's floor report isn't released by rule here: only WK-13's
        // purge-by-worker removes it.
        for rule in rules.iter().filter(|r| !r.starts_with(WORKER_FLOOR_PREFIX)) {
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
                    deleted_any |= !deleted.is_empty();
                    released(&free);
                    total += free.len();
                    if deleted.len() < crate::MARK_CHUNK {
                        break;
                    }
                }
            }
        }
        if deleted_any || self.scrub_pending {
            self.scrub_wal();
        }
        Ok(total)
    }

    /// Every hash `rule` holds, adult holds included: the quarantined and
    /// the tombstoned ones. A scan, as `rule` has no index (§20.7).
    pub fn held_by_rule(&self, rule: &str) -> Result<RuleHolds> {
        let mut out = (Vec::new(), Vec::new());
        for (table, into) in [("quarantine", &mut out.0), ("tombstones", &mut out.1)] {
            let mut stmt = self
                .conn
                .prepare_cached(&format!("SELECT hash FROM {table} WHERE rule IN (?1, ?2)"))?;
            let rows = stmt.query_map(params![rule, held_rule(rule, true)], |r| {
                r.get::<_, Vec<u8>>(0)
            })?;
            for h in rows {
                into.push(hash20(h?)?);
            }
        }
        Ok(out)
    }

    /// Releases what `rule` holds of `hashes` only, quarantine and
    /// tombstones: a list version that no longer lists them. A hold by
    /// another rule stays. Deletes in transactions of up to
    /// [`MARK_CHUNK`](crate::MARK_CHUNK) hashes and calls `released`
    /// after each with the hashes no longer held by anything. Returns how
    /// many were released.
    pub fn release_rule_hashes(
        &mut self,
        rule: &str,
        hashes: &[[u8; 20]],
        mut released: impl FnMut(&[[u8; 20]]),
    ) -> Result<usize> {
        // As `release_rules`: a worker's floor report isn't released by
        // rule here.
        if rule.starts_with(WORKER_FLOOR_PREFIX) {
            return Ok(0);
        }
        let adult = held_rule(rule, true);
        let mut total = 0;
        let mut deleted_any = false;
        for chunk in hashes.chunks(crate::MARK_CHUNK) {
            let tx = self.conn.transaction()?;
            let mut free = Vec::new();
            {
                let mut del = [
                    tx.prepare_cached(
                        "DELETE FROM quarantine WHERE hash = ?1 AND rule IN (?2, ?3)",
                    )?,
                    tx.prepare_cached(
                        "DELETE FROM tombstones WHERE hash = ?1 AND rule IN (?2, ?3)",
                    )?,
                ];
                let mut held = tx.prepare_cached(
                    "SELECT 1 FROM quarantine WHERE hash = ?1
                     UNION ALL SELECT 1 FROM tombstones WHERE hash = ?1",
                )?;
                for h in chunk {
                    let mut n = 0;
                    for d in &mut del {
                        n += d.execute(params![&h[..], rule, adult])?;
                    }
                    if n > 0 {
                        deleted_any = true;
                        if !held.exists([&h[..]])? {
                            free.push(*h);
                        }
                    }
                }
            }
            tx.commit()?;
            released(&free);
            total += free.len();
        }
        if deleted_any || self.scrub_pending {
            self.scrub_wal();
        }
        Ok(total)
    }

    /// After deleting sealed names or torrents: secure_delete zeroes the
    /// freed pages in the main file, but the WAL still holds the frames
    /// written when the rows went in, until it's checkpointed and
    /// truncated (owner decision: deleted data is overwritten).
    ///
    /// A PASSIVE checkpoint first copies the frames back without taking
    /// the write lock. The TRUNCATE that follows needs that lock and every
    /// reader to be done with the WAL, so it waits at most
    /// [`SCRUB_WAIT`] instead of the store's 5 s busy timeout: a write on
    /// another connection (an ingest batch) waits for the lock meanwhile,
    /// and its writes begin IMMEDIATE ([`begin_write`](crate::begin_write))
    /// so that wait is the busy timeout, not an instant SQLITE_BUSY.
    ///
    /// Best effort, and it never fails the delete that already committed.
    /// A busy reader, a writer on another connection or an I/O error sets
    /// `scrub_pending`, and the next delete on this connection tries again.
    /// So does the writer's hourly [`purge_filter`](Store::purge_filter),
    /// which scrubs every time: the WAL is shared, so that covers deletes
    /// made on any connection. The shutdown checkpoint truncates it too.
    /// A no-op for an in-memory store.
    pub(crate) fn scrub_wal(&mut self) {
        let _ = self
            .conn
            .query_row("PRAGMA wal_checkpoint(PASSIVE)", [], |_| Ok(()));
        let _ = self.conn.busy_timeout(SCRUB_WAIT);
        let done = self
            .conn
            .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |r| {
                r.get::<_, i64>(0)
            })
            .is_ok_and(|busy| busy == 0);
        let _ = self.conn.busy_timeout(crate::schema::BUSY_TIMEOUT);
        self.scrub_pending = !done;
    }

    /// [`release`](Store::release) for many hashes in one transaction,
    /// with one WAL scrub at the end rather than one per hash. Returns,
    /// per hash, whether a row was removed (a hash a worker's floor report
    /// holds stays held, as with `release`).
    pub fn release_many(&mut self, hashes: &[[u8; 20]]) -> Result<Vec<bool>> {
        let tx = self.conn.transaction()?;
        let mut had = Vec::with_capacity(hashes.len());
        {
            let mut tomb = tx.prepare_cached(&release_tombstone_sql())?;
            let mut quar = tx.prepare_cached("DELETE FROM quarantine WHERE hash = ?1")?;
            for h in hashes {
                had.push(tomb.execute([&h[..]])? + quar.execute([&h[..]])? > 0);
            }
        }
        tx.commit()?;
        if had.iter().any(|&h| h) {
            self.scrub_wal();
        }
        Ok(had)
    }

    /// Per hash, whether a tombstone or quarantine row holds it now: what
    /// the writer and the releases ask under `HeldSet`'s lock, so intake's
    /// set follows the store exactly (`2026-10-31-B-held-exact.md`).
    pub fn held_among(&self, hashes: &[[u8; 20]]) -> Result<Vec<bool>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT EXISTS (SELECT 1 FROM tombstones WHERE hash = ?1)
                 OR EXISTS (SELECT 1 FROM quarantine WHERE hash = ?1)",
        )?;
        hashes
            .iter()
            .map(|h| Ok(stmt.query_row([&h[..]], |r| r.get::<_, bool>(0))?))
            .collect()
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

    /// The rule `hash` is quarantined under, as stored (an adult hold's
    /// with its suffix), if it's quarantined.
    pub fn quarantine_rule(&self, hash: &[u8; 20]) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row(
                "SELECT rule FROM quarantine WHERE hash = ?1",
                [&hash[..]],
                |r| r.get(0),
            )
            .optional()?)
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

    /// `rule`'s quarantine made in `since..=until`, oldest first, after the
    /// `(at, hash)` cursor `after`: what a rule's stand-in made while its
    /// row couldn't be read (docs/decisions/2026-11-02-F-filter-standin-undo.md).
    /// A stand-in never holds as an adult hold.
    pub fn quarantine_between(
        &self,
        rule: &str,
        (since, until): (i64, i64),
        after: Option<(i64, [u8; 20])>,
        limit: usize,
    ) -> Result<Vec<HoldRow>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT hash, at, sealed FROM quarantine
             WHERE rule = ?1 AND at >= ?2 AND at <= ?3 AND (at, hash) > (?4, ?5)
             ORDER BY at, hash LIMIT ?6",
        )?;
        let (at, hash) = after.unwrap_or((i64::MIN, [0; 20]));
        let rows = stmt.query_map(
            params![rule, since, until, at, &hash[..], limit as i64],
            |r| {
                Ok((
                    r.get::<_, Vec<u8>>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, Option<Vec<u8>>>(2)?,
                ))
            },
        )?;
        let mut out = Vec::new();
        for row in rows {
            let (hash, at, sealed) = row?;
            out.push(HoldRow {
                hash: hash20(hash)?,
                rule: rule.to_owned(),
                at,
                sealed,
            });
        }
        Ok(out)
    }

    /// The hashes adult holds made in `since..=until` hold, in quarantine
    /// or as tombstones.
    pub fn adult_holds_between(&self, (since, until): (i64, i64)) -> Result<Vec<[u8; 20]>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT DISTINCT hash FROM (
                 SELECT hash, rule, at FROM quarantine
                 UNION ALL SELECT hash, rule, at FROM tombstones
             ) WHERE at >= ?1 AND at <= ?2 AND substr(rule, -length(?3)) = ?3",
        )?;
        let rows = stmt.query_map(params![since, until, ADULT_HOLD], |r| {
            r.get::<_, Vec<u8>>(0)
        })?;
        rows.map(|h| hash20(h?)).collect()
    }

    /// How many of `rule`'s tombstones were made in `since..=until`.
    pub fn tombstones_between(&self, rule: &str, (since, until): (i64, i64)) -> Result<usize> {
        let n: i64 = self.conn.query_row(
            "SELECT count(*) FROM tombstones WHERE rule = ?1 AND at >= ?2 AND at <= ?3",
            params![rule, since, until],
            |r| r.get(0),
        )?;
        Ok(n as usize)
    }

    /// How many torrents `rule` applied `action` to (not in shadow mode)
    /// in `since..=until`, as `filter_hits` still records them.
    pub fn hits_between(
        &self,
        rule: &str,
        action: u8,
        (since, until): (i64, i64),
    ) -> Result<usize> {
        let n: i64 = self.conn.query_row(
            "SELECT count(DISTINCT hash) FROM filter_hits
             WHERE rule = ?1 AND action = ?2 AND at >= ?3 AND at <= ?4",
            params![rule, action, since, until],
            |r| r.get(0),
        )?;
        Ok(n as usize)
    }

    /// Releases these quarantine rows if each is still as read: a row a
    /// writer has replaced since (another rule, a newer time) stays. One
    /// transaction. Returns the hashes nothing holds any more, for intake.
    pub fn release_rows(&mut self, rows: &[HoldRow]) -> Result<Vec<[u8; 20]>> {
        if rows.is_empty() {
            return Ok(Vec::new());
        }
        let tx = crate::begin_write(&mut self.conn)?;
        let mut free = Vec::new();
        {
            let mut del = tx.prepare_cached(
                "DELETE FROM quarantine WHERE hash = ?1 AND rule = ?2 AND at = ?3",
            )?;
            let mut held = tx.prepare_cached("SELECT 1 FROM tombstones WHERE hash = ?1")?;
            for r in rows {
                if del.execute(params![&r.hash[..], r.rule, r.at])? > 0
                    && !held.exists([&r.hash[..]])?
                {
                    free.push(r.hash);
                }
            }
        }
        tx.commit()?;
        self.scrub_wal();
        Ok(free)
    }

    /// The newest applied filter hit on `hash` (not a shadow one), if
    /// `filter_hits` still holds one ([`Store::latest_hits`]).
    pub fn latest_hit(&self, hash: &[u8; 20]) -> Result<Option<FilterHit>> {
        Ok(self.latest_hits(std::slice::from_ref(hash))?.remove(hash))
    }

    /// The newest applied filter hit (not a shadow one) on each of
    /// `hashes` that `filter_hits` still holds one for: the decision
    /// ingest last recorded, under the torrent's first swarm hash. There's
    /// no index on `hash`, so each chunk of [`LATEST_HITS_CHUNK`] hashes
    /// is one scan of the table: for rare paths (SP-4's release hook and
    /// Undo, RP-5), never ingest.
    pub fn latest_hits(
        &self,
        hashes: &[[u8; 20]],
    ) -> Result<std::collections::HashMap<[u8; 20], FilterHit>> {
        let mut out: std::collections::HashMap<[u8; 20], (i64, FilterHit)> =
            std::collections::HashMap::new();
        let mut wanted: Vec<&[u8; 20]> = hashes.iter().collect();
        wanted.sort_unstable();
        wanted.dedup();
        for chunk in wanted.chunks(LATEST_HITS_CHUNK) {
            let marks = vec!["?"; chunk.len()].join(", ");
            let sql = format!(
                "SELECT hash, rule, action, at, evidence, id FROM filter_hits
                 WHERE action & {SHADOW} = 0 AND hash IN ({marks})"
            );
            let mut stmt = self.conn.prepare(&sql)?;
            let rows = stmt.query_map(
                rusqlite::params_from_iter(chunk.iter().map(|h| &h[..])),
                |r| {
                    Ok((
                        r.get::<_, Vec<u8>>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, u8>(2)?,
                        r.get::<_, i64>(3)?,
                        r.get::<_, Vec<u8>>(4)?,
                        r.get::<_, i64>(5)?,
                    ))
                },
            )?;
            for row in rows {
                let (h, rule, action, at, evidence, id) = row?;
                let hash = hash20(h)?;
                let newer = out
                    .get(&hash)
                    .is_none_or(|(old_id, old)| (at, id) > (old.at, *old_id));
                if newer {
                    out.insert(
                        hash,
                        (
                            id,
                            FilterHit {
                                hash,
                                rule,
                                action,
                                at,
                                evidence,
                            },
                        ),
                    );
                }
            }
        }
        Ok(out.into_iter().map(|(h, (_, hit))| (h, hit)).collect())
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

    #[test]
    fn the_latest_hit_is_the_newest_applied_one() {
        let s = Store::open_in_memory(StoreConfig::default()).unwrap();
        assert_eq!(s.latest_hit(&[8; 20]).unwrap(), None);
        s.conn
            .execute(
                "INSERT INTO filter_hits (hash, rule, action, at, evidence)
                 VALUES (?1, 'SP-2', 2, 21, x''), (?1, 'SP-3', 1, 22, x''),
                        (?1, 'SP-4', 130, 23, x''), (?2, 'U-1', 2, 30, x'')",
                params![&[8u8; 20][..], &[9u8; 20][..]],
            )
            .unwrap();
        let h = s.latest_hit(&[8; 20]).unwrap().unwrap();
        assert_eq!(
            (h.rule.as_str(), h.action, h.at),
            ("SP-3", 1, 22),
            "not the shadow one"
        );
        assert_eq!(s.latest_hit(&[9; 20]).unwrap().unwrap().rule, "U-1");
    }

    /// Many hashes in one call, past a chunk: each hash's newest applied
    /// hit, by time and then by insertion, as written.
    #[test]
    fn latest_hits_in_chunks_find_each_hashs_newest_applied_hit() {
        let s = Store::open_in_memory(StoreConfig::default()).unwrap();
        let hash = |i: u32| {
            let mut h = [0u8; 20];
            h[..4].copy_from_slice(&i.to_le_bytes());
            h
        };
        let n = LATEST_HITS_CHUNK as u32 * 2 + 7;
        let mut want: std::collections::HashMap<[u8; 20], (String, i64)> = Default::default();
        for i in 0..n {
            for (k, rule) in ["SP-2", "SP-3", "U-1"].iter().enumerate() {
                if (i + k as u32).is_multiple_of(3) {
                    continue;
                }
                let shadow = (i + k as u32).is_multiple_of(5);
                let at = i64::from((i * 7 + k as u32 * 3) % 4);
                s.conn
                    .execute(
                        "INSERT INTO filter_hits (hash, rule, action, at, evidence)
                         VALUES (?1, ?2, ?3, ?4, x'')",
                        params![&hash(i)[..], rule, if shadow { 0x82 } else { 2 }, at],
                    )
                    .unwrap();
                // Later inserts win a tie.
                if !shadow && want.get(&hash(i)).is_none_or(|(_, old)| at >= *old) {
                    want.insert(hash(i), ((*rule).to_owned(), at));
                }
            }
        }
        let all: Vec<[u8; 20]> = (0..n + 3).map(hash).collect();
        let got = s.latest_hits(&all).unwrap();
        assert!(got.len() > LATEST_HITS_CHUNK);
        assert_eq!(got.len(), want.len());
        for (h, (rule, at)) in &want {
            let hit = &got[h];
            assert_eq!((&hit.rule, hit.at), (rule, *at));
            assert_eq!(s.latest_hit(h).unwrap().as_ref(), Some(hit));
        }
    }

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

    #[test]
    fn held_among_reads_both_tables() {
        let mut s = Store::open_in_memory(crate::StoreConfig::default()).unwrap();
        let tx = s.conn.transaction().unwrap();
        let f = |h: &'static [[u8; 20]], held| Filtered {
            hashes: h,
            rule: "U-1",
            action: 4,
            held: Some(held),
            sealed: None,
            evidence: &[],
            at: 10,
            expires_at: 20,
            adult_hold: false,
        };
        write(
            &tx,
            &[
                f(&[[1; 20]], Held::Tombstone),
                f(&[[2; 20]], Held::Quarantine),
            ],
        )
        .unwrap();
        tx.commit().unwrap();
        assert_eq!(
            s.held_among(&[[1; 20], [2; 20], [3; 20]]).unwrap(),
            vec![true, true, false]
        );
        s.release(&[1; 20]).unwrap();
        assert_eq!(s.held_among(&[[1; 20]]).unwrap(), vec![false]);
    }

    /// An expired quarantine on a hash that's also tombstoned isn't
    /// reported as released: the writer drops what's released from its
    /// held set, and the tombstone still refuses the hash.
    #[test]
    fn an_expired_quarantine_under_a_tombstone_is_not_released() {
        let mut s = Store::open_in_memory(StoreConfig::default()).unwrap();
        for (table, h) in [("quarantine", 1u8), ("quarantine", 2), ("tombstones", 2)] {
            let sql = if table == "quarantine" {
                "INSERT INTO quarantine (hash, rule, sealed, at, expires_at)
                 VALUES (?1, 'CS-3', NULL, 10, 20)"
            } else {
                "INSERT INTO tombstones (hash, rule, at) VALUES (?1, 'CS-4', 10)"
            };
            s.conn.execute(sql, [&[h; 20][..]]).unwrap();
        }
        let (released, _) = s.purge_filter(100, 0).unwrap();
        assert_eq!(released, vec![[1; 20]]);
        assert!(s.quarantined(10).unwrap().is_empty());
        assert_eq!(
            s.filter_state().unwrap().held,
            vec![([2; 20], Held::Tombstone)]
        );
    }

    /// Deleting a held hash through any of the store's paths leaves its
    /// sealed name neither in the database file nor in the WAL. The name
    /// is large enough to sit on overflow pages, which secure_delete's
    /// FAST mode would leave behind in the freelist.
    #[test]
    fn deleted_holds_leave_no_trace_on_disk() {
        let marker = b"sealed-name-marker-5c1e7b";
        let mut sealed: Vec<u8> = (0..20_000u32)
            .map(|i| (i.wrapping_mul(2_654_435_761) >> 24) as u8)
            .collect();
        sealed.extend_from_slice(marker);
        let contains = |hay: &[u8]| hay.windows(marker.len()).any(|w| w == marker);
        type Delete = fn(&mut Store, [u8; 20]);
        let paths: [(&str, Delete); 4] = [
            ("purge", |s, _| {
                s.purge_filter(100, 0).unwrap();
            }),
            ("release", |s, h| {
                assert!(s.release(&h).unwrap());
            }),
            ("release_rules", |s, _| {
                assert_eq!(s.release_rules(&["U-1".into()], |_| {}).unwrap(), 1);
            }),
            ("release_many", |s, h| {
                assert_eq!(s.release_many(&[h, [0; 20]]).unwrap(), vec![true, false]);
            }),
        ];
        for (name, delete) in paths {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("t.db");
            let mut s = Store::open(&path, StoreConfig::default()).unwrap();
            let h = [9u8; 20];
            let tx = s.conn.transaction().unwrap();
            write(
                &tx,
                &[Filtered {
                    hashes: &[h],
                    rule: "U-1",
                    action: 4,
                    held: Some(Held::Quarantine),
                    sealed: Some(&sealed),
                    evidence: &[],
                    at: 10,
                    expires_at: 20,
                    adult_hold: false,
                }],
            )
            .unwrap();
            tx.commit().unwrap();
            let wal = dir.path().join("t.db-wal");
            let on_disk = || {
                let mut all = std::fs::read(&path).unwrap();
                all.extend(std::fs::read(&wal).unwrap_or_default());
                all
            };
            assert!(contains(&on_disk()), "{name}: the name was written");
            delete(&mut s, h);
            assert!(!contains(&on_disk()), "{name}: the deleted name survived");
        }
    }

    /// A reader's snapshot that keeps one connection's scrub busy: the
    /// hourly purge on another connection finishes it, since they share
    /// the WAL, and the busy scrub leaves the normal busy timeout behind.
    #[test]
    fn a_purge_on_any_connection_finishes_a_busy_scrub() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.db");
        let wal = dir.path().join("t.db-wal");
        let mut api = Store::open(&path, StoreConfig::default()).unwrap();
        let mut writer = Store::open(&path, StoreConfig::default()).unwrap();
        let marker = b"busy-scrub-marker-2b7d";
        let h = [7u8; 20];
        let row = Filtered {
            hashes: &[h],
            rule: "U-1",
            action: 4,
            held: Some(Held::Quarantine),
            sealed: Some(marker),
            evidence: &[],
            at: 10,
            expires_at: 20,
            adult_hold: false,
        };
        let tx = api.conn.transaction().unwrap();
        write(&tx, &[row]).unwrap();
        tx.commit().unwrap();
        let on_disk = || {
            let mut all = std::fs::read(&path).unwrap();
            all.extend(std::fs::read(&wal).unwrap_or_default());
            all.windows(marker.len()).any(|w| w == marker)
        };
        let reader = rusqlite::Connection::open(&path).unwrap();
        reader
            .execute_batch("BEGIN; SELECT count(*) FROM quarantine;")
            .unwrap();
        assert!(api.release(&h).unwrap());
        assert!(api.scrub_pending, "the reader kept the scrub busy");
        assert!(on_disk(), "the WAL still holds the name");
        let timeout: i64 = api
            .conn
            .pragma_query_value(None, "busy_timeout", |r| r.get(0))
            .unwrap();
        assert_eq!(timeout, crate::schema::BUSY_TIMEOUT.as_millis() as i64);
        reader.execute_batch("COMMIT").unwrap();
        // Deletes nothing, and still scrubs.
        assert_eq!(writer.purge_filter(0, 0).unwrap(), (vec![], 0));
        assert!(
            !on_disk(),
            "the purge finished the other connection's scrub"
        );
    }

    /// Ingest on one connection waits out a scrub that holds the write
    /// lock on another (its TRUNCATE checkpoint, kept busy by a reader)
    /// instead of failing with "database is locked".
    #[test]
    fn ingest_waits_out_a_busy_scrub_on_another_connection() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.db");
        let mut api = Store::open(&path, StoreConfig::default()).unwrap();
        let mut writer = Store::open(&path, StoreConfig::default()).unwrap();
        let torrent = |n: u32| {
            let name = format!("Torrent.{n:06}");
            let mut raw = format!("d6:lengthi5e4:name{}:{name}", name.len()).into_bytes();
            raw.extend_from_slice(b"12:piece lengthi16384e6:pieces20:");
            raw.extend_from_slice(&[0; 20]);
            raw.push(b'e');
            raw
        };
        let insert = |s: &mut Store, n: u32| {
            let raw = torrent(n);
            let info = lodestar_metainfo::Info::parse(&raw).unwrap();
            s.insert(&[crate::NewTorrent {
                info: &info,
                raw_info: Some(&raw),
                discovered_at: 1,
                metadata_at: 1,
                extra_flags: 0,
                source: crate::SourceId::new(0).unwrap(),
            }])
            .map(|_| ())
        };
        // WAL frames the reader's snapshot pins, so every scrub is busy.
        insert(&mut writer, 0).unwrap();
        let reader = rusqlite::Connection::open(&path).unwrap();
        reader
            .execute_batch("BEGIN; SELECT count(*) FROM torrents;")
            .unwrap();
        let scrubs = std::thread::spawn(move || {
            // 10 busy scrubs wait about 10 × SCRUB_WAIT (≈ 1 s) in all, well
            // under BUSY_TIMEOUT (5 s), so an insert that waits never times out.
            for _ in 0..10 {
                api.scrub_wal();
                assert!(api.scrub_pending, "the reader kept the scrub busy");
            }
        });
        let started = std::time::Instant::now();
        let (mut n, mut failed, mut first) = (0, 0, None);
        while !scrubs.is_finished() {
            assert!(started.elapsed().as_secs() < 120, "hang guard");
            n += 1;
            if let Err(e) = insert(&mut writer, n) {
                failed += 1;
                first.get_or_insert(e.to_string());
            }
        }
        scrubs.join().unwrap();
        reader.execute_batch("COMMIT").unwrap();
        assert!(n > 0);
        assert_eq!(failed, 0, "of {n} inserts; first error: {first:?}");
    }

    /// WK-10d: a worker's floor report is a tombstone under
    /// `worker-floor:<id>`. It never renames an existing row (a local
    /// floor row above all), an expiring adult hold never replaces it, and
    /// no release removes it, by hash or by rule.
    #[test]
    fn a_workers_floor_report_is_a_floor_hold() {
        let mut s = Store::open_in_memory(StoreConfig::default()).unwrap();
        assert!(is_floor_hold("CS-8a") && is_floor_hold("worker-floor:7"));
        assert!(!is_floor_hold("CS-5a") && !is_floor_hold("worker-floor"));
        // A local floor row exists for [1]; [2] and [3] are new.
        let tx = s.conn.transaction().unwrap();
        let local = [[1u8; 20]];
        write(
            &tx,
            &[Filtered {
                hashes: &local,
                rule: "CS-8a",
                action: 4,
                held: Some(Held::Tombstone),
                sealed: None,
                evidence: &[],
                at: 5,
                expires_at: 0,
                adult_hold: false,
            }],
        )
        .unwrap();
        tx.commit().unwrap();
        let added = s
            .tombstone_from_worker(&[[1; 20], [2; 20], [3; 20]], 7, 10)
            .unwrap();
        assert_eq!(added, 2, "the local row is kept, not counted");
        let rules: std::collections::BTreeMap<[u8; 20], String> = s
            .tombstones(10)
            .unwrap()
            .into_iter()
            .map(|t| (t.hash, t.rule))
            .collect();
        assert_eq!(rules[&[1; 20]], "CS-8a", "never renamed");
        assert_eq!(rules[&[2; 20]], "worker-floor:7");
        // Again: nothing changes.
        assert_eq!(s.tombstone_from_worker(&[[2; 20]], 9, 11).unwrap(), 0);
        assert_eq!(
            s.held_among(&[[2; 20], [4; 20]]).unwrap(),
            vec![true, false]
        );

        // An adult hold expiring over [3] doesn't replace the report.
        let tx = s.conn.transaction().unwrap();
        let adult = [[3u8; 20]];
        write(
            &tx,
            &[Filtered {
                hashes: &adult,
                rule: "CS-5a",
                action: 4,
                held: Some(Held::Quarantine),
                sealed: None,
                evidence: &[],
                at: 10,
                expires_at: 20,
                adult_hold: true,
            }],
        )
        .unwrap();
        tx.commit().unwrap();
        s.purge_filter(100, 0).unwrap();
        let t3 = s
            .tombstones(10)
            .unwrap()
            .into_iter()
            .find(|t| t.hash == [3; 20])
            .unwrap();
        assert_eq!(t3.rule, "worker-floor:7");

        // A later hold under another rule (ingest, refile, hold_claims)
        // doesn't take the row over; a local floor rule may (below).
        let weaker = [[2u8; 20]];
        let hold = |s: &mut Store, rule: &'static str, at: i64| {
            let tx = s.conn.transaction().unwrap();
            write(
                &tx,
                &[Filtered {
                    hashes: &weaker,
                    rule,
                    action: 4,
                    held: Some(Held::Tombstone),
                    sealed: None,
                    evidence: &[],
                    at,
                    expires_at: 0,
                    adult_hold: false,
                }],
            )
            .unwrap();
            tx.commit().unwrap();
        };
        hold(&mut s, "SRC-2", 30);
        let rule_of = |s: &Store, h: [u8; 20]| {
            s.tombstones(10)
                .unwrap()
                .into_iter()
                .find(|t| t.hash == h)
                .map(|t| t.rule)
        };
        assert_eq!(rule_of(&s, [2; 20]).as_deref(), Some("worker-floor:7"));
        // The prefix is exact: another case is a different rule.
        assert_eq!(s.tombstone_from_worker(&[[8; 20]], 1, 1).unwrap(), 1);
        s.conn
            .execute(
                "UPDATE tombstones SET rule = 'Worker-Floor:1' WHERE hash = ?1",
                [&[8u8; 20][..]],
            )
            .unwrap();
        assert!(s.release(&[8; 20]).unwrap(), "not a worker's report");

        // No release removes a report: by hash, many, or by its rule.
        assert!(!s.release(&[2; 20]).unwrap());
        assert_eq!(
            s.release_many(&[[2; 20], [3; 20]]).unwrap(),
            vec![false, false]
        );
        assert_eq!(
            s.release_rules(&["worker-floor:7".into()], |_| {}).unwrap(),
            0
        );
        assert_eq!(
            s.release_rule_hashes("worker-floor:7", &[[2; 20]], |_| {})
                .unwrap(),
            0
        );
        assert_eq!(s.held_among(&[[2; 20], [3; 20]]).unwrap(), vec![true, true]);
        // A local floor rule may take over a report.
        hold(&mut s, "CS-8a", 40);
        assert_eq!(rule_of(&s, [2; 20]).as_deref(), Some("CS-8a"));
        // The locked floor's own rows are released as before.
        assert!(s.release(&[1; 20]).unwrap());
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

    /// A stand-in's quarantine is read by rule and time window, and
    /// released only while unchanged: a writer's newer hold stays.
    #[test]
    fn quarantine_between_reads_and_release_rows_keeps_replaced_rows() {
        let mut s = Store::open_in_memory(StoreConfig::default()).unwrap();
        let rows: &[(&str, u8, &str, i64)] = &[
            ("quarantine", 1, "U-3", 5),
            ("quarantine", 2, "U-3", 20),
            ("quarantine", 3, "U-3", 21),
            ("quarantine", 4, "U-3", 40),
            ("quarantine", 5, "CS-5a+adult", 22),
            ("tombstones", 6, "CS-5a+adult", 23),
            ("tombstones", 5, "CS-5a+adult", 24),
            ("tombstones", 7, "U-4", 24),
        ];
        for (table, h, rule, at) in rows {
            let cols = if *table == "quarantine" {
                "(hash, rule, at, expires_at) VALUES (?1, ?2, ?3, 99)"
            } else {
                "(hash, rule, at) VALUES (?1, ?2, ?3)"
            };
            s.conn
                .execute(
                    &format!("INSERT INTO {table} {cols}"),
                    params![&[*h; 20][..], rule, at],
                )
                .unwrap();
        }
        let hashes = |v: &[HoldRow]| v.iter().map(|r| r.hash[0]).collect::<Vec<_>>();
        let q = s.quarantine_between("U-3", (10, 30), None, 10).unwrap();
        assert_eq!(hashes(&q), [2, 3], "only the window");
        assert_eq!(
            hashes(&s.quarantine_between("U-3", (5, 20), None, 10).unwrap()),
            [1, 2]
        );
        let page = s
            .quarantine_between("U-3", (10, 30), Some((20, [2; 20])), 10)
            .unwrap();
        assert_eq!(hashes(&page), [3]);
        assert_eq!(s.adult_holds_between((10, 30)).unwrap().len(), 2);
        assert_eq!(s.tombstones_between("U-4", (10, 30)).unwrap(), 1);
        assert_eq!(s.tombstones_between("U-4", (25, 30)).unwrap(), 0);
        s.conn
            .execute(
                "INSERT INTO filter_hits (hash, rule, action, at, evidence)
                 VALUES (?1, 'U-3', 2, 21, x''), (?1, 'U-3', 2, 22, x''), (?2, 'U-3', 130, 21, x'')",
                params![&[8u8; 20][..], &[9u8; 20][..]],
            )
            .unwrap();
        assert_eq!(
            s.hits_between("U-3", 2, (10, 30)).unwrap(),
            1,
            "once, not in shadow"
        );
        // A writer holds 2 again meanwhile: its row isn't the one read.
        s.conn
            .execute(
                "UPDATE quarantine SET rule = 'U-9', at = 30 WHERE hash = ?1",
                [&[2u8; 20][..]],
            )
            .unwrap();
        let freed = s.release_rows(&q).unwrap();
        assert_eq!(freed, vec![[3; 20]]);
        assert!(s.held_among(&[[2; 20]]).unwrap()[0]);
    }
}
