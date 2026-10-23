//! Migration snapshots, `<db>.pre-v<N>[.<k>].db` (architecture §7, §12):
//! the copies [`schema`](crate::schema) takes with `VACUUM INTO` before
//! each migration, for `lodestar rollback`.
//!
//! A snapshot is a full copy of the store, so it would keep what the live
//! store later purges: sealed quarantine names, deleted torrents (owner
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

use std::collections::{BTreeSet, HashSet};
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

/// How long opening a snapshot waits for a lock. Nothing else should hold
/// one (see the module docs); this only keeps a stray `sqlite3` shell from
/// stalling the writer.
const SNAPSHOT_BUSY: Duration = Duration::from_millis(200);

/// Mirrors `filter::ADULT_HOLD`; the purge-equivalence test keeps them in
/// step.
const ADULT_HOLD: &str = "+adult";

/// One snapshot file of the store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    pub path: PathBuf,
    pub name: String,
    /// Taken before migrating to this version, so it holds `version - 1`.
    pub version: i64,
    /// The `k` of a repeat snapshot of the same migration (0 for the first).
    pub repeat: u32,
    /// When it was taken (Unix seconds): its modification time.
    pub taken_at: Option<i64>,
}

impl Snapshot {
    /// When it's deleted (Unix seconds), if its time is known.
    pub fn expires_at(&self) -> Option<i64> {
        self.taken_at.map(expires_at)
    }
}

/// When a snapshot taken at `taken_at` is deleted.
pub fn expires_at(taken_at: i64) -> i64 {
    taken_at.saturating_add(RETAIN_SECS)
}

/// The snapshots of the store at `db`, oldest migration first. Only
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
        let Some((version, repeat)) = parse_name(stem, &name) else {
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
            version,
            repeat,
        });
    }
    out.sort_by_key(|s| (s.version, s.repeat));
    Ok(out)
}

/// `(N, k)` from `<stem>.pre-v<N>.db` or `<stem>.pre-v<N>.<k>.db`, the
/// names `schema::snapshot_path` gives.
fn parse_name(stem: &str, name: &str) -> Option<(i64, u32)> {
    let rest = name
        .strip_prefix(stem)?
        .strip_prefix(".pre-v")?
        .strip_suffix(".db")?;
    let (n, k) = match rest.split_once('.') {
        Some((n, k)) => (n, Some(k)),
        None => (rest, None),
    };
    let digits = |s: &str| !s.is_empty() && s.len() <= 9 && s.bytes().all(|b| b.is_ascii_digit());
    if !digits(n) || !k.is_none_or(digits) {
        return None;
    }
    Some((n.parse().ok()?, k.map_or(Some(0), |k| k.parse().ok())?))
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
}

impl Plan {
    /// The newest schema version this table knows. A new migration must
    /// say here what its schema means for the purge.
    pub(crate) const KNOWN_THROUGH: i64 = 9;

    /// Per schema version (see the PR's table and `docs/decisions`):
    ///
    /// | version | torrents | quarantine, tombstones, filter hits |
    /// |---|---|---|
    /// | 1–3 | purged | absent |
    /// | 4–9 | purged | purged |
    /// | other | skipped with a warning | skipped |
    pub(crate) fn for_version(version: i64) -> Option<Plan> {
        match version {
            // V1 torrents; V2 provenance, Insights, fetch queue; V3
            // reindex log. No filter tables.
            1..=3 => Some(Plan {
                torrents: true,
                filter: false,
            }),
            // V4 adds cs_hashes, tombstones, quarantine (hash, rule,
            // sealed, at, expires_at) and filter_hits (at), unchanged
            // since. V5–V9 add swarm, subscription, rule and
            // classification tables; the swarm and correction rows cascade
            // from torrents.
            4..=Self::KNOWN_THROUGH => Some(Plan {
                torrents: true,
                filter: true,
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
    /// Snapshots skipped or failed, and why; for the log.
    pub warnings: Vec<String>,
}

/// What the live store holds, read once per tick.
struct Live {
    /// Quarantined hashes that still have a sealed name.
    sealed: HashSet<[u8; 20]>,
    /// Every held hash with when it was recorded: CS-1 entries,
    /// tombstones and quarantine.
    held: Vec<([u8; 20], i64)>,
}

impl Live {
    /// Held hashes recorded before `since` aren't read.
    fn read(conn: &Connection, since: i64) -> rusqlite::Result<Live> {
        let mut sealed = HashSet::new();
        let mut stmt = conn.prepare("SELECT hash FROM quarantine WHERE sealed IS NOT NULL")?;
        for h in stmt.query_map([], |r| r.get::<_, Vec<u8>>(0))? {
            if let Ok(h) = <[u8; 20]>::try_from(h?) {
                sealed.insert(h);
            }
        }
        let mut held = Vec::new();
        let mut stmt = conn.prepare(
            "SELECT hash, added_at FROM cs_hashes WHERE added_at >= ?1
             UNION ALL SELECT hash, at FROM tombstones WHERE at >= ?1
             UNION ALL SELECT hash, at FROM quarantine WHERE at >= ?1",
        )?;
        let rows = stmt.query_map([since], |r| {
            Ok((r.get::<_, Vec<u8>>(0)?, r.get::<_, i64>(1)?))
        })?;
        for row in rows {
            let (h, at) = row?;
            if let Ok(h) = <[u8; 20]>::try_from(h) {
                held.push((h, at));
            }
        }
        Ok(Live { sealed, held })
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
        if all.is_empty() {
            return report;
        }
        let since = all
            .iter()
            .map(|s| s.taken_at.map_or(i64::MIN, held_since))
            .min()
            .unwrap_or(i64::MIN);
        let mut live = None;
        for snap in all {
            if snap
                .expires_at()
                .is_some_and(|expires_at| now >= expires_at)
            {
                match remove_snapshot(&snap.path) {
                    Ok(()) => report.expired.push(snap.path),
                    Err(e) => report.warnings.push(format!(
                        "deleting the expired snapshot {} failed: {e}",
                        snap.path.display()
                    )),
                }
                continue;
            }
            if live.is_none() {
                match Live::read(&self.conn, since) {
                    Ok(l) => live = Some(l),
                    Err(e) => {
                        report.warnings.push(format!(
                            "reading held hashes for the snapshot purge failed: {e}"
                        ));
                        return report;
                    }
                }
            }
            let live = live.as_ref().expect("read above");
            match purge_one(&snap, live, now, hits_before) {
                Ok(Outcome::Skipped(why)) => report.warnings.push(why),
                Ok(Outcome::Unchanged) => {}
                Ok(Outcome::Purged {
                    torrents,
                    quarantine,
                }) => {
                    report.torrents += torrents;
                    report.quarantine += quarantine;
                    report.purged.push(snap.path);
                }
                Err(e) => report.warnings.push(format!(
                    "purging the snapshot {} failed (it was left as it was): {e}",
                    snap.path.display()
                )),
            }
        }
        report
    }
}

enum Outcome {
    Skipped(String),
    Unchanged,
    Purged { torrents: usize, quarantine: usize },
}

/// Deletes a snapshot and any side file SQLite left beside it. A plain
/// unlink: overwriting first doesn't reach the old blocks on copy-on-write
/// filesystems or SSDs, so it would only cost a write of the whole file.
fn remove_snapshot(path: &Path) -> std::io::Result<()> {
    std::fs::remove_file(path)?;
    for suffix in ["-wal", "-shm", "-journal"] {
        let mut side = path.as_os_str().to_os_string();
        side.push(suffix);
        match std::fs::remove_file(PathBuf::from(side)) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e),
            _ => {}
        }
    }
    Ok(())
}

fn purge_one(snap: &Snapshot, live: &Live, now: i64, hits_before: i64) -> crate::Result<Outcome> {
    // Never created: a snapshot that vanished meanwhile is an error, not a
    // new empty file.
    let mut conn = Connection::open_with_flags(
        &snap.path,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    conn.busy_timeout(SNAPSHOT_BUSY)?;
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
        let held = version.map_or_else(|| "no schema version".into(), |v| format!("schema v{v}"));
        return Ok(Outcome::Skipped(format!(
            "the snapshot {} holds {held}, which this version doesn't know how to purge; it's \
             left as it is and deleted when it's {RETAIN_DAYS} days old",
            snap.path.display()
        )));
    };

    // Look first, so a snapshot with nothing to purge isn't written (nor
    // its modification time touched).
    let since = snap.taken_at.map_or(i64::MIN, held_since);
    let mut ids = BTreeSet::new();
    if plan.torrents {
        for (h, at) in &live.held {
            if *at < since {
                continue;
            }
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
    if ids.is_empty() && unsealed.is_empty() && expiring == 0 && hits == 0 {
        return Ok(Outcome::Unchanged);
    }

    // The WAL holds only the new pages, where the rollback journal would
    // hold copies of the old ones, sealed names included.
    conn.pragma_update(None, "secure_delete", true)?;
    conn.pragma_update(None, "foreign_keys", true)?;
    let mode: String = conn.pragma_update_and_check(None, "journal_mode", "WAL", |r| r.get(0))?;
    let wal = mode.eq_ignore_ascii_case("wal");
    let result = (|| -> rusqlite::Result<(usize, usize)> {
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
        tx.commit()?;
        Ok((torrents, quarantine))
    })();
    if wal {
        // Copies the new pages into the file and empties the WAL, then
        // goes back to a single file, as `VACUUM INTO` wrote it.
        let _ = conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()));
        let _ = conn.pragma_update(None, "journal_mode", "DELETE");
    }
    drop(conn);
    if let Some(taken) = snap.taken_at {
        // The expiry goes by the modification time: keep it the time the
        // snapshot was taken.
        let _ = set_modified(&snap.path, taken);
    }
    let (torrents, quarantine) = result?;
    Ok(Outcome::Purged {
        torrents,
        quarantine,
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
        let db = Path::new("/data/lodestar.db");
        let first = crate::schema::snapshot_path(db, 7);
        assert_eq!(
            parse_name("lodestar", first.file_name().unwrap().to_str().unwrap()),
            Some((7, 0))
        );
        assert_eq!(parse_name("lodestar", "lodestar.pre-v4.2.db"), Some((4, 2)));
        for bad in [
            "lodestar.pre-v.db",
            "lodestar.pre-v4..db",
            "lodestar.pre-v4.x.db",
            "lodestar.pre-v4.db-wal",
            "lodestar.pre-v4.db-journal",
            "other.pre-v4.db",
            "lodestar.pre-v-1.db",
            "lodestar.before-rollback-20261022-000000.db",
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
        assert!(!side(&snap, "-wal").exists() && !side(&snap, "-journal").exists());
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
}
