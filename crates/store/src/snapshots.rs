//! Store snapshots (architecture §7, §12): the copies of the store that
//! `lodestar rollback` restores or keeps.
//!
//! - `<db>.pre-v<N>[.<k>].db`, taken by [`schema`](crate::schema) with
//!   `VACUUM INTO` before each migration;
//! - `<db>.before-rollback-<time>.db`, the copy of the store a rollback
//!   keeps before restoring a snapshot over it.
//!
//! Each is a full copy of the store, so it would keep what the live store
//! later purges: sealed quarantine names, deleted torrents (owner
//! decisions §2.16, §2.17). On the writer's hourly purge tick,
//! [`Store::purge_snapshots`]:
//!
//! - **deletes snapshots older than [`RETAIN_SECS`]** (90 days, the
//!   rollback window), going by the file's modification time, which
//!   `VACUUM INTO` sets when it writes the snapshot and this module puts
//!   back after each purge;
//! - **applies the live purge to the others**, matched to the schema the
//!   snapshot holds ([`Plan::for_version`]): expired quarantine (adult
//!   holds become tombstones, as live), sealed names the live store no
//!   longer holds, filter hits past their retention, and torrents the live
//!   store has deleted since the snapshot was taken (by hash: every
//!   deletion leaves the hash in `cs_hashes`, `tombstones` or
//!   `quarantine`). A snapshot of a schema this build doesn't know is left
//!   alone, with a warning.
//!
//! Writes use `secure_delete` and the WAL, then a TRUNCATE checkpoint and
//! a switch back to the rollback journal, so no pre-image of a purged page
//! is ever written and the snapshot ends as one file again.
//!
//! **A running rollback never loses its snapshot.** Only a writable store
//! connection runs this, and `lodestar rollback` (M6-cli's store lock)
//! refuses while any connection has the store open, and keeps the server
//! from opening it while the rollback runs.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, UNIX_EPOCH};

use rusqlite::{Connection, OpenFlags, OptionalExtension, params};

use crate::{InfoHash, Store};

/// Snapshots are kept this many days after they were taken.
pub const RETAIN_DAYS: i64 = 90;
/// [`RETAIN_DAYS`] in seconds.
pub const RETAIN_SECS: i64 = RETAIN_DAYS * 86_400;

/// Held hashes are looked for in a snapshot if they were recorded up to
/// this long before it was taken: a big store's `VACUUM INTO` takes a
/// while, and the modification time is set when it ends.
const TAKEN_MARGIN: i64 = 86_400;

/// After a pass over a snapshot, the next one looks up only holds
/// recorded since, less this much: a hold written on another connection
/// just before the pass may commit just after it.
const RECHECK_MARGIN: i64 = 600;

/// How long opening a snapshot waits for a lock. Nothing else should hold
/// one (see the module docs); this only keeps a stray `sqlite3` shell from
/// stalling the writer.
const SNAPSHOT_BUSY: Duration = Duration::from_millis(200);

/// Mirrors `filter::ADULT_HOLD`; the purge-equivalence test keeps them in
/// step.
const ADULT_HOLD: &str = "+adult";

/// `cs_hashes.origin` of a list import. Confirmed (0) and reported (1)
/// entries come from your own actions.
const IMPORTED: i64 = 2;

/// Which copy of the store a snapshot is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Kind {
    /// `<db>.pre-v<version>[.<repeat>].db`: taken before migrating to
    /// `version`, so it holds `version - 1`. `repeat` is the `k` of a
    /// repeat snapshot of the same migration (0 for the first).
    PreMigration { version: i64, repeat: u32 },
    /// `<db>.before-rollback-<time>.db`: the store as it was before a
    /// rollback replaced it.
    BeforeRollback,
}

/// One snapshot file of the store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    pub path: PathBuf,
    pub name: String,
    pub kind: Kind,
    /// When it was taken (Unix seconds): its modification time.
    pub taken_at: Option<i64>,
}

impl Snapshot {
    /// When it's deleted (Unix seconds), if its time is known.
    pub fn expires_at(&self) -> Option<i64> {
        self.taken_at.map(expires_at)
    }

    /// The migration it was taken before, for a pre-migration snapshot.
    pub fn version(&self) -> Option<i64> {
        match self.kind {
            Kind::PreMigration { version, .. } => Some(version),
            Kind::BeforeRollback => None,
        }
    }
}

/// When a snapshot taken at `taken_at` is deleted.
pub fn expires_at(taken_at: i64) -> i64 {
    taken_at.saturating_add(RETAIN_SECS)
}

/// The snapshots of the store at `db`: pre-migration ones oldest
/// migration first, then the copies rollbacks kept, oldest first. Only
/// regular files count: a symlink or folder with a snapshot's name isn't
/// one this build wrote.
pub fn list(db: &Path) -> std::io::Result<Vec<Snapshot>> {
    let dir = match db.parent() {
        Some(d) if !d.as_os_str().is_empty() => d,
        _ => Path::new("."),
    };
    let Some(stem) = db.file_stem().and_then(|s| s.to_str()) else {
        return Ok(Vec::new());
    };
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    let mut out = Vec::new();
    for entry in entries {
        let entry = entry?;
        let Some(name) = entry.file_name().to_str().map(str::to_string) else {
            continue;
        };
        let Some(kind) = parse_name(stem, &name) else {
            continue;
        };
        if !entry.file_type().is_ok_and(|t| t.is_file()) {
            continue;
        }
        let path = entry.path();
        out.push(Snapshot {
            taken_at: modified(&path),
            path,
            name,
            kind,
        });
    }
    // The rollback copies' names sort by time.
    out.sort_by(|a, b| (a.kind, &a.name).cmp(&(b.kind, &b.name)));
    Ok(out)
}

/// The kind of snapshot `name` is, for the store `<stem>.db`:
/// `<stem>.pre-v<N>[.<k>].db` (the names `schema::snapshot_path` gives)
/// or `<stem>.before-rollback-<YYYYMMDD>-<HHMMSS>.db` (rollback's).
pub fn parse_name(stem: &str, name: &str) -> Option<Kind> {
    let rest = name.strip_prefix(stem)?.strip_suffix(".db")?;
    let digits = |s: &str| !s.is_empty() && s.len() <= 9 && s.bytes().all(|b| b.is_ascii_digit());
    if let Some(stamp) = rest.strip_prefix(".before-rollback-") {
        let (day, time) = stamp.split_once('-')?;
        return (day.len() == 8 && time.len() == 6 && digits(day) && digits(time))
            .then_some(Kind::BeforeRollback);
    }
    let rest = rest.strip_prefix(".pre-v")?;
    let (n, k) = match rest.split_once('.') {
        Some((n, k)) => (n, Some(k)),
        None => (rest, None),
    };
    if !digits(n) || !k.is_none_or(digits) {
        return None;
    }
    Some(Kind::PreMigration {
        version: n.parse().ok()?,
        repeat: k.map_or(Some(0), |k| k.parse().ok())?,
    })
}

/// The oldest held hash that can matter to a snapshot taken at `taken_at`.
fn held_since(taken_at: i64) -> i64 {
    taken_at.saturating_sub(TAKEN_MARGIN)
}

fn modified(path: &Path) -> Option<i64> {
    let t = std::fs::metadata(path).ok()?.modified().ok()?;
    i64::try_from(t.duration_since(UNIX_EPOCH).ok()?.as_secs()).ok()
}

/// What to purge in a snapshot holding a given schema version.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Plan {
    /// Delete torrents the live store deleted (`torrents`, v1+; child
    /// rows go by `ON DELETE CASCADE`).
    torrents: bool,
    /// Expire quarantine, clear sealed names and old filter hits
    /// (`quarantine`, `tombstones`, `filter_hits`, v4+).
    filter: bool,
    /// Drop logged searches the live store pruned or cleared
    /// (`query_log`, v10+).
    query_log: bool,
}

impl Plan {
    /// The newest schema version this table knows. A new migration must
    /// say here what its schema means for the purge.
    pub(crate) const KNOWN_THROUGH: i64 = 10;

    /// Per schema version (see the PR's table and `docs/decisions`):
    ///
    /// | version | torrents | quarantine, tombstones, filter hits | query log |
    /// |---|---|---|---|
    /// | 1–3 | purged | absent | absent |
    /// | 4–9 | purged | purged | absent |
    /// | 10 | purged | purged | purged |
    /// | other | skipped with a warning | skipped | skipped |
    pub(crate) fn for_version(version: i64) -> Option<Plan> {
        match version {
            // V1 torrents; V2 provenance, Insights, fetch queue; V3
            // reindex log. No filter tables.
            1..=3 => Some(Plan {
                torrents: true,
                filter: false,
                query_log: false,
            }),
            // V4 adds cs_hashes, tombstones, quarantine (hash, rule,
            // sealed, at, expires_at) and filter_hits (at), unchanged
            // since but for V10's indexes. V5–V9 add swarm, subscription,
            // rule and classification tables; the swarm and correction
            // rows cascade from torrents.
            4..=9 => Some(Plan {
                torrents: true,
                filter: true,
                query_log: false,
            }),
            // V10 adds your data (saved searches, alerts, marks,
            // collections), API keys and the query log. Alerts, marks and
            // collection entries cascade from torrents. The query log is
            // pruned oldest first, so what the live store pruned is what's
            // older than its oldest row. Deleted or changed searches,
            // marks, notes and keys leave no trace to match a snapshot
            // against: they go with the snapshot at 90 days.
            10..=Self::KNOWN_THROUGH => Some(Plan {
                torrents: true,
                filter: true,
                query_log: true,
            }),
            _ => None,
        }
    }
}

/// What [`Store::purge_snapshots`] did.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct PurgeReport {
    /// Snapshots deleted for age.
    pub expired: Vec<PathBuf>,
    /// Snapshots that had something purged.
    pub purged: Vec<PathBuf>,
    /// Torrents deleted from snapshots.
    pub torrents: usize,
    /// Quarantine rows ended (expired) or sealed names cleared.
    pub quarantine: usize,
    /// Logged searches deleted.
    pub query_log: usize,
    /// Snapshots skipped or failed, and why; for the log.
    pub warnings: Vec<String>,
}

/// How far each snapshot has been checked against the live store's holds,
/// kept on the writer's [`Store`] for the life of the process. The first
/// pass after a start (or over a new or replaced file) looks up every hold
/// since the snapshot was taken; later ones only those since the last.
#[derive(Debug, Default)]
pub(crate) struct Marks(HashMap<PathBuf, Mark>);

#[derive(Debug, Clone, Copy)]
struct Mark {
    /// The file it was for: another time means another file.
    taken_at: Option<i64>,
    /// The `now` of the last complete pass.
    through: i64,
}

impl Marks {
    /// The oldest hold recorded at or after which `snap` still needs a
    /// lookup.
    fn since(&self, snap: &Snapshot) -> i64 {
        let taken = snap.taken_at.map_or(i64::MIN, held_since);
        match self.0.get(&snap.path) {
            Some(m) if m.taken_at == snap.taken_at => {
                taken.max(m.through.saturating_sub(RECHECK_MARGIN))
            }
            _ => taken,
        }
    }
}

/// What the live store holds, read once per tick.
struct Live {
    /// Quarantined hashes that still have a sealed name.
    sealed: HashSet<[u8; 20]>,
    /// Hashes you confirmed or reported as CS-1: looked up in every pass,
    /// whenever they were recorded. Few, and `add_cs_hashes` keeps an
    /// imported entry's old time when you report it.
    reported: Vec<[u8; 20]>,
    /// The other holds (imported CS-1 entries, tombstones, quarantine)
    /// with when they were recorded.
    held: Vec<([u8; 20], i64)>,
    /// The oldest logged search, `(at, id)`; `None` when the log is
    /// empty (cleared, or never on).
    oldest_query: Option<(i64, i64)>,
}

impl Live {
    /// Holds recorded before `since` (other than `reported`) aren't read.
    fn read(conn: &Connection, since: i64) -> rusqlite::Result<Live> {
        let hash = |h: Vec<u8>| <[u8; 20]>::try_from(h).ok();
        let mut sealed = HashSet::new();
        let mut stmt = conn.prepare("SELECT hash FROM quarantine WHERE sealed IS NOT NULL")?;
        for h in stmt.query_map([], |r| r.get::<_, Vec<u8>>(0))? {
            sealed.extend(hash(h?));
        }
        let mut reported = Vec::new();
        let mut stmt = conn.prepare("SELECT hash FROM cs_hashes WHERE origin != ?1")?;
        for h in stmt.query_map([IMPORTED], |r| r.get::<_, Vec<u8>>(0))? {
            reported.extend(hash(h?));
        }
        let mut held = Vec::new();
        let mut stmt = conn.prepare(
            "SELECT hash, added_at FROM cs_hashes WHERE origin = ?2 AND added_at >= ?1
             UNION ALL SELECT hash, at FROM tombstones WHERE at >= ?1
             UNION ALL SELECT hash, at FROM quarantine WHERE at >= ?1",
        )?;
        let rows = stmt.query_map(params![since, IMPORTED], |r| {
            Ok((r.get::<_, Vec<u8>>(0)?, r.get::<_, i64>(1)?))
        })?;
        for row in rows {
            let (h, at) = row?;
            held.extend(hash(h).map(|h| (h, at)));
        }
        let oldest_query = conn
            .query_row(
                "SELECT at, id FROM query_log ORDER BY at, id LIMIT 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        Ok(Live {
            sealed,
            reported,
            held,
            oldest_query,
        })
    }
}

impl Store {
    /// Deletes snapshots past [`RETAIN_SECS`] and purges the rest the way
    /// [`purge_filter`](Store::purge_filter) and
    /// [`delete_torrents`](Store::delete_torrents) purged the live store
    /// (see the module docs). Run it on the purge tick, with the same
    /// `now` and `hits_before`.
    ///
    /// Best effort: a snapshot that can't be read or purged is left as it
    /// was and named in the report's warnings. A no-op for an in-memory
    /// store or a read-only connection.
    pub fn purge_snapshots(&mut self, now: i64, hits_before: i64) -> PurgeReport {
        let mut report = PurgeReport::default();
        let Some(db) = self.path.clone() else {
            return report;
        };
        let all = match list(&db) {
            Ok(all) => all,
            Err(e) => {
                report
                    .warnings
                    .push(format!("listing the store's snapshots failed: {e}"));
                return report;
            }
        };
        self.snapshot_marks
            .0
            .retain(|path, _| all.iter().any(|s| &s.path == path));
        let mut keep = Vec::new();
        for snap in all {
            if snap.expires_at().is_none_or(|at| now < at) {
                keep.push(snap);
                continue;
            }
            self.snapshot_marks.0.remove(&snap.path);
            let (gone, failed) = remove_snapshot(&snap.path);
            for (path, e) in failed {
                report.warnings.push(format!(
                    "deleting the expired snapshot file {} failed: {e}",
                    path.display()
                ));
            }
            if gone {
                report.expired.push(snap.path);
            }
        }
        let Some(since) = keep.iter().map(|s| self.snapshot_marks.since(s)).min() else {
            return report;
        };
        let live = match Live::read(&self.conn, since) {
            Ok(l) => l,
            Err(e) => {
                report.warnings.push(format!(
                    "reading held hashes for the snapshot purge failed: {e}"
                ));
                return report;
            }
        };
        for snap in keep {
            let since = self.snapshot_marks.since(&snap);
            match purge_one(&snap, &live, since, now, hits_before) {
                Ok(Outcome::Skipped(why)) => report.warnings.push(why),
                Ok(Outcome::Unchanged) => {}
                Ok(Outcome::Purged {
                    torrents,
                    quarantine,
                    query_log,
                }) => {
                    report.torrents += torrents;
                    report.quarantine += quarantine;
                    report.query_log += query_log;
                    report.purged.push(snap.path.clone());
                }
                Err(e) => {
                    report.warnings.push(format!(
                        "purging the snapshot {} failed (it was left as it was): {e}",
                        snap.path.display()
                    ));
                    continue;
                }
            }
            self.snapshot_marks.0.insert(
                snap.path,
                Mark {
                    taken_at: snap.taken_at,
                    through: now,
                },
            );
        }
        report
    }
}

enum Outcome {
    Skipped(String),
    Unchanged,
    Purged {
        torrents: usize,
        quarantine: usize,
        query_log: usize,
    },
}

/// Deletes a snapshot and any side file SQLite left beside it: whether
/// the snapshot itself is gone, and the files that couldn't be deleted. A
/// plain unlink: overwriting first doesn't reach the old blocks on
/// copy-on-write filesystems or SSDs, so it would only cost a write of the
/// whole file.
fn remove_snapshot(path: &Path) -> (bool, Vec<(PathBuf, std::io::Error)>) {
    let mut failed = Vec::new();
    let mut remove = |p: PathBuf| match std::fs::remove_file(&p) {
        Ok(()) => true,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => true,
        Err(e) => {
            failed.push((p, e));
            false
        }
    };
    let gone = remove(path.to_path_buf());
    for suffix in ["-wal", "-shm", "-journal"] {
        let mut side = path.as_os_str().to_os_string();
        side.push(suffix);
        remove(PathBuf::from(side));
    }
    (gone, failed)
}

/// Opens a snapshot for writing; never creates one, so a snapshot that
/// vanished meanwhile is an error, not a new empty file.
fn open_rw(path: &Path) -> rusqlite::Result<Connection> {
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    conn.busy_timeout(SNAPSHOT_BUSY)?;
    Ok(conn)
}

/// Folds a WAL left beside a snapshot (a purge interrupted between its
/// commit and checkpoint) into the file and goes back to the rollback
/// journal, keeping the file's modification time. `lodestar rollback`
/// calls it before checking and copying a snapshot, so the copy holds the
/// purge. Whether there was a WAL to fold.
pub fn settle(path: &Path) -> rusqlite::Result<bool> {
    let taken = modified(path);
    let conn = open_rw(path)?;
    let folded = settle_conn(&conn)?;
    drop(conn);
    if let Some(taken) = taken.filter(|_| folded) {
        let _ = set_modified(path, taken);
    }
    Ok(folded)
}

fn settle_conn(conn: &Connection) -> rusqlite::Result<bool> {
    let mode: String = conn.query_row("PRAGMA journal_mode", [], |r| r.get(0))?;
    if !mode.eq_ignore_ascii_case("wal") {
        return Ok(false);
    }
    conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()))?;
    conn.pragma_update(None, "journal_mode", "DELETE")?;
    Ok(true)
}

fn purge_one(
    snap: &Snapshot,
    live: &Live,
    since: i64,
    now: i64,
    hits_before: i64,
) -> crate::Result<Outcome> {
    let mut conn = open_rw(&snap.path)?;
    // A WAL left by an interrupted purge goes first, so it's either
    // folded in or nothing is written.
    let folded = settle_conn(&conn)?;
    let restore_time = |conn: Connection| {
        drop(conn);
        if let Some(taken) = snap.taken_at {
            // The expiry goes by the modification time: keep it the time
            // the snapshot was taken.
            let _ = set_modified(&snap.path, taken);
        }
    };
    let has_migrations: bool = conn
        .query_row(
            "SELECT 1 FROM sqlite_schema WHERE type = 'table' AND name = 'schema_migrations'",
            [],
            |_| Ok(true),
        )
        .optional()?
        .unwrap_or(false);
    let version: Option<i64> = if has_migrations {
        conn.query_row("SELECT max(version) FROM schema_migrations", [], |r| {
            r.get(0)
        })?
    } else {
        None
    };
    let Some(plan) = version.and_then(Plan::for_version) else {
        if folded {
            restore_time(conn);
        }
        let held = version.map_or_else(|| "no schema version".into(), |v| format!("schema v{v}"));
        return Ok(Outcome::Skipped(format!(
            "the snapshot {} holds {held}, which this version doesn't know how to purge; it's \
             left as it is and deleted when it's {RETAIN_DAYS} days old",
            snap.path.display()
        )));
    };

    // Look first, so a snapshot with nothing to purge isn't written (nor
    // its modification time touched).
    let mut ids = BTreeSet::new();
    if plan.torrents {
        let recent = live.held.iter().filter(|(_, at)| *at >= since);
        for h in live.reported.iter().chain(recent.map(|(h, _)| h)) {
            ids.extend(crate::find_any(&conn, std::iter::once(InfoHash(*h)))?);
        }
    }
    let mut unsealed = Vec::new();
    let (mut expiring, mut hits) = (0i64, 0i64);
    if plan.filter {
        let mut stmt = conn.prepare("SELECT hash FROM quarantine WHERE sealed IS NOT NULL")?;
        for h in stmt.query_map([], |r| r.get::<_, Vec<u8>>(0))? {
            let h = h?;
            if !<[u8; 20]>::try_from(h.as_slice()).is_ok_and(|h| live.sealed.contains(&h)) {
                unsealed.push(h);
            }
        }
        drop(stmt);
        expiring = conn.query_row(
            "SELECT count(*) FROM quarantine WHERE expires_at <= ?1",
            [now],
            |r| r.get(0),
        )?;
        hits = conn.query_row(
            "SELECT count(*) FROM filter_hits WHERE at < ?1",
            [hits_before],
            |r| r.get(0),
        )?;
    }
    // Rows before the live log's oldest (all of them if it's empty).
    let (log_at, log_id) = live.oldest_query.unwrap_or((i64::MAX, i64::MAX));
    let logged = if plan.query_log {
        conn.query_row(
            "SELECT count(*) FROM query_log WHERE (at, id) < (?1, ?2)",
            [log_at, log_id],
            |r| r.get::<_, i64>(0),
        )?
    } else {
        0
    };
    if ids.is_empty() && unsealed.is_empty() && expiring == 0 && hits == 0 && logged == 0 {
        if folded {
            restore_time(conn);
        }
        return Ok(Outcome::Unchanged);
    }

    // The WAL holds only the new pages, where the rollback journal would
    // hold copies of the old ones, sealed names included.
    conn.pragma_update(None, "secure_delete", true)?;
    conn.pragma_update(None, "foreign_keys", true)?;
    let mode: String = conn.pragma_update_and_check(None, "journal_mode", "WAL", |r| r.get(0))?;
    let wal = mode.eq_ignore_ascii_case("wal");
    let result = (|| -> rusqlite::Result<(usize, usize, usize)> {
        let tx = crate::begin_write(&mut conn)?;
        let mut torrents = 0;
        for id in &ids {
            torrents += tx.execute("DELETE FROM torrents WHERE id = ?1", [id])?;
        }
        let mut quarantine = 0;
        if plan.filter {
            for h in &unsealed {
                quarantine += tx.execute(
                    "UPDATE quarantine SET sealed = NULL WHERE hash = ?1 AND expires_at > ?2",
                    params![h, now],
                )?;
            }
            // As `Store::purge_filter`: an expired adult hold becomes a
            // tombstone, replacing any but the floor's.
            let floor = crate::filter::FLOOR_RULES;
            tx.execute(
                "INSERT INTO tombstones (hash, rule, at)
                 SELECT hash, rule, at FROM quarantine
                 WHERE expires_at <= ?1 AND substr(rule, -length(?2)) = ?2
                 ON CONFLICT (hash) DO UPDATE SET rule = excluded.rule, at = excluded.at
                 WHERE tombstones.rule NOT IN (?3, ?4, ?5)",
                params![now, ADULT_HOLD, floor[0], floor[1], floor[2]],
            )?;
            quarantine += tx.execute("DELETE FROM quarantine WHERE expires_at <= ?1", [now])?;
            tx.execute("DELETE FROM filter_hits WHERE at < ?1", [hits_before])?;
        }
        let mut query_log = 0;
        if plan.query_log {
            query_log = tx.execute(
                "DELETE FROM query_log WHERE (at, id) < (?1, ?2)",
                [log_at, log_id],
            )?;
        }
        tx.commit()?;
        Ok((torrents, quarantine, query_log))
    })();
    if wal {
        // Copies the new pages into the file and empties the WAL, then
        // goes back to a single file, as `VACUUM INTO` wrote it.
        let _ = conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()));
        let _ = conn.pragma_update(None, "journal_mode", "DELETE");
    }
    restore_time(conn);
    let (torrents, quarantine, query_log) = result?;
    Ok(Outcome::Purged {
        torrents,
        quarantine,
        query_log,
    })
}

fn set_modified(path: &Path, secs: i64) -> std::io::Result<()> {
    let at = UNIX_EPOCH + Duration::from_secs(u64::try_from(secs).unwrap_or(0));
    std::fs::OpenOptions::new()
        .write(true)
        .open(path)?
        .set_modified(at)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::MIGRATIONS;
    use crate::{Store, StoreConfig};

    const DAY: i64 = 86_400;
    /// The tests' clock: snapshots are taken at `T`.
    const T: i64 = 1_800_000_000;

    fn contains(path: &Path, marker: &[u8]) -> bool {
        let hay = std::fs::read(path).unwrap();
        hay.windows(marker.len()).any(|w| w == marker)
    }

    fn quick_check(path: &Path) {
        let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
        let r: String = conn
            .query_row("PRAGMA quick_check", [], |r| r.get(0))
            .unwrap();
        assert_eq!(r, "ok", "{}", path.display());
    }

    fn side(path: &Path, suffix: &str) -> PathBuf {
        let mut s = path.as_os_str().to_os_string();
        s.push(suffix);
        PathBuf::from(s)
    }

    fn insert_torrent(conn: &Connection, id: i64, hash: [u8; 20], name: &str) {
        conn.execute(
            "INSERT INTO torrents (id, info_hash, name, size, piece_len, piece_count,
                 file_count, info_size, flags, discovered_at, metadata_at, last_seen_at)
             VALUES (?1, ?2, ?3, 0, 0, 0, 0, 0, 0, 0, 0, 0)",
            params![id, &hash[..], name],
        )
        .unwrap();
        // A child row, which must go with it (ON DELETE CASCADE).
        conn.execute(
            "INSERT INTO torrent_files (torrent_id, codec, data) VALUES (?1, 0, ?2)",
            params![id, format!("files of {name}").into_bytes()],
        )
        .unwrap();
    }

    fn quarantine(conn: &Connection, hash: [u8; 20], rule: &str, sealed: &[u8], expires_at: i64) {
        conn.execute(
            "INSERT INTO quarantine (hash, rule, sealed, at, expires_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![&hash[..], rule, sealed, expires_at - 30 * DAY, expires_at],
        )
        .unwrap();
    }

    /// A snapshot of the live store, as `schema` takes it, dated `at`.
    fn take(store: &Store, db: &Path, version: i64, at: i64) -> PathBuf {
        let path = crate::schema::snapshot_path(db, version);
        store
            .connection()
            .execute("VACUUM INTO ?1", [path.to_string_lossy()])
            .unwrap();
        set_modified(&path, at).unwrap();
        path
    }

    /// A database at schema `version`, built from this build's own
    /// migrations: what a snapshot taken before migration `version + 1`
    /// holds.
    fn fixture(path: &Path, version: i64) -> Connection {
        let conn = Connection::open(path).unwrap();
        conn.execute_batch(
            "CREATE TABLE schema_migrations (
                 version    INTEGER PRIMARY KEY,
                 applied_at INTEGER NOT NULL
             ) STRICT;",
        )
        .unwrap();
        for (v, sql) in MIGRATIONS.iter().filter(|(v, _)| *v <= version) {
            conn.execute_batch(sql).unwrap();
            conn.execute(
                "INSERT INTO schema_migrations (version, applied_at) VALUES (?1, 0)",
                [v],
            )
            .unwrap();
        }
        conn
    }

    fn live(dir: &Path) -> (Store, PathBuf) {
        let db = dir.join("lodestar.db");
        (Store::open(&db, StoreConfig::default()).unwrap(), db)
    }

    #[test]
    fn names_match_the_ones_schema_gives() {
        let pre = |version, repeat| Some(Kind::PreMigration { version, repeat });
        let db = Path::new("/data/lodestar.db");
        let first = crate::schema::snapshot_path(db, 7);
        assert_eq!(
            parse_name("lodestar", first.file_name().unwrap().to_str().unwrap()),
            pre(7, 0)
        );
        assert_eq!(parse_name("lodestar", "lodestar.pre-v4.2.db"), pre(4, 2));
        assert_eq!(
            parse_name("lodestar", "lodestar.before-rollback-20261022-000000.db"),
            Some(Kind::BeforeRollback)
        );
        for bad in [
            "lodestar.pre-v.db",
            "lodestar.pre-v4..db",
            "lodestar.pre-v4.x.db",
            "lodestar.pre-v4.db-wal",
            "lodestar.pre-v4.db-journal",
            "other.pre-v4.db",
            "lodestar.pre-v-1.db",
            "lodestar.pre-v99999999999.db",
            "lodestar.before-rollback-2026102-000000.db",
            "lodestar.before-rollback-20261022-00000x.db",
            "lodestar.before-rollback-20261022-000000.db-wal",
            "lodestar.before-rollback-.db",
            "lodestar.rollback-20261022-000000.tmp",
            "lodestar.db",
        ] {
            assert_eq!(parse_name("lodestar", bad), None, "{bad}");
        }
    }

    /// A new migration must decide what its schema means for the purge.
    #[test]
    fn every_known_schema_has_a_plan() {
        let latest = MIGRATIONS.last().unwrap().0;
        assert_eq!(
            Plan::KNOWN_THROUGH,
            latest,
            "a migration was added: say in Plan::for_version how to purge its schema"
        );
        for v in 1..=latest {
            let plan = Plan::for_version(v).unwrap();
            assert!(plan.torrents);
            assert_eq!(plan.filter, v >= 4, "v{v}");
            assert_eq!(plan.query_log, v >= 10, "v{v}");
        }
        assert_eq!(Plan::for_version(0), None);
        assert_eq!(Plan::for_version(latest + 1), None);
    }

    /// The brief's first test: a sealed name in a snapshot taken before
    /// its quarantine expired is gone from the snapshot's bytes once the
    /// purge runs. So is a torrent deleted since, and a sealed name the
    /// live store released.
    #[test]
    fn a_snapshot_loses_what_the_live_store_purged() {
        let dir = tempfile::tempdir().unwrap();
        let (mut store, db) = live(dir.path());
        let expiring = b"sealed-snapshot-marker-5e1a";
        let released = b"released-snapshot-marker-77c0";
        let deleted = "deleted-torrent-snapshot-marker-90b3";
        let kept = "kept-torrent-snapshot-marker-1f2d";
        quarantine(store.connection(), [1; 20], "CS-3", expiring, T + 10 * DAY);
        quarantine(store.connection(), [2; 20], "CS-3", released, T + 20 * DAY);
        insert_torrent(store.connection(), 1, [3; 20], deleted);
        insert_torrent(store.connection(), 2, [4; 20], kept);
        let snap = take(&store, &db, 10, T);
        assert!(contains(&snap, expiring) && contains(&snap, deleted.as_bytes()));

        // After the snapshot: one torrent reported and deleted, one hold
        // released.
        store.add_cs_hashes(&[[3; 20]], 1, T + DAY).unwrap();
        store.delete_torrents(&[[3; 20]]).unwrap();
        store.release(&[2; 20]).unwrap();
        // Before expiry: the deleted torrent and the released name go.
        let r = store.purge_snapshots(T + 2 * DAY, 0);
        assert_eq!(r.warnings, Vec::<String>::new());
        assert_eq!((r.torrents, r.quarantine), (1, 1));
        assert_eq!(r.purged, vec![snap.clone()]);
        assert!(!contains(&snap, deleted.as_bytes()));
        assert!(!contains(&snap, released));
        assert!(contains(&snap, expiring), "not expired yet");
        assert!(contains(&snap, kept.as_bytes()));

        // Past expiry, on the same tick as the live purge.
        let now = T + 11 * DAY;
        store.purge_filter(now, 0).unwrap();
        let r = store.purge_snapshots(now, 0);
        assert_eq!(r.warnings, Vec::<String>::new());
        assert_eq!(r.quarantine, 1);
        assert!(!contains(&snap, expiring));
        assert!(contains(&snap, kept.as_bytes()));
        for suffix in ["-wal", "-shm", "-journal"] {
            assert!(!side(&snap, suffix).exists(), "{suffix} left behind");
        }
        quick_check(&snap);
        let conn = Connection::open(&snap).unwrap();
        let mode: String = conn
            .query_row("PRAGMA journal_mode", [], |r| r.get(0))
            .unwrap();
        assert_eq!(
            mode, "delete",
            "a single file again, as VACUUM INTO wrote it"
        );
        let files: i64 = conn
            .query_row("SELECT count(*) FROM torrent_files", [], |r| r.get(0))
            .unwrap();
        assert_eq!(files, 1, "child rows cascade");
        drop(conn);
        assert_eq!(modified(&snap), Some(T), "the time it was taken is kept");

        // Nothing left to do: the file isn't written.
        let before = std::fs::read(&snap).unwrap();
        let r = store.purge_snapshots(now + 1, 0);
        assert_eq!(r, PurgeReport::default());
        assert_eq!(std::fs::read(&snap).unwrap(), before);
    }

    /// The snapshot purge does what `purge_filter` does to the same rows:
    /// keeps the duplicated SQL (and `ADULT_HOLD`) in step.
    #[test]
    fn the_quarantine_purge_matches_the_live_one() {
        let dir = tempfile::tempdir().unwrap();
        let (mut store, db) = live(dir.path());
        let c = store.connection();
        quarantine(c, [1; 20], "CS-3", b"a", T + DAY);
        quarantine(c, [2; 20], "CS-3+adult", b"b", T + DAY);
        quarantine(c, [3; 20], "CS-4+adult", b"c", T + DAY);
        quarantine(c, [4; 20], "CS-3", b"d", T + 5 * DAY);
        quarantine(c, [5; 20], "CS-4+adult", b"e", T + 5 * DAY);
        for (h, rule) in [([3u8; 20], "CS-2"), ([2; 20], "CS-1a")] {
            c.execute(
                "INSERT INTO tombstones (hash, rule, at) VALUES (?1, ?2, 5)",
                params![&h[..], rule],
            )
            .unwrap();
        }
        for at in [T - DAY, T + DAY] {
            c.execute(
                "INSERT INTO filter_hits (hash, rule, action, at, evidence)
                 VALUES (x'00', 'CS-3', 0, ?1, x'')",
                [at],
            )
            .unwrap();
        }
        let snap = take(&store, &db, 10, T);
        let now = T + 2 * DAY;
        store.purge_filter(now, T).unwrap();
        let r = store.purge_snapshots(now, T);
        assert_eq!(r.warnings, Vec::<String>::new());

        let dump = |conn: &Connection| -> Vec<String> {
            let mut out = Vec::new();
            for sql in [
                "SELECT hex(hash) || rule || coalesce(hex(sealed), '-') || at || ':' || expires_at
                 FROM quarantine ORDER BY hash",
                "SELECT hex(hash) || rule || at FROM tombstones ORDER BY hash",
                "SELECT at FROM filter_hits ORDER BY id",
            ] {
                let mut stmt = conn.prepare(sql).unwrap();
                let rows = stmt
                    .query_map([], |r| r.get::<_, rusqlite::types::Value>(0))
                    .unwrap();
                out.extend(rows.map(|v| format!("{:?}", v.unwrap())));
                out.push("|".into());
            }
            out
        };
        let snap_conn = Connection::open(&snap).unwrap();
        assert_eq!(dump(&snap_conn), dump(store.connection()));
    }

    /// Snapshots of older schemas purge what their schema has; unknown
    /// ones and files that aren't databases are left byte for byte.
    #[test]
    fn old_schemas_purge_and_unknown_ones_are_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let (mut store, db) = live(dir.path());
        let gone = "old-schema-torrent-marker-c4e8";
        let sealed = b"old-schema-sealed-marker-0d9f";
        // v3: torrents only, no filter tables.
        let v3 = crate::schema::snapshot_path(&db, 4);
        let conn = fixture(&v3, 3);
        insert_torrent(&conn, 1, [3; 20], gone);
        insert_torrent(&conn, 2, [4; 20], "stays");
        drop(conn);
        // v6: quarantine as well.
        let v6 = crate::schema::snapshot_path(&db, 7);
        let conn = fixture(&v6, 6);
        insert_torrent(&conn, 1, [3; 20], gone);
        quarantine(&conn, [9; 20], "CS-3", sealed, T + DAY);
        drop(conn);
        // A newer schema than this build knows, and a file that isn't a
        // database.
        let v99 = crate::schema::snapshot_path(&db, 100);
        let conn = fixture(&v99, 6);
        conn.execute(
            "INSERT INTO schema_migrations (version, applied_at) VALUES (99, 0)",
            [],
        )
        .unwrap();
        insert_torrent(&conn, 1, [3; 20], gone);
        drop(conn);
        let junk = crate::schema::snapshot_path(&db, 8);
        std::fs::write(&junk, b"not a database at all").unwrap();
        for p in [&v3, &v6, &v99, &junk] {
            set_modified(p, T).unwrap();
        }
        let untouched = [&v99, &junk].map(|p| std::fs::read(p).unwrap());

        store.add_cs_hashes(&[[3; 20]], 1, T + DAY).unwrap();
        let r = store.purge_snapshots(T + 2 * DAY, 0);
        assert_eq!(r.purged, vec![v3.clone(), v6.clone()]);
        assert_eq!((r.torrents, r.quarantine), (2, 1));
        assert_eq!(r.warnings.len(), 2, "{:?}", r.warnings);
        // In name order: pre-v8 (the junk), then pre-v100.
        assert!(r.warnings[0].contains("left as it was"), "{:?}", r.warnings);
        assert!(r.warnings[1].contains("schema v99"), "{:?}", r.warnings);
        for p in [&v3, &v6] {
            assert!(!contains(p, gone.as_bytes()), "{}", p.display());
            assert!(!contains(p, b"files of old-schema"), "{}", p.display());
            quick_check(p);
        }
        assert!(contains(&v3, b"stays"));
        assert!(!contains(&v6, sealed));
        assert_eq!([&v99, &junk].map(|p| std::fs::read(p).unwrap()), untouched);
        quick_check(&v99);
    }

    /// Deterministic clock: a snapshot goes when it's [`RETAIN_DAYS`] old,
    /// not a second before, with any side file SQLite left.
    #[test]
    fn expiry_deletes_only_snapshots_past_the_window() {
        let dir = tempfile::tempdir().unwrap();
        let (mut store, db) = live(dir.path());
        let old = take(&store, &db, 10, T);
        let new = take(&store, &db, 10, T + DAY);
        std::fs::write(side(&old, "-journal"), b"").unwrap();
        assert_eq!(new.file_name().unwrap(), "lodestar.pre-v10.1.db");
        let listed = list(&db).unwrap();
        assert_eq!(listed[0].expires_at(), Some(T + RETAIN_SECS));

        let r = store.purge_snapshots(T + RETAIN_SECS - 1, 0);
        assert!(r.expired.is_empty() && old.exists());
        let r = store.purge_snapshots(T + RETAIN_SECS, 0);
        assert_eq!(r.expired, vec![old.clone()]);
        assert!(!old.exists() && !side(&old, "-journal").exists());
        assert!(new.exists());
        assert_eq!(list(&db).unwrap().len(), 1);
        assert!(db.exists(), "the store itself is never a snapshot");
    }

    /// Nothing to do without a file-backed writer.
    #[test]
    fn in_memory_and_read_only_stores_do_nothing() {
        let mut mem = Store::open_in_memory(StoreConfig::default()).unwrap();
        assert_eq!(mem.purge_snapshots(T, 0), PurgeReport::default());
        let dir = tempfile::tempdir().unwrap();
        let (store, db) = live(dir.path());
        let snap = take(&store, &db, 10, T);
        let mut reader = Store::open_reader(&db, StoreConfig::default()).unwrap();
        assert_eq!(
            reader.purge_snapshots(T + RETAIN_SECS, 0),
            PurgeReport::default()
        );
        assert!(snap.exists());
    }

    /// The review's case: a hash an imported list already had (its
    /// `added_at` predates the snapshot) is reported afterwards, and the
    /// live store deletes its torrent. The report keeps the old time, so a
    /// time window alone would miss it.
    #[test]
    fn a_reported_hash_from_an_older_import_is_purged() {
        let dir = tempfile::tempdir().unwrap();
        let (mut store, db) = live(dir.path());
        let marker = "reported-after-import-marker-a61c";
        insert_torrent(store.connection(), 1, [7; 20], marker);
        store.add_cs_hashes(&[[7; 20]], 2, T - 30 * DAY).unwrap();
        let snap = take(&store, &db, 10, T);
        // Purged once with nothing to do, so the next pass is incremental.
        assert_eq!(store.purge_snapshots(T + DAY, 0), PurgeReport::default());

        store.add_cs_hashes(&[[7; 20]], 1, T + 2 * DAY).unwrap();
        store.delete_torrents(&[[7; 20]]).unwrap();
        let r = store.purge_snapshots(T + 3 * DAY, 0);
        assert_eq!(r.torrents, 1, "{r:?}");
        assert!(!contains(&snap, marker.as_bytes()));
    }

    /// After a full pass, only holds recorded since the last pass (less a
    /// margin) are looked up; a replaced file gets a full pass again.
    #[test]
    fn later_passes_look_up_only_new_holds() {
        let dir = tempfile::tempdir().unwrap();
        let (mut store, db) = live(dir.path());
        for id in 1..=3u8 {
            insert_torrent(store.connection(), i64::from(id), [id; 20], "t");
        }
        let tomb = |store: &Store, h: u8, at: i64| {
            store
                .connection()
                .execute(
                    "INSERT INTO tombstones (hash, rule, at) VALUES (?1, 'FK-4', ?2)",
                    params![&[h; 20][..], at],
                )
                .unwrap();
        };
        let snap = take(&store, &db, 10, T);
        let now = T + DAY;
        assert_eq!(store.purge_snapshots(now, 0), PurgeReport::default());
        // Recorded inside the last pass's margin: still looked up.
        tomb(&store, 1, now - RECHECK_MARGIN);
        // Before the margin: a full pass would have found it already, so
        // it isn't looked up again (here it wasn't, by construction).
        tomb(&store, 2, now - RECHECK_MARGIN - 1);
        let r = store.purge_snapshots(now + 3600, 0);
        assert_eq!(r.torrents, 1, "{r:?}");
        // The same name with another time is another file: full pass.
        set_modified(&snap, T + 1).unwrap();
        let r = store.purge_snapshots(now + 7200, 0);
        assert_eq!(r.torrents, 1, "{r:?}");
        let left: i64 = Connection::open(&snap)
            .unwrap()
            .query_row("SELECT count(*) FROM torrents", [], |r| r.get(0))
            .unwrap();
        assert_eq!(left, 1);
    }

    /// The copy of the store a rollback keeps gets the same purge and
    /// expiry as a pre-migration snapshot.
    #[test]
    fn copies_kept_by_a_rollback_are_purged_and_expire() {
        let dir = tempfile::tempdir().unwrap();
        let (mut store, db) = live(dir.path());
        let sealed = b"before-rollback-sealed-marker-3b8e";
        quarantine(store.connection(), [1; 20], "CS-3", sealed, T + DAY);
        let kept = dir
            .path()
            .join("lodestar.before-rollback-20270115-080000.db");
        store
            .connection()
            .execute("VACUUM INTO ?1", [kept.to_string_lossy()])
            .unwrap();
        set_modified(&kept, T).unwrap();
        let pre = take(&store, &db, 10, T);
        let listed = list(&db).unwrap();
        assert_eq!(
            listed.iter().map(|s| s.kind).collect::<Vec<_>>(),
            [
                Kind::PreMigration {
                    version: 10,
                    repeat: 0
                },
                Kind::BeforeRollback
            ]
        );

        let now = T + 2 * DAY;
        store.purge_filter(now, 0).unwrap();
        let r = store.purge_snapshots(now, 0);
        assert_eq!(r.purged, vec![pre.clone(), kept.clone()]);
        assert!(!contains(&kept, sealed));
        quick_check(&kept);
        let r = store.purge_snapshots(T + RETAIN_SECS, 0);
        assert_eq!(r.expired, vec![pre, kept.clone()]);
        assert!(!kept.exists());
    }

    /// A purge interrupted between its commit and checkpoint leaves a WAL
    /// beside the snapshot; [`settle`] (which rollback runs before copying)
    /// folds it in, so the copy holds the purge.
    #[test]
    fn settle_folds_a_left_over_wal() {
        let dir = tempfile::tempdir().unwrap();
        let (store, db) = live(dir.path());
        let open = take(&store, &db, 10, T);
        assert!(!settle(&open).unwrap(), "nothing to fold");
        // Copy the file and its WAL while a connection has them open: what
        // a process that died mid-purge leaves.
        let conn = Connection::open(&open).unwrap();
        conn.pragma_update(None, "journal_mode", "WAL").unwrap();
        conn.execute("INSERT INTO settings (key, value) VALUES ('k', 1)", [])
            .unwrap();
        let snap = crate::schema::snapshot_path(&db, 11);
        std::fs::copy(&open, &snap).unwrap();
        std::fs::copy(side(&open, "-wal"), side(&snap, "-wal")).unwrap();
        drop(conn);
        set_modified(&snap, T).unwrap();

        assert!(settle(&snap).unwrap());
        assert!(!side(&snap, "-wal").exists());
        assert_eq!(modified(&snap), Some(T));
        let conn = Connection::open_with_flags(&snap, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
        let mode: String = conn
            .query_row("PRAGMA journal_mode", [], |r| r.get(0))
            .unwrap();
        assert_eq!(mode, "delete");
        let v: i64 = conn
            .query_row("SELECT value FROM settings WHERE key = 'k'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(v, 1);
    }

    /// A side file that can't be deleted is reported on its own; the
    /// snapshot itself still counts as deleted.
    #[test]
    fn a_stuck_side_file_does_not_hide_the_deletion() {
        let dir = tempfile::tempdir().unwrap();
        let (mut store, db) = live(dir.path());
        let snap = take(&store, &db, 10, T);
        std::fs::create_dir(side(&snap, "-shm")).unwrap();
        let r = store.purge_snapshots(T + RETAIN_SECS, 0);
        assert_eq!(r.expired, vec![snap.clone()]);
        assert_eq!(r.warnings.len(), 1, "{:?}", r.warnings);
        assert!(r.warnings[0].contains("-shm"), "{:?}", r.warnings);
        assert!(!snap.exists());
    }

    /// The live store prunes its query log oldest first, or clears it;
    /// the snapshot loses the same searches.
    #[test]
    fn logged_searches_follow_the_live_prune_and_clear() {
        let dir = tempfile::tempdir().unwrap();
        let (mut store, db) = live(dir.path());
        let log = |store: &Store, at: i64, q: &str| {
            store
                .connection()
                .execute(
                    "INSERT INTO query_log (at, query, results, latency_us) VALUES (?1, ?2, 0, 1)",
                    params![at, q],
                )
                .unwrap();
        };
        log(&store, T - 2 * DAY, "logged-search-old-marker-61d2");
        log(&store, T - DAY, "logged-search-kept-marker-0a7e");
        let snap = take(&store, &db, 11, T);
        store.prune_query_log(T - DAY, 1000).unwrap();
        let r = store.purge_snapshots(T + DAY, 0);
        assert_eq!(r.query_log, 1, "{r:?}");
        assert!(!contains(&snap, b"logged-search-old-marker-61d2"));
        assert!(contains(&snap, b"logged-search-kept-marker-0a7e"));

        store.clear_query_log().unwrap();
        // A search logged after the clear doesn't keep the old ones.
        log(&store, T + 2 * DAY, "new");
        let r = store.purge_snapshots(T + 3 * DAY, 0);
        assert_eq!(r.query_log, 1, "{r:?}");
        assert!(!contains(&snap, b"logged-search-kept-marker-0a7e"));
        quick_check(&snap);
    }

    /// The hourly cost with many holds (the review's measure): 200k
    /// tombstones recorded after the snapshot of a 20k-torrent store.
    /// `cargo test -p lodestar-store --lib purge_cost -- --ignored --nocapture`
    #[test]
    #[ignore = "timing, run by hand"]
    fn purge_cost() {
        let dir = tempfile::tempdir().unwrap();
        let (mut store, db) = live(dir.path());
        {
            let conn = store.connection();
            conn.execute_batch("BEGIN").unwrap();
            for id in 1..=20_000i64 {
                let mut h = [0u8; 20];
                h[..8].copy_from_slice(&id.to_be_bytes());
                insert_torrent(conn, id, h, "t");
            }
            conn.execute_batch("COMMIT").unwrap();
        }
        take(&store, &db, 10, T);
        {
            let conn = store.connection();
            conn.execute_batch("BEGIN").unwrap();
            for i in 0..200_000u64 {
                let mut h = [0xffu8; 20];
                h[..8].copy_from_slice(&i.to_be_bytes());
                conn.execute(
                    "INSERT INTO tombstones (hash, rule, at) VALUES (?1, 'FK-4', ?2)",
                    params![&h[..], T + 600],
                )
                .unwrap();
            }
            conn.execute_batch("COMMIT").unwrap();
        }
        for (i, now) in [T + 2 * 3600, T + 3 * 3600, T + 4 * 3600]
            .into_iter()
            .enumerate()
        {
            let start = std::time::Instant::now();
            let r = store.purge_snapshots(now, 0);
            assert!(r.warnings.is_empty(), "{:?}", r.warnings);
            eprintln!("tick {}: {:?}", i + 1, start.elapsed());
        }
    }
}
