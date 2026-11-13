//! An existing Lodestar data directory (`lodestar.db` and `index/`),
//! opened strictly read-only for `--data-dir`.
//!
//! - **Nothing in it changes.** The store is opened with SQLite's
//!   `immutable` flag, so no `-wal` or `-shm` is created and nothing is
//!   locked or written. The index is opened only when it's complete and at
//!   this build's schema, and no writer is taken. Opening a tantivy reader
//!   briefly takes `.tantivy-meta.lock` (opened for writing, created if
//!   missing), so an index without that file is refused rather than given
//!   one, and the index can't be on a read-only mount.
//! - **Not while a server has it.** A server holds an exclusive lock on
//!   `index/.lock` and its index writers' locks. Each that exists is taken
//!   shared here and kept until the run ends, so a busy one refuses the run.
//!   A store with a `-wal` or `-shm` beside it is refused too: SQLite
//!   removes both when the last connection closes cleanly, so either means
//!   a server is running (even one with search off, which holds no index
//!   lock) or stopped without closing the store, and `immutable` would
//!   read past the log.
//! - **A server started meanwhile** opens and may migrate the store before
//!   it reaches the index lock this run holds (and with no `index/.lock`
//!   yet, nothing stops it). Every store read happens while opening, so the
//!   store's size, mtime and side files are checked again once the reads
//!   are done ([`DataDir::check_unchanged`]), and a change refuses the run.
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
    db: PathBuf,
    /// The store's length and mtime when it was opened.
    stamp: (u64, std::time::SystemTime),
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

        let stamp = store_stamp(&db)?;

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
        let has = |name: &str| idx.join(name).is_file() && idx.join("files").join(name).is_file();
        if !has("meta.json") {
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
        // A reader takes this lock while it opens; it mustn't be created.
        if !has(".tantivy-meta.lock") {
            return Err(format!(
                "{}: the index has no .tantivy-meta.lock (copied without its lock files?). \
                 Opening a reader would create it; start Lodestar on it once, or copy it whole",
                idx.display()
            ));
        }
        let index = SearchIndex::open(&idx).map_err(|e| {
            format!(
                "{}: the index doesn't open: {e}. It must be this build's, and writable: a \
                 reader locks .tantivy-meta.lock, so a read-only mount isn't supported (score \
                 a writable copy)",
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
            db,
            stamp,
            _locks: locks,
        })
    }

    /// Refuses if the store changed since it was opened: a server started
    /// meanwhile. Call it after the last store read.
    pub fn check_unchanged(&self) -> Result<(), String> {
        if store_stamp(&self.db)? != self.stamp {
            return Err(format!(
                "{}: the store changed while it was read (a Lodestar started meanwhile?): stop \
                 it and run this again",
                self.db.display()
            ));
        }
        Ok(())
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

/// The store's length and mtime, refusing while a `-wal` or `-shm` is
/// beside it.
fn store_stamp(db: &Path) -> Result<(u64, std::time::SystemTime), String> {
    for suffix in ["-wal", "-shm"] {
        let side = side_file(db, suffix);
        if side.exists() {
            return Err(format!(
                "{}: the store is open, or wasn't closed cleanly ({} exists). Stop Lodestar \
                 cleanly (or start and stop it once), then run this again",
                db.display(),
                side.display()
            ));
        }
    }
    let m = std::fs::metadata(db).map_err(|e| format!("{}: {e}", db.display()))?;
    let mtime = m.modified().map_err(|e| format!("{}: {e}", db.display()))?;
    Ok((m.len(), mtime))
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
    let uri = format!("{}?mode=ro&immutable=1", file_uri(&abs)?);
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
fn file_uri(path: &Path) -> Result<String, String> {
    #[cfg(unix)]
    let bytes = {
        use std::os::unix::ffi::OsStrExt as _;
        path.as_os_str().as_bytes().to_vec()
    };
    #[cfg(not(unix))]
    let bytes = {
        let s = path
            .to_str()
            .ok_or_else(|| format!("{}: not a Unicode path", path.display()))?;
        windows_uri_path(s)?.into_bytes()
    };
    Ok(encode(&bytes))
}

/// A Windows path in a URI's form: `C:\a\b` is `/C:/a/b`. A network
/// (UNC) path is refused: SQLite reads its host as a URI authority.
#[cfg_attr(unix, allow(dead_code))]
fn windows_uri_path(s: &str) -> Result<String, String> {
    let s = s.strip_prefix(r"\\?\").unwrap_or(s);
    if s.starts_with(r"\\") || s.starts_with(r"UNC\") {
        return Err(format!(
            "{s}: a network (UNC) path isn't supported: map it to a drive letter, or copy the \
             data directory locally"
        ));
    }
    Ok(format!("/{}", s.replace('\\', "/")))
}

fn encode(bytes: &[u8]) -> String {
    let mut out = String::from("file:");
    for &b in bytes {
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

    #[test]
    fn uris_escape_what_sqlite_would_read_as_syntax() {
        assert_eq!(
            encode(b"/data/my store?#%/lodestar.db"),
            "file:/data/my%20store%3F%23%25/lodestar.db"
        );
        assert_eq!(encode(b"/a/\xff"), "file:/a/%FF");
    }

    #[test]
    fn windows_paths_become_uri_paths() {
        assert_eq!(
            windows_uri_path(r"C:\Data\lodestar.db").unwrap(),
            "/C:/Data/lodestar.db"
        );
        assert_eq!(windows_uri_path(r"\\?\D:\x").unwrap(), "/D:/x");
        assert!(windows_uri_path(r"\\server\share\x").is_err());
        assert!(windows_uri_path(r"\\?\UNC\server\share\x").is_err());
    }

    #[test]
    fn side_files_append_to_the_name() {
        assert_eq!(
            side_file(Path::new("d/lodestar.db"), "-wal"),
            PathBuf::from("d/lodestar.db-wal")
        );
    }
}
