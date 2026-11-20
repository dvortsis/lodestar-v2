//! Identify claims in snapshots never outlive their source's retention
//! (X-snapshot-claims-retention, `docs/decisions/2026-11-20-X-snapshot-claims-retention.md`).
//!
//! A snapshot is kept for [`RETAIN_SECS`](super::RETAIN_SECS) and holds the
//! claims as they were when it was taken, so a lookup service's claim near
//! its limit would outlive that limit inside the snapshot. Each purge pass
//! therefore drops, for every source with a terms record, the claims the
//! live retention sweep (`identify::sweep_expired` in `lodestar-pipeline`)
//! would drop at the same moment, or more:
//!
//! - **the retention** is the record's, read by [`max_age_secs`]. A record
//!   that doesn't read gets the default, 180 days, as live. Where the live
//!   store records terms for the same plugin, the shorter of the two
//!   applies: the host rewrites a restored store's record with the
//!   plugin's current terms when it registers it;
//! - **the clock** is `now`, but never behind the snapshot's own
//!   high-water mark for the source, nor behind the live store's mark for
//!   the same plugin, so a clock that steps back keeps nothing;
//! - **a claim stamped more than [`FUTURE_SLACK_SECS`] ahead** has no age
//!   to trust and goes, as live.
//!
//! A source with no terms record in the snapshot or the live store isn't
//! an identify source (the runtime records terms before a plugin's first
//! claim), so its claims stay, as live.
//!
//! A snapshot whose claims can't be checked however often the pass tries
//! ([`persistent`]) fails closed: the hourly pass deletes it and a
//! rollback refuses it (`super::purge_snapshots`, `super::purge_for_restore`,
//! `super::apply_source_purges`). Any other error is retried next pass.
//! The hourly pass spends at most [`PASS_BUDGET`] deleting claims.
//!
//! `lodestar-store` doesn't depend on `lodestar-sources`, where `Terms`
//! lives (it depends on the store), so this is a small reader of the
//! record's stored format. `crates/pipeline/tests/snapshot_terms.rs` pins
//! it, the keys and the slack to `Terms::record`, `Terms::from_record` and
//! `Retention::drop_after`.

use std::cell::Cell;
use std::collections::HashMap;
use std::time::{Duration, Instant};

use rusqlite::{Connection, OptionalExtension, params};

/// The `source_state` key of a plugin's terms record (`identify::TERMS_KEY`).
pub const TERMS_KEY: &str = "enr10/terms";
/// The `source_state` key of the retention clock's high-water mark
/// (`identify::CLOCK_KEY`), eight bytes, big-endian Unix seconds.
pub const CLOCK_KEY: &str = "enr10/clock";
/// A claim stamped further ahead of the clock than this is dropped
/// (`identify::FUTURE_SLACK_SECS`).
pub const FUTURE_SLACK_SECS: i64 = 3_600;

const DAY: u64 = 86_400;
/// `Retention::DEFAULT`: what an unreadable record means.
pub const DEFAULT_MAX_AGE_SECS: u64 = 180 * DAY;
/// `Retention::MIN_MAX_AGE`: a shorter limit reads as this.
const MIN_MAX_AGE_SECS: u64 = 2 * DAY;
/// `Terms::record`'s version byte.
const RECORD_VERSION: u8 = 1;

/// Claim rows one transaction deletes. The bundled SQLite has no
/// `DELETE … LIMIT`, so the rows are chosen first, by key.
pub(crate) const CHUNK: usize = 2_000;

/// Claim rows the hourly pass deletes from one snapshot at most, a
/// backstop under [`PASS_BUDGET`]. A rollback's copy has no limit: it must
/// hold none before it's restored.
pub(crate) const MAX_PER_PASS: usize = 1_000_000;

/// How long one hourly pass spends deleting claims, across every
/// snapshot, while it holds the writer's store: under 1 % of the hour.
/// Only the deletes count against it, not the rest of the purge, and the
/// pass starts at a different snapshot each hour, so a backlog in one
/// can't starve the others. Checked between chunks, so a pass overruns it
/// by one chunk at most. A backlog left over goes on the next pass (a
/// warning names it), and the one-day margin before the limit leaves 24
/// passes for it.
pub(crate) const PASS_BUDGET: Duration = Duration::from_secs(30);

/// How much one purge may delete: `rows` per snapshot, in what's left of
/// the pass's `budget`, which each purge draws down by the time it spends.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Limit<'a> {
    pub(crate) rows: usize,
    pub(crate) budget: &'a Cell<Duration>,
}

/// Whether `e` shows the snapshot itself can't be checked, however often
/// the pass tries: damaged, not a database, or missing a table or column
/// the check reads. Then the snapshot fails closed. A busy lock, an I/O
/// error, a full disk, no memory or an interrupt is retried on the next
/// pass instead: the one-day margin before the limit leaves time.
pub(crate) fn persistent(e: &crate::Error) -> bool {
    match e {
        crate::Error::Corrupt(_) => true,
        crate::Error::Sqlite(e) => persistent_sqlite(e),
        _ => false,
    }
}

pub(crate) fn persistent_sqlite(e: &rusqlite::Error) -> bool {
    use rusqlite::ErrorCode;
    if matches!(
        e.sqlite_error_code(),
        Some(ErrorCode::DatabaseCorrupt | ErrorCode::NotADatabase)
    ) {
        return true;
    }
    let text = e.to_string();
    text.contains("no such table") || text.contains("no such column")
}

/// The retention limit, in seconds, a stored terms record holds: the
/// record's `max_age`, floored at two days, or the default when the record
/// doesn't read. Exactly `Terms::from_record(v).map_or(Retention::DEFAULT,
/// |t| t.retention)`.
pub fn max_age_secs(record: &[u8]) -> u64 {
    read_max_age(record)
        .unwrap_or(DEFAULT_MAX_AGE_SECS)
        .max(MIN_MAX_AGE_SECS)
}

fn read_max_age(v: &[u8]) -> Option<u64> {
    let [RECORD_VERSION, flags, rest @ ..] = v else {
        return None;
    };
    if flags & !0b111 != 0 {
        return None;
    }
    Some(u64::from_be_bytes(rest.try_into().ok()?))
}

/// The age, in seconds, at which a claim under `max_age` is dropped: one
/// day less, so a sweep that runs late still drops it before the limit
/// (`Retention::drop_after`).
pub fn drop_after(max_age: u64) -> i64 {
    i64::try_from(max_age.max(MIN_MAX_AGE_SECS))
        .unwrap_or(i64::MAX)
        .saturating_sub(DAY as i64)
}

/// A clock mark's value, as the live sweep reads it: the first eight bytes,
/// big-endian. `None` when it's shorter.
pub fn read_clock(v: &[u8]) -> Option<i64> {
    Some(i64::from_be_bytes(v.get(..8)?.try_into().ok()?))
}

/// The claims of one source that are due: those stamped before `before`
/// or after `after`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Window {
    pub before: i64,
    pub after: i64,
}

/// The window the live sweep would use for a source with retention
/// `max_age` at clock `now`: dropped at its drop age, or stamped beyond the
/// slack.
pub fn window(max_age: u64, now: i64) -> Window {
    Window {
        before: now.saturating_sub(drop_after(max_age)).saturating_add(1),
        after: now.saturating_add(FUTURE_SLACK_SECS),
    }
}

/// What the live store says about one plugin: its recorded retention
/// limit and its retention mark, each if it has one.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct LivePlugin {
    pub(crate) max_age: Option<u64>,
    pub(crate) mark: Option<i64>,
}

/// What the live store says about identify sources, by plugin, read once
/// per pass.
#[derive(Debug, Default)]
pub(crate) struct LiveTerms(pub(crate) HashMap<String, LivePlugin>);

impl LiveTerms {
    pub(crate) fn read(conn: &Connection) -> rusqlite::Result<LiveTerms> {
        if !super::has_table(conn, "source_state")? {
            return Ok(LiveTerms::default());
        }
        let mut out = HashMap::new();
        for (_, plugin, record, mark) in source_rows(conn)? {
            out.insert(
                plugin,
                LivePlugin {
                    max_age: record.as_deref().map(max_age_secs),
                    mark: mark.as_deref().and_then(read_clock),
                },
            );
        }
        Ok(LiveTerms(out))
    }
}

/// Every source as `(id, plugin, terms record, retention mark)`.
type SourceRow = (i64, String, Option<Vec<u8>>, Option<Vec<u8>>);

fn source_rows(conn: &Connection) -> rusqlite::Result<Vec<SourceRow>> {
    let mut stmt = conn.prepare(
        "SELECT s.id, s.plugin,
                (SELECT value FROM source_state WHERE source_id = s.id AND key = ?1),
                (SELECT value FROM source_state WHERE source_id = s.id AND key = ?2)
         FROM sources s",
    )?;
    stmt.query_map([TERMS_KEY, CLOCK_KEY], |r| {
        Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
    })?
    .collect()
}

/// Whether `conn` holds stamped claims (V15's column).
pub(crate) fn stamped(conn: &Connection) -> rusqlite::Result<bool> {
    has_column(conn, "torrent_claims", "stored_at")
}

/// Whether `conn` has everything else the claims purge reads and writes:
/// sources, their state, and the backfill's marker.
pub(crate) fn checkable(conn: &Connection) -> rusqlite::Result<bool> {
    Ok(super::has_table(conn, "sources")?
        && super::has_table(conn, "source_state")?
        && has_column(conn, "torrent_parsed", "parser_ver")?)
}

fn has_column(conn: &Connection, table: &str, column: &str) -> rusqlite::Result<bool> {
    Ok(conn
        .query_row(
            "SELECT 1 FROM pragma_table_info(?1) WHERE name = ?2",
            [table, column],
            |_| Ok(()),
        )
        .optional()?
        .is_some())
}

/// One source's due claims in a snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Due {
    source: i64,
    window: Window,
    /// The clock used, and whether the snapshot's own mark is behind it.
    now: i64,
    mark_behind: bool,
    /// Whether any claim is in the window.
    any: bool,
}

/// Every source with a terms record in `conn` (a snapshot at a schema with
/// claim stamps) or, for the same plugin, in the live store, with the
/// window its claims are dropped by at `now`: the clock never behind the
/// snapshot's mark for the source nor the live store's for the plugin.
pub(crate) fn due(conn: &Connection, live: &LiveTerms, now: i64) -> rusqlite::Result<Vec<Due>> {
    let mut out = Vec::new();
    let mut any = conn.prepare(
        "SELECT 1 FROM torrent_claims WHERE source_id = ?1 AND stored_at < ?2
         UNION ALL
         SELECT 1 FROM torrent_claims WHERE source_id = ?1 AND stored_at > ?3
         LIMIT 1",
    )?;
    for (source, plugin, record, mark) in source_rows(conn)? {
        let own = record.as_deref().map(max_age_secs);
        let theirs = live.0.get(&plugin).copied().unwrap_or_default();
        let max_age = match (own, theirs.max_age) {
            (Some(a), Some(b)) => a.min(b),
            (Some(a), None) | (None, Some(a)) => a,
            (None, None) => continue,
        };
        let mark = mark.as_deref().and_then(read_clock);
        let clock = now
            .max(mark.unwrap_or(i64::MIN))
            .max(theirs.mark.unwrap_or(i64::MIN));
        let window = window(max_age, clock);
        let found = any
            .query_row(params![source, window.before, window.after], |_| Ok(()))
            .optional()?
            .is_some();
        out.push(Due {
            source,
            window,
            now: clock,
            mark_behind: mark.is_none_or(|m| m < clock),
            any: found,
        });
    }
    Ok(out)
}

impl Due {
    pub(crate) fn any(&self) -> bool {
        self.any
    }

    pub(crate) fn mark_behind(&self) -> bool {
        self.mark_behind
    }
}

/// Deletes the due claims of every source in `due`, [`CHUNK`] rows per
/// transaction, within `limit` (none: every one). Each deleted row
/// marks its torrent for the backfill, as live. Resumable without a
/// cursor: a deleted row no longer matches. Returns the rows deleted and
/// whether any due row is left.
pub(crate) fn purge(
    conn: &mut Connection,
    due: &[Due],
    limit: Option<Limit<'_>>,
) -> rusqlite::Result<(usize, bool)> {
    let start = Instant::now();
    let deadline = limit.map(|l| start + l.budget.get());
    let result = purge_within(conn, due, limit.map(|l| l.rows), deadline);
    if let Some(l) = limit {
        l.budget.set(l.budget.get().saturating_sub(start.elapsed()));
    }
    result
}

fn purge_within(
    conn: &mut Connection,
    due: &[Due],
    rows: Option<usize>,
    deadline: Option<Instant>,
) -> rusqlite::Result<(usize, bool)> {
    // `done` counts the rows chosen, for the cap; `deleted` the rows gone.
    let (mut done, mut deleted) = (0usize, 0usize);
    for d in due.iter().filter(|d| d.any) {
        loop {
            let room = rows.map_or(CHUNK, |r| r.saturating_sub(done).min(CHUNK));
            if room == 0 || deadline.is_some_and(|d| Instant::now() >= d) {
                return Ok((deleted, true));
            }
            let tx = crate::begin_write(conn)?;
            let ids = {
                let mut stmt = tx.prepare_cached(
                    "SELECT torrent_id FROM torrent_claims WHERE source_id = ?1 AND stored_at < ?2
                     UNION ALL
                     SELECT torrent_id FROM torrent_claims WHERE source_id = ?1 AND stored_at > ?3
                     LIMIT ?4",
                )?;
                stmt.query_map(
                    params![d.source, d.window.before, d.window.after, room as i64],
                    |r| r.get::<_, i64>(0),
                )?
                .collect::<rusqlite::Result<Vec<i64>>>()?
            };
            {
                let mut delete = tx.prepare_cached(
                    "DELETE FROM torrent_claims WHERE torrent_id = ?1 AND source_id = ?2",
                )?;
                let mut mark = tx.prepare_cached(
                    "UPDATE torrent_parsed SET parser_ver = 0 WHERE torrent_id = ?1",
                )?;
                for id in &ids {
                    if delete.execute(params![id, d.source])? > 0 {
                        mark.execute([id])?;
                        deleted += 1;
                    }
                }
            }
            tx.commit()?;
            done += ids.len();
            if ids.len() < room {
                break;
            }
        }
    }
    Ok((deleted, false))
}

/// Raises each source's retention mark in `conn` to the clock its purge
/// used, so a restored store's own sweep never runs behind the store it
/// replaced. In the caller's transaction.
pub(crate) fn raise_marks(conn: &Connection, due: &[Due]) -> rusqlite::Result<()> {
    let mut stmt = conn.prepare_cached(
        "INSERT INTO source_state (source_id, key, value) VALUES (?1, ?2, ?3)
         ON CONFLICT (source_id, key) DO UPDATE SET value = excluded.value",
    )?;
    for d in due.iter().filter(|d| d.mark_behind) {
        stmt.execute(params![d.source, CLOCK_KEY, &d.now.to_be_bytes()[..]])?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::path::{Path, PathBuf};

    use proptest::prelude::*;
    use rusqlite::Connection;

    use super::*;
    use crate::schema::MIGRATIONS;
    use crate::{SourceId, Store, StoreConfig};

    const DAY: i64 = 86_400;
    const T: i64 = 1_800_000_000;

    /// A terms record as `Terms::record` writes it, with flags 0.
    fn record(days: u64) -> Vec<u8> {
        let mut v = vec![RECORD_VERSION, 0];
        v.extend((days * DAY as u64).to_be_bytes());
        v
    }

    fn torrent(conn: &Connection, id: i64) {
        let mut hash = [0u8; 20];
        hash[..8].copy_from_slice(&id.to_be_bytes());
        conn.execute(
            "INSERT INTO torrents (id, info_hash, name, size, piece_len, piece_count,
                 file_count, info_size, flags, discovered_at, metadata_at, last_seen_at)
             VALUES (?1, ?2, 't', 0, 0, 0, 0, 0, 0, 0, 0, 0)",
            params![id, &hash[..]],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO torrent_parsed (torrent_id, parser_ver, data) VALUES (?1, 7, x'00')",
            [id],
        )
        .unwrap();
    }

    fn claim(conn: &Connection, id: i64, source: i64, at: i64) {
        conn.execute(
            "INSERT INTO torrent_claims (torrent_id, source_id, flags, data, stored_at)
             VALUES (?1, ?2, 0, x'00000000000000', ?3)",
            params![id, source, at],
        )
        .unwrap();
    }

    fn state(conn: &Connection, source: i64, key: &str, value: &[u8]) {
        conn.execute(
            "INSERT INTO source_state (source_id, key, value) VALUES (?1, ?2, ?3)
             ON CONFLICT (source_id, key) DO UPDATE SET value = excluded.value",
            params![source, key, value],
        )
        .unwrap();
    }

    /// The stamps a database holds, by torrent, for `source`.
    fn stamps(path: &Path, source: i64) -> Vec<(i64, i64)> {
        let conn = Connection::open(path).unwrap();
        stamps_in(&conn, source)
    }

    fn stamps_in(conn: &Connection, source: i64) -> Vec<(i64, i64)> {
        let mut stmt = conn
            .prepare(
                "SELECT torrent_id, stored_at FROM torrent_claims WHERE source_id = ?1
                 ORDER BY torrent_id",
            )
            .unwrap();
        stmt.query_map([source], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    }

    fn parser_ver(path: &Path, id: i64) -> i64 {
        Connection::open(path)
            .unwrap()
            .query_row(
                "SELECT parser_ver FROM torrent_parsed WHERE torrent_id = ?1",
                [id],
                |r| r.get(0),
            )
            .unwrap()
    }

    /// A live store with three sources: `tmdb` (10-day terms), `wiki`
    /// (default terms) and `feed` (no terms: not an identify source).
    fn live(dir: &Path) -> (Store, PathBuf, [SourceId; 3]) {
        let db = dir.join("lodestar.db");
        let mut store = Store::open(&db, StoreConfig::default()).unwrap();
        let tmdb = store.register_source("tmdb", "1", T).unwrap();
        let wiki = store.register_source("wiki", "1", T).unwrap();
        let feed = store.register_source("feed", "1", T).unwrap();
        store
            .set_plugin_state(tmdb, TERMS_KEY, Some(&record(10)))
            .unwrap();
        store
            .set_plugin_state(wiki, TERMS_KEY, Some(&record(180)))
            .unwrap();
        (store, db, [tmdb, wiki, feed])
    }

    /// A snapshot of the live store, dated `T`.
    fn take(store: &Store, db: &Path) -> PathBuf {
        let path = crate::schema::snapshot_path(db, 99);
        store
            .connection()
            .execute("VACUUM INTO ?1", [path.to_string_lossy()])
            .unwrap();
        super::super::set_modified(&path, T).unwrap();
        path
    }

    #[test]
    fn the_pass_drops_expired_identify_claims_and_nothing_else() {
        let dir = tempfile::tempdir().unwrap();
        let (store, db, [tmdb, wiki, feed]) = live(dir.path());
        let (tmdb, wiki, feed) = (
            i64::from(tmdb.get()),
            i64::from(wiki.get()),
            i64::from(feed.get()),
        );
        let conn = store.connection();
        for id in 1..=6 {
            torrent(conn, id);
        }
        claim(conn, 1, tmdb, T - 9 * DAY - 1); // past 10 days less a day
        claim(conn, 2, tmdb, T - 8 * DAY); // within
        claim(conn, 3, tmdb, T + 2 * 3_600); // stamped beyond the slack
        claim(conn, 4, wiki, T - 100 * DAY); // within its 180 days
        claim(conn, 5, feed, T - 1_000 * DAY); // not an identify source
        claim(conn, 6, wiki, T - 9 * DAY - 1); // another source's terms
        let snap = take(&store, &db);
        let mut store = store;
        let r = store.purge_snapshots(T, 0);
        assert!(r.warnings.is_empty(), "{:?}", r.warnings);
        assert_eq!(r.claims, 2);
        assert_eq!(r.purged, vec![snap.clone()]);
        assert_eq!(stamps(&snap, tmdb), vec![(2, T - 8 * DAY)]);
        assert_eq!(
            stamps(&snap, wiki),
            vec![(4, T - 100 * DAY), (6, T - 9 * DAY - 1)]
        );
        assert_eq!(stamps(&snap, feed), vec![(5, T - 1_000 * DAY)]);
        // Dropped claims mark their torrent for the backfill, as live; the
        // torrents themselves stay.
        assert_eq!(parser_ver(&snap, 1), 0);
        assert_eq!(parser_ver(&snap, 3), 0);
        assert_eq!(parser_ver(&snap, 2), 7);
        assert_eq!(parser_ver(&snap, 5), 7);
        // The live store is the live sweep's: untouched here.
        assert_eq!(stamps_in(store.connection(), tmdb).len(), 3);
        // Its time is kept, and the next pass has nothing to do.
        assert_eq!(super::super::modified(&snap), Some(T));
        let again = store.purge_snapshots(T + 3_600, 0);
        assert!(again.purged.is_empty() && again.warnings.is_empty());
        // A day on, the claim within its terms then is due too.
        let later = store.purge_snapshots(T + DAY, 0);
        assert_eq!(later.claims, 1);
        assert!(stamps(&snap, tmdb).is_empty());
    }

    #[test]
    fn a_clock_behind_a_mark_still_drops() {
        let dir = tempfile::tempdir().unwrap();
        let (mut store, db, [tmdb, wiki, _]) = live(dir.path());
        let (tmdb, wiki) = (i64::from(tmdb.get()), i64::from(wiki.get()));
        let conn = store.connection();
        torrent(conn, 1);
        torrent(conn, 2);
        claim(conn, 1, tmdb, T - 5 * DAY);
        claim(conn, 2, wiki, T - 170 * DAY);
        // The snapshot's own mark for tmdb is 30 days ahead of the clock.
        state(conn, tmdb, CLOCK_KEY, &(T + 30 * DAY).to_be_bytes());
        let snap = take(&store, &db);
        // The live store holds no mark: only the snapshot's counts.
        store
            .connection()
            .execute("DELETE FROM source_state WHERE key = ?1", [CLOCK_KEY])
            .unwrap();
        let r = store.purge_snapshots(T, 0);
        assert_eq!(r.claims, 1, "{:?}", r.warnings);
        assert!(stamps(&snap, tmdb).is_empty());
        assert_eq!(stamps(&snap, wiki).len(), 1, "wiki's own mark is behind");
        // A live mark 10 days ahead drops wiki's 170-day-old claim too.
        state(
            store.connection(),
            wiki,
            CLOCK_KEY,
            &(T + 10 * DAY).to_be_bytes(),
        );
        let r = store.purge_snapshots(T, 0);
        assert_eq!(r.claims, 1, "{:?}", r.warnings);
        assert!(stamps(&snap, wiki).is_empty());
    }

    #[test]
    fn unreadable_missing_and_tighter_records() {
        let dir = tempfile::tempdir().unwrap();
        let (mut store, db, [tmdb, wiki, feed]) = live(dir.path());
        let (tmdb, wiki, feed) = (
            i64::from(tmdb.get()),
            i64::from(wiki.get()),
            i64::from(feed.get()),
        );
        let conn = store.connection();
        for id in 1..=5 {
            torrent(conn, id);
        }
        // An unreadable record reads as the default, 180 days.
        state(conn, wiki, TERMS_KEY, &[9, 9]);
        claim(conn, 1, wiki, T - 179 * DAY);
        claim(conn, 2, wiki, T - 170 * DAY);
        // tmdb's snapshot record says 400 days; the live one, 10.
        state(conn, tmdb, TERMS_KEY, &record(400));
        claim(conn, 3, tmdb, T - 20 * DAY);
        claim(conn, 4, feed, T - 20 * DAY);
        let snap = take(&store, &db);
        state(store.connection(), tmdb, TERMS_KEY, &record(10));
        // The live store records terms for `feed` the snapshot lacks.
        state(store.connection(), feed, TERMS_KEY, &record(10));
        let r = store.purge_snapshots(T, 0);
        assert!(r.warnings.is_empty(), "{:?}", r.warnings);
        assert_eq!(r.claims, 3);
        assert_eq!(stamps(&snap, wiki), vec![(2, T - 170 * DAY)]);
        assert!(stamps(&snap, tmdb).is_empty());
        assert!(stamps(&snap, feed).is_empty());
        assert_eq!(max_age_secs(&[]), DEFAULT_MAX_AGE_SECS);
        assert_eq!(max_age_secs(&record(0)), 2 * DAY as u64);
    }

    /// An hour: more than any test spends.
    fn hour() -> Cell<Duration> {
        Cell::new(Duration::from_secs(3_600))
    }

    /// A schema-current database in memory with sources 1 (`a`, the
    /// given record) and 2 (`b`, no record).
    fn memory(record_a: &[u8]) -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE schema_migrations (version INTEGER PRIMARY KEY,
                 applied_at INTEGER NOT NULL) STRICT;",
        )
        .unwrap();
        for (_, sql) in MIGRATIONS {
            conn.execute_batch(sql).unwrap();
        }
        conn.execute_batch(
            "INSERT INTO sources (id, plugin, version, state, added_at)
             VALUES (1, 'a', '1', 0, 0), (2, 'b', '1', 0, 0);",
        )
        .unwrap();
        state(&conn, 1, TERMS_KEY, record_a);
        conn
    }

    #[test]
    fn an_interrupted_purge_completes_on_the_next() {
        let mut conn = memory(&record(10));
        let n = CHUNK as i64 + 7;
        conn.execute_batch("BEGIN").unwrap();
        for id in 1..=n {
            torrent(&conn, id);
            claim(&conn, id, 1, T - 100 * DAY);
            claim(&conn, id, 2, T - 100 * DAY);
        }
        torrent(&conn, n + 1);
        claim(&conn, n + 1, 1, T);
        conn.execute_batch("COMMIT").unwrap();
        let due = due(&conn, &LiveTerms::default(), T).unwrap();
        // Stopped partway (a cap here; a crash between chunks is the same:
        // each chunk commits alone).
        assert_eq!(
            purge(
                &mut conn,
                &due,
                Some(Limit {
                    rows: 5,
                    budget: &hour()
                })
            )
            .unwrap(),
            (5, true)
        );
        assert_eq!(stamps_in(&conn, 1).len(), CHUNK + 3);
        // The next pass, over more than one chunk, finishes it.
        let due = super::due(&conn, &LiveTerms::default(), T).unwrap();
        assert_eq!(
            purge(
                &mut conn,
                &due,
                Some(Limit {
                    rows: MAX_PER_PASS,
                    budget: &hour()
                })
            )
            .unwrap(),
            (CHUNK + 2, false)
        );
        assert_eq!(stamps_in(&conn, 1), vec![(n + 1, T)]);
        assert_eq!(stamps_in(&conn, 2).len(), n as usize);
        let due = super::due(&conn, &LiveTerms::default(), T).unwrap();
        assert!(!due.iter().any(Due::any));
    }

    #[test]
    fn a_restore_brings_back_no_expired_claim_and_keeps_the_clock() {
        let dir = tempfile::tempdir().unwrap();
        let (store, db, [tmdb, _, _]) = live(dir.path());
        let tmdb = i64::from(tmdb.get());
        let conn = store.connection();
        torrent(conn, 1);
        torrent(conn, 2);
        claim(conn, 1, tmdb, T - 6 * DAY);
        claim(conn, 2, tmdb, T - DAY);
        state(conn, tmdb, CLOCK_KEY, &T.to_be_bytes());
        let snap = take(&store, &db);
        // The live store's clock has reached T + 5 days; the wall clock
        // has stepped back to T.
        state(conn, tmdb, CLOCK_KEY, &(T + 5 * DAY).to_be_bytes());
        let staged = dir.path().join("lodestar.rollback-x.tmp");
        std::fs::copy(&snap, &staged).unwrap();
        super::super::purge_for_restore(store.connection(), &staged, T)
            .unwrap()
            .unwrap();
        // The claim 6 days old by the wall clock is 11 by the live mark, so
        // it's gone (dropped at 9); the one a day old stays, as the live
        // sweep keeps it until 9 days by its mark.
        assert_eq!(stamps(&staged, tmdb), vec![(2, T - DAY)]);
        let mark: Vec<u8> = Connection::open(&staged)
            .unwrap()
            .query_row(
                "SELECT value FROM source_state WHERE source_id = ?1 AND key = ?2",
                params![tmdb, CLOCK_KEY],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(read_clock(&mark), Some(T + 5 * DAY));
    }

    /// The update rollback's path (`apply_source_purges`, with no source
    /// purge recorded) drops expired claims and raises the marks, by the
    /// wall clock.
    #[test]
    fn an_update_rollback_brings_back_no_expired_claim() {
        let dir = tempfile::tempdir().unwrap();
        let (store, db, [tmdb, _, _]) = live(dir.path());
        let tmdb = i64::from(tmdb.get());
        let now = super::super::modified(&db).unwrap();
        let conn = store.connection();
        torrent(conn, 1);
        torrent(conn, 2);
        claim(conn, 1, tmdb, now - 30 * DAY);
        claim(conn, 2, tmdb, now - DAY);
        let snap = take(&store, &db);
        super::super::apply_source_purges(store.connection(), &snap, now).unwrap();
        assert_eq!(stamps(&snap, tmdb), vec![(2, now - DAY)]);
        let conn = Connection::open(&snap).unwrap();
        let mark: Vec<u8> = conn
            .query_row(
                "SELECT value FROM source_state WHERE source_id = ?1 AND key = ?2",
                params![tmdb, CLOCK_KEY],
                |r| r.get(0),
            )
            .unwrap();
        assert!(read_clock(&mark).unwrap() >= now);
        drop(conn);
        assert_eq!(super::super::modified(&snap), Some(T));
        assert!(!super::super::side_exists(&snap, "-wal"));
    }

    /// A snapshot whose claims can't be checked is deleted, never kept.
    #[test]
    fn a_snapshot_whose_claims_cannot_be_checked_is_deleted() {
        let dir = tempfile::tempdir().unwrap();
        let (mut store, db, [tmdb, _, _]) = live(dir.path());
        torrent(store.connection(), 1);
        claim(store.connection(), 1, i64::from(tmdb.get()), T - 30 * DAY);
        let snap = take(&store, &db);
        Connection::open(&snap)
            .unwrap()
            .execute_batch("DROP TABLE source_state")
            .unwrap();
        super::super::set_modified(&snap, T).unwrap();
        let r = store.purge_snapshots(T, 0);
        assert_eq!(r.unchecked, vec![snap.clone()], "{:?}", r.warnings);
        assert!(!snap.exists());
        // A rollback refuses such a copy.
        let staged = dir.path().join("lodestar.rollback-y.tmp");
        let snap = take(&store, &db);
        std::fs::copy(&snap, &staged).unwrap();
        Connection::open(&staged)
            .unwrap()
            .execute_batch("DROP TABLE source_state")
            .unwrap();
        assert!(super::super::purge_for_restore(store.connection(), &staged, T).is_err());
    }

    /// A held lock never deletes a snapshot: it's retried, and the next
    /// pass purges it. The exclusive lock stops `purge_one` at its first
    /// schema read, so this covers the purge's existing `Err` path and the
    /// daily stuck report; `a_lock_in_the_claims_stage_is_retried` reaches
    /// the claims stage's own retry branch, and `only_lasting_errors_fail_closed`
    /// covers the classifier the rest of the claims stage shares (its
    /// read and delete errors can't be met here without fault injection).
    #[test]
    fn a_busy_snapshot_is_kept_and_retried() {
        let dir = tempfile::tempdir().unwrap();
        let (mut store, db, [tmdb, _, _]) = live(dir.path());
        let tmdb = i64::from(tmdb.get());
        torrent(store.connection(), 1);
        claim(store.connection(), 1, tmdb, T - 30 * DAY);
        let snap = take(&store, &db);
        let holder = Connection::open(&snap).unwrap();
        holder.execute_batch("BEGIN EXCLUSIVE").unwrap();
        let r = store.purge_snapshots(T, 0);
        assert!(r.unchecked.is_empty() && !r.warnings.is_empty());
        assert_eq!(r.stuck, vec![(snap.clone(), "busy")]);
        // Still stuck an hour on: not reported again until a day has gone.
        let r = store.purge_snapshots(T + 3_600, 0);
        assert!(r.stuck.is_empty() && !r.warnings.is_empty());
        let r = store.purge_snapshots(T + 86_400, 0);
        assert_eq!(r.stuck.len(), 1);
        holder.execute_batch("ROLLBACK").unwrap();
        drop(holder);
        assert!(snap.exists());
        let r = store.purge_snapshots(T + 86_400 + 3_600, 0);
        assert_eq!(r.claims, 1, "{:?}", r.warnings);
        assert!(r.stuck.is_empty());
        assert!(stamps(&snap, tmdb).is_empty());
    }

    /// A newer schema's claims stage meets a writer's lock (the write lock
    /// lets it read, so the error comes from its own write): retried, never
    /// deleted, and reported as stuck.
    #[test]
    fn a_lock_in_the_claims_stage_is_retried() {
        let dir = tempfile::tempdir().unwrap();
        let (mut store, db, [tmdb, _, _]) = live(dir.path());
        let tmdb = i64::from(tmdb.get());
        torrent(store.connection(), 1);
        claim(store.connection(), 1, tmdb, T - 30 * DAY);
        let snap = take(&store, &db);
        Connection::open(&snap)
            .unwrap()
            .execute(
                "INSERT INTO schema_migrations (version, applied_at) VALUES (?1, 0)",
                [crate::snapshots::Plan::KNOWN_THROUGH + 1],
            )
            .unwrap();
        super::super::set_modified(&snap, T).unwrap();
        let holder = Connection::open(&snap).unwrap();
        holder
            .execute_batch("BEGIN IMMEDIATE; UPDATE torrents SET name = name WHERE id = 1;")
            .unwrap();
        let r = store.purge_snapshots(T, 0);
        assert!(r.unchecked.is_empty(), "{:?}", r.warnings);
        assert!(
            r.warnings
                .iter()
                .any(|w| w.contains("tried again next pass")),
            "{:?}",
            r.warnings
        );
        assert_eq!(r.stuck.len(), 1, "{:?}", r.warnings);
        holder.execute_batch("ROLLBACK").unwrap();
        drop(holder);
        let r = store.purge_snapshots(T + 3_600, 0);
        assert_eq!(r.claims, 1, "{:?}", r.warnings);
        assert!(stamps(&snap, tmdb).is_empty());
    }

    /// Damage, a file that isn't a database, and a missing table or column
    /// persist; busy, I/O, full, no memory and an interrupt pass.
    #[test]
    fn only_lasting_errors_fail_closed() {
        use rusqlite::ffi;
        let code = |c| rusqlite::Error::SqliteFailure(ffi::Error::new(c), None);
        for c in [ffi::SQLITE_CORRUPT, ffi::SQLITE_NOTADB] {
            assert!(persistent_sqlite(&code(c)), "{c}");
        }
        for c in [
            ffi::SQLITE_BUSY,
            ffi::SQLITE_LOCKED,
            ffi::SQLITE_IOERR,
            ffi::SQLITE_FULL,
            ffi::SQLITE_NOMEM,
            ffi::SQLITE_INTERRUPT,
            ffi::SQLITE_CANTOPEN,
        ] {
            assert!(!persistent_sqlite(&code(c)), "{c}");
        }
        let conn = Connection::open_in_memory().unwrap();
        let missing = conn.prepare("SELECT x FROM nowhere").unwrap_err();
        assert!(persistent_sqlite(&missing));
        conn.execute_batch("CREATE TABLE t (a)").unwrap();
        let column = conn.prepare("SELECT b FROM t").unwrap_err();
        assert!(persistent_sqlite(&column));
        assert!(persistent(&crate::Error::Corrupt("x")));
        assert!(!persistent(&crate::Error::Sqlite(code(ffi::SQLITE_BUSY))));
    }

    /// With the pass's budget spent, nothing more is deleted, and the rest
    /// is left for the next pass. A purge draws the budget down by the time
    /// it spends, and only that.
    #[test]
    fn the_time_budget_stops_the_purge() {
        let mut conn = memory(&record(10));
        torrent(&conn, 1);
        claim(&conn, 1, 1, T - 100 * DAY);
        let due = due(&conn, &LiveTerms::default(), T).unwrap();
        let spent = Cell::new(Duration::ZERO);
        let limit = Limit {
            rows: MAX_PER_PASS,
            budget: &spent,
        };
        assert_eq!(purge(&mut conn, &due, Some(limit)).unwrap(), (0, true));
        assert_eq!(stamps_in(&conn, 1).len(), 1);
        let budget = hour();
        let limit = Limit {
            rows: MAX_PER_PASS,
            budget: &budget,
        };
        assert_eq!(purge(&mut conn, &due, Some(limit)).unwrap(), (1, false));
        assert!(budget.get() < Duration::from_secs(3_600));
        assert!(budget.get() > Duration::from_secs(3_500));
    }

    /// A newer schema with stamped claims but without the tables the check
    /// reads is deleted: it can't be checked.
    #[test]
    fn a_newer_schema_that_cannot_be_checked_is_deleted() {
        let dir = tempfile::tempdir().unwrap();
        let (mut store, db, [tmdb, _, _]) = live(dir.path());
        torrent(store.connection(), 1);
        claim(store.connection(), 1, i64::from(tmdb.get()), T - 30 * DAY);
        let snap = take(&store, &db);
        Connection::open(&snap)
            .unwrap()
            .execute_batch(&format!(
                "INSERT INTO schema_migrations (version, applied_at) VALUES ({}, 0);
                 ALTER TABLE source_state RENAME TO plugin_state;",
                crate::snapshots::Plan::KNOWN_THROUGH + 1
            ))
            .unwrap();
        super::super::set_modified(&snap, T).unwrap();
        let r = store.purge_snapshots(T, 0);
        assert_eq!(r.unchecked, vec![snap.clone()], "{:?}", r.warnings);
        assert!(!snap.exists());
    }

    /// A schema newer than this build's plans loses its expired claims all
    /// the same; one without stamped claims is left as it was.
    #[test]
    fn a_newer_schema_loses_its_expired_claims() {
        let dir = tempfile::tempdir().unwrap();
        let (mut store, db, [tmdb, _, _]) = live(dir.path());
        let tmdb = i64::from(tmdb.get());
        torrent(store.connection(), 1);
        torrent(store.connection(), 2);
        claim(store.connection(), 1, tmdb, T - 30 * DAY);
        claim(store.connection(), 2, tmdb, T - DAY);
        let snap = take(&store, &db);
        let newer = Connection::open(&snap).unwrap();
        newer
            .execute(
                "INSERT INTO schema_migrations (version, applied_at) VALUES (?1, 0)",
                [crate::snapshots::Plan::KNOWN_THROUGH + 1],
            )
            .unwrap();
        drop(newer);
        super::super::set_modified(&snap, T).unwrap();
        let r = store.purge_snapshots(T, 0);
        assert_eq!(r.claims, 1, "{:?}", r.warnings);
        assert_eq!(stamps(&snap, tmdb), vec![(2, T - DAY)]);
        assert_eq!(super::super::modified(&snap), Some(T));
        // Without the stamps: nothing to check, nothing written.
        Connection::open(&snap)
            .unwrap()
            .execute_batch("DROP INDEX torrent_claims_stored; ALTER TABLE torrent_claims DROP COLUMN stored_at;")
            .unwrap();
        super::super::set_modified(&snap, T).unwrap();
        let r = store.purge_snapshots(T + DAY, 0);
        assert_eq!(r.claims, 0);
        assert!(r.unchecked.is_empty() && snap.exists());
    }

    proptest! {
        #![proptest_config(ProptestConfig { cases: 48, ..ProptestConfig::default() })]

        /// Whatever the terms, stamps, snapshot mark and pass clocks
        /// (stepping back included), after each pass no claim of the
        /// identify source is at or past its drop age by the clock used
        /// (never behind the mark), none is stamped beyond the slack,
        /// every other one stays, and no other source's claim is touched.
        #[test]
        fn snapshot_retention_holds_for_any_terms_claims_and_clocks(
            days in 0u64..400,
            garbled in any::<bool>(),
            claims in proptest::collection::vec((any::<bool>(), -500i64..5), 1..12),
            mark in proptest::option::of(-100i64..100),
            live_days in proptest::option::of(0u64..400),
            live_mark in proptest::option::of(-100i64..100),
            steps in proptest::collection::vec(-200i64..200, 1..8),
        ) {
            let rec = if garbled { vec![1, 8, 0] } else { record(days) };
            let own_max = if garbled { 180 * DAY } else { (days as i64).max(2) * DAY };
            // The live store's record for the same plugin, when it has one,
            // can only shorten it.
            let max_age = live_days.map_or(own_max, |d| own_max.min((d as i64).max(2) * DAY));
            let live_mark = live_mark.map(|m| T + m * DAY);
            let live = LiveTerms(HashMap::from([(
                "a".to_string(),
                LivePlugin {
                    max_age: live_days.map(|d| max_age_secs(&record(d))),
                    mark: live_mark,
                },
            )]));
            let mut conn = memory(&rec);
            let mark = mark.map(|m| T + m * DAY);
            if let Some(m) = mark {
                state(&conn, 1, CLOCK_KEY, &m.to_be_bytes());
            }
            let mut rows = Vec::new();
            for (i, (own, offset)) in claims.iter().enumerate() {
                let id = i as i64 + 1;
                torrent(&conn, id);
                let source = if *own { 1 } else { 2 };
                let at = T + offset * DAY;
                claim(&conn, id, source, at);
                rows.push((id, source, at));
            }
            let mut clock = T;
            let mut gone = vec![false; rows.len()];
            for step in steps {
                clock += step * DAY;
                let used = clock
                    .max(mark.unwrap_or(i64::MIN))
                    .max(live_mark.unwrap_or(i64::MIN));
                let d = due(&conn, &live, clock).unwrap();
                prop_assert_eq!(purge(&mut conn, &d, Some(Limit { rows: MAX_PER_PASS, budget: &hour() })).unwrap().1, false);
                let held = stamps_in(&conn, 1);
                for (n, &(id, source, at)) in rows.iter().enumerate() {
                    if source == 2 {
                        continue;
                    }
                    gone[n] |= at <= used - (max_age - DAY) || at > used + FUTURE_SLACK_SECS;
                    let got = held.iter().find(|(t, _)| *t == id).map(|(_, a)| *a);
                    prop_assert_eq!(got, (!gone[n]).then_some(at));
                    // Independently of the formula: nothing kept is as old
                    // as the limit by the clock used.
                    prop_assert!(got.is_none() || used - at < max_age);
                    prop_assert!(got.is_none() || at - used <= 3_600);
                }
                let others = stamps_in(&conn, 2);
                prop_assert_eq!(others.len(), rows.iter().filter(|r| r.1 == 2).count());
            }
        }
    }
}
