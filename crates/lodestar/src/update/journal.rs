//! The rollback journal (§12.2), `<data>/bin/rollback.json`. Each step is
//! idempotent, and the journal is updated and fsynced after each, so a
//! crash at any point is finished at the next start, before anything runs:
//!
//! 1. (the supervisor) stop the child: SIGTERM, 30 s, SIGKILL, wait;
//! 2. write the journal, with the snapshot chosen only when
//!    `<failed>/migration.json` exists and the newest `pre-v<from+1>`
//!    snapshot verifies (settled, `quick_check`, schema = `from`); its
//!    SHA-256 is recorded before step 4;
//! 3. append the outcome to `history.json` (keyed by the journal);
//! 4. restore the store, only with a chosen snapshot: delete `-wal`,
//!    `-shm` and `-journal`, rename the snapshot over `lodestar.db`, fsync
//!    the folder. On a repeat, a missing snapshot whose SHA-256 matches the
//!    store counts as done;
//! 5. remove the `index/` entries not in `pending.index_before`;
//! 6. repoint `active` at the previous version (remove it for the image),
//!    add the failed version to `skipped`, and write `pending` for the
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
use super::pending::{self, Active, Kind, Migration, Pending, Skip};
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

/// Step 2: writes the journal for rolling `failed` back to `previous`.
pub fn begin(
    ctx: &Ctx,
    failed: &str,
    previous: &str,
    reason: &str,
    index_before: Option<&[String]>,
    lost_torrents: Option<u64>,
    now: u64,
) -> io::Result<Journal> {
    let migration = layout::parse_version(failed)
        .and_then(|v| read::<Migration>(&ctx.bin.overlay(&v).join("migration.json")));
    let snapshot = migration.and_then(|m| choose_snapshot(&ctx.db, m.from, &m.before));
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
    ctx.bin.write(FILE, &journal)?;
    super::testing::fault("journal", 2);
    Ok(journal)
}

/// The snapshot a rollback of `failed` would restore, if any (its name),
/// without writing anything.
pub fn preview(ctx: &Ctx, failed: &str) -> Option<String> {
    let m: Migration = layout::parse_version(failed)
        .and_then(|v| read(&ctx.bin.overlay(&v).join("migration.json")))?;
    choose_snapshot(&ctx.db, m.from, &m.before).map(|s| s.name)
}

/// The journal in place, if any.
pub fn current(bin: &Bin) -> io::Result<Option<Journal>> {
    bin.read(FILE)
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
    if fs::symlink_metadata(&path).is_err() {
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
    let mut skipped: Vec<Skip> = ctx.bin.read("skipped").ok().flatten().unwrap_or_default();
    if !skipped.iter().any(|s| s.version == j.failed) {
        skipped.push(Skip {
            version: j.failed.clone(),
            reason: "failed".into(),
        });
        ctx.bin.write("skipped", &skipped)?;
    }
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

fn read<T: serde::de::DeserializeOwned>(path: &Path) -> Option<T> {
    layout::read_json(path).ok().flatten()
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
    fn no_snapshot_without_a_migration_marker_or_a_matching_schema() {
        let s = setup();
        let v = Version::parse("0.9.0").unwrap();
        fs::remove_file(s.ctx.bin.overlay(&v).join("migration.json")).unwrap();
        assert!(begin_it(&s).snapshot.is_none());
        let s = setup();
        let m = Migration {
            from: 1,
            to: 2,
            started_at: 1,
            before: vec![],
        };
        fs::write(
            s.ctx.bin.overlay(&v).join("migration.json"),
            serde_json::to_vec(&m).unwrap(),
        )
        .unwrap();
        assert!(begin_it(&s).snapshot.is_none());
    }
}
