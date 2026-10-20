//! Schema migrations and connection setup (architecture §7).
//!
//! Each milestone adds the tables it writes as a new migration; migrations
//! are append-only and never edited once released.

use std::path::Path;

use rusqlite::{Connection, OptionalExtension};

use crate::{Error, Result};

/// `(version, sql)`, applied in order, each in its own transaction.
pub(crate) const MIGRATIONS: &[(i64, &str)] = &[(1, V1)];

/// M0: the canonical torrent tables plus settings and zstd dictionaries.
///
/// Ids are `AUTOINCREMENT` so a deleted id is never reused: hot-column
/// slots, index documents and the percolator watermark all key on it.
/// Child tables use `torrent_id INTEGER PRIMARY KEY`, i.e. they are keyed by
/// rowid with no separate index.
const V1: &str = "
CREATE TABLE settings (
    key   TEXT PRIMARY KEY,
    value ANY NOT NULL
) STRICT;

CREATE TABLE zstd_dicts (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    kind       TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    samples    INTEGER NOT NULL,
    data       BLOB NOT NULL
) STRICT;

CREATE TABLE torrents (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    -- v1 hash, or the truncated v2 hash for pure v2 torrents.
    info_hash     BLOB NOT NULL UNIQUE CHECK (length(info_hash) = 20),
    info_hash_v2  BLOB CHECK (info_hash_v2 IS NULL OR length(info_hash_v2) = 32),
    -- Decoded name; the raw bytes and their encoding are kept only when
    -- they aren't plain UTF-8, so decoding can be redone.
    name          TEXT NOT NULL,
    name_raw      BLOB,
    name_enc      INTEGER,
    size          INTEGER NOT NULL,
    piece_len     INTEGER NOT NULL,
    piece_count   INTEGER NOT NULL,
    file_count    INTEGER NOT NULL,
    info_size     INTEGER NOT NULL,
    flags         INTEGER NOT NULL,
    discovered_at INTEGER NOT NULL,
    metadata_at   INTEGER NOT NULL,
    last_seen_at  INTEGER NOT NULL,
    -- Derived in M3; recomputable at any time.
    content_type  INTEGER,
    junk_score    INTEGER,
    group_key     INTEGER,
    comp          BLOB
) STRICT;

-- Hybrid and v2 torrents are also announced under the truncated v2 hash.
CREATE UNIQUE INDEX torrents_v2 ON torrents (substr(info_hash_v2, 1, 20))
    WHERE info_hash_v2 IS NOT NULL;

CREATE TABLE torrent_files (
    torrent_id INTEGER PRIMARY KEY REFERENCES torrents (id) ON DELETE CASCADE,
    codec      INTEGER NOT NULL,
    dict_id    INTEGER REFERENCES zstd_dicts (id),
    data       BLOB NOT NULL
) STRICT;

CREATE TABLE torrent_extra (
    torrent_id INTEGER PRIMARY KEY REFERENCES torrents (id) ON DELETE CASCADE,
    data       BLOB NOT NULL
) STRICT;

CREATE TABLE torrent_raw (
    torrent_id INTEGER PRIMARY KEY REFERENCES torrents (id) ON DELETE CASCADE,
    data       BLOB NOT NULL
) STRICT;

CREATE TABLE torrent_parsed (
    torrent_id INTEGER PRIMARY KEY REFERENCES torrents (id) ON DELETE CASCADE,
    parser_ver INTEGER NOT NULL,
    data       BLOB NOT NULL
) STRICT;
";

/// Pragmas that persist in the file and can only be set before the first
/// table exists, then the per-connection ones.
pub(crate) fn configure(conn: &Connection, mmap_size: u64) -> Result<()> {
    let page_count: i64 = conn.pragma_query_value(None, "page_count", |r| r.get(0))?;
    if page_count == 0 {
        conn.pragma_update(None, "page_size", 16384)?;
        conn.pragma_update(None, "auto_vacuum", "INCREMENTAL")?;
    }
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    conn.pragma_update(None, "foreign_keys", true)?;
    conn.pragma_update(None, "mmap_size", mmap_size as i64)?;
    conn.pragma_update(None, "temp_store", "MEMORY")?;
    conn.busy_timeout(std::time::Duration::from_secs(5))?;
    Ok(())
}

/// Brings the schema up to date. Refuses to touch a database written by a
/// newer version, and snapshots an existing database before changing it
/// (architecture §12).
pub(crate) fn migrate(conn: &mut Connection, path: Option<&Path>) -> Result<()> {
    migrate_with(conn, path, MIGRATIONS)
}

fn migrate_with(
    conn: &mut Connection,
    path: Option<&Path>,
    migrations: &[(i64, &str)],
) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS schema_migrations (
             version    INTEGER PRIMARY KEY,
             applied_at INTEGER NOT NULL
         ) STRICT;",
    )?;
    let current: i64 = conn
        .query_row("SELECT max(version) FROM schema_migrations", [], |r| {
            r.get::<_, Option<i64>>(0)
        })
        .optional()?
        .flatten()
        .unwrap_or(0);
    let latest = migrations.last().map_or(0, |(v, _)| *v);
    if current > latest {
        return Err(Error::SchemaTooNew {
            found: current,
            supported: latest,
        });
    }
    if current == latest {
        return Ok(());
    }
    if current > 0 {
        if let Some(path) = path {
            let backup = path.with_extension(format!("pre-v{}.db", current + 1));
            conn.execute("VACUUM INTO ?1", [backup.to_string_lossy()])?;
        }
    }
    for (version, sql) in migrations.iter().filter(|(v, _)| *v > current) {
        let tx = conn.transaction()?;
        tx.execute_batch(sql)?;
        tx.execute(
            "INSERT INTO schema_migrations (version, applied_at) VALUES (?1, unixepoch())",
            [version],
        )?;
        tx.commit()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const FAKE: &[(i64, &str)] = &[
        (1, "CREATE TABLE a (x INTEGER) STRICT;"),
        (2, "CREATE TABLE b (x INTEGER) STRICT;"),
    ];

    fn version(conn: &Connection) -> i64 {
        conn.query_row("SELECT max(version) FROM schema_migrations", [], |r| {
            r.get(0)
        })
        .unwrap()
    }

    #[test]
    fn applies_pending_migrations_and_snapshots_first() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.db");
        let mut conn = Connection::open(&path).unwrap();
        migrate_with(&mut conn, Some(&path), &FAKE[..1]).unwrap();
        assert_eq!(version(&conn), 1);
        // A fresh database has nothing to protect: no snapshot.
        assert!(!dir.path().join("t.pre-v1.db").exists());

        migrate_with(&mut conn, Some(&path), FAKE).unwrap();
        assert_eq!(version(&conn), 2);
        let backup = Connection::open(dir.path().join("t.pre-v2.db")).unwrap();
        assert_eq!(version(&backup), 1);

        // Idempotent.
        migrate_with(&mut conn, Some(&path), FAKE).unwrap();
        assert_eq!(version(&conn), 2);
    }

    #[test]
    fn refuses_newer_schema() {
        let mut conn = Connection::open_in_memory().unwrap();
        migrate_with(&mut conn, None, FAKE).unwrap();
        let err = migrate_with(&mut conn, None, &FAKE[..1]).unwrap_err();
        assert!(matches!(
            err,
            Error::SchemaTooNew {
                found: 2,
                supported: 1
            }
        ));
    }

    #[test]
    fn failed_migration_rolls_back() {
        let mut conn = Connection::open_in_memory().unwrap();
        let broken: &[(i64, &str)] = &[(1, "CREATE TABLE ok (x INTEGER); CREATE TABLE ok (y);")];
        assert!(migrate_with(&mut conn, None, broken).is_err());
        let tables: i64 = conn
            .query_row(
                "SELECT count(*) FROM sqlite_schema WHERE name = 'ok'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(tables, 0);
    }
}
