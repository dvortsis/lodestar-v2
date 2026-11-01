//! The rollback journal (§12.2), `<data>/bin/rollback.json`. Each step is
//! idempotent, and the journal is updated and fsynced after each, so a
//! crash at any point is finished at the next start, before anything runs:
//!
//! 1. (the supervisor) stop the child: SIGTERM, 30 s, SIGKILL, wait;
//! 2. write the journal, with the snapshot chosen only when
//!    `<failed>/migration.json` exists and the newest `pre-v<from+1>`
//!    snapshot it took verifies (settled, `quick_check`, schema = `from`);
//!    its SHA-256 is recorded before step 4. With the marker but no such
//!    snapshot, the rollback is refused unless the store is still at
//!    `from`: the previous version couldn't read it;
//! 3. append the outcome to `history.json` (keyed by the journal);
//! 4. restore the store, only with a chosen snapshot: delete `-wal`,
//!    `-shm` and `-journal`, rename the snapshot over `lodestar.db`, fsync
//!    the folder. On a repeat, a missing snapshot whose SHA-256 matches the
//!    store counts as done;
//! 5. remove the `index/` entries not in `pending.index_before`;
//! 6. repoint `active` at the previous version (remove it for the image),
//!    add the failed version to `skipped` (`failed`, or `manual`; an
//!    unreadable `skipped` stops here), and write `pending` for the
//!    previous version (`kind: restored`), so its probation survives a
//!    crash before step 8;
//! 7. delete the failed overlay, then the journal;
//! 8. (the supervisor) start the previous version.
//!
//! While the journal exists, nothing opens the store: every store opener
//! refuses. A stop signal finishes the current step only and leaves the
//! journal.

use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use lodestar_store::snapshots::{self, Kind as SnapKind};
use lodestar_update::Sha256;
use serde::{Deserialize, Serialize};
use sha2::Digest as _;

use super::history::{self, Entry, Outcome};
use super::layout::{self, Bin};
use super::pending::{self, Active, Kind, Migration, Pending};
use crate::cli::util;

pub const FILE: &str = "rollback.json";
const FORMAT: u32 = 1;
/// The last step the journal itself runs.
const LAST: u8 = 7;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Journal {
    pub format: u32,
    /// When it was begun (Unix seconds); keys its history entry.
    pub at: u64,
    pub failed: String,
    /// A version, or `"image"`.
    pub previous: String,
    /// `failed`, `manual`, …
    pub reason: String,
    pub snapshot: Option<SnapshotRef>,
    pub remove_index: Vec<String>,
    pub lost_torrents: Option<u64>,
    /// The version the image runs (for `"image"` in `pending`).
    pub image_version: String,
    pub steps_done: u8,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SnapshotRef {
    /// A file name beside the store, never a path.
    pub name: String,
    pub sha256: String,
}

/// Where the journal works.
#[derive(Debug, Clone)]
pub struct Ctx {
    pub bin: Bin,
    pub db: PathBuf,
    pub index_root: PathBuf,
    /// A stop signal arrived: finish the current step, then stop.
    pub stop: Arc<AtomicBool>,
    /// The version the image runs (for `"image"` in `pending`).
    pub image_version: String,
}

/// Why a journal didn't finish. It stays in place either way.
#[derive(Debug, PartialEq, Eq)]
pub enum Unfinished {
    /// A stop signal: the current step finished, the rest waits.
    Stopped,
    /// A step failed; nothing more runs until it's fixed.
    Failed(String),
}

/// Why a rollback wasn't begun.
#[derive(Debug)]
pub enum NotBegun {
    /// It would strand the store: nothing was written, `active` stands.
    Refused(String),
    /// The journal couldn't be written.
    Io(io::Error),
}

impl std::fmt::Display for NotBegun {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            NotBegun::Refused(why) => f.write_str(why),
            NotBegun::Io(e) => write!(f, "writing the rollback journal: {e}"),
        }
    }
}

/// Step 2: writes the journal for rolling `failed` back to `previous`.
/// Refused when `failed` migrated the store and no snapshot from before
/// it verifies (see [`restore_plan`]).
pub fn begin(
    ctx: &Ctx,
    failed: &str,
    previous: &str,
    reason: &str,
    index_before: Option<&[String]>,
    lost_torrents: Option<u64>,
    now: u64,
) -> Result<Journal, NotBegun> {
    let snapshot = restore_plan(ctx, failed, previous).map_err(NotBegun::Refused)?;
    // Nothing is restored without a snapshot, so nothing stored is lost.
    let lost_torrents = if snapshot.is_some() {
        lost_torrents
    } else {
        Some(0)
    };
    let remove_index = match index_before {
        Some(before) => list_index(&ctx.index_root)
            .into_iter()
            .filter(|n| !before.contains(n))
            .collect(),
        None => Vec::new(),
    };
    let journal = Journal {
        format: FORMAT,
        at: now,
        failed: failed.to_string(),
        previous: previous.to_string(),
        reason: reason.to_string(),
        snapshot,
        remove_index,
        lost_torrents,
        image_version: ctx.image_version.clone(),
        steps_done: 2,
    };
    ctx.bin.write(FILE, &journal).map_err(NotBegun::Io)?;
    super::testing::fault("journal", 2);
    Ok(journal)
}

/// The snapshot a rollback of `failed` would restore (its name), or why
/// it's refused, without writing anything.
pub fn preview(ctx: &Ctx, failed: &str, previous: &str) -> Result<Option<String>, String> {
    restore_plan(ctx, failed, previous).map(|s| s.map(|s| s.name))
}

/// Whether a rollback of `failed` looks possible now, cheaply and without
/// writing anything (Roll back in the API offers it on this): `failed`
/// didn't migrate the store, or a `pre-v<from+1>` snapshot of its
/// migration is still there, or the store is still at `from`. The
/// snapshot is verified only when the rollback begins (step 2, in the
/// supervisor), which can still refuse it.
pub fn kept(ctx: &Ctx, failed: &str) -> bool {
    let Some(v) = layout::parse_version(failed) else {
        return true;
    };
    let m: Migration = match layout::read_json(&ctx.bin.overlay(&v).join("migration.json")) {
        Ok(None) => return true,
        Ok(Some(m)) => m,
        Err(_) => return false,
    };
    let listed = snapshots::list(&ctx.db).is_ok_and(|all| {
        all.iter().any(|s| {
            !m.before.contains(&s.name)
                && matches!(s.kind, SnapKind::PreMigration { version, .. } if version == m.from + 1)
        })
    });
    if listed {
        return true;
    }
    let schema = store_schema(&ctx.db);
    matches!(schema, Ok(None)) || schema == Ok(Some(m.from))
}

/// What a rollback of `failed` restores. Without `<failed>/migration.json`
/// it never touched the schema: nothing. With it, the snapshot this
/// migration took, if it verifies; else nothing only while the store is
/// still at the schema it migrated from (no migration step committed).
/// Otherwise the rollback is refused: `previous` couldn't read the store.
///
/// Back to the image (this build), whatever the marker says, the store it
/// starts with must be at a schema this build reads: that catches a
/// migration nothing recorded.
fn restore_plan(ctx: &Ctx, failed: &str, previous: &str) -> Result<Option<SnapshotRef>, String> {
    let plan = marked_plan(ctx, failed, previous)?;
    if previous == pending::IMAGE {
        let latest = lodestar_store::latest_schema_version();
        let after = match &plan {
            Some(_) => marked_from(ctx, failed),
            None => store_schema(&ctx.db).map_err(|e| {
                format!(
                    "the store's schema can't be read ({e}), so it can't be checked against \
                     this version's. Nothing was changed"
                )
            })?,
        };
        if let Some(after) = after.filter(|a| *a > latest) {
            return Err(format!(
                "the store would be at schema {after}, newer than this version reads \
                 ({latest}), so {previous} couldn't read it. Nothing was changed: restore a \
                 backup first (see \"If an update fails\" in docs/operations.md)"
            ));
        }
    }
    Ok(plan)
}

/// The `from` in `failed`'s migration marker (the schema a restored
/// snapshot holds).
fn marked_from(ctx: &Ctx, failed: &str) -> Option<i64> {
    let v = layout::parse_version(failed)?;
    let m: Migration = layout::read_json(&ctx.bin.overlay(&v).join("migration.json"))
        .ok()
        .flatten()?;
    Some(m.from)
}

/// [`restore_plan`] from `<failed>/migration.json` alone.
fn marked_plan(ctx: &Ctx, failed: &str, previous: &str) -> Result<Option<SnapshotRef>, String> {
    let Some(v) = layout::parse_version(failed) else {
        return Ok(None);
    };
    let marker = ctx.bin.overlay(&v).join("migration.json");
    let m: Migration = match layout::read_json(&marker) {
        Ok(None) => return Ok(None),
        Ok(Some(m)) => m,
        Err(e) => {
            return Err(format!(
                "{failed} began upgrading the store, and its record of that can't be read \
                 ({e}). Rolling back to {previous} could leave it a store it can't read, so \
                 nothing was changed"
            ));
        }
    };
    if let Some(snap) = choose_snapshot(&ctx.db, m.from, &m.before) {
        return Ok(Some(snap));
    }
    match store_schema(&ctx.db) {
        Ok(None) => Ok(None),
        Ok(Some(now)) if now == m.from => Ok(None),
        Ok(Some(now)) => Err(format!(
            "{failed} upgraded the store from schema {} to {now}, and no snapshot from \
             before the upgrade verifies, so {previous} couldn't read it. Nothing was \
             changed: restore a backup first (see \"If an update fails\" in \
             docs/operations.md)",
            m.from
        )),
        Err(e) => Err(format!(
            "{failed} began upgrading the store, no snapshot from before it verifies, and \
             the store's schema can't be read ({e}). Nothing was changed"
        )),
    }
}

/// The store's schema version, read-only; `None` when there's no store.
fn store_schema(db: &Path) -> Result<Option<i64>, String> {
    match fs::symlink_metadata(db) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.to_string()),
        Ok(_) => {}
    }
    let conn = util::open_read_only(db).map_err(|e| e.to_string())?;
    util::schema_version(&conn)
        .map(Some)
        .map_err(|e| e.to_string())
}

/// The journal in place, if any. One in a format this build doesn't
/// know, or at a step it never writes, can't be read.
pub fn current(bin: &Bin) -> io::Result<Option<Journal>> {
    let Some(j) = bin.read::<Journal>(FILE)? else {
        return Ok(None);
    };
    if j.format != FORMAT || !(2..LAST).contains(&j.steps_done) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "{FILE} has format {} at step {}, which this build can't finish",
                j.format, j.steps_done
            ),
        ));
    }
    Ok(Some(j))
}

/// Runs the remaining steps (3 to 7). On success the journal is gone and
/// the previous version is ready to start.
pub fn finish(ctx: &Ctx, mut journal: Journal) -> Result<(), Unfinished> {
    while journal.steps_done < LAST {
        if ctx.stop.load(Ordering::SeqCst) {
            return Err(Unfinished::Stopped);
        }
        step(ctx, &mut journal)?;
    }
    Ok(())
}

/// Runs the next step and records it in the journal.
fn step(ctx: &Ctx, journal: &mut Journal) -> Result<(), Unfinished> {
    let failed = |what: &str, e: &dyn std::fmt::Display| Unfinished::Failed(format!("{what}: {e}"));
    let step = journal.steps_done + 1;
    super::testing::slow("journal");
    match step {
        3 => history::append(
            &ctx.bin,
            Entry {
                version: journal.failed.clone(),
                from: journal.previous.clone(),
                level: None,
                at: journal.at,
                result: Outcome::RolledBack,
                reason: Some(journal.reason.clone()),
                lost_torrents: journal.lost_torrents,
                journal: Some(journal.at),
            },
        )
        .map_err(|e| failed("recording the rollback in history.json", &e))?,
        4 => restore(&ctx.db, journal.snapshot.as_ref()).map_err(Unfinished::Failed)?,
        5 => {
            for name in &journal.remove_index {
                remove_index_entry(&ctx.index_root, name)
                    .map_err(|e| failed(&format!("removing index/{name}"), &e))?;
            }
        }
        6 => repoint(ctx, journal).map_err(|e| failed("repointing the active version", &e))?,
        _ => {
            if let Some(v) = layout::parse_version(&journal.failed) {
                ctx.bin
                    .remove_overlay(&v)
                    .map_err(|e| failed("deleting the failed version", &e))?;
            }
            ctx.bin
                .remove(FILE)
                .map_err(|e| failed("removing the journal", &e))?;
            journal.steps_done = LAST;
            super::testing::fault("journal", LAST);
            return Ok(());
        }
    }
    journal.steps_done = step;
    ctx.bin
        .write(FILE, &*journal)
        .map_err(|e| failed("updating the journal", &e))?;
    super::testing::fault("journal", step);
    Ok(())
}

/// The newest `pre-v<from+1>` snapshot this migration took (none named in
/// `before`), if it verifies as the store at `from`: its name and SHA-256.
fn choose_snapshot(db: &Path, from: i64, before: &[String]) -> Option<SnapshotRef> {
    let all = snapshots::list(db).ok()?;
    let snap = all
        .into_iter()
        .filter(|s| !before.contains(&s.name))
        .filter(|s| matches!(s.kind, SnapKind::PreMigration { version, .. } if version == from + 1))
        .max_by_key(|s| s.kind)?;
    snapshots::settle(&snap.path).ok()?;
    let conn = util::open_read_only(&snap.path).ok()?;
    util::quick_check(&conn).ok()?;
    if util::schema_version(&conn).ok()? != from {
        return None;
    }
    drop(conn);
    let sha256 = sha256_file(&snap.path).ok()?.to_hex();
    Some(SnapshotRef {
        name: snap.name,
        sha256,
    })
}

/// Step 4. Renames, never copies, so it needs no free space.
fn restore(db: &Path, snapshot: Option<&SnapshotRef>) -> Result<(), String> {
    let Some(snap) = snapshot else {
        return Ok(());
    };
    let dir = db.parent().unwrap_or(Path::new("."));
    let stem = db.file_stem().and_then(|s| s.to_str()).unwrap_or("");
    if !matches!(
        snapshots::parse_name(stem, &snap.name),
        Some(SnapKind::PreMigration { .. })
    ) {
        return Err(format!(
            "the journal names an invalid snapshot ({})",
            snap.name
        ));
    }
    let path = dir.join(&snap.name);
    let meta = fs::symlink_metadata(&path);
    if meta.as_ref().is_ok_and(|m| !m.is_file()) {
        return Err(format!(
            "{} isn't a regular file; restore a backup by hand",
            path.display()
        ));
    }
    if meta.is_err() {
        // A repeat: the snapshot was already renamed over the store.
        return match sha256_file(db) {
            Ok(h) if h.to_hex() == snap.sha256 => Ok(()),
            _ => Err(format!(
                "the snapshot {} is gone and {} doesn't match it; restore a backup by hand",
                path.display(),
                db.display()
            )),
        };
    }
    // The WAL first: no process holds the store, and a stale WAL must
    // never be applied to the restored file.
    for side in util::side_files(db) {
        util::remove_if_present(&side).map_err(|e| format!("removing {}: {e}", side.display()))?;
    }
    fs::rename(&path, db).map_err(|e| {
        format!(
            "restoring {} over {}: {e}. The snapshot is still at {}",
            path.display(),
            db.display(),
            path.display()
        )
    })?;
    layout::sync_dir(dir);
    // Whatever was renamed (a swap after the check), the store must be a
    // regular file now; the journal stays until it is.
    if !fs::symlink_metadata(db).is_ok_and(|m| m.is_file()) {
        return Err(format!(
            "{} isn't a regular file after the restore; restore a backup by hand",
            db.display()
        ));
    }
    Ok(())
}

/// Step 6.
fn repoint(ctx: &Ctx, j: &Journal) -> io::Result<()> {
    let restored = if j.previous == pending::IMAGE {
        ctx.bin.remove("active")?;
        j.image_version.clone()
    } else {
        ctx.bin.write(
            "active",
            &Active {
                version: j.previous.clone(),
                previous: pending::IMAGE.to_string(),
            },
        )?;
        j.previous.clone()
    };
    // The failed version's heartbeat and baseline don't carry over.
    ctx.bin.remove("probation.json")?;
    let reason = if j.reason == "failed" {
        "failed"
    } else {
        "manual"
    };
    pending::skip(&ctx.bin, &j.failed, reason)?;
    ctx.bin.write(
        "pending",
        &Pending {
            format: pending::PENDING_FORMAT,
            version: restored,
            previous: j.failed.clone(),
            kind: Kind::Restored,
            attempts: 0,
            used_ms: 0,
            index_before: list_index(&ctx.index_root),
        },
    )
}

/// `index/`'s entries (folder names).
pub fn list_index(root: &Path) -> Vec<String> {
    let mut out: Vec<String> = fs::read_dir(root)
        .map(|r| {
            r.filter_map(Result::ok)
                .filter_map(|e| e.file_name().to_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();
    out.sort();
    out
}

fn remove_index_entry(root: &Path, name: &str) -> io::Result<()> {
    if name.is_empty() || name.contains('/') || name == "." || name == ".." {
        return Ok(());
    }
    let path = root.join(name);
    let removed = match fs::symlink_metadata(&path) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
        Ok(m) if m.is_dir() => fs::remove_dir_all(&path),
        Ok(_) => fs::remove_file(&path),
    };
    removed.or_else(|e| {
        if e.kind() == io::ErrorKind::NotFound {
            Ok(())
        } else {
            Err(e)
        }
    })
}

fn sha256_file(path: &Path) -> io::Result<Sha256> {
    let mut f = layout::open_nofollow(path)?;
    let mut h = sha2::Sha256::new();
    let mut buf = vec![0u8; 1 << 16];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            return Ok(Sha256(h.finalize().into()));
        }
        h.update(&buf[..n]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::update::pending::Skip;
    use lodestar_store::{Store, StoreConfig};

    struct Setup {
        _dir: tempfile::TempDir,
        ctx: Ctx,
    }

    fn marker(db: &Path) -> String {
        let conn = lodestar_store::rusqlite::Connection::open(db).unwrap();
        conn.query_row("SELECT value FROM settings WHERE key = 'marker'", [], |r| {
            r.get(0)
        })
        .unwrap()
    }

    fn set_marker(store: &Store, v: &str) {
        store
            .connection()
            .execute(
                "INSERT INTO settings (key, value) VALUES ('marker', ?1)
                 ON CONFLICT (key) DO UPDATE SET value = excluded.value",
                [v],
            )
            .unwrap();
    }

    /// A store at its latest schema whose "migration" by 0.9.0 left a
    /// `pre-v<L+1>` snapshot holding marker `first`; the store now says
    /// `second`, with a WAL beside it. 0.9.0 is active, the image before.
    fn setup() -> Setup {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("lodestar.db");
        let store = Store::open(&db, StoreConfig::default()).unwrap();
        set_marker(&store, "first");
        let latest = lodestar_store::latest_schema_version();
        let snap = dir.path().join(format!("lodestar.pre-v{}.db", latest + 1));
        store
            .connection()
            .execute("VACUUM INTO ?1", [snap.to_str().unwrap()])
            .unwrap();
        set_marker(&store, "second");
        std::mem::forget(store); // keep the WAL, as a crashed child would
        let bin = Bin::open(dir.path()).unwrap();
        let v = Version::parse("0.9.0").unwrap();
        fs::create_dir(bin.overlay(&v)).unwrap();
        let m = Migration {
            from: latest,
            to: latest + 1,
            started_at: 1,
            before: vec![],
        };
        fs::write(
            bin.overlay(&v).join("migration.json"),
            serde_json::to_vec(&m).unwrap(),
        )
        .unwrap();
        bin.write(
            "active",
            &Active {
                version: "0.9.0".into(),
                previous: "image".into(),
            },
        )
        .unwrap();
        let index = dir.path().join("index");
        fs::create_dir_all(index.join("v1-t1")).unwrap();
        fs::create_dir_all(index.join("v2-t1")).unwrap();
        let ctx = Ctx {
            bin,
            db,
            index_root: index,
            stop: Arc::new(AtomicBool::new(false)),
            image_version: "0.5.0".into(),
        };
        Setup { _dir: dir, ctx }
    }

    use lodestar_update::semver::Version;

    fn begin_it(s: &Setup) -> Journal {
        let before = vec!["v1-t1".to_string()];
        begin(
            &s.ctx,
            "0.9.0",
            "image",
            "failed",
            Some(&before),
            Some(12),
            77,
        )
        .unwrap()
    }

    fn assert_rolled_back(s: &Setup) {
        let c = &s.ctx;
        assert!(current(&c.bin).unwrap().is_none());
        assert_eq!(marker(&c.db), "first");
        for side in util::side_files(&c.db) {
            assert!(!side.exists(), "{}", side.display());
        }
        assert!(!c.bin.overlay(&Version::parse("0.9.0").unwrap()).exists());
        assert!(!c.bin.exists("active"));
        let skipped: Vec<Skip> = c.bin.read("skipped").unwrap().unwrap();
        assert_eq!(
            skipped,
            [Skip {
                version: "0.9.0".into(),
                reason: "failed".into()
            }]
        );
        let p: Pending = c.bin.read("pending").unwrap().unwrap();
        assert_eq!(
            (p.version.as_str(), p.kind, p.attempts),
            ("0.5.0", Kind::Restored, 0)
        );
        let h: history::History = c.bin.read(history::FILE).unwrap().unwrap();
        assert_eq!(h.entries.len(), 1);
        assert_eq!(h.entries[0].lost_torrents, Some(12));
        assert_eq!(list_index(&c.index_root), ["v1-t1"]);
    }

    #[test]
    fn a_crash_after_any_step_finishes_at_the_next_start() {
        for k in 2..=LAST {
            let s = setup();
            let mut j = begin_it(&s);
            assert!(j.snapshot.is_some(), "the snapshot verifies");
            while j.steps_done < k {
                step(&s.ctx, &mut j).unwrap();
            }
            // The crash: whatever the journal on disk says is next.
            if let Some(again) = current(&s.ctx.bin).unwrap() {
                finish(&s.ctx, again).unwrap();
            }
            assert_rolled_back(&s);
        }
    }

    /// §12.2: no rollback can undo a revocation. `trust.json` (the
    /// revocations and the freshness marks) is never part of what a
    /// rollback restores, whichever step it crashes after.
    #[test]
    fn trust_survives_a_rollback() {
        for k in 2..=LAST {
            let s = setup();
            let trust = br#"{"revoked":["zq7revoked"],"channels":{"stable":{"hwm_ts":99}}}"#;
            fs::write(s.ctx.bin.path(super::super::trust_file::FILE), trust).unwrap();
            let mut j = begin_it(&s);
            while j.steps_done < k {
                step(&s.ctx, &mut j).unwrap();
            }
            if let Some(again) = current(&s.ctx.bin).unwrap() {
                finish(&s.ctx, again).unwrap();
            }
            assert_rolled_back(&s);
            assert_eq!(
                fs::read(s.ctx.bin.path(super::super::trust_file::FILE)).unwrap(),
                trust,
                "journal:{k}"
            );
        }
    }

    /// Roll back's cheap check: no marker, or the snapshot still there,
    /// or the store still at `from`; not when the snapshot is gone after
    /// a migration step committed, or the marker can't be read.
    #[test]
    fn kept_needs_the_snapshot_only_after_a_migration() {
        let s = setup();
        assert!(kept(&s.ctx, "0.9.0"));
        assert!(kept(&s.ctx, "0.8.0"), "no marker: nothing to restore");
        let latest = lodestar_store::latest_schema_version();
        let snap = s
            .ctx
            .db
            .with_file_name(format!("lodestar.pre-v{}.db", latest + 1));
        fs::remove_file(&snap).unwrap();
        // The store is still at `from` (the "migration" wrote no step).
        assert!(kept(&s.ctx, "0.9.0"));
        let marker = s
            .ctx
            .bin
            .overlay(&Version::parse("0.9.0").unwrap())
            .join("migration.json");
        let m = Migration {
            from: latest - 1,
            to: latest,
            started_at: 1,
            before: vec![],
        };
        fs::write(&marker, serde_json::to_vec(&m).unwrap()).unwrap();
        assert!(!kept(&s.ctx, "0.9.0"), "migrated, and no snapshot");
        fs::write(&marker, b"{").unwrap();
        assert!(!kept(&s.ctx, "0.9.0"), "an unreadable marker");
    }

    #[test]
    fn a_crash_inside_the_restore_counts_it_done_by_its_hash() {
        let s = setup();
        let mut j = begin_it(&s);
        step(&s.ctx, &mut j).unwrap(); // 3
        // The rename happened; the journal still says 3.
        restore(&s.ctx.db, j.snapshot.as_ref()).unwrap();
        finish(&s.ctx, current(&s.ctx.bin).unwrap().unwrap()).unwrap();
        assert_rolled_back(&s);
    }

    #[test]
    fn a_stop_leaves_the_journal_and_a_failed_restore_keeps_it() {
        let s = setup();
        let j = begin_it(&s);
        s.ctx.stop.store(true, Ordering::SeqCst);
        assert_eq!(finish(&s.ctx, j.clone()), Err(Unfinished::Stopped));
        assert_eq!(current(&s.ctx.bin).unwrap().unwrap().steps_done, 2);
        s.ctx.stop.store(false, Ordering::SeqCst);
        // The snapshot vanished and the store isn't it: nothing proceeds.
        let snap = s.ctx.db.with_file_name(&j.snapshot.as_ref().unwrap().name);
        fs::remove_file(snap).unwrap();
        assert!(matches!(finish(&s.ctx, j), Err(Unfinished::Failed(_))));
        let left = current(&s.ctx.bin).unwrap().unwrap();
        assert_eq!(left.steps_done, 3);
        assert!(s.ctx.bin.exists("active"));
    }

    /// A `pre-v` snapshot that existed before the migration began (an
    /// earlier upgrade's) is never restored, even if it verifies: an
    /// attempt that failed before taking its own leaves the store alone.
    #[test]
    fn a_stale_snapshot_from_before_the_migration_is_never_restored() {
        let s = setup();
        let v = Version::parse("0.9.0").unwrap();
        let latest = lodestar_store::latest_schema_version();
        let m = Migration {
            from: latest,
            to: latest + 1,
            started_at: 1,
            before: vec![format!("lodestar.pre-v{}.db", latest + 1)],
        };
        fs::write(
            s.ctx.bin.overlay(&v).join("migration.json"),
            serde_json::to_vec(&m).unwrap(),
        )
        .unwrap();
        let j = begin_it(&s);
        assert!(j.snapshot.is_none());
        assert_eq!(j.lost_torrents, Some(0), "nothing restored, nothing lost");
        finish(&s.ctx, j).unwrap();
        assert_eq!(marker(&s.ctx.db), "second", "the store is left alone");
    }

    #[test]
    fn no_snapshot_without_a_migration_marker() {
        let s = setup();
        let v = Version::parse("0.9.0").unwrap();
        fs::remove_file(s.ctx.bin.overlay(&v).join("migration.json")).unwrap();
        let j = begin_it(&s);
        assert!(j.snapshot.is_none());
        finish(&s.ctx, j).unwrap();
        assert_eq!(marker(&s.ctx.db), "second", "the store is left alone");
    }

    fn write_marker(s: &Setup, from: i64) {
        let v = Version::parse("0.9.0").unwrap();
        let m = Migration {
            from,
            to: from + 1,
            started_at: 1,
            before: vec![],
        };
        fs::write(
            s.ctx.bin.overlay(&v).join("migration.json"),
            serde_json::to_vec(&m).unwrap(),
        )
        .unwrap();
    }

    fn assert_refused(s: &Setup, r: Result<Journal, NotBegun>) {
        assert!(matches!(r, Err(NotBegun::Refused(_))), "{r:?}");
        assert!(current(&s.ctx.bin).unwrap().is_none(), "no journal");
        assert!(s.ctx.bin.exists("active"), "active stands");
        assert!(preview(&s.ctx, "0.9.0", "image").is_err());
    }

    /// The store moved past what the marker says it migrated from, and no
    /// snapshot verifies: the previous version couldn't read it.
    #[test]
    fn a_migrated_store_without_a_verified_snapshot_is_never_rolled_back() {
        let s = setup();
        write_marker(&s, 1);
        let before = vec!["v1-t1".to_string()];
        assert_refused(
            &s,
            begin(&s.ctx, "0.9.0", "image", "manual", Some(&before), None, 1),
        );
        // A snapshot that doesn't verify (damaged) doesn't count either.
        let s = setup();
        let latest = lodestar_store::latest_schema_version();
        let snap = s
            .ctx
            .db
            .with_file_name(format!("lodestar.pre-v{latest}.db"));
        fs::write(&snap, b"not a database").unwrap();
        write_marker(&s, latest - 1);
        assert_refused(&s, begin(&s.ctx, "0.9.0", "image", "failed", None, None, 1));
        // An unreadable marker refuses too.
        let s = setup();
        let v = Version::parse("0.9.0").unwrap();
        fs::write(s.ctx.bin.overlay(&v).join("migration.json"), b"{").unwrap();
        assert_refused(&s, begin(&s.ctx, "0.9.0", "image", "failed", None, None, 1));
    }

    #[test]
    fn a_journal_in_another_format_or_at_an_unknown_step_is_damage() {
        let s = setup();
        let mut j = begin_it(&s);
        for (format, step) in [
            (2, 2),
            (FORMAT, 0),
            (FORMAT, 1),
            (FORMAT, LAST),
            (FORMAT, 9),
        ] {
            j.format = format;
            j.steps_done = step;
            s.ctx.bin.write(FILE, &j).unwrap();
            let e = current(&s.ctx.bin).unwrap_err();
            assert_eq!(e.kind(), io::ErrorKind::InvalidData, "{format} {step}");
        }
    }

    #[test]
    fn a_snapshot_that_isnt_a_regular_file_is_never_restored() {
        let s = setup();
        let j = begin_it(&s);
        // A symlink to a valid copy: a rename would happily move the link
        // over the store.
        let snap = s.ctx.db.with_file_name(&j.snapshot.as_ref().unwrap().name);
        let elsewhere = s.ctx.db.with_file_name("copy.db");
        fs::rename(&snap, &elsewhere).unwrap();
        std::os::unix::fs::symlink(&elsewhere, &snap).unwrap();
        assert!(matches!(finish(&s.ctx, j), Err(Unfinished::Failed(_))));
        assert_eq!(current(&s.ctx.bin).unwrap().unwrap().steps_done, 3);
        assert!(fs::symlink_metadata(&s.ctx.db).unwrap().is_file());
    }

    #[test]
    fn a_manual_rollback_skips_as_manual_and_an_unreadable_skipped_stops_it() {
        let s = setup();
        let j = begin(&s.ctx, "0.9.0", "image", "manual", None, None, 5).unwrap();
        finish(&s.ctx, j).unwrap();
        let skipped: Vec<Skip> = s.ctx.bin.read("skipped").unwrap().unwrap();
        assert_eq!(skipped[0].reason, "manual");
        let s = setup();
        fs::write(s.ctx.bin.path("skipped"), b"[{").unwrap();
        let j = begin_it(&s);
        assert!(matches!(finish(&s.ctx, j), Err(Unfinished::Failed(_))));
        assert_eq!(current(&s.ctx.bin).unwrap().unwrap().steps_done, 5);
        assert_eq!(fs::read(s.ctx.bin.path("skipped")).unwrap(), b"[{");
    }

    /// A migration nothing recorded (no marker) still can't strand the
    /// image: a store past the schema this build reads is never rolled
    /// back to it. Back to another overlay, it isn't this build's call.
    #[test]
    fn a_store_past_this_builds_schema_is_never_rolled_back_to_the_image() {
        let s = setup();
        let v = Version::parse("0.9.0").unwrap();
        fs::remove_file(s.ctx.bin.overlay(&v).join("migration.json")).unwrap();
        let latest = lodestar_store::latest_schema_version();
        let conn = lodestar_store::rusqlite::Connection::open(&s.ctx.db).unwrap();
        conn.execute(
            "INSERT INTO schema_migrations (version, applied_at) VALUES (?1, 0)",
            [latest + 1],
        )
        .unwrap();
        drop(conn);
        assert_refused(&s, begin(&s.ctx, "0.9.0", "image", "failed", None, None, 1));
        assert!(begin(&s.ctx, "0.9.0", "0.8.0", "failed", None, None, 1).is_ok());
    }
}
