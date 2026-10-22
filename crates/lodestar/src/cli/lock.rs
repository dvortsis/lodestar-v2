//! Exclusive use of the store for the offline commands (`rollback`,
//! `vacuum`, `reindex`), taken through SQLite's own file locks so that it
//! sees every process with the store open, the server's read-only
//! connections included, on every platform.
//!
//! In WAL mode each open connection keeps a shared lock on the database
//! file. Leaving WAL needs an exclusive one, so `journal_mode = DELETE`
//! fails at once with "database is locked" while anyone else has the
//! store open. Set beforehand, `locking_mode = EXCLUSIVE` keeps that lock
//! until the connection closes: a server started meanwhile can't open the
//! store (its busy timeout runs out) rather than racing the command.
//! Leaving WAL also checkpoints it, so a WAL left by a crash is folded in,
//! not lost.

use std::path::{Path, PathBuf};
use std::time::Duration;

use lodestar_store::rusqlite::{Connection, ErrorCode, OpenFlags};

use super::util::{Failure, Outcome, failed};

/// How long `lodestar_store` waits on a busy store: the longest a process
/// that opened the store while it was locked can still be waiting.
#[cfg_attr(not(unix), allow(dead_code))]
pub const STORE_BUSY_TIMEOUT: Duration = Duration::from_secs(5);

/// The store, held exclusively until dropped or [released](Self::release).
#[derive(Debug)]
pub struct StoreLock {
    /// `None` once closed.
    conn: Option<Connection>,
    db: PathBuf,
}

impl StoreLock {
    /// Locks the store at `db`, or refuses while any other process has it
    /// open.
    pub fn acquire(db: &Path) -> Outcome<StoreLock> {
        if !db.is_file() {
            return Err(Failure::Failed(format!("no store at {}", db.display())));
        }
        let conn = Connection::open_with_flags(
            db,
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(failed(format!("opening {}", db.display())))?;
        conn.busy_timeout(Duration::ZERO)
            .map_err(failed("setting the busy timeout"))?;
        let in_use = |e: &lodestar_store::rusqlite::Error| {
            matches!(
                e.sqlite_error_code(),
                Some(ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked)
            )
        };
        let refuse = || {
            Failure::Refused(format!(
                "the store {} is open in another process. Stop Lodestar (and any other \
                 lodestar command) first.",
                db.display()
            ))
        };
        let take = || -> lodestar_store::rusqlite::Result<String> {
            conn.pragma_update(None, "locking_mode", "EXCLUSIVE")?;
            let mode: String =
                conn.pragma_update_and_check(None, "journal_mode", "DELETE", |r| r.get(0))?;
            // A write takes the exclusive lock, which the locking mode keeps.
            conn.execute_batch("BEGIN EXCLUSIVE; COMMIT;")?;
            Ok(mode)
        };
        match take() {
            Ok(mode) if mode.eq_ignore_ascii_case("delete") => Ok(StoreLock {
                conn: Some(conn),
                db: db.to_path_buf(),
            }),
            Ok(_) => Err(refuse()),
            Err(e) if in_use(&e) => Err(refuse()),
            Err(e) => Err(Failure::Failed(format!("locking {}: {e}", db.display()))),
        }
    }

    pub fn conn(&self) -> &Connection {
        self.conn.as_ref().expect("open until consumed")
    }

    /// Puts the store back in WAL mode and unlocks it. Dropping the lock
    /// does the same, ignoring errors.
    pub fn release(mut self) -> Outcome {
        let conn = self.conn.take().expect("open until consumed");
        conn.pragma_update(None, "journal_mode", "WAL")
            .map_err(failed(format!(
                "restoring WAL mode on {}",
                self.db.display()
            )))?;
        conn.close()
            .map_err(|(_, e)| Failure::Failed(format!("closing {}: {e}", self.db.display())))
    }

    /// Replaces the store with `new_file` (a complete, checked database in
    /// the same directory) by a rename, so a crash leaves one or the other
    /// whole, and unlocks.
    ///
    /// The stale `-wal`, `-shm` and `-journal` go first, while the lock
    /// still keeps everyone out, so none is ever applied to the new file.
    /// On Unix the rename happens under the lock, which then stays held
    /// for longer than [`STORE_BUSY_TIMEOUT`]: a process that opened the
    /// old file meanwhile gives up instead of carrying on with the
    /// unlinked file. Windows can't rename over an open file, so the lock
    /// is closed first; a process that opens the store in that moment
    /// makes the rename fail, and the store is left as it was.
    ///
    /// The connection is closed as it is: switching it back to WAL after
    /// the rename would create a `-wal` beside the new file.
    pub fn replace_with(mut self, new_file: &Path) -> Outcome {
        for side in super::util::side_files(&self.db) {
            super::util::remove_if_present(&side)
                .map_err(failed(format!("removing {}", side.display())))?;
        }
        let db = self.db.clone();
        let rename = || {
            std::fs::rename(new_file, &db).map_err(failed(format!(
                "replacing {} with {} (the store is unchanged)",
                db.display(),
                new_file.display()
            )))
        };
        #[cfg(unix)]
        {
            rename()?;
            if let Some(dir) = db.parent() {
                super::util::sync_dir(dir);
            }
            std::thread::sleep(STORE_BUSY_TIMEOUT + Duration::from_secs(1));
            self.close_as_is();
        }
        #[cfg(not(unix))]
        {
            self.close_as_is();
            rename()?;
        }
        Ok(())
    }

    fn close_as_is(&mut self) {
        if let Some(conn) = self.conn.take() {
            let _ = conn.close();
        }
    }
}

impl Drop for StoreLock {
    fn drop(&mut self) {
        if let Some(conn) = self.conn.take() {
            let _ = conn.pragma_update(None, "journal_mode", "WAL");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lodestar_store::{Store, StoreConfig};

    #[test]
    fn excludes_open_stores_and_blocks_new_ones() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("lodestar.db");
        let store = Store::open(&db, StoreConfig::default()).unwrap();
        // A writer (and a reader) beside it: refused, and nothing changed.
        let reader = Store::open_reader(&db, StoreConfig::default()).unwrap();
        assert!(matches!(StoreLock::acquire(&db), Err(Failure::Refused(_))));
        drop(store);
        assert!(matches!(StoreLock::acquire(&db), Err(Failure::Refused(_))));
        drop(reader);

        let lock = StoreLock::acquire(&db).unwrap();
        let mode: String = lock
            .conn()
            .query_row("PRAGMA journal_mode", [], |r| r.get(0))
            .unwrap();
        assert_eq!(mode, "delete");
        // Held: nobody else gets in, not even a reader.
        let other = Connection::open(&db).unwrap();
        assert!(
            other
                .query_row("SELECT count(*) FROM torrents", [], |r| r.get::<_, i64>(0))
                .is_err()
        );
        assert!(matches!(StoreLock::acquire(&db), Err(Failure::Refused(_))));
        drop(other);
        lock.release().unwrap();

        // Released: the store opens normally, in WAL mode again.
        let store = Store::open(&db, StoreConfig::default()).unwrap();
        let mode: String = store
            .connection()
            .query_row("PRAGMA journal_mode", [], |r| r.get(0))
            .unwrap();
        assert_eq!(mode, "wal");
    }

    #[test]
    fn a_crash_left_wal_is_folded_in() {
        let dir = tempfile::tempdir().unwrap();
        let live = dir.path().join("live.db");
        let store = Store::open(&live, StoreConfig::default()).unwrap();
        store
            .connection()
            .execute(
                "INSERT INTO settings (key, value) VALUES ('cli.test', 7)",
                [],
            )
            .unwrap();
        // Copy the file and its WAL while it's open: a crash's leftovers.
        let crashed = dir.path().join("crashed.db");
        std::fs::copy(&live, &crashed).unwrap();
        let mut wal = crashed.as_os_str().to_os_string();
        wal.push("-wal");
        std::fs::copy(dir.path().join("live.db-wal"), &wal).unwrap();
        drop(store);

        let lock = StoreLock::acquire(&crashed).unwrap();
        let v: i64 = lock
            .conn()
            .query_row(
                "SELECT value FROM settings WHERE key = 'cli.test'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(v, 7);
        lock.release().unwrap();
    }

    #[test]
    fn a_missing_store_fails() {
        let dir = tempfile::tempdir().unwrap();
        assert!(matches!(
            StoreLock::acquire(&dir.path().join("nope.db")),
            Err(Failure::Failed(_))
        ));
        assert!(!dir.path().join("nope.db").exists());
    }
}
