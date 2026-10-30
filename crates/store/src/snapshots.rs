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
//!   back after each purge. A snapshot whose time can't be read is
//!   deleted too: nothing shows it's inside the window. One dated ahead
//!   of the clock is redated to now, so it goes 90 days after it's first
//!   seen;
//! - **applies the live purge to the others**, matched to the schema the
//!   snapshot holds ([`Plan::for_version`]): expired quarantine (adult
//!   holds become tombstones, as live), sealed names the live store no
//!   longer holds, filter hits past their retention, and torrents the live
//!   store has deleted since the snapshot was taken (by hash: every
//!   deletion records its hash and time in `snapshot_deletions`,
//!   independently of temporary fetch holds). A snapshot of a schema this
//!   build doesn't know is left alone, with a warning.
//!
//! **The deletion journal is bounded.** A journal row is looked up only in
//! snapshots taken up to [`TAKEN_MARGIN`] after it, so a hash deleted and
//! fetched again stays in snapshots taken since. Rows older than
//! [`RETAIN_SECS`] plus that margin are dropped on every pass: every
//! snapshot they could match has expired. A pass that scrubbed every
//! snapshot it could also drops the rows it read. A snapshot of an unknown
//! schema doesn't hold them back (this build can never purge it, and it
//! expires at 90 days like the rest); a failed purge or unlink does.
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

/// Held and deleted hashes are looked for in a snapshot if they were
/// recorded up to this long before it was taken: a big store's `VACUUM
/// INTO` takes a while, and the modification time is set when it ends.
const TAKEN_MARGIN: i64 = 86_400;

/// Deletion journal rows older than this are dropped: they match only
/// snapshots taken before `now - RETAIN_SECS`, which have expired.
const JOURNAL_SECS: i64 = RETAIN_SECS + TAKEN_MARGIN;

/// After a pass over a snapshot, the next one looks up only holds
/// recorded since, less this much: a hold written on another connection
/// just before the pass may commit just after it.
const RECHECK_MARGIN: i64 = 600;

/// A full pass runs at least this often, and whenever the clock went back
/// since the last pass: a hold recorded with a time older than the last
/// pass less [`RECHECK_MARGIN`] (the clock stepped back, or the machine
/// slept between reading the time and committing) is still found.
const FULL_EVERY: i64 = 86_400;

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

impl Kind {
    /// What it is, for the log.
    pub fn describe(self) -> &'static str {
        match self {
            Kind::PreMigration { .. } => "migration snapshot",
            Kind::BeforeRollback => "rollback copy",
        }
    }
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

/// Whether this build can purge a snapshot holding schema `version`, as
/// [`purge_for_restore`] must before `lodestar rollback` restores it.
pub fn can_purge(version: i64) -> bool {
    Plan::for_version(version).is_some()
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

/// Deletes the copies `lodestar rollback` staged beside the store at `db`
/// and never renamed into place (it was interrupted):
/// `<stem>.rollback-<YYYYMMDD>-<HHMMSS>.tmp` and SQLite's side files. Each
/// may be a snapshot copy the store's deletions weren't applied to yet.
/// Run only while no rollback holds the store. The files that couldn't be
/// deleted.
pub fn remove_stale_stages(db: &Path) -> Vec<(PathBuf, std::io::Error)> {
    let dir = match db.parent() {
        Some(d) if !d.as_os_str().is_empty() => d,
        _ => Path::new("."),
    };
    let Some(stem) = db.file_stem().and_then(|s| s.to_str()) else {
        return Vec::new();
    };
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) => return vec![(dir.to_path_buf(), e)],
    };
    let mut failed = Vec::new();
    for entry in entries.flatten() {
        let Some(name) = entry.file_name().to_str().map(str::to_string) else {
            continue;
        };
        if !is_stage(stem, &name) || !entry.file_type().is_ok_and(|t| t.is_file()) {
            continue;
        }
        match std::fs::remove_file(entry.path()) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => failed.push((entry.path(), e)),
        }
    }
    failed
}

/// Whether `name` is a rollback's staged copy of the store `<stem>.db`, or
/// a side file of one.
fn is_stage(stem: &str, name: &str) -> bool {
    let Some((stamp, side)) = name
        .strip_prefix(stem)
        .and_then(|r| r.strip_prefix(".rollback-"))
        .and_then(|r| r.split_once(".tmp"))
    else {
        return false;
    };
    let b = stamp.as_bytes();
    b.len() == 15
        && b[8] == b'-'
        && b.iter()
            .enumerate()
            .all(|(i, c)| i == 8 || c.is_ascii_digit())
        && ["", "-wal", "-shm", "-journal"].contains(&side)
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
    /// Drop API keys, feed tokens, saved searches and collections the
    /// live store deleted, and revoke the keys it revoked (v10+).
    user_data: bool,
    /// Drop import candidates whose hash the live store deleted or holds
    /// (`import_candidates`, v13+).
    candidates: bool,
}

impl Plan {
    /// The newest schema version this table knows. A new migration must
    /// say here what its schema means for the purge.
    pub(crate) const KNOWN_THROUGH: i64 = 13;

    /// Per schema version (see the PR's table and `docs/decisions`):
    ///
    /// | version | torrents | quarantine, tombstones, filter hits | query log |
    /// |---|---|---|---|
    /// | 1–3 | purged | absent | absent |
    /// | 4–9 | purged | purged | absent |
    /// | 10–12 | purged | purged | purged |
    /// | 13 | purged, and import candidates | purged | purged |
    /// | other | skipped with a warning | skipped | skipped |
    pub(crate) fn for_version(version: i64) -> Option<Plan> {
        match version {
            // V1 torrents; V2 provenance, Insights, fetch queue; V3
            // reindex log. No filter tables.
            1..=3 => Some(Plan {
                torrents: true,
                filter: false,
                query_log: false,
                user_data: false,
                candidates: false,
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
                user_data: false,
                candidates: false,
            }),
            // V10 adds your data (saved searches, alerts, marks,
            // collections), API keys and the query log. Alerts, marks and
            // collection entries cascade from torrents. The query log is
            // pruned oldest first, so what the live store pruned is what's
            // older than its oldest row. `api_keys`, `saved_searches` and
            // `collections` have AUTOINCREMENT ids, so a snapshot row
            // whose id the live store has issued but no longer holds was
            // deleted there. Edited searches, marks and notes leave no
            // trace: they go with the snapshot at 90 days. V11 adds
            // Insights' day tables, its state and the removal trigger: a
            // torrent the purge deletes is counted as removed in the
            // snapshot, and its import comes off the total, as it did
            // live (B11b's decisions). V12 adds the live store's deletion
            // journal and its trigger: a V12 snapshot's own journal gains
            // the rows the purge deletes, which a rollback to it keeps.
            10..=12 => Some(Plan {
                torrents: true,
                filter: true,
                query_log: true,
                user_data: true,
                candidates: false,
            }),
            // V13 adds imports (IMP-1). Candidates hold hashes and names
            // that aren't torrents, so they go by hash; the other import
            // tables cascade from torrents or hold no names.
            13..=Self::KNOWN_THROUGH => Some(Plan {
                torrents: true,
                filter: true,
                query_log: true,
                user_data: true,
                candidates: true,
            }),
            _ => None,
        }
    }
}

/// What [`Store::purge_snapshots`] did.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct PurgeReport {
    /// Snapshots deleted for age.
    pub expired: Vec<Snapshot>,
    /// Snapshots that had something purged.
    pub purged: Vec<PathBuf>,
    /// Torrents deleted from snapshots.
    pub torrents: usize,
    /// Quarantine rows ended (expired) or sealed names cleared.
    pub quarantine: usize,
    /// Logged searches deleted.
    pub query_log: usize,
    /// API keys, saved searches and collections deleted, and keys
    /// revoked.
    pub user_data: usize,
    /// Import candidates deleted from snapshots.
    pub candidates: usize,
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
    /// The `now` of the last full pass.
    full_at: i64,
}

impl Marks {
    /// The oldest hold recorded at or after which `snap` still needs a
    /// lookup at `now`, and whether that's a full pass.
    fn since(&self, snap: &Snapshot, now: i64) -> (i64, bool) {
        // A time ahead of the clock says nothing of when it was taken:
        // look up everything, as for an unknown one.
        let taken = snap
            .taken_at
            .filter(|t| *t <= now)
            .map_or(i64::MIN, held_since);
        match self.0.get(&snap.path) {
            Some(m)
                if m.taken_at == snap.taken_at
                    && now >= m.through
                    && now.saturating_sub(m.full_at) < FULL_EVERY =>
            {
                (taken.max(m.through.saturating_sub(RECHECK_MARGIN)), false)
            }
            _ => (taken, true),
        }
    }
}

/// What the live store holds, read once per tick.
struct Live {
    /// The deletion journal: hashes the store deleted, with when.
    deleted: Vec<([u8; 20], i64)>,
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
    /// Per table in [`USER_TABLES`]: the highest id it has issued
    /// (`sqlite_sequence`) and the ids it holds.
    user_ids: Vec<(i64, HashSet<i64>)>,
    /// Revoked API keys and feed tokens: id and when.
    revoked: HashMap<i64, i64>,
}

/// V10 tables with AUTOINCREMENT ids whose deleted rows a snapshot loses.
/// Deleting a saved search also takes its alerts and feed tokens
/// (`ON DELETE CASCADE`).
const USER_TABLES: [&str; 3] = ["api_keys", "saved_searches", "collections"];

impl Live {
    /// Holds recorded before `since` (other than `reported`) aren't read.
    ///
    /// The writer's store has the newest schema; a rollback's may be older
    /// (one rolled back already). A table its schema lacks reads as empty,
    /// as it never recorded anything there, except the query log: one the
    /// store never had says nothing of what a snapshot's should lose.
    fn read(conn: &Connection, since: i64) -> rusqlite::Result<Live> {
        let hash = |h: Vec<u8>| <[u8; 20]>::try_from(h).ok();
        let mut deleted = Vec::new();
        if has_table(conn, "snapshot_deletions")? {
            let mut stmt = conn.prepare("SELECT hash, at FROM snapshot_deletions")?;
            let rows =
                stmt.query_map([], |r| Ok((r.get::<_, Vec<u8>>(0)?, r.get::<_, i64>(1)?)))?;
            for row in rows {
                let (h, at) = row?;
                deleted.extend(hash(h).map(|h| (h, at)));
            }
        }
        let mut sealed = HashSet::new();
        let mut reported = Vec::new();
        let mut held = Vec::new();
        // V4 adds cs_hashes, tombstones and quarantine together.
        if has_table(conn, "cs_hashes")? {
            Self::read_holds(conn, since, &mut sealed, &mut reported, &mut held)?;
        }
        let oldest_query = if has_table(conn, "query_log")? {
            conn.query_row(
                "SELECT at, id FROM query_log ORDER BY at, id LIMIT 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?
        } else {
            // Before every row: nothing goes.
            Some((i64::MIN, i64::MIN))
        };
        let mut user_ids = Vec::with_capacity(USER_TABLES.len());
        // V10 adds them together; without them none goes (see `purge_one`).
        if has_table(conn, "api_keys")? {
            for table in USER_TABLES {
                let seq: i64 = conn
                    .query_row(
                        "SELECT seq FROM sqlite_sequence WHERE name = ?1",
                        [table],
                        |r| r.get(0),
                    )
                    .optional()?
                    .unwrap_or(0);
                let mut stmt = conn.prepare(&format!("SELECT id FROM {table}"))?;
                let ids = stmt
                    .query_map([], |r| r.get(0))?
                    .collect::<rusqlite::Result<HashSet<i64>>>()?;
                user_ids.push((seq, ids));
            }
        }
        let revoked = if has_table(conn, "api_keys")? {
            let mut stmt =
                conn.prepare("SELECT id, revoked_at FROM api_keys WHERE revoked_at IS NOT NULL")?;
            stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect::<rusqlite::Result<HashMap<i64, i64>>>()?
        } else {
            HashMap::new()
        };
        Ok(Live {
            deleted,
            sealed,
            reported,
            held,
            oldest_query,
            user_ids,
            revoked,
        })
    }

    fn read_holds(
        conn: &Connection,
        since: i64,
        sealed: &mut HashSet<[u8; 20]>,
        reported: &mut Vec<[u8; 20]>,
        held: &mut Vec<([u8; 20], i64)>,
    ) -> rusqlite::Result<()> {
        let hash = |h: Vec<u8>| <[u8; 20]>::try_from(h).ok();
        let mut stmt = conn.prepare("SELECT hash FROM quarantine WHERE sealed IS NOT NULL")?;
        for h in stmt.query_map([], |r| r.get::<_, Vec<u8>>(0))? {
            sealed.extend(hash(h?));
        }
        let mut stmt = conn.prepare("SELECT hash FROM cs_hashes WHERE origin != ?1")?;
        for h in stmt.query_map([IMPORTED], |r| r.get::<_, Vec<u8>>(0))? {
            reported.extend(hash(h?));
        }
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
        Ok(())
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
        // Whether a snapshot may still hold a deletion this pass didn't
        // scrub: then the journal rows it read stay.
        let mut blocked = false;
        // An unpurged copy an interrupted rollback left: no rollback runs
        // while the store is open.
        for (path, e) in remove_stale_stages(&db) {
            blocked = true;
            report.warnings.push(format!(
                "deleting {}, left by an interrupted rollback, failed: {e}",
                path.display()
            ));
        }
        let mut keep = Vec::new();
        // Snapshots dated ahead of the clock, redated to now this pass.
        let mut redated = HashSet::new();
        for mut snap in all {
            // A time ahead of the clock (taken while it ran fast, say before
            // NTP synced) would keep the file past 90 days: it's redated to
            // now, so it expires 90 days after it was first seen.
            if snap.taken_at.is_some_and(|t| t > now) && set_modified(&snap.path, now).is_ok() {
                self.snapshot_marks.0.remove(&snap.path);
                snap.taken_at = Some(now);
                redated.insert(snap.path.clone());
            }
            if snap.expires_at().is_some_and(|at| now < at) {
                keep.push(snap);
                continue;
            }
            if snap.taken_at.is_none() {
                match std::fs::metadata(&snap.path) {
                    // Can't be looked at now (or gone): next pass.
                    Err(e) => {
                        if e.kind() != std::io::ErrorKind::NotFound {
                            blocked = true;
                            report.warnings.push(format!(
                                "reading the snapshot {} failed: {e}",
                                snap.path.display()
                            ));
                        }
                        continue;
                    }
                    // The safe bound for a time that can't be read: the
                    // file can't be shown to be inside the window, and
                    // keeping it would keep what it holds past the
                    // journal's 91 days.
                    Ok(_) => report.warnings.push(format!(
                        "the snapshot {} has no readable modification time, so its age is \
                         unknown; it's deleted as if {RETAIN_DAYS} days old",
                        snap.path.display()
                    )),
                }
            }
            self.snapshot_marks.0.remove(&snap.path);
            let (gone, failed) = remove_snapshot(&snap.path);
            for (path, e) in failed {
                blocked = true;
                report.warnings.push(format!(
                    "deleting the expired snapshot file {} failed: {e}",
                    path.display()
                ));
            }
            if gone {
                report.expired.push(snap);
            }
        }
        if let Err(e) = self.conn.execute(
            "DELETE FROM snapshot_deletions WHERE at < ?1",
            [now.saturating_sub(JOURNAL_SECS)],
        ) {
            report.warnings.push(format!(
                "dropping old snapshot deletion evidence failed: {e}"
            ));
        }
        // A redated snapshot's real time is unknown: everything is looked
        // up in it this pass.
        let since_for = |marks: &Marks, s: &Snapshot| {
            if redated.contains(&s.path) {
                (i64::MIN, true)
            } else {
                marks.since(s, now)
            }
        };
        let Some(since) = keep
            .iter()
            .map(|s| since_for(&self.snapshot_marks, s).0)
            .min()
        else {
            // No snapshot left to hold a deletion, unless an unlink failed.
            if !blocked && let Err(e) = self.conn.execute("DELETE FROM snapshot_deletions", []) {
                report
                    .warnings
                    .push(format!("clearing snapshot deletion evidence failed: {e}"));
            }
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
            let (since, full) = since_for(&self.snapshot_marks, &snap);
            match purge_one(&snap, &live, since, now, hits_before, None) {
                // Not purged, so no mark; and never purged by this build,
                // so it doesn't hold the journal back (see the module docs).
                Ok(Outcome::Skipped(why)) => {
                    report.warnings.push(why);
                    continue;
                }
                Ok(Outcome::Unchanged) => {}
                Ok(Outcome::Purged(n)) => {
                    report.torrents += n.torrents;
                    report.quarantine += n.quarantine;
                    report.query_log += n.query_log;
                    report.user_data += n.user_data;
                    report.candidates += n.candidates;
                    report.purged.push(snap.path.clone());
                }
                Err(e) => {
                    blocked = true;
                    report.warnings.push(format!(
                        "purging the snapshot {} failed (it was left as it was): {e}",
                        snap.path.display()
                    ));
                    continue;
                }
            }
            let full_at = match self.snapshot_marks.0.get(&snap.path) {
                Some(m) if !full => m.full_at,
                _ => now,
            };
            self.snapshot_marks.0.insert(
                snap.path,
                Mark {
                    taken_at: snap.taken_at,
                    through: now,
                    full_at,
                },
            );
        }
        // Every snapshot that could hold them has lost the rows read. A
        // row added or moved forward meanwhile (by time) stays.
        if !blocked {
            let acknowledged = (|| -> rusqlite::Result<()> {
                let tx = crate::begin_write(&mut self.conn)?;
                {
                    let mut delete = tx.prepare_cached(
                        "DELETE FROM snapshot_deletions WHERE hash = ?1 AND at = ?2",
                    )?;
                    for (hash, at) in &live.deleted {
                        delete.execute(params![&hash[..], at])?;
                    }
                }
                tx.commit()
            })();
            if let Err(e) = acknowledged {
                report.warnings.push(format!(
                    "acknowledging snapshot deletion evidence failed: {e}"
                ));
            }
        }
        report
    }
}

enum Outcome {
    Skipped(String),
    Unchanged,
    Purged(Counts),
}

#[derive(Debug, Default)]
struct Counts {
    torrents: usize,
    quarantine: usize,
    query_log: usize,
    user_data: usize,
    candidates: usize,
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

/// What a rollback carries into its copy from the store it replaces.
struct Carry<'a> {
    /// Deletion journal rows, merged into the copy's own journal when it
    /// has one.
    journal: &'a [([u8; 20], i64)],
    /// The store, whose holds are merged into the copy's when both have
    /// hold tables (V4+): see [`carry_holds`].
    holds: &'a Connection,
}

/// Applies the live purge to one snapshot, and for a rollback's copy
/// merges `carry` into it, in one transaction.
fn purge_one(
    snap: &Snapshot,
    live: &Live,
    since: i64,
    now: i64,
    hits_before: i64,
    carry: Option<&Carry<'_>>,
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
    let has_migrations = has_table(&conn, "schema_migrations")?;
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
    let mut candidates = BTreeSet::new();
    if plan.torrents {
        // `since` is no earlier than a day before the snapshot was taken:
        // a hash deleted before that and fetched again stays in it.
        let recent = (live.held.iter().chain(&live.deleted)).filter(|(_, at)| *at >= since);
        let mut find = plan
            .candidates
            .then(|| conn.prepare(CANDIDATE_BY_HASH))
            .transpose()?;
        for h in live.reported.iter().chain(recent.map(|(h, _)| h)) {
            ids.extend(crate::find_any(&conn, std::iter::once(InfoHash(*h)))?);
            if let Some(stmt) = &mut find {
                for c in stmt.query_map([&h[..]], |r| r.get::<_, Vec<u8>>(0))? {
                    candidates.insert(c?);
                }
            }
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
    // Deleted by id, and revoked since.
    let mut gone_user: Vec<(&str, i64)> = Vec::new();
    let mut revoke = Vec::new();
    if plan.user_data {
        for (table, (seq, held)) in USER_TABLES.iter().zip(&live.user_ids) {
            let mut stmt = conn.prepare(&format!("SELECT id FROM {table} WHERE id <= ?1"))?;
            for id in stmt.query_map([seq], |r| r.get::<_, i64>(0))? {
                let id = id?;
                if !held.contains(&id) {
                    gone_user.push((table, id));
                }
            }
        }
        let mut stmt = conn.prepare("SELECT id FROM api_keys WHERE revoked_at IS NULL")?;
        for id in stmt.query_map([], |r| r.get::<_, i64>(0))? {
            let id = id?;
            if let Some(at) = live.revoked.get(&id) {
                revoke.push((id, *at));
            }
        }
    }
    // Journal rows the snapshot lacks, or holds at an older time.
    let mut journal = Vec::new();
    let carried = carry.map_or(&[][..], |c| c.journal);
    if !carried.is_empty() && has_table(&conn, "snapshot_deletions")? {
        let mut stmt = conn.prepare("SELECT at FROM snapshot_deletions WHERE hash = ?1")?;
        for (h, at) in carried {
            let held: Option<i64> = stmt.query_row([&h[..]], |r| r.get(0)).optional()?;
            if held.is_none_or(|held| held < *at) {
                journal.push((h, *at));
            }
        }
    }
    // The store's holds, for a copy with hold tables from a store that
    // has them (both V4+, which adds all three).
    let holds = match carry {
        Some(c) if plan.filter && has_table(c.holds, "cs_hashes")? => Some(c.holds),
        _ => None,
    };
    if ids.is_empty()
        && holds.is_none()
        && journal.is_empty()
        && candidates.is_empty()
        && unsealed.is_empty()
        && expiring == 0
        && hits == 0
        && logged == 0
        && gone_user.is_empty()
        && revoke.is_empty()
    {
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
    let result = (|| -> rusqlite::Result<Counts> {
        let tx = crate::begin_write(&mut conn)?;
        let mut torrents = 0;
        {
            // Cached: the delete carries Insights' trigger (V11).
            let mut delete = tx.prepare_cached("DELETE FROM torrents WHERE id = ?1")?;
            for id in &ids {
                torrents += delete.execute([id])?;
            }
        }
        let mut candidates_gone = 0;
        for h in &candidates {
            candidates_gone += tx.execute("DELETE FROM import_candidates WHERE hash = ?1", [h])?;
        }
        // Before the quarantine purge below, which then expires and
        // unseals the merged rows as the store's own tick would.
        if let Some(live) = holds {
            carry_holds(&tx, live)?;
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
        let mut user_data = 0;
        for (table, id) in &gone_user {
            user_data += tx.execute(&format!("DELETE FROM {table} WHERE id = ?1"), [id])?;
        }
        for (id, at) in &revoke {
            user_data += tx.execute(
                "UPDATE api_keys SET revoked_at = ?2 WHERE id = ?1 AND revoked_at IS NULL",
                [id, at],
            )?;
        }
        for (h, at) in &journal {
            tx.execute(
                "INSERT INTO snapshot_deletions (hash, at) VALUES (?1, ?2)
                 ON CONFLICT (hash) DO UPDATE SET at = max(at, excluded.at)",
                params![&h[..], at],
            )?;
        }
        tx.commit()?;
        Ok(Counts {
            torrents,
            quarantine,
            query_log,
            user_data,
            candidates: candidates_gone,
        })
    })();
    if wal {
        // Copies the new pages into the file and empties the WAL, then
        // goes back to a single file, as `VACUUM INTO` wrote it.
        let _ = conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()));
        let _ = conn.pragma_update(None, "journal_mode", "DELETE");
    }
    restore_time(conn);
    Ok(Outcome::Purged(result?))
}

/// How strong a tombstone or quarantine hold under `rule` is, in SQL with
/// the floor's rules as `?1`–`?3` and the adult mark as `?4`: the floor's
/// can't be released, then an adult hold (the one filtering can't
/// rebuild, see `Store::purge_filter`), then any other.
fn hold_rank(rule: &str) -> String {
    format!(
        "CASE WHEN {rule} IN (?1, ?2, ?3) THEN 2 \
         WHEN substr({rule}, -length(?4)) = ?4 THEN 1 ELSE 0 END"
    )
}

/// Merges the holds of `live` (the store a rollback replaces) into its
/// copy, in the copy's transaction, so the restored store goes on
/// refusing what was held after the snapshot (`docs/decisions`). Nothing
/// is loosened: a hold only the copy has stays (a release since the
/// snapshot is undone), and on a conflict
/// - a CS-1 entry is kept as `add_cs_hashes` keeps it: your own
///   confirmation or report outranks an import;
/// - a tombstone or quarantine row keeps the stronger hold
///   ([`hold_rank`]), then the later one, the store's on a tie.
///
/// Rows are copied as they are: a sealed name stays sealed, and the
/// quarantine purge that follows expires or unseals them as the store
/// did.
fn carry_holds(tx: &Connection, live: &Connection) -> rusqlite::Result<()> {
    let floor = crate::filter::FLOOR_RULES;
    let mut add = tx.prepare(
        "INSERT INTO cs_hashes (hash, origin, added_at) VALUES (?1, ?2, ?3)
         ON CONFLICT (hash) DO UPDATE SET origin = excluded.origin
         WHERE cs_hashes.origin = ?4",
    )?;
    let mut stmt = live.prepare("SELECT hash, origin, added_at FROM cs_hashes")?;
    let mut rows = stmt.query([])?;
    while let Some(r) = rows.next()? {
        let (hash, origin, at): (Vec<u8>, i64, i64) = (r.get(0)?, r.get(1)?, r.get(2)?);
        add.execute(params![hash, origin, at, IMPORTED])?;
    }
    let mut add = tx.prepare(&format!(
        "INSERT INTO tombstones (hash, rule, at) VALUES (?5, ?6, ?7)
         ON CONFLICT (hash) DO UPDATE SET rule = excluded.rule, at = excluded.at
         WHERE ({}, excluded.at) >= ({}, tombstones.at)",
        hold_rank("excluded.rule"),
        hold_rank("tombstones.rule"),
    ))?;
    let mut stmt = live.prepare("SELECT hash, rule, at FROM tombstones")?;
    let mut rows = stmt.query([])?;
    while let Some(r) = rows.next()? {
        let (hash, rule, at): (Vec<u8>, String, i64) = (r.get(0)?, r.get(1)?, r.get(2)?);
        add.execute(params![
            floor[0], floor[1], floor[2], ADULT_HOLD, hash, rule, at
        ])?;
    }
    let mut add = tx.prepare(&format!(
        "INSERT INTO quarantine (hash, rule, sealed, at, expires_at)
         VALUES (?5, ?6, ?7, ?8, ?9)
         ON CONFLICT (hash) DO UPDATE SET rule = excluded.rule, sealed = excluded.sealed,
             at = excluded.at, expires_at = excluded.expires_at
         WHERE ({}, excluded.at) >= ({}, quarantine.at)",
        hold_rank("excluded.rule"),
        hold_rank("quarantine.rule"),
    ))?;
    let mut stmt = live.prepare("SELECT hash, rule, sealed, at, expires_at FROM quarantine")?;
    let mut rows = stmt.query([])?;
    while let Some(r) = rows.next()? {
        let (hash, rule, sealed, at, expires_at): (Vec<u8>, String, Option<Vec<u8>>, i64, i64) =
            (r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?);
        add.execute(params![
            floor[0], floor[1], floor[2], ADULT_HOLD, hash, rule, sealed, at, expires_at
        ])?;
    }
    Ok(())
}

fn has_table(conn: &Connection, name: &str) -> rusqlite::Result<bool> {
    Ok(conn
        .query_row(
            "SELECT 1 FROM sqlite_schema WHERE type = 'table' AND name = ?1",
            [name],
            |_| Ok(true),
        )
        .optional()?
        .unwrap_or(false))
}

/// Applies the store's deletions to `staged`, a copy of a snapshot that
/// `lodestar rollback` is about to put in place of the store `live` (BB-1
/// for rollback, `docs/decisions`): the purge the writer's tick would
/// apply to the snapshot, matching **every** journal and hold row by
/// hash, whenever it was recorded. The tick's window (from a day before
/// the snapshot's modification time) would let a deleted torrent back
/// into a copy whose time is later than when it was taken (one copied
/// without keeping its time); the cost is that a torrent deleted and
/// fetched again since goes too, to be fetched again.
///
/// In the same transaction, `live`'s whole journal is merged into the
/// copy's own, so the restored store goes on purging the other snapshots,
/// and its holds into the copy's ([`carry_holds`]), so it goes on
/// refusing what they hold. The copy ends as one file, with no WAL beside
/// it.
///
/// `Ok(None)` when the copy holds a schema this build can't purge: then
/// it mustn't be restored.
pub fn purge_for_restore(
    live: &Connection,
    staged: &Path,
    now: i64,
) -> crate::Result<Option<usize>> {
    let since = i64::MIN;
    let store = live;
    let live = Live::read(store, since)?;
    let snap = Snapshot {
        path: staged.to_path_buf(),
        name: String::new(),
        kind: Kind::BeforeRollback,
        // The copy's own time doesn't matter: it's never put back.
        taken_at: None,
    };
    // Filter hits aren't deletions: the restored store's own tick ages
    // them out.
    let carry = Carry {
        journal: &live.deleted,
        holds: store,
    };
    let outcome = purge_one(&snap, &live, since, now, i64::MIN, Some(&carry))?;
    // `purge_one` checkpoints without checking: a WAL left beside the
    // copy would be lost with the rename, so it's folded in here.
    settle(staged)?;
    let mut wal = staged.as_os_str().to_os_string();
    wal.push("-wal");
    if Path::new(&wal).exists() {
        return Err(crate::Error::Corrupt("the restored copy still has a WAL"));
    }
    Ok(match outcome {
        Outcome::Skipped(_) => None,
        Outcome::Unchanged => Some(0),
        Outcome::Purged(n) => Some(n.torrents),
    })
}

/// A snapshot's candidates under a hash, primary or truncated v2.
const CANDIDATE_BY_HASH: &str = "SELECT hash FROM import_candidates WHERE hash = ?1
     UNION ALL
     SELECT hash FROM import_candidates
     WHERE hash_v2 IS NOT NULL AND substr(hash_v2, 1, 20) = ?1";

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

    fn paths(snaps: &[Snapshot]) -> Vec<PathBuf> {
        snaps.iter().map(|s| s.path.clone()).collect()
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

    /// The deletion journal, by hash.
    fn journal(store: &Store) -> Vec<([u8; 20], i64)> {
        let mut stmt = store
            .connection()
            .prepare("SELECT hash, at FROM snapshot_deletions ORDER BY hash")
            .unwrap();
        stmt.query_map([], |r| {
            Ok((r.get::<_, Vec<u8>>(0)?.try_into().unwrap(), r.get(1)?))
        })
        .unwrap()
        .map(Result::unwrap)
        .collect()
    }

    /// Dates the whole journal `at` on the tests' clock: the trigger uses
    /// the real one.
    fn stamp(store: &Store, at: i64) {
        store
            .connection()
            .execute("UPDATE snapshot_deletions SET at = ?1", [at])
            .unwrap();
    }

    fn journal_row(store: &Store, hash: [u8; 20], at: i64) {
        store
            .connection()
            .execute(
                "INSERT INTO snapshot_deletions (hash, at) VALUES (?1, ?2)",
                params![&hash[..], at],
            )
            .unwrap();
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

    #[test]
    fn stage_names_are_only_a_rollbacks() {
        for name in [
            "lodestar.rollback-20261030-120000.tmp",
            "lodestar.rollback-20261030-120000.tmp-wal",
            "lodestar.rollback-20261030-120000.tmp-shm",
            "lodestar.rollback-20261030-120000.tmp-journal",
        ] {
            assert!(is_stage("lodestar", name), "{name}");
        }
        for name in [
            "lodestar.db",
            "lodestar.pre-v4.db",
            "lodestar.rollback-20261030-120000.tmp.bak",
            "lodestar.rollback-2026103-120000.tmp",
            "lodestar.rollback-20261030x120000.tmp",
            "other.rollback-20261030-120000.tmp",
            "lodestar.before-rollback-20261030-120000.db",
        ] {
            assert!(!is_stage("lodestar", name), "{name}");
        }
    }

    /// A rollback's copy loses what the store deleted, at any schema with
    /// a plan, keeps the journal when it has one, and is refused at a
    /// schema without. The whole journal applies, not only the rows from
    /// a day before the copy's time: a snapshot copied without keeping
    /// its time looks taken after deletions it still holds.
    #[test]
    fn purge_for_restore_applies_the_journal_to_a_copy() {
        let dir = tempfile::tempdir().unwrap();
        let (store, _db) = live(dir.path());
        journal_row(&store, [5; 20], T);
        journal_row(&store, [6; 20], T - 10 * DAY);
        for version in [3, 11, 12, Plan::KNOWN_THROUGH] {
            let copy = dir.path().join(format!("copy-v{version}.tmp"));
            let conn = fixture(&copy, version);
            insert_torrent(&conn, 1, [5; 20], "deleted since");
            insert_torrent(&conn, 2, [6; 20], "deleted before its time");
            insert_torrent(&conn, 3, [7; 20], "kept");
            drop(conn);
            // Dated well after both deletions, as `cp` without `-p` dates it.
            set_modified(&copy, T + 30 * DAY).unwrap();
            let n = purge_for_restore(store.connection(), &copy, T + 31 * DAY).unwrap();
            assert_eq!(n, Some(2), "v{version}");
            assert!(
                !contains(&copy, b"files of deleted before its time"),
                "v{version}"
            );
            assert!(!contains(&copy, b"files of deleted since"), "v{version}");
            assert!(!side(&copy, "-wal").exists());
            quick_check(&copy);
            let conn = Connection::open(&copy).unwrap();
            let names: Vec<String> = conn
                .prepare("SELECT name FROM torrents")
                .unwrap()
                .query_map([], |r| r.get(0))
                .unwrap()
                .map(Result::unwrap)
                .collect();
            assert_eq!(names, ["kept"], "v{version}");
            if version >= 12 {
                let rows: Vec<(Vec<u8>, i64)> = conn
                    .prepare("SELECT hash, at FROM snapshot_deletions ORDER BY hash")
                    .unwrap()
                    .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
                    .unwrap()
                    .map(Result::unwrap)
                    .collect();
                // The purge's own delete is journalled at the real time,
                // which the carried row's older one doesn't move back.
                assert_eq!(rows.len(), 2, "v{version}");
                assert_eq!(rows[0].0, vec![5; 20]);
                assert!(rows[0].1 >= T);
                assert_eq!(rows[1].0, vec![6; 20]);
                assert!(rows[1].1 >= T - 10 * DAY);
            }
        }
        let copy = dir.path().join("copy-new.tmp");
        let conn = fixture(&copy, Plan::KNOWN_THROUGH);
        insert_torrent(&conn, 1, [5; 20], "deleted since");
        conn.execute(
            "INSERT INTO schema_migrations (version, applied_at) VALUES (?1, 0)",
            [Plan::KNOWN_THROUGH + 1],
        )
        .unwrap();
        drop(conn);
        assert!(!can_purge(Plan::KNOWN_THROUGH + 1));
        let n = purge_for_restore(store.connection(), &copy, T + DAY).unwrap();
        assert_eq!(n, None);
    }

    /// A store rolled back already can be older than the copy: what its
    /// schema lacks takes nothing from the copy, but its journal and holds
    /// still do, and both are carried into the copy.
    #[test]
    fn purge_for_restore_from_an_older_store() {
        let dir = tempfile::tempdir().unwrap();
        for live_version in [3, 9, 11, 12] {
            let live = fixture(
                &dir.path().join(format!("live-v{live_version}.db")),
                live_version,
            );
            if live_version >= 12 {
                live.execute(
                    "INSERT INTO snapshot_deletions (hash, at) VALUES (?1, ?2)",
                    params![&[5u8; 20][..], T],
                )
                .unwrap();
            }
            if live_version >= 4 {
                live.execute(
                    "INSERT INTO tombstones (hash, rule, at) VALUES (?1, 'CS-3', ?2)",
                    params![&[6u8; 20][..], T],
                )
                .unwrap();
            }
            if live_version >= 10 {
                // Its log isn't empty, so it wasn't cleared.
                live.execute(
                    "INSERT INTO query_log (at, query, latency_us) VALUES (?1, 'live', 0)",
                    [T - DAY],
                )
                .unwrap();
            }
            let copy = dir.path().join(format!("copy-for-v{live_version}.tmp"));
            let conn = fixture(&copy, Plan::KNOWN_THROUGH);
            insert_torrent(&conn, 1, [5; 20], "deleted since");
            conn.execute(
                "INSERT INTO query_log (at, query, latency_us) VALUES (?1, 'kept', 0)",
                [T],
            )
            .unwrap();
            insert_torrent(&conn, 2, [6; 20], "held since");
            drop(conn);
            let n = purge_for_restore(&live, &copy, T + DAY).unwrap();
            let expect = usize::from(live_version >= 12) + usize::from(live_version >= 4);
            assert_eq!(n, Some(expect), "v{live_version}");
            let conn = Connection::open(&copy).unwrap();
            let logged: i64 = conn
                .query_row("SELECT count(*) FROM query_log", [], |r| r.get(0))
                .unwrap();
            assert_eq!(logged, 1, "v{live_version}");
            let tombstones: Vec<Vec<u8>> = conn
                .prepare("SELECT hash FROM tombstones")
                .unwrap()
                .query_map([], |r| r.get(0))
                .unwrap()
                .map(Result::unwrap)
                .collect();
            let held = if live_version >= 4 {
                vec![vec![6; 20]]
            } else {
                vec![]
            };
            assert_eq!(tombstones, held, "v{live_version}");
            // The journal row is merged, at its time (the purge's own
            // delete is journalled at the real one, which is earlier).
            let rows: Vec<(Vec<u8>, i64)> = conn
                .prepare("SELECT hash, at FROM snapshot_deletions WHERE hash = ?1")
                .unwrap()
                .query_map([&[5u8; 20][..]], |r| Ok((r.get(0)?, r.get(1)?)))
                .unwrap()
                .map(Result::unwrap)
                .collect();
            if live_version >= 12 {
                assert_eq!(rows.len(), 1, "v{live_version}");
                assert!(rows[0].1 >= T, "v{live_version}");
            } else {
                assert!(rows.is_empty(), "v{live_version}");
            }
        }
    }

    /// Every table's rows, for comparing a copy's holds.
    fn rows(path: &Path, sql: &str) -> Vec<String> {
        let conn = Connection::open(path).unwrap();
        let mut stmt = conn.prepare(sql).unwrap();
        let n = stmt.column_count();
        stmt.query_map([], |r| {
            Ok((0..n)
                .map(|i| match r.get_ref(i).unwrap() {
                    rusqlite::types::ValueRef::Blob(b) if b.len() == 20 => format!("h{}", b[0]),
                    rusqlite::types::ValueRef::Blob(b) => String::from_utf8_lossy(b).into(),
                    rusqlite::types::ValueRef::Text(t) => String::from_utf8_lossy(t).into(),
                    rusqlite::types::ValueRef::Integer(i) => i.to_string(),
                    v => format!("{v:?}"),
                })
                .collect::<Vec<_>>()
                .join(" "))
        })
        .unwrap()
        .map(Result::unwrap)
        .collect()
    }

    /// A rollback's copy gains the holds recorded since the snapshot
    /// (CS-1 entries, imported ones too, tombstones and quarantine, its
    /// sealed name still sealed), and the torrents they hold go. On a
    /// conflict the stronger hold, then the later one, wins; a hold only
    /// the copy has stays.
    #[test]
    fn purge_for_restore_carries_the_holds() {
        let dir = tempfile::tempdir().unwrap();
        let (mut store, _db) = live(dir.path());
        let c = store.connection();
        let tomb = |h: u8, rule: &str, at: i64| {
            c.execute(
                "INSERT INTO tombstones (hash, rule, at) VALUES (?1, ?2, ?3)",
                params![&[h; 20][..], rule, at],
            )
            .unwrap();
        };
        // New since the snapshot.
        tomb(3, "CS-3", T + DAY);
        quarantine(c, [4; 20], "CS-2a", b"sealed-new-name", T + 30 * DAY);
        // Conflicts: a floor tombstone and an adult one outrank a later
        // one; otherwise the later wins.
        tomb(10, "CS-3", T + DAY);
        tomb(11, "CS-4", T + DAY);
        tomb(12, "CS-4", T + DAY);
        quarantine(c, [13; 20], "CS-2a", b"sealed-requeued", T + 40 * DAY);
        quarantine(c, [15; 20], "CS-2a", b"sealed-plain", T + 40 * DAY);
        // Expired: carried, then turned into a tombstone as live.
        quarantine(c, [16; 20], "CS-9+adult", b"sealed-expired", T);
        store.add_cs_hashes(&[[1; 20]], 1, T + DAY).unwrap();
        store.add_cs_hashes(&[[2; 20]], 2, T + DAY).unwrap();
        store.add_cs_hashes(&[[7; 20]], 0, T + DAY).unwrap();
        store.add_cs_hashes(&[[8; 20]], 2, T + DAY).unwrap();

        for version in [3, 4, 9, 12, Plan::KNOWN_THROUGH] {
            let copy = dir.path().join(format!("held-v{version}.tmp"));
            let conn = fixture(&copy, version);
            insert_torrent(&conn, 1, [3; 20], "tombstoned since");
            insert_torrent(&conn, 2, [1; 20], "reported since");
            insert_torrent(&conn, 3, [2; 20], "imported since");
            insert_torrent(&conn, 4, [9; 20], "kept");
            if version >= 4 {
                let tomb = |h: u8, rule: &str, at: i64| {
                    conn.execute(
                        "INSERT INTO tombstones (hash, rule, at) VALUES (?1, ?2, ?3)",
                        params![&[h; 20][..], rule, at],
                    )
                    .unwrap();
                };
                tomb(10, "CS-1a", T - DAY);
                tomb(11, "CS-3", T - DAY);
                tomb(12, "CS-3+adult", T - DAY);
                tomb(14, "CS-3", T - DAY);
                quarantine(&conn, [13; 20], "CS-2a", b"sealed-old", T + 5 * DAY);
                quarantine(&conn, [15; 20], "CS-9+adult", b"sealed-adult", T + 5 * DAY);
                for (h, origin) in [(7u8, 2i64), (8, 1)] {
                    conn.execute(
                        "INSERT INTO cs_hashes (hash, origin, added_at) VALUES (?1, ?2, ?3)",
                        params![&[h; 20][..], origin, T - DAY],
                    )
                    .unwrap();
                }
            }
            drop(conn);
            let n = purge_for_restore(store.connection(), &copy, T + 2 * DAY).unwrap();
            assert_eq!(n, Some(3), "v{version}");
            quick_check(&copy);
            assert_eq!(
                rows(&copy, "SELECT name FROM torrents"),
                ["kept"],
                "v{version}"
            );
            if version < 4 {
                // No hold tables: nothing is added to the schema.
                let conn = Connection::open(&copy).unwrap();
                assert!(!has_table(&conn, "tombstones").unwrap());
                continue;
            }
            assert_eq!(
                rows(&copy, "SELECT hash, origin FROM cs_hashes ORDER BY hash"),
                ["h1 1", "h2 2", "h7 0", "h8 1"],
                "v{version}"
            );
            let d = |n: i64| (T + n * DAY).to_string();
            assert_eq!(
                rows(&copy, "SELECT hash, rule, at FROM tombstones ORDER BY hash"),
                [
                    format!("h3 CS-3 {}", d(1)),
                    format!("h10 CS-1a {}", d(-1)),
                    format!("h11 CS-4 {}", d(1)),
                    format!("h12 CS-3+adult {}", d(-1)),
                    format!("h14 CS-3 {}", d(-1)),
                    format!("h16 CS-9+adult {}", d(-30)),
                ],
                "v{version}"
            );
            assert_eq!(
                rows(
                    &copy,
                    "SELECT hash, rule, sealed, expires_at FROM quarantine ORDER BY hash"
                ),
                [
                    format!("h4 CS-2a sealed-new-name {}", d(30)),
                    format!("h13 CS-2a sealed-requeued {}", d(40)),
                    format!("h15 CS-9+adult sealed-adult {}", d(5)),
                ],
                "v{version}"
            );
            assert!(!contains(&copy, b"sealed-expired"), "v{version}");
        }
    }

    /// The tick deletes what an interrupted rollback left.
    #[test]
    fn the_tick_deletes_an_interrupted_rollbacks_copy() {
        let dir = tempfile::tempdir().unwrap();
        let (mut store, _db) = live(dir.path());
        let stage = dir.path().join("lodestar.rollback-20261030-120000.tmp");
        std::fs::write(&stage, b"copy").unwrap();
        store.purge_snapshots(T, 0);
        assert!(!stage.exists());
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
            assert_eq!(plan.user_data, v >= 10, "v{v}");
            assert_eq!(plan.candidates, v >= 13, "v{v}");
        }
        assert_eq!(Plan::for_version(0), None);
        assert_eq!(Plan::for_version(latest + 1), None);
    }

    #[test]
    fn deletion_evidence_survives_expiry_release_restart_and_failed_snapshot() {
        for release in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let (mut store, db) = live(dir.path());
            let hash = [41; 20];
            insert_torrent(store.connection(), 1, hash, "purged ordinary quarantine");
            insert_torrent(store.connection(), 2, [42; 20], "keep this torrent");
            let snap = take(&store, &db, 12, T);
            quarantine(store.connection(), hash, "U-1", b"sealed", T + 30 * DAY);
            store.delete_torrents(&[hash]).unwrap();
            stamp(&store, T + 30 * DAY);
            if release {
                store.release(&hash).unwrap();
            }
            store.purge_filter(T + 31 * DAY, 0).unwrap();
            assert!(store.quarantined(10).unwrap().is_empty());

            // A failed pass must not acknowledge the durable evidence.
            let locked = Connection::open(&snap).unwrap();
            locked.execute_batch("BEGIN EXCLUSIVE").unwrap();
            assert!(!store.purge_snapshots(T + 31 * DAY, 0).warnings.is_empty());
            locked.execute_batch("ROLLBACK").unwrap();
            drop(locked);
            drop(store);
            let mut store = Store::open(&db, StoreConfig::default()).unwrap();
            let report = store.purge_snapshots(T + 32 * DAY, 0);
            assert!(report.warnings.is_empty(), "{:?}", report.warnings);
            assert_eq!(report.torrents, 1);
            assert!(!contains(&snap, b"purged ordinary quarantine"));
            assert!(contains(&snap, b"keep this torrent"));
            assert_eq!(journal(&store), []);
            quick_check(&snap);
        }
    }

    #[test]
    fn journal_matches_hashes_across_different_snapshot_ids_and_old_schemas() {
        let dir = tempfile::tempdir().unwrap();
        let (mut store, db) = live(dir.path());
        let hash = [51; 20];
        insert_torrent(store.connection(), 7, hash, "live copy");
        let snap = crate::schema::snapshot_path(&db, 2);
        let conn = fixture(&snap, 1);
        insert_torrent(&conn, 99, hash, "older copy");
        insert_torrent(&conn, 7, [52; 20], "different history");
        drop(conn);
        set_modified(&snap, T).unwrap();
        store.delete_torrents(&[hash]).unwrap();
        stamp(&store, T + DAY / 2);
        let report = store.purge_snapshots(T + DAY, 0);
        assert!(report.warnings.is_empty(), "{:?}", report.warnings);
        assert_eq!(report.torrents, 1);
        assert!(!contains(&snap, b"older copy"));
        assert!(contains(&snap, b"different history"));
    }

    #[test]
    fn rolled_back_delete_does_not_create_snapshot_obligation() {
        let dir = tempfile::tempdir().unwrap();
        let (store, _) = live(dir.path());
        insert_torrent(store.connection(), 1, [61; 20], "retained");
        store
            .connection()
            .execute_batch("BEGIN; DELETE FROM torrents; ROLLBACK;")
            .unwrap();
        assert_eq!(journal(&store), []);
    }

    fn set_v2(conn: &Connection, id: i64, v2: [u8; 32]) {
        conn.execute(
            "UPDATE torrents SET info_hash_v2 = ?2 WHERE id = ?1",
            params![id, &v2[..]],
        )
        .unwrap();
    }

    /// V12's backfill: the holds of a V11 store go in at their own time
    /// (the later of a hash's two), except a hash that's live again,
    /// by its v1 hash or its truncated v2 one.
    #[test]
    fn migrating_v11_journals_holds_at_their_time_except_live_hashes() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("lodestar.db");
        let conn = fixture(&db, 11);
        let tomb = |h: u8, at: i64| {
            conn.execute(
                "INSERT INTO tombstones (hash, rule, at) VALUES (?1, 'FK-4', ?2)",
                params![&[h; 20][..], at],
            )
            .unwrap();
        };
        quarantine(&conn, [1; 20], "CS-3", b"s", 100 + 30 * DAY);
        tomb(2, 200);
        quarantine(&conn, [3; 20], "CS-3+adult", b"s", 300 + 30 * DAY);
        tomb(3, 400);
        quarantine(&conn, [4; 20], "CS-3", b"s", 500 + 30 * DAY);
        insert_torrent(&conn, 1, [4; 20], "live by v1");
        tomb(5, 600);
        insert_torrent(&conn, 2, [6; 20], "live by v2");
        let mut v2 = [5; 32];
        v2[20..].fill(9);
        set_v2(&conn, 2, v2);
        drop(conn);

        let store = Store::open(&db, StoreConfig::default()).unwrap();
        assert_eq!(
            journal(&store),
            [([1; 20], 100), ([2; 20], 200), ([3; 20], 400)]
        );
    }

    /// A store at V12's pre-release shape (builds from `a7c6d5c8` until
    /// `at` was added: no time column, a trigger that wrote none) is
    /// repaired when opened: rows dated now, live hashes dropped, the
    /// trigger replaced, and the purge runs. Opening again changes nothing.
    #[test]
    fn opening_a_pre_release_v12_store_repairs_its_journal() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("lodestar.db");
        let conn = fixture(&db, 11);
        conn.execute_batch(
            "CREATE TABLE snapshot_deletions (
                 hash BLOB PRIMARY KEY CHECK (length(hash) = 20)
             ) STRICT, WITHOUT ROWID;
             CREATE TRIGGER snapshot_torrent_deleted AFTER DELETE ON torrents BEGIN
                 INSERT OR IGNORE INTO snapshot_deletions (hash) VALUES (old.info_hash);
                 INSERT OR IGNORE INTO snapshot_deletions (hash)
                     SELECT substr(old.info_hash_v2, 1, 20) WHERE old.info_hash_v2 IS NOT NULL;
             END;
             INSERT INTO schema_migrations (version, applied_at) VALUES (12, 0);",
        )
        .unwrap();
        insert_torrent(&conn, 1, [81; 20], "live again");
        insert_torrent(&conn, 2, [82; 20], "deleted-marker-0c5f");
        conn.execute("DELETE FROM torrents WHERE id = 2", [])
            .unwrap();
        conn.execute(
            "INSERT INTO snapshot_deletions (hash) VALUES (?1)",
            [&[81u8; 20][..]],
        )
        .unwrap();
        drop(conn);
        let clock = || {
            let t = std::time::SystemTime::now().duration_since(UNIX_EPOCH);
            i64::try_from(t.unwrap().as_secs()).unwrap()
        };

        let before = clock();
        let mut store = Store::open(&db, StoreConfig::default()).unwrap();
        let after = clock();
        let rows = journal(&store);
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(rows[0].0, [82; 20]);
        assert!((before..=after).contains(&rows[0].1), "{rows:?}");
        let trigger = |conn: &Connection| -> String {
            conn.query_row(
                "SELECT sql FROM sqlite_schema WHERE name = 'snapshot_torrent_deleted'",
                [],
                |r| r.get(0),
            )
            .unwrap()
        };
        let fresh = Store::open_in_memory(StoreConfig::default()).unwrap();
        assert_eq!(trigger(store.connection()), trigger(fresh.connection()));
        store.delete_torrents(&[[81; 20]]).unwrap();
        let rows = journal(&store);
        assert_eq!(rows[0].0, [81; 20]);
        assert!(rows[0].1 >= before, "{rows:?}");

        // The purge reads the column; a snapshot from before the deletion
        // loses the torrent. So does V13's pre-migration snapshot, taken
        // on open while `[81; 20]` was still stored.
        let snap = crate::schema::snapshot_path(&db, 12);
        let conn = fixture(&snap, 11);
        insert_torrent(&conn, 2, [82; 20], "deleted-marker-0c5f");
        drop(conn);
        set_modified(&snap, before - DAY).unwrap();
        let r = store.purge_snapshots(after, 0);
        assert!(r.warnings.is_empty(), "{:?}", r.warnings);
        assert_eq!(r.torrents, 2);
        assert!(!contains(&snap, b"deleted-marker-0c5f"));
        journal_row(&store, [83; 20], 7);
        drop(store);
        let store = Store::open(&db, StoreConfig::default()).unwrap();
        assert_eq!(journal(&store), [([83; 20], 7)], "repaired once");
    }

    /// The trigger journals a hybrid's v1 hash and its truncated v2 one,
    /// at the time of the delete; deleting a hash again moves it forward.
    #[test]
    fn a_hybrid_delete_journals_both_hashes_and_a_repeat_moves_the_time() {
        let dir = tempfile::tempdir().unwrap();
        let (mut store, _) = live(dir.path());
        let v1 = [21; 20];
        let mut v2 = [22; 32];
        v2[20..].fill(23);
        insert_torrent(store.connection(), 1, v1, "hybrid");
        set_v2(store.connection(), 1, v2);
        let clock = || {
            let t = std::time::SystemTime::now().duration_since(UNIX_EPOCH);
            i64::try_from(t.unwrap().as_secs()).unwrap()
        };
        let before = clock();
        store.delete_torrents(&[v1]).unwrap();
        let after = clock();
        let rows = journal(&store);
        assert_eq!(
            rows.iter().map(|(h, _)| *h).collect::<Vec<_>>(),
            [v1, [22; 20]]
        );
        for (_, at) in &rows {
            assert!(
                (before..=after).contains(at),
                "{at} not in {before}..={after}"
            );
        }

        stamp(&store, 5);
        insert_torrent(store.connection(), 2, v1, "fetched again");
        store.delete_torrents(&[v1]).unwrap();
        let rows = journal(&store);
        assert!(rows[0].1 >= before, "{rows:?}");
        assert_eq!(rows[1], ([22; 20], 5), "untouched");
    }

    /// With no snapshot left, the journal goes, unless an expired one
    /// couldn't be deleted in full: then it stays for the next pass.
    #[test]
    fn with_no_snapshots_left_the_journal_clears_only_after_a_clean_pass() {
        let dir = tempfile::tempdir().unwrap();
        let (mut store, db) = live(dir.path());
        journal_row(&store, [31; 20], T);
        let r = store.purge_snapshots(T + DAY, 0);
        assert_eq!(r, PurgeReport::default());
        assert_eq!(journal(&store), []);

        journal_row(&store, [32; 20], T);
        let snap = take(&store, &db, 10, T);
        std::fs::create_dir(side(&snap, "-journal")).unwrap();
        let r = store.purge_snapshots(T + RETAIN_SECS, 0);
        assert_eq!(paths(&r.expired), vec![snap.clone()]);
        assert_eq!(r.warnings.len(), 1, "{:?}", r.warnings);
        assert_eq!(journal(&store), [([32; 20], T)], "a failed unlink keeps it");

        std::fs::remove_dir(side(&snap, "-journal")).unwrap();
        let r = store.purge_snapshots(T + RETAIN_SECS + 1, 0);
        assert!(r.warnings.is_empty(), "{:?}", r.warnings);
        assert_eq!(journal(&store), []);
    }

    /// A snapshot of a schema this build can't purge doesn't hold the
    /// journal back, and gets no mark: it's looked at afresh each pass.
    #[test]
    fn an_unknown_schema_neither_holds_the_journal_nor_takes_a_mark() {
        let dir = tempfile::tempdir().unwrap();
        let (mut store, db) = live(dir.path());
        let hash = [41; 20];
        insert_torrent(store.connection(), 1, hash, "deleted-marker-3b1e");
        let known = take(&store, &db, 12, T);
        let unknown = crate::schema::snapshot_path(&db, 100);
        let conn = fixture(&unknown, 11);
        conn.execute(
            "INSERT INTO schema_migrations (version, applied_at) VALUES (99, 0)",
            [],
        )
        .unwrap();
        insert_torrent(&conn, 1, hash, "deleted-marker-3b1e");
        drop(conn);
        set_modified(&unknown, T).unwrap();
        store.delete_torrents(&[hash]).unwrap();
        stamp(&store, T + DAY);

        let r = store.purge_snapshots(T + 2 * DAY, 0);
        assert_eq!(r.torrents, 1);
        assert_eq!(r.warnings.len(), 1, "{:?}", r.warnings);
        assert!(r.warnings[0].contains("schema v99"), "{:?}", r.warnings);
        assert!(!contains(&known, b"deleted-marker-3b1e"));
        assert!(contains(&unknown, b"deleted-marker-3b1e"));
        assert_eq!(journal(&store), []);
        assert!(store.snapshot_marks.0.contains_key(&known));
        assert!(!store.snapshot_marks.0.contains_key(&unknown));
    }

    /// A row is looked up only in snapshots taken up to a day after it:
    /// a hash deleted and fetched again stays in snapshots taken since.
    /// A snapshot dated ahead of the clock is redated to now and matched
    /// against every row, then expires 90 days after it was first seen.
    #[test]
    fn a_deletion_matches_only_snapshots_taken_up_to_a_day_after_it() {
        let dir = tempfile::tempdir().unwrap();
        let (mut store, db) = live(dir.path());
        let hash = [51; 20];
        insert_torrent(store.connection(), 1, hash, "refetched-marker-6d02");
        let before = take(&store, &db, 10, T);
        let edge = take(&store, &db, 10, T + 5 * DAY);
        let after = take(&store, &db, 10, T + 5 * DAY + 1);
        let ahead = take(&store, &db, 10, T + 50 * DAY);
        // Deleted at T + 4 days, then fetched again (live, not in the
        // journal's way).
        store.delete_torrents(&[hash]).unwrap();
        stamp(&store, T + 4 * DAY);
        insert_torrent(store.connection(), 2, hash, "refetched-marker-6d02");

        let r = store.purge_snapshots(T + 6 * DAY, 0);
        assert!(r.warnings.is_empty(), "{:?}", r.warnings);
        assert_eq!(r.purged, vec![before.clone(), edge.clone(), ahead.clone()]);
        for p in [&before, &edge, &ahead] {
            assert!(!contains(p, b"refetched-marker-6d02"), "{}", p.display());
        }
        assert!(contains(&after, b"refetched-marker-6d02"));
        assert_eq!(modified(&ahead), Some(T + 6 * DAY));
        let r = store.purge_snapshots(T + 6 * DAY + RETAIN_SECS, 0);
        assert_eq!(paths(&r.expired), vec![before, edge, after, ahead]);
    }

    /// Rows past the window go on every pass, even one that keeps the
    /// rest because a snapshot couldn't be purged.
    #[test]
    fn rows_older_than_the_window_go_on_every_pass() {
        let dir = tempfile::tempdir().unwrap();
        let (mut store, db) = live(dir.path());
        let snap = take(&store, &db, 10, T);
        let now = T + DAY;
        journal_row(&store, [61; 20], now - JOURNAL_SECS - 1);
        journal_row(&store, [62; 20], now - JOURNAL_SECS);
        journal_row(&store, [63; 20], T);
        let locked = Connection::open(&snap).unwrap();
        locked.execute_batch("BEGIN EXCLUSIVE").unwrap();
        let r = store.purge_snapshots(now, 0);
        assert_eq!(r.warnings.len(), 1, "{:?}", r.warnings);
        assert_eq!(
            journal(&store),
            [([62; 20], now - JOURNAL_SECS), ([63; 20], T)]
        );
        locked.execute_batch("ROLLBACK").unwrap();
    }

    /// A snapshot whose time can't be read (here, before 1970) can't be
    /// shown to be inside the window: it's deleted, with a warning, and
    /// doesn't hold the journal back.
    #[test]
    fn a_snapshot_with_an_unreadable_time_is_deleted() {
        let dir = tempfile::tempdir().unwrap();
        let (mut store, db) = live(dir.path());
        let snap = take(&store, &db, 10, T);
        std::fs::OpenOptions::new()
            .write(true)
            .open(&snap)
            .unwrap()
            .set_modified(UNIX_EPOCH - Duration::from_secs(86_400))
            .unwrap();
        assert_eq!(list(&db).unwrap()[0].taken_at, None);
        journal_row(&store, [71; 20], T);
        let r = store.purge_snapshots(T + DAY, 0);
        assert_eq!(paths(&r.expired), vec![snap.clone()]);
        assert_eq!(r.warnings.len(), 1, "{:?}", r.warnings);
        assert!(r.warnings[0].contains("no readable modification time"));
        assert!(!snap.exists());
        assert_eq!(journal(&store), []);
    }

    /// A snapshot whose time is unknown and whose file can't be looked at
    /// (an error other than "not found") is left as it was, with a
    /// warning, and holds back the journal rows it may still hold; a
    /// later pass that can read it purges it and acknowledges them.
    ///
    /// CI runs as root on Linux, so permissions can't make the call fail,
    /// and a symlink (a loop, say) isn't a snapshot to `list`, so it never
    /// reaches the call. The failure is a path past the system's limit
    /// instead: `list` reads the folder and sees a regular file, but
    /// `metadata` on the full path fails with ENAMETOOLONG. No symlink is
    /// crossed on the way (macOS limits a symlink's target plus the rest
    /// of the path too), so the snapshot's folder is built short and
    /// moved into place by renaming it. SQLite can't open a path that
    /// long, so the store is opened in a short folder, and only the first
    /// pass looks for its snapshots in the long one (`purge_snapshots`
    /// uses `path` for nothing else); the fix moves the snapshot, under
    /// the same name, beside the store. Linux and macOS only: Windows'
    /// long-path support reads the file.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn a_snapshot_that_cannot_be_looked_at_holds_the_journal() {
        // The longest path the system takes, without the terminating NUL.
        const MAX: usize = if cfg!(target_os = "macos") {
            1023
        } else {
            4095
        };
        const NAME: &str = "lodestar.pre-v10.db";
        let dir = tempfile::tempdir().unwrap();
        // macOS's temporary folder is under /var, a symlink.
        let base = dir.path().canonicalize().unwrap();
        let (mut store, live_db) = live(&base);
        // A folder whose own path fits, but not with the snapshot's name.
        let target = MAX - 2;
        let mut deep = base.join("far");
        while deep.as_os_str().len() < target - 2 {
            // Each part adds its length and a separator; the last fills
            // the rest exactly.
            let rest = target - 2 - deep.as_os_str().len();
            deep.push("d".repeat(if rest > 250 { 150 } else { rest - 1 }));
        }
        std::fs::create_dir_all(&deep).unwrap();
        let far = deep.join("s");
        assert_eq!(far.as_os_str().len(), target);
        assert!(far.join(NAME).as_os_str().len() > MAX);
        let near = base.join("s");
        std::fs::create_dir(&near).unwrap();

        let hash = [81; 20];
        insert_torrent(store.connection(), 1, hash, "unreadable-marker-9c4f");
        let snap = take(&store, &live_db, 10, T);
        assert_eq!(snap, base.join(NAME));
        std::fs::rename(&snap, near.join(NAME)).unwrap();
        std::fs::rename(&near, &far).unwrap();
        store.delete_torrents(&[hash]).unwrap();
        stamp(&store, T + DAY / 2);
        let held = journal(&store);
        assert_eq!(held.len(), 1);

        store.path = Some(far.join("lodestar.db"));
        let snaps = list(&far.join("lodestar.db")).unwrap();
        assert_eq!(paths(&snaps), vec![far.join(NAME)]);
        assert_eq!(snaps[0].taken_at, None);
        let report = store.purge_snapshots(T + DAY, 0);
        assert_eq!(report.warnings.len(), 1, "{:?}", report.warnings);
        let failed = format!("reading the snapshot {} failed: ", far.join(NAME).display());
        assert!(
            report.warnings[0].starts_with(&failed),
            "{:?}",
            report.warnings
        );
        assert!(
            report.expired.is_empty() && report.purged.is_empty(),
            "{report:?}"
        );
        assert_eq!(journal(&store), held, "not acknowledged");

        std::fs::rename(&far, &near).unwrap();
        assert!(contains(&near.join(NAME), b"unreadable-marker-9c4f"));
        std::fs::rename(near.join(NAME), &snap).unwrap();
        store.path = Some(live_db);
        let report = store.purge_snapshots(T + 2 * DAY, 0);
        assert!(report.warnings.is_empty(), "{:?}", report.warnings);
        assert_eq!(report.purged, vec![snap.clone()]);
        assert_eq!(report.torrents, 1);
        assert!(!contains(&snap, b"unreadable-marker-9c4f"));
        assert_eq!(journal(&store), []);
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
        assert_eq!(paths(&r.expired), vec![old.clone()]);
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

    /// A V13 snapshot loses an import candidate whose hash the live store
    /// tombstoned after it was taken: the trigger journals it, as a
    /// deletion.
    #[test]
    fn a_held_candidate_leaves_v13_snapshots() {
        let dir = tempfile::tempdir().unwrap();
        let (mut store, db) = live(dir.path());
        let marker = "candidate-name-marker-5d2a";
        store
            .connection()
            .execute_batch(&format!(
                "INSERT INTO import_sources (id, adapter, dataset, schema_sig)
                     VALUES (1, 'magnetico', x'01', x'02');
                 INSERT INTO import_jobs (id, source, adapter_ver, contract_ver, phase, control,
                     options, created_at, updated_at) VALUES (1, 1, 1, 1, 2, 0, x'', 0, 0);
                 INSERT INTO import_candidates (hash, job, name, missing, privacy, at)
                     VALUES (x'{}', 1, CAST('{marker}' AS BLOB), 6, 0, 0);",
                "33".repeat(20)
            ))
            .unwrap();
        let snap = take(&store, &db, 13, T);
        store
            .connection()
            .execute(
                "INSERT INTO tombstones (hash, rule, at) VALUES (?1, 'CS-2', ?2)",
                rusqlite::params![&[0x33u8; 20][..], T + DAY],
            )
            .unwrap();
        stamp(&store, T + DAY);

        let r = store.purge_snapshots(T + 2 * DAY, 0);
        assert!(r.warnings.is_empty(), "{:?}", r.warnings);
        assert_eq!(r.candidates, 1, "{r:?}");
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
        assert_eq!(paths(&r.expired), vec![pre, kept.clone()]);
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
        assert_eq!(paths(&r.expired), vec![snap.clone()]);
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

    /// A hold recorded with a time older than the last pass (the clock
    /// stepped back, or the machine slept before committing) is found by
    /// the full pass that runs when the clock went back, and by the daily
    /// one.
    #[test]
    fn full_passes_catch_holds_behind_the_mark() {
        let dir = tempfile::tempdir().unwrap();
        let (mut store, db) = live(dir.path());
        for id in 1..=2u8 {
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
        take(&store, &db, 10, T);
        let now = T + 10 * DAY;
        assert_eq!(store.purge_snapshots(now, 0), PurgeReport::default());

        // Behind the mark: the next incremental pass misses it...
        tomb(&store, 1, now - 2 * RECHECK_MARGIN);
        assert_eq!(store.purge_snapshots(now + 3600, 0).torrents, 0);
        // ...the daily full pass finds it.
        assert_eq!(store.purge_snapshots(now + FULL_EVERY, 0).torrents, 1);

        // The clock went back: a full pass at once.
        tomb(&store, 2, now);
        assert_eq!(store.purge_snapshots(now + FULL_EVERY - 60, 0).torrents, 1);
    }

    /// Restoring a V10 snapshot mustn't bring back API keys, feed tokens,
    /// saved searches or collections deleted live, nor keys revoked live.
    #[test]
    fn deleted_and_revoked_user_data_follows_the_live_store() {
        let dir = tempfile::tempdir().unwrap();
        let (mut store, db) = live(dir.path());
        let c = store.connection();
        for (id, prefix) in [(1, "keyprefixa1"), (2, "keyprefixb2"), (3, "keyprefixc3")] {
            c.execute(
                "INSERT INTO api_keys (id, kind, name, prefix, hash, scopes, created_at)
                 VALUES (?1, 0, ?2, ?2, zeroblob(32), 1, 0)",
                params![id, prefix],
            )
            .unwrap();
        }
        for (id, name) in [(1, "saved-search-gone-marker-9c1f"), (2, "kept-search")] {
            c.execute(
                "INSERT INTO saved_searches (id, name, query, ast, alert, rss, watermark,
                     created_at, updated_at)
                 VALUES (?1, ?2, 'q', '{}', 0, 1, 0, 0, 0)",
                params![id, name],
            )
            .unwrap();
        }
        // A feed token for the search that goes: it goes with it.
        c.execute(
            "INSERT INTO api_keys (id, kind, name, prefix, hash, scopes, saved_search_id,
                 created_at)
             VALUES (4, 1, 'feed', 'feedtoken4', zeroblob(32), 1, 1, 0)",
            [],
        )
        .unwrap();
        c.execute(
            "INSERT INTO collections (id, name, created_at, updated_at)
             VALUES (1, 'collection-gone-marker-4e0b', 0, 0)",
            [],
        )
        .unwrap();
        let snap = take(&store, &db, 11, T);
        c.execute_batch(
            "DELETE FROM api_keys WHERE id = 1;
             UPDATE api_keys SET revoked_at = 77 WHERE id = 2;
             DELETE FROM saved_searches WHERE id = 1;
             DELETE FROM collections WHERE id = 1;",
        )
        .unwrap();

        let r = store.purge_snapshots(T + DAY, 0);
        assert_eq!(r.warnings, Vec::<String>::new());
        // Keys 1 and 4 (the feed token went live with its search), key 2
        // revoked, the search and the collection.
        assert_eq!(r.user_data, 5, "{r:?}");
        assert!(!contains(&snap, b"keyprefixa1"));
        assert!(!contains(&snap, b"feedtoken4"));
        assert!(!contains(&snap, b"saved-search-gone-marker-9c1f"));
        assert!(!contains(&snap, b"collection-gone-marker-4e0b"));
        let conn = Connection::open(&snap).unwrap();
        let keys: Vec<(i64, Option<i64>)> = conn
            .prepare("SELECT id, revoked_at FROM api_keys ORDER BY id")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(keys, vec![(2, Some(77)), (3, None)]);
        drop(conn);
        quick_check(&snap);
        assert_eq!(store.purge_snapshots(T + DAY + 1, 0).user_data, 0);
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
