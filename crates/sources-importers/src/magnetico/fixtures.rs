//! Magnetico databases built in the tests, from magneticod's own schema
//! history (`pkg/persistence/sqlite3.go`): each layout runs the same DDL
//! magneticod ran to reach it, so a fixture is shaped like a real source.

use std::path::Path;

use rusqlite::types::Value;
use rusqlite::{Connection, params};

/// A schema magneticod has written, plus shapes a damaged or hand-made
/// source can have.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Layout {
    /// The initial schema: no index on `files.torrent_id`.
    V0,
    /// `info_hash_index` made unique; still no files index.
    V1,
    /// Swarm and readme columns, and `readme_index`.
    V2,
    /// The observed schema: FTS5, its triggers, `modified_on`.
    V3,
    /// v3 without the optional `n_seeders` column.
    V3NoSeeders,
    /// v3's columns with no constraints, so malformed rows can be stored.
    Lax,
}

const V0: &str = "
    CREATE TABLE torrents (
        id             INTEGER PRIMARY KEY,
        info_hash      BLOB NOT NULL UNIQUE,
        name           TEXT NOT NULL,
        total_size     INTEGER NOT NULL CHECK(total_size > 0),
        discovered_on  INTEGER NOT NULL CHECK(discovered_on > 0)
    );
    CREATE TABLE files (
        id          INTEGER PRIMARY KEY,
        torrent_id  INTEGER REFERENCES torrents ON DELETE CASCADE ON UPDATE RESTRICT,
        size        INTEGER NOT NULL,
        path        TEXT NOT NULL
    );";

const V1: &str = "
    DROP INDEX IF EXISTS info_hash_index;
    CREATE UNIQUE INDEX info_hash_index ON torrents (info_hash);
    PRAGMA user_version = 1;";

const V2: &str = "
    ALTER TABLE torrents ADD COLUMN updated_on INTEGER CHECK (updated_on > 0) DEFAULT NULL;
    ALTER TABLE torrents ADD COLUMN n_seeders  INTEGER CHECK ((updated_on IS NOT NULL AND n_seeders >= 0) OR (updated_on IS NULL AND n_seeders IS NULL)) DEFAULT NULL;
    ALTER TABLE torrents ADD COLUMN n_leechers INTEGER CHECK ((updated_on IS NOT NULL AND n_leechers >= 0) OR (updated_on IS NULL AND n_leechers IS NULL)) DEFAULT NULL;
    ALTER TABLE files ADD COLUMN is_readme INTEGER CHECK (is_readme IS NULL OR is_readme=1) DEFAULT NULL;
    ALTER TABLE files ADD COLUMN content   TEXT    CHECK ((content IS NULL AND is_readme IS NULL) OR (content IS NOT NULL AND is_readme=1)) DEFAULT NULL;
    CREATE UNIQUE INDEX readme_index ON files (torrent_id, is_readme);
    PRAGMA user_version = 2;";

const V3: &str = r##"
    CREATE VIRTUAL TABLE torrents_idx USING fts5(name, content='torrents', content_rowid='id', tokenize="porter unicode61 separators ' !""#$%&''()*+,-./:;<=>?@[\]^_`{|}~'");
    INSERT INTO torrents_idx(rowid, name) SELECT id, name FROM torrents;
    CREATE TRIGGER torrents_idx_ai_t AFTER INSERT ON torrents BEGIN
      INSERT INTO torrents_idx(rowid, name) VALUES (new.id, new.name);
    END;
    CREATE TRIGGER torrents_idx_ad_t AFTER DELETE ON torrents BEGIN
      INSERT INTO torrents_idx(torrents_idx, rowid, name) VALUES('delete', old.id, old.name);
    END;
    CREATE TRIGGER torrents_idx_au_t AFTER UPDATE ON torrents BEGIN
      INSERT INTO torrents_idx(torrents_idx, rowid, name) VALUES('delete', old.id, old.name);
      INSERT INTO torrents_idx(rowid, name) VALUES (new.id, new.name);
    END;
    ALTER TABLE torrents ADD COLUMN modified_on INTEGER NOT NULL
        CHECK (modified_on >= discovered_on AND (updated_on IS NOT NULL OR modified_on >= updated_on))
        DEFAULT 32503680000;
    CREATE TRIGGER "torrents_modified_on_default_t" AFTER INSERT ON "torrents" BEGIN
      UPDATE "torrents" SET "modified_on" = NEW."discovered_on" WHERE "id" = NEW."id" AND NEW."modified_on" = 32503680000;
    END;
    UPDATE torrents SET modified_on = (SELECT MAX(discovered_on, IFNULL(updated_on, 0)));
    CREATE INDEX modified_on_index ON torrents (modified_on);
    PRAGMA user_version = 3;"##;

/// v3 with `n_seeders` left out of the v2 step.
const V3_NO_SEEDERS_V2: &str = "
    ALTER TABLE torrents ADD COLUMN updated_on INTEGER CHECK (updated_on > 0) DEFAULT NULL;
    ALTER TABLE torrents ADD COLUMN n_leechers INTEGER DEFAULT NULL;
    ALTER TABLE files ADD COLUMN is_readme INTEGER CHECK (is_readme IS NULL OR is_readme=1) DEFAULT NULL;
    ALTER TABLE files ADD COLUMN content   TEXT    CHECK ((content IS NULL AND is_readme IS NULL) OR (content IS NOT NULL AND is_readme=1)) DEFAULT NULL;
    CREATE UNIQUE INDEX readme_index ON files (torrent_id, is_readme);
    PRAGMA user_version = 2;";

const LAX: &str = "
    CREATE TABLE torrents (
        id INTEGER PRIMARY KEY, info_hash, name, total_size, discovered_on,
        updated_on, n_seeders, n_leechers, modified_on
    );
    CREATE TABLE files (
        id INTEGER PRIMARY KEY, torrent_id, size, path, is_readme, content
    );
    CREATE INDEX readme_index ON files (torrent_id, is_readme);
    PRAGMA user_version = 3;";

/// One file row. Values are untyped so a test can store any storage class.
#[derive(Debug, Clone)]
pub struct FileRow {
    pub torrent_id: Value,
    pub size: Value,
    pub path: Value,
    pub readme: Option<Vec<u8>>,
}

/// One torrent row and its files.
#[derive(Debug, Clone)]
pub struct TorrentRow {
    pub info_hash: Value,
    pub name: Value,
    pub total_size: Value,
    pub discovered_on: Value,
    /// `(path, size)`, attached to this torrent's id.
    pub files: Vec<(Value, Value)>,
}

impl TorrentRow {
    /// A well-formed row: 20-byte hash from `seed`, a UTF-8 name and
    /// files whose sizes sum to the total.
    pub fn good(seed: u32, files: &[(&str, i64)]) -> TorrentRow {
        let mut hash = [0u8; 20];
        hash[..4].copy_from_slice(&seed.to_be_bytes());
        hash[19] = 1;
        let total: i64 = files.iter().map(|f| f.1).sum();
        TorrentRow {
            info_hash: Value::Blob(hash.to_vec()),
            name: Value::Text(format!("fixture torrent {seed}")),
            total_size: Value::Integer(total.max(1)),
            discovered_on: Value::Integer(1_600_000_000 + i64::from(seed)),
            files: files
                .iter()
                .map(|(p, s)| (Value::Text((*p).to_owned()), Value::Integer(*s)))
                .collect(),
        }
    }
}

/// Creates a database at `path` in `layout`, in WAL mode as magneticod
/// leaves it.
pub fn create(path: &Path, layout: Layout) -> Connection {
    let conn = Connection::open(path).expect("open fixture");
    conn.pragma_update(None, "journal_mode", "WAL")
        .expect("wal");
    let steps: &[&str] = match layout {
        Layout::V0 => &[V0],
        Layout::V1 => &[V0, V1],
        Layout::V2 => &[V0, V1, V2],
        Layout::V3 => &[V0, V1, V2, V3],
        Layout::V3NoSeeders => &[V0, V1, V3_NO_SEEDERS_V2, V3],
        Layout::Lax => &[LAX],
    };
    for step in steps {
        conn.execute_batch(step).expect("fixture schema");
    }
    conn
}

fn has_column(conn: &Connection, table: &str, column: &str) -> bool {
    conn.prepare(&format!(
        "SELECT 1 FROM pragma_table_info('{table}') WHERE name = ?1"
    ))
    .and_then(|mut s| s.exists([column]))
    .expect("table_info")
}

/// Inserts `rows`, assigning ids in order. CHECK constraints are relaxed
/// so a test can store values a damaged source could hold.
pub fn insert(conn: &mut Connection, rows: &[TorrentRow]) -> Vec<i64> {
    let readme = has_column(conn, "files", "is_readme");
    // magneticod v3's `modified_on` default trigger fires before the FTS
    // insert trigger on current SQLite and corrupts the FTS index, so the
    // fixture supplies the value and the default trigger stays idle.
    let modified = has_column(conn, "torrents", "modified_on");
    let tx = conn.transaction().expect("tx");
    tx.pragma_update(None, "ignore_check_constraints", true)
        .expect("relax checks");
    let mut ids = Vec::with_capacity(rows.len());
    for row in rows {
        if modified {
            tx.execute(
                "INSERT INTO torrents (info_hash, name, total_size, discovered_on, modified_on) \
                 VALUES (?1, ?2, ?3, ?4, ?4)",
                params![row.info_hash, row.name, row.total_size, row.discovered_on],
            )
        } else {
            tx.execute(
                "INSERT INTO torrents (info_hash, name, total_size, discovered_on) VALUES (?1, ?2, ?3, ?4)",
                params![row.info_hash, row.name, row.total_size, row.discovered_on],
            )
        }
        .expect("insert torrent");
        let id = tx.last_insert_rowid();
        for (path, size) in &row.files {
            insert_file(
                &tx,
                readme,
                &FileRow {
                    torrent_id: Value::Integer(id),
                    size: size.clone(),
                    path: path.clone(),
                    readme: None,
                },
            );
        }
        ids.push(id);
    }
    tx.commit().expect("commit");
    ids
}

/// Inserts one file row as given, including orphans and readme rows.
pub fn insert_file(conn: &Connection, readme: bool, file: &FileRow) {
    if readme {
        let (flag, content) = match &file.readme {
            Some(c) => (Value::Integer(1), Value::Blob(c.clone())),
            None => (Value::Null, Value::Null),
        };
        conn.execute(
            "INSERT INTO files (torrent_id, size, path, is_readme, content) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![file.torrent_id, file.size, file.path, flag, content],
        )
        .expect("insert file");
    } else {
        conn.execute(
            "INSERT INTO files (torrent_id, size, path) VALUES (?1, ?2, ?3)",
            params![file.torrent_id, file.size, file.path],
        )
        .expect("insert file");
    }
}

/// Builds a closed, quiescent fixture: the WAL is checkpointed and removed
/// on close, as after a clean magneticod shutdown.
pub fn build(path: &Path, layout: Layout, rows: &[TorrentRow]) -> Vec<i64> {
    let mut conn = create(path, layout);
    let ids = insert(&mut conn, rows);
    conn.close().expect("close fixture");
    ids
}

#[cfg(test)]
mod tests {
    use super::*;

    fn columns(conn: &Connection, table: &str) -> Vec<String> {
        let mut s = conn
            .prepare(&format!("SELECT name FROM pragma_table_info('{table}')"))
            .unwrap();
        s.query_map([], |r| r.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap()
    }

    #[test]
    fn layouts_have_magneticods_columns_and_versions() {
        let dir = tempfile::tempdir().unwrap();
        for (layout, version, torrents, files) in [
            (Layout::V0, 0, 5, 4),
            (Layout::V1, 1, 5, 4),
            (Layout::V2, 2, 8, 6),
            (Layout::V3, 3, 9, 6),
            (Layout::V3NoSeeders, 3, 8, 6),
            (Layout::Lax, 3, 9, 6),
        ] {
            let path = dir.path().join(format!("{layout:?}.sqlite3"));
            build(&path, layout, &[TorrentRow::good(1, &[("a/b", 5)])]);
            let conn = Connection::open(&path).unwrap();
            let v: i64 = conn
                .pragma_query_value(None, "user_version", |r| r.get(0))
                .unwrap();
            assert_eq!(v, version, "{layout:?}");
            assert_eq!(columns(&conn, "torrents").len(), torrents, "{layout:?}");
            assert_eq!(columns(&conn, "files").len(), files, "{layout:?}");
        }
    }

    #[test]
    fn v3_sets_modified_on_and_fills_fts() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("v3.sqlite3");
        build(&path, Layout::V3, &[TorrentRow::good(7, &[("x", 3)])]);
        let conn = Connection::open(&path).unwrap();
        let (d, m): (i64, i64) = conn
            .query_row("SELECT discovered_on, modified_on FROM torrents", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert_eq!(d, m);
        let n: i64 = conn
            .query_row("SELECT count(*) FROM torrents_idx", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn build_leaves_no_wal_siblings() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("q.sqlite3");
        build(&path, Layout::V3, &[TorrentRow::good(1, &[("a", 1)])]);
        let names: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names, vec![std::ffi::OsString::from("q.sqlite3")]);
    }

    #[test]
    fn malformed_values_can_be_stored() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bad.sqlite3");
        let mut row = TorrentRow::good(1, &[]);
        row.info_hash = Value::Text("abc".into());
        row.name = Value::Blob(vec![0xff, 0xfe, b'a']);
        row.total_size = Value::Integer(-5);
        row.files = vec![(Value::Null, Value::Integer(-1))];
        build(&path, Layout::Lax, &[row]);
        let conn = Connection::open(&path).unwrap();
        let t: String = conn
            .query_row("SELECT typeof(info_hash) FROM torrents", [], |r| r.get(0))
            .unwrap();
        assert_eq!(t, "text");
    }
}
