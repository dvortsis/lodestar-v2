//! An existing Lodestar data directory (`lodestar.db` and `index/`),
//! opened strictly read-only for `--data-dir`.
//!
//! - **Nothing in it changes.** The store is opened with SQLite's
//!   `immutable` flag, so no `-wal` or `-shm` is created and nothing is
//!   locked or written. The index is opened only when it's complete and at
//!   this build's schema, so opening creates nothing, and no writer is
//!   taken.
//! - **Not while a server has it.** The server holds an exclusive lock on
//!   `index/.lock` and its index writers' locks. Each that exists is taken
//!   shared here and kept until the run ends, so a busy one refuses the run
//!   and a server started meanwhile refuses to start. A store with a
//!   write-ahead log that isn't folded in is refused as well: `immutable`
//!   would read past it, and it means a server is running or stopped
//!   without closing the store.
//! - **Only this build's schemas.** A store or index at another version is
//!   refused, never migrated.

use std::collections::HashMap;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use lodestar_search::SearchIndex;
use lodestar_search::index_dir;
use lodestar_store::latest_schema_version;
use lodestar_store::rusqlite::{Connection, OpenFlags, OptionalExtension};

/// The store's file in a data directory (`lodestar`'s `Config::db_path`).
const DB: &str = "lodestar.db";
/// The index root, holding one directory per schema version.
const INDEX: &str = "index";

/// The opened directory; its locks are held until it's dropped.
pub struct DataDir {
    pub index: Arc<SearchIndex>,
    conn: Connection,
    /// Torrents in the store.
    pub torrents: u64,
    /// The index's directory name, `v{schema}-t{tokenizer}`.
    pub index_version: String,
    /// A day after the newest discovery: the clock searches run at, so a
    /// run over the same store ranks the same.
    pub now: i64,
    _locks: Vec<File>,
}

impl DataDir {
    pub fn open(dir: &Path) -> Result<DataDir, String> {
        if !dir.is_dir() {
            return Err(format!("{}: not a directory", dir.display()));
        }
        let db = dir.join(DB);
        if !db.is_file() {
            return Err(format!("{}: no store (lodestar.db) here", dir.display()));
        }
        let root = dir.join(INDEX);
        let idx = index_dir(&root);
        let index_version = idx
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();

        let locks = hold_locks(&[
            root.join(".lock"),
            idx.join(".tantivy-writer.lock"),
            idx.join("files").join(".tantivy-writer.lock"),
        ])
        .map_err(|e| format!("{}: {e}", dir.display()))?;

        let wal = side_file(&db, "-wal");
        if std::fs::metadata(&wal).is_ok_and(|m| m.len() > 0) {
            return Err(format!(
                "{}: the store has a write-ahead log that isn't folded in. Lodestar is running, \
                 or it stopped without closing the store: stop it cleanly (or start and stop it \
                 once), then run this again",
                dir.display()
            ));
        }

        let conn = open_immutable(&db)?;
        let found: Option<i64> = conn
            .query_row("SELECT max(version) FROM schema_migrations", [], |r| {
                r.get(0)
            })
            .map_err(|e| format!("{}: not a Lodestar store ({e})", db.display()))?;
        let want = latest_schema_version();
        if found != Some(want) {
            return Err(format!(
                "{}: the store is at schema {}, and this build reads {want}. Score it with the \
                 matching build; this tool never migrates a store",
                db.display(),
                found.map_or("none".to_string(), |v| v.to_string())
            ));
        }

        // The index must be complete at this build's version: opening one
        // that isn't there would create it.
        if !idx.join("meta.json").is_file() || !idx.join("files").join("meta.json").is_file() {
            let others = versions(&root);
            return Err(format!(
                "{}: no complete {index_version} index (this build's schema); found {}. Start \
                 the matching Lodestar version to build it; this tool never builds or upgrades \
                 an index",
                root.display(),
                if others.is_empty() {
                    "none".to_string()
                } else {
                    others.join(", ")
                }
            ));
        }
        let index = SearchIndex::open(&idx).map_err(|e| {
            format!(
                "{}: the index doesn't open as this build's: {e}",
                idx.display()
            )
        })?;

        let count = |sql: &str| -> Result<Option<i64>, String> {
            conn.query_row(sql, [], |r| r.get(0))
                .map_err(|e| format!("{}: {e}", db.display()))
        };
        let torrents = count("SELECT count(*) FROM torrents")?.unwrap_or(0) as u64;
        let newest = count("SELECT max(discovered_at) FROM torrents")?.unwrap_or(0);
        Ok(DataDir {
            index: Arc::new(index),
            conn,
            torrents,
            index_version,
            now: newest + 86_400,
            _locks: locks,
        })
    }

    /// The torrent ids of the hashes the store holds, by hash.
    pub fn ids(&self, hashes: &[(String, [u8; 20])]) -> Result<HashMap<String, u64>, String> {
        let mut stmt = self
            .conn
            .prepare("SELECT id FROM torrents WHERE info_hash = ?1")
            .map_err(|e| e.to_string())?;
        let mut out = HashMap::new();
        for (key, bytes) in hashes {
            let id: Option<i64> = stmt
                .query_row([&bytes[..]], |r| r.get(0))
                .optional()
                .map_err(|e| e.to_string())?;
            if let Some(id) = id {
                out.insert(key.clone(), id as u64);
            }
        }
        Ok(out)
    }
}

/// Takes a shared lock on each lock file that exists, or says the
/// directory is in use. A missing one is left missing: creating it would
/// change the directory, and nobody holds a lock that isn't there.
fn hold_locks(paths: &[PathBuf]) -> Result<Vec<File>, String> {
    let mut held = Vec::new();
    for p in paths {
        let file = match File::open(p) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(format!("opening {}: {e}", p.display())),
        };
        match file.try_lock_shared() {
            Ok(()) => held.push(file),
            Err(std::fs::TryLockError::WouldBlock) => {
                return Err(format!(
                    "in use: a running Lodestar holds {}. Stop it first, or score a copy of the \
                     data directory",
                    p.display()
                ));
            }
            Err(std::fs::TryLockError::Error(e)) => {
                return Err(format!("locking {}: {e}", p.display()));
            }
        }
    }
    Ok(held)
}

fn side_file(db: &Path, suffix: &str) -> PathBuf {
    let mut s = db.as_os_str().to_owned();
    s.push(suffix);
    PathBuf::from(s)
}

/// The index versions under `root`, complete or not, for the refusal.
fn versions(root: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(root)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().is_dir())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with('v'))
        .collect();
    v.sort();
    v
}

/// The store, read-only and immutable: SQLite then takes no lock and
/// creates no `-wal` or `-shm`.
fn open_immutable(db: &Path) -> Result<Connection, String> {
    let abs = std::path::absolute(db).map_err(|e| format!("{}: {e}", db.display()))?;
    let uri = format!("{}?mode=ro&immutable=1", file_uri(&abs));
    Connection::open_with_flags(
        &uri,
        OpenFlags::SQLITE_OPEN_READ_ONLY
            | OpenFlags::SQLITE_OPEN_URI
            | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|e| format!("{}: {e}", db.display()))
}

/// An absolute path as a `file:` URI (RFC 8089, as SQLite reads it):
/// `/` separators, a leading `/` before a drive letter, and every byte
/// outside the unreserved set percent-encoded.
fn file_uri(path: &Path) -> String {
    let s = path.to_string_lossy();
    let s = if cfg!(windows) {
        let s = s.strip_prefix(r"\\?\").unwrap_or(&s).replace('\\', "/");
        if s.starts_with('/') {
            s
        } else {
            format!("/{s}")
        }
    } else {
        s.into_owned()
    };
    let mut out = String::from("file:");
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"/-._~:".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(not(windows))]
    #[test]
    fn uris_escape_what_sqlite_would_read_as_syntax() {
        assert_eq!(
            file_uri(Path::new("/data/my store?#%/lodestar.db")),
            "file:/data/my%20store%3F%23%25/lodestar.db"
        );
    }

    #[test]
    fn side_files_append_to_the_name() {
        assert_eq!(
            side_file(Path::new("d/lodestar.db"), "-wal"),
            PathBuf::from("d/lodestar.db-wal")
        );
    }
}
