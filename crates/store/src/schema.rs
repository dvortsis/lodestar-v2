//! Schema migrations and connection setup (architecture §7).
//!
//! Each milestone adds the tables it writes as a new migration; migrations
//! are append-only and never edited once released.

use std::path::Path;

use rusqlite::{Connection, OptionalExtension};

use crate::{Error, Result};

/// `(version, sql)`, applied in order, each in its own transaction.
pub(crate) const MIGRATIONS: &[(i64, &str)] = &[
    (1, V1),
    (2, V2),
    (3, V3),
    (4, V4),
    (5, V5),
    (6, V6),
    (7, V7),
    (8, V8),
    (9, V9),
];

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

/// M1: provenance, the shared id sequence, Insights counters and the
/// persisted fetch queue (architecture §5, §7, §18.5).
///
/// - `id_seq` is the one id space for every item table. Rows insert
///   explicit ids from it inside the writer's transaction, so an item keeps
///   its id across tables (unverified → torrents, overlay networks).
///   `torrents.id` stays `AUTOINCREMENT` as a second guard against reuse.
/// - `torrents.sources` is a bitset over `sources.id` (0..=63);
///   `first_source` is the source that found it first. Pre-M1 rows have
///   neither.
/// - The rollup and cohort tables hold additive counters keyed by hour.
///   Cohorts also carry the source, so the funnel can be split by it.
/// - `fetch_queue` holds keys still waiting for metadata across restarts.
///   No peers are stored with them: peer addresses are transient (§18.7)
///   and stale by the time we restart anyway.
const V2: &str = "
CREATE TABLE id_seq (
    one  INTEGER PRIMARY KEY CHECK (one = 1),
    next INTEGER NOT NULL CHECK (next > 0)
) STRICT;

INSERT INTO id_seq (one, next) SELECT 1, max(
    coalesce((SELECT seq FROM sqlite_sequence WHERE name = 'torrents'), 0),
    coalesce((SELECT max(id) FROM torrents), 0)
) + 1;

CREATE TABLE sources (
    id       INTEGER PRIMARY KEY CHECK (id BETWEEN 0 AND 63),
    plugin   TEXT NOT NULL UNIQUE,
    version  TEXT NOT NULL,
    state    INTEGER NOT NULL,
    added_at INTEGER NOT NULL
) STRICT;

ALTER TABLE torrents ADD COLUMN sources INTEGER NOT NULL DEFAULT 0;
ALTER TABLE torrents ADD COLUMN first_source INTEGER
    CHECK (first_source IS NULL OR first_source BETWEEN 0 AND 63);

CREATE TABLE rollup_hour (
    hour   INTEGER NOT NULL,
    metric INTEGER NOT NULL,
    dim    INTEGER NOT NULL,
    key    INTEGER NOT NULL,
    value  INTEGER NOT NULL,
    PRIMARY KEY (hour, metric, dim, key)
) STRICT, WITHOUT ROWID;

CREATE TABLE cohort_hour (
    cohort INTEGER NOT NULL,
    source INTEGER NOT NULL,
    stage  INTEGER NOT NULL,
    reason INTEGER NOT NULL,
    count  INTEGER NOT NULL,
    PRIMARY KEY (cohort, source, stage, reason)
) STRICT, WITHOUT ROWID;

CREATE TABLE events (
    id     INTEGER PRIMARY KEY,
    at     INTEGER NOT NULL,
    kind   INTEGER NOT NULL,
    detail TEXT NOT NULL
) STRICT;
CREATE INDEX events_at ON events (at);

CREATE TABLE fetch_queue (
    info_hash BLOB PRIMARY KEY CHECK (length(info_hash) = 20),
    source    INTEGER NOT NULL CHECK (source BETWEEN 0 AND 63),
    cohort    INTEGER NOT NULL,
    strategy  INTEGER NOT NULL,
    priority  INTEGER NOT NULL,
    attempts  INTEGER NOT NULL,
    flags     INTEGER NOT NULL
) STRICT, WITHOUT ROWID;
";

/// M2: torrents changed after they were stored (a partial import upgraded
/// by a fetched info dict), for the search indexer to re-index. The
/// indexer follows `seq` with its own watermark; rows are few (one per
/// upgrade), so they're kept.
const V3: &str = "
CREATE TABLE reindex_log (
    seq        INTEGER PRIMARY KEY AUTOINCREMENT,
    torrent_id INTEGER NOT NULL
) STRICT;
";

/// M4 add-on: content filtering (§20.7). Only what filtering needs to work
/// is kept: no table records who changed a filter, when, or why (§20.11).
const V4: &str = "
-- CS-1 entries (§20.5.2), every hash form. origin: 0 confirmed,
-- 1 reported, 2 imported.
CREATE TABLE cs_hashes (
    hash     BLOB PRIMARY KEY CHECK (length(hash) = 20),
    origin   INTEGER NOT NULL,
    added_at INTEGER NOT NULL
) STRICT, WITHOUT ROWID;

-- CS-8 payload signatures.
CREATE TABLE cs_sigs (
    sig      INTEGER PRIMARY KEY,
    added_at INTEGER NOT NULL
) STRICT;

-- Core rules you switched off. Every rule is on unless listed here; the
-- floor is never listed (§20.5.3).
CREATE TABLE core_membership (
    rule    TEXT PRIMARY KEY,
    enabled INTEGER NOT NULL
) STRICT, WITHOUT ROWID;

-- Dropped torrents, so they aren't fetched again (§20.3).
CREATE TABLE tombstones (
    hash BLOB PRIMARY KEY CHECK (length(hash) = 20),
    rule TEXT NOT NULL,
    at   INTEGER NOT NULL
) STRICT, WITHOUT ROWID;

-- Held torrents: hash, rule and a sealed name only (§20.3).
CREATE TABLE quarantine (
    hash       BLOB PRIMARY KEY CHECK (length(hash) = 20),
    rule       TEXT NOT NULL,
    sealed     BLOB,
    at         INTEGER NOT NULL,
    expires_at INTEGER NOT NULL
) STRICT, WITHOUT ROWID;
CREATE INDEX quarantine_expiry ON quarantine (expires_at);

-- Filter decisions for the review queue and the 30-day replay: no names
-- (§20.11).
CREATE TABLE filter_hits (
    id       INTEGER PRIMARY KEY,
    hash     BLOB NOT NULL,
    rule     TEXT NOT NULL,
    action   INTEGER NOT NULL,
    at       INTEGER NOT NULL,
    evidence BLOB NOT NULL
) STRICT;
CREATE INDEX filter_hits_at ON filter_hits (at);
";

/// M4: swarm health (architecture §7, §16 M4).
///
/// - `trackers` is the scrape list. Ids are `AUTOINCREMENT` within u16 and
///   never reused, because `swarm_trackers` packs them: a reused id would
///   credit old counts to a new tracker.
/// - `swarm` has one row per scraped torrent; a torrent with no row has
///   never been scraped. `dht_*` are NULL without a DHT estimate.
///   `next_scrape_at` is indexed for the scheduler's "due" lane.
/// - `swarm_trackers` holds per-tracker counts, only for trackers that
///   report the hash (s+l+c > 0).
/// - `swarm_history` is a fixed-size ring (`history` module), kept only
///   for swarms that were ever non-zero or that a user looked at.
const V5: &str = "
CREATE TABLE trackers (
    id       INTEGER PRIMARY KEY AUTOINCREMENT CHECK (id BETWEEN 1 AND 65535),
    url      TEXT NOT NULL UNIQUE,
    added_at INTEGER NOT NULL,
    enabled  INTEGER NOT NULL DEFAULT 1 CHECK (enabled IN (0, 1)),
    state    INTEGER NOT NULL DEFAULT 0,
    state_at INTEGER,
    message  TEXT
) STRICT;

CREATE TABLE swarm (
    torrent_id         INTEGER PRIMARY KEY REFERENCES torrents (id) ON DELETE CASCADE,
    seeders            INTEGER NOT NULL,
    leechers           INTEGER NOT NULL,
    completed          INTEGER NOT NULL,
    trackers_reporting INTEGER NOT NULL,
    trackers_checked   INTEGER NOT NULL,
    dht_seeds          INTEGER,
    dht_peers          INTEGER,
    last_scrape_at     INTEGER NOT NULL,
    next_scrape_at     INTEGER NOT NULL,
    flags              INTEGER NOT NULL
) STRICT;
CREATE INDEX swarm_due ON swarm (next_scrape_at);

CREATE TABLE swarm_trackers (
    torrent_id INTEGER PRIMARY KEY REFERENCES torrents (id) ON DELETE CASCADE,
    data       BLOB NOT NULL
) STRICT;

CREATE TABLE swarm_history (
    torrent_id INTEGER PRIMARY KEY REFERENCES torrents (id) ON DELETE CASCADE,
    data       BLOB NOT NULL
) STRICT;
";

/// Blocklist subscriptions (§20.13): what to fetch, how it went, and the
/// parsed list (hashes, 20 bytes each, or encoded IP ranges). A version
/// held back by the sanity gates waits as `pending = 1`.
const V6: &str = "
CREATE TABLE subscriptions (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    url           TEXT NOT NULL UNIQUE,
    name          TEXT NOT NULL,
    -- 0 content (hashes), 1 IP ranges.
    kind          INTEGER NOT NULL,
    -- NULL: detected on each fetch.
    format        TEXT,
    -- The filter action code, for content lists.
    action        INTEGER NOT NULL,
    -- A child-abuse hash list you marked as CS-1b.
    floor         INTEGER NOT NULL DEFAULT 0,
    enabled       INTEGER NOT NULL DEFAULT 1,
    interval_secs INTEGER NOT NULL,
    etag          TEXT,
    last_modified TEXT,
    checked_at    INTEGER,
    updated_at    INTEGER,
    entries       INTEGER NOT NULL DEFAULT 0,
    status        TEXT NOT NULL DEFAULT 'new',
    error         TEXT
) STRICT;

CREATE TABLE subscription_data (
    sub_id  INTEGER NOT NULL REFERENCES subscriptions (id) ON DELETE CASCADE,
    pending INTEGER NOT NULL,
    entries INTEGER NOT NULL,
    data    BLOB NOT NULL,
    PRIMARY KEY (sub_id, pending)
) STRICT, WITHOUT ROWID;
";

/// Your rules, pack switches, the CSX-5 override list, exceptions and
/// *Always allow* (§20.7). Each row holds only what filtering needs: no
/// author, reason or history (§20.11). `rule_set_prev` keeps the one
/// previous rule set for Undo.
const V7: &str = "
CREATE TABLE filter_rules (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    position     INTEGER NOT NULL,
    query        TEXT NOT NULL,
    action       INTEGER NOT NULL,
    enabled      INTEGER NOT NULL DEFAULT 1,
    shadow_until INTEGER,
    unless_query TEXT
) STRICT;

-- Packs (by name) and pack rules (by id) whose switch was set.
CREATE TABLE pack_switches (
    id      TEXT PRIMARY KEY,
    enabled INTEGER NOT NULL
) STRICT, WITHOUT ROWID;

-- kind: 0 exact torrents (20-byte hashes), 1 keyword, 2 query.
-- strength: 0 override, 1 neutralize.
CREATE TABLE override_list (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    kind         INTEGER NOT NULL,
    value        BLOB NOT NULL,
    strength     INTEGER NOT NULL,
    scope        TEXT,
    created_at   INTEGER NOT NULL,
    expires_at   INTEGER,
    shadow_until INTEGER,
    hits         INTEGER NOT NULL DEFAULT 0
) STRICT;

-- kind: 0 exact torrents, 2 query, 3 payload signature (8 bytes).
CREATE TABLE filter_exceptions (
    id    INTEGER PRIMARY KEY AUTOINCREMENT,
    rule  TEXT NOT NULL,
    kind  INTEGER NOT NULL,
    value BLOB NOT NULL
) STRICT;

CREATE TABLE filter_allow (
    id    INTEGER PRIMARY KEY AUTOINCREMENT,
    query TEXT NOT NULL
) STRICT;

-- Must-match (1) and must-not-match (0) examples as hashed tokens.
CREATE TABLE filter_fixtures (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    rule       TEXT NOT NULL,
    must_match INTEGER NOT NULL,
    tokens     BLOB NOT NULL
) STRICT;

CREATE TABLE rule_set_prev (
    id   INTEGER PRIMARY KEY CHECK (id = 1),
    data BLOB NOT NULL
) STRICT;
";

/// M4 add-on: your corrections to classification (§20.7, §20.9): Is
/// adult / Not adult and Not junk, re-applied whenever the torrent is
/// classified again.
const V8: &str = "
CREATE TABLE class_corrections (
    torrent_id INTEGER PRIMARY KEY REFERENCES torrents (id) ON DELETE CASCADE,
    -- 1 Is adult, 0 Not adult, NULL no correction.
    adult      INTEGER CHECK (adult IN (0, 1)),
    not_junk   INTEGER NOT NULL CHECK (not_junk IN (0, 1))
) STRICT;
";

/// M4 add-on: classification rules (§20.9): corrections that apply to
/// every torrent a query matches, the way a per-torrent correction applies
/// to one.
const V9: &str = "
CREATE TABLE class_rules (
    id    INTEGER PRIMARY KEY AUTOINCREMENT,
    query TEXT NOT NULL,
    -- 1 Is adult, 0 Not adult.
    adult INTEGER NOT NULL CHECK (adult IN (0, 1))
) STRICT;
";

/// How long a connection waits for a lock another connection holds.
pub(crate) const BUSY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

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
    // Deleted rows are zeroed, not left in free pages: a copy of the data
    // folder (which also holds seal.key) mustn't yield purged quarantine
    // names or deleted torrents (owner, 2026-10-22).
    conn.pragma_update(None, "secure_delete", true)?;
    conn.busy_timeout(BUSY_TIMEOUT)?;
    Ok(())
}

/// Brings the schema up to date. Refuses to touch a database written by a
/// newer version, and snapshots an existing database before changing it
/// (architecture §12).
pub(crate) fn migrate(conn: &mut Connection, path: Option<&Path>) -> Result<()> {
    migrate_with(conn, path, MIGRATIONS)
}

/// For read-only connections: the schema must be exactly this build's.
pub(crate) fn check_current(conn: &Connection) -> Result<()> {
    let current: i64 = conn
        .query_row("SELECT max(version) FROM schema_migrations", [], |r| {
            r.get::<_, Option<i64>>(0)
        })
        .optional()?
        .flatten()
        .unwrap_or(0);
    let latest = MIGRATIONS.last().map_or(0, |(v, _)| *v);
    if current > latest {
        return Err(Error::SchemaTooNew {
            found: current,
            supported: latest,
        });
    }
    if current < latest {
        return Err(Error::Corrupt(
            "schema not migrated yet; open it for writing first",
        ));
    }
    Ok(())
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
            let backup = snapshot_path(path, current + 1);
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

/// `<db>.pre-v<N>.db`, or `<db>.pre-v<N>.<k>.db` if that exists already:
/// `VACUUM INTO` refuses existing files, and an earlier snapshot (say, from
/// a migration that failed and is being retried) must never be lost.
fn snapshot_path(db: &Path, version: i64) -> std::path::PathBuf {
    let candidate = |k: u32| match k {
        0 => db.with_extension(format!("pre-v{version}.db")),
        k => db.with_extension(format!("pre-v{version}.{k}.db")),
    };
    (0..)
        .map(candidate)
        .find(|p| !p.exists())
        .expect("unbounded search")
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

    /// The id sequence starts after every id torrents has ever used,
    /// including deleted ones.
    /// Migrations apply above the newest recorded one, so a gap (a
    /// number merged out of order) would be skipped forever.
    #[test]
    fn versions_are_contiguous() {
        for (i, (v, _)) in MIGRATIONS.iter().enumerate() {
            assert_eq!(*v, i as i64 + 1);
        }
    }

    #[test]
    fn v2_seeds_the_id_sequence_from_existing_ids() {
        let mut conn = Connection::open_in_memory().unwrap();
        migrate_with(&mut conn, None, &MIGRATIONS[..1]).unwrap();
        let insert = |conn: &Connection, id: i64| {
            conn.execute(
                "INSERT INTO torrents (id, info_hash, name, size, piece_len, piece_count,
                     file_count, info_size, flags, discovered_at, metadata_at, last_seen_at)
                 VALUES (?1, ?2, 'x', 0, 0, 0, 0, 0, 0, 0, 0, 0)",
                rusqlite::params![id, vec![id as u8; 20]],
            )
            .unwrap();
        };
        insert(&conn, 7);
        insert(&conn, 41);
        conn.execute("DELETE FROM torrents WHERE id = 41", [])
            .unwrap();
        migrate_with(&mut conn, None, MIGRATIONS).unwrap();
        let next: i64 = conn
            .query_row("SELECT next FROM id_seq", [], |r| r.get(0))
            .unwrap();
        assert_eq!(next, 42);
        let (sources, first): (i64, Option<i64>) = conn
            .query_row("SELECT sources, first_source FROM torrents", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert_eq!((sources, first), (0, None), "pre-provenance rows");
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

    #[test]
    fn existing_snapshot_does_not_block_migration() {
        // A migration that failed after its snapshot leaves the file behind;
        // the retry must still be able to snapshot and migrate.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.db");
        let mut conn = Connection::open(&path).unwrap();
        migrate_with(&mut conn, Some(&path), &FAKE[..1]).unwrap();
        std::fs::write(dir.path().join("t.pre-v2.db"), b"older snapshot").unwrap();
        migrate_with(&mut conn, Some(&path), FAKE).unwrap();
        assert_eq!(version(&conn), 2);
        assert_eq!(
            std::fs::read(dir.path().join("t.pre-v2.db")).unwrap(),
            b"older snapshot",
            "existing snapshots are never overwritten"
        );
        let backup = Connection::open(dir.path().join("t.pre-v2.1.db")).unwrap();
        assert_eq!(version(&backup), 1);
    }
}
