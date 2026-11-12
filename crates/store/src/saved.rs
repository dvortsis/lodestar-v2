//! Saved searches and their alerts (architecture §8 "Saved searches →
//! alerts", §10.8).
//!
//! The percolator (B9) runs each saved search over the torrents above its
//! `watermark` after an index commit, then records the hits and the new
//! watermark in one transaction ([`Store::record_percolation`]), so a
//! crash between the two can only re-run a range, never skip one; a
//! repeated hit is ignored. The store keeps the query text and AST as the
//! API gives them: parsing and compiling belong to the search crate.
//!
//! **Promoted torrents** (SRC-2b, `docs/decisions/2026-11-02-SRC-2b.md`)
//! keep an id below the watermarks, so the forward run never sees them.
//! [`Store::promote`] queues each kept id in `settings`, keyed by its
//! `reindex_log` entry; the percolator reads the queue in that order
//! ([`Store::promoted_due`]) and records the hits and drops the rows it
//! covered in one transaction ([`Store::record_promoted`]).

use rusqlite::{Connection, OptionalExtension, Transaction, params};

use crate::user_data::{check_name, check_text, sql_limit};
use crate::{Error, Result, Store, flags};

/// What a caller sets on a saved search.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SavedSearchInput<'a> {
    pub name: &'a str,
    /// The query as typed, at most 2,048 bytes (the parser's limit).
    pub query: &'a str,
    /// The parsed AST as JSON, at most 64 KiB.
    pub ast: &'a str,
    /// The sort the results open with, e.g. `-seeders`.
    pub sort: Option<&'a str>,
    /// Notify about new hits in the app (SSE).
    pub alert: bool,
    /// Serve the hits as an RSS feed.
    pub rss: bool,
    /// Other notification channels (webhook, ntfy, Discord, email): bits
    /// defined by the notifier (B9), persisted, append only.
    pub channels: u32,
    /// Paused: the percolator skips it. Its channel choices are kept, so
    /// resuming restores them (§10.8, "Pausing is a toggle").
    pub paused: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SavedSearch {
    pub id: i64,
    pub name: String,
    pub query: String,
    pub ast: String,
    pub sort: Option<String>,
    pub alert: bool,
    pub rss: bool,
    pub channels: u32,
    pub paused: bool,
    /// The highest torrent id evaluated against it.
    pub watermark: i64,
    pub created_at: i64,
    pub updated_at: i64,
}

/// A saved search with what the list view shows beside it (§10.8).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SavedSearchSummary {
    pub search: SavedSearch,
    pub alerts: u64,
    pub unread: u64,
    /// When its newest alert was recorded; `None` before its first hit.
    pub last_alert_at: Option<i64>,
}

/// One hit of a saved search.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Alert {
    pub id: i64,
    pub saved_search_id: i64,
    pub torrent_id: i64,
    pub created_at: i64,
    pub read_at: Option<i64>,
}

/// One saved search's result for a percolation run: every torrent id in
/// `(watermark, through]` that it matched.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SearchHits<'a> {
    pub saved_search_id: i64,
    pub torrent_ids: &'a [i64],
}

/// One queued promotion (see the module docs).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PromotedDue {
    /// Its `reindex_log` entry.
    pub seq: i64,
    pub torrent_id: i64,
    /// The highest saved-search id when it was promoted. Ids are
    /// `AUTOINCREMENT`, so a search above it was created later and
    /// doesn't alert on it (no backfill, as for the forward run).
    pub searches_through: i64,
}

/// The queue's `settings` keys: this prefix and the `reindex_log` seq in
/// 20 digits, so key order is seq order. Values are text, as every
/// setting a prefix read returns must be.
const PROMOTED: &str = "percolator.promoted.";

fn promoted_key(seq: i64) -> String {
    format!("{PROMOTED}{:020}", seq.max(0))
}

/// Queues a kept promotion whose `reindex_log` entry is `seq`, in the
/// promotion's own transaction.
pub(crate) fn queue_promoted(tx: &Connection, seq: i64) -> Result<()> {
    tx.prepare_cached(
        "INSERT INTO settings (key, value)
         SELECT ?1, CAST(coalesce(max(id), 0) AS TEXT) FROM saved_searches",
    )?
    .execute([promoted_key(seq)])?;
    Ok(())
}

const COLUMNS: &str =
    "id, name, query, ast, sort, alert, rss, channels, paused, watermark, created_at, updated_at";

fn search_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<SavedSearch> {
    Ok(SavedSearch {
        id: r.get(0)?,
        name: r.get(1)?,
        query: r.get(2)?,
        ast: r.get(3)?,
        sort: r.get(4)?,
        alert: r.get(5)?,
        rss: r.get(6)?,
        channels: r.get(7)?,
        paused: r.get(8)?,
        watermark: r.get(9)?,
        created_at: r.get(10)?,
        updated_at: r.get(11)?,
    })
}

fn alert_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Alert> {
    Ok(Alert {
        id: r.get(0)?,
        saved_search_id: r.get(1)?,
        torrent_id: r.get(2)?,
        created_at: r.get(3)?,
        read_at: r.get(4)?,
    })
}

fn check(s: &SavedSearchInput<'_>) -> Result<()> {
    check_name(s.name)?;
    if s.query.len() > 2048 {
        return Err(Error::Invalid("query is over 2,048 bytes"));
    }
    if s.ast.len() > 65_536 {
        return Err(Error::Invalid("ast is over 64 KiB"));
    }
    if let Some(sort) = s.sort {
        check_text(sort, 32, "sort must be 1-32 characters")?;
    }
    Ok(())
}

/// A JSON check the database refused is the caller's mistake.
fn invalid_ast(e: rusqlite::Error) -> Error {
    match e.sqlite_error_code() {
        Some(rusqlite::ErrorCode::ConstraintViolation) => Error::Invalid("ast is not JSON"),
        _ => Error::Sqlite(e),
    }
}

impl Store {
    /// Saves a search. `watermark` is the index's current one, so it alerts
    /// only on torrents that become searchable from now on.
    pub fn create_saved_search(
        &mut self,
        s: &SavedSearchInput<'_>,
        watermark: i64,
        now: i64,
    ) -> Result<i64> {
        check(s)?;
        self.conn
            .execute(
                "INSERT INTO saved_searches
                     (name, query, ast, sort, alert, rss, channels, paused, watermark,
                      created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?10)",
                params![
                    s.name,
                    s.query,
                    s.ast,
                    s.sort,
                    s.alert,
                    s.rss,
                    s.channels,
                    s.paused,
                    watermark.max(0),
                    now
                ],
            )
            .map_err(invalid_ast)?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn saved_search(&self, id: i64) -> Result<Option<SavedSearch>> {
        Ok(self
            .conn
            .prepare_cached(&format!(
                "SELECT {COLUMNS} FROM saved_searches WHERE id = ?1"
            ))?
            .query_row([id], search_row)
            .optional()?)
    }

    /// Every saved search, oldest first, with its alert counts.
    pub fn saved_searches(&self) -> Result<Vec<SavedSearchSummary>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT s.id, s.name, s.query, s.ast, s.sort, s.alert, s.rss, s.channels,
                 s.paused, s.watermark, s.created_at, s.updated_at,
                 (SELECT count(*) FROM alerts a WHERE a.saved_search_id = s.id),
                 (SELECT count(*) FROM alerts a
                      WHERE a.saved_search_id = s.id AND a.read_at IS NULL),
                 (SELECT a.created_at FROM alerts a WHERE a.saved_search_id = s.id
                      ORDER BY a.id DESC LIMIT 1)
             FROM saved_searches s ORDER BY s.id",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(SavedSearchSummary {
                search: search_row(r)?,
                alerts: crate::get_u64(r, 12)?,
                unread: crate::get_u64(r, 13)?,
                last_alert_at: r.get(14)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Replaces a saved search's fields; `false` if it doesn't exist. A
    /// changed query keeps the watermark: hits from then on are matched
    /// against the new query. Resuming a paused search moves its watermark
    /// up to `watermark` (the index's current one), so it doesn't alert on
    /// everything indexed while it was paused.
    pub fn update_saved_search(
        &mut self,
        id: i64,
        s: &SavedSearchInput<'_>,
        watermark: i64,
        now: i64,
    ) -> Result<bool> {
        check(s)?;
        let tx = crate::begin_write(&mut self.conn)?;
        let old: Option<(String, String, String)> = tx
            .prepare_cached("SELECT name, query, ast FROM saved_searches WHERE id = ?1")?
            .query_row([id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .optional()?;
        let Some((name, query, ast)) = old else {
            return Ok(false);
        };
        tx.execute(
            "UPDATE saved_searches SET name = ?2, query = ?3, ast = ?4, sort = ?5,
                 alert = ?6, rss = ?7, channels = ?8, paused = ?9, updated_at = ?11,
                 watermark = CASE WHEN paused = 1 AND ?9 = 0
                     THEN max(watermark, ?10) ELSE watermark END
             WHERE id = ?1",
            params![
                id, s.name, s.query, s.ast, s.sort, s.alert, s.rss, s.channels, s.paused,
                watermark, now
            ],
        )
        .map_err(invalid_ast)?;
        tx.commit()?;
        // The old text stays in WAL frames until a scrub (§2.17).
        if name != s.name || query != s.query || ast != s.ast {
            self.after_user_delete();
        }
        Ok(true)
    }

    /// Deletes a saved search with its alerts and feed tokens.
    pub fn delete_saved_search(&mut self, id: i64) -> Result<bool> {
        let deleted = self
            .conn
            .execute("DELETE FROM saved_searches WHERE id = ?1", [id])?;
        if deleted > 0 {
            self.after_user_delete();
        }
        Ok(deleted > 0)
    }

    /// The saved searches a percolation up to `through` has to run: not
    /// paused, and with a watermark below it. Oldest first.
    pub fn percolation_due(&self, through: i64) -> Result<Vec<SavedSearch>> {
        let mut stmt = self.conn.prepare_cached(&format!(
            "SELECT {COLUMNS} FROM saved_searches
             WHERE paused = 0 AND watermark < ?1 ORDER BY id"
        ))?;
        let rows = stmt.query_map([through], search_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Records one percolation run up to `through`, in one transaction:
    /// each search's hits become alerts and its watermark moves to
    /// `through`. Include every search that ran, with no hits if it matched
    /// nothing, so its watermark still moves.
    ///
    /// Hits at or below a search's watermark are skipped (that range was
    /// recorded already, or the search was resumed past it), as are
    /// torrents deleted meanwhile and searches deleted meanwhile. A hit
    /// above `through` is a bug in the caller and fails the whole run.
    /// Returns the alerts created, for the SSE `alerts` topic.
    pub fn record_percolation(
        &mut self,
        through: i64,
        results: &[SearchHits<'_>],
        now: i64,
    ) -> Result<Vec<Alert>> {
        if results
            .iter()
            .any(|r| r.torrent_ids.iter().any(|&t| t > through))
        {
            return Err(Error::Invalid("hit above the percolation range"));
        }
        let tx = crate::begin_write(&mut self.conn)?;
        let mut created = Vec::new();
        for r in results {
            record_hits(&tx, through, r, now, &mut created)?;
        }
        tx.commit()?;
        Ok(created)
    }

    /// A saved search's alerts, newest recorded first: up to `limit` below
    /// the alert id `before` (the previous page's last), or from the
    /// newest. Newest recorded, not highest torrent id: a promoted
    /// torrent's alert is new though its id is old (SRC-2b), and marking
    /// read through the top row must cover it.
    pub fn alerts(
        &self,
        saved_search_id: i64,
        before: Option<i64>,
        limit: usize,
    ) -> Result<Vec<Alert>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT id, saved_search_id, torrent_id, created_at, read_at FROM alerts
             WHERE saved_search_id = ?1 AND id < ?2
             ORDER BY id DESC LIMIT ?3",
        )?;
        let rows = stmt.query_map(
            params![
                saved_search_id,
                before.unwrap_or(i64::MAX),
                sql_limit(limit)
            ],
            alert_row,
        )?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Unread alerts per saved search, for searches that have any.
    pub fn unread_counts(&self) -> Result<Vec<(i64, u64)>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT saved_search_id, count(*) FROM alerts WHERE read_at IS NULL
             GROUP BY saved_search_id ORDER BY saved_search_id",
        )?;
        let rows = stmt.query_map([], |r| Ok((r.get(0)?, crate::get_u64(r, 1)?)))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Marks unread alerts with ids up to `through_id` read, for one saved
    /// search or (`None`) all of them. Alerts that arrived after the page
    /// the user saw (higher ids) stay unread. Returns how many changed.
    pub fn mark_alerts_read(
        &mut self,
        saved_search_id: Option<i64>,
        through_id: i64,
        now: i64,
    ) -> Result<usize> {
        let changed = match saved_search_id {
            Some(s) => self.conn.execute(
                "UPDATE alerts SET read_at = ?3
                 WHERE saved_search_id = ?1 AND read_at IS NULL AND id <= ?2",
                params![s, through_id, now],
            )?,
            None => self.conn.execute(
                "UPDATE alerts SET read_at = ?2 WHERE read_at IS NULL AND id <= ?1",
                params![through_id, now],
            )?,
        };
        Ok(changed)
    }

    /// Keeps each saved search's newest `keep` alerts and deletes the
    /// rest, read or not, so a broad search can't grow without bound.
    /// Returns how many were deleted.
    pub fn prune_alerts(&mut self, keep: usize) -> Result<usize> {
        let tx = crate::begin_write(&mut self.conn)?;
        let ids: Vec<i64> = {
            let mut stmt = tx.prepare("SELECT id FROM saved_searches")?;
            stmt.query_map([], |r| r.get(0))?
                .collect::<rusqlite::Result<_>>()?
        };
        let mut deleted = 0;
        {
            let mut stmt = tx.prepare_cached(
                "DELETE FROM alerts WHERE saved_search_id = ?1 AND id <= (
                     SELECT id FROM alerts WHERE saved_search_id = ?1
                     ORDER BY id DESC LIMIT 1 OFFSET ?2)",
            )?;
            for id in ids {
                deleted += stmt.execute(params![id, sql_limit(keep)])?;
            }
        }
        tx.commit()?;
        if deleted > 0 {
            self.after_user_delete();
        }
        Ok(deleted)
    }
}

/// What may be shown of a torrent `t` joined to an alert: never one a
/// filter rule hid (`?hidden` holds [`flags::HIDDEN`], plus
/// [`flags::PRIVACY`] where private torrents are left out too: a BEP 27
/// private one, a private source's find, or a remote row not yet
/// verified), and, when `?type` is a content type code (Adult's, while "Show
/// adult content" is off), never one classified as it. A torrent counts
/// as classified only with a `torrent_parsed` row (as `Store::enrichment`
/// reads it), so this is exactly the API's per-row `withheld` rule.
const SHOWN: &str = "(t.flags & :hidden) = 0
    AND NOT (:type IS NOT NULL AND coalesce(t.content_type, 0) = :type
             AND EXISTS (SELECT 1 FROM torrent_parsed p WHERE p.torrent_id = t.id))";

/// The shown-only reads (B9) and the percolator's guarded record.
impl Store {
    /// Unread alerts across every saved search on torrents search would
    /// show (see [`SHOWN`]): the nav badge, equal to the sum of the
    /// per-search `unread` the list view shows (§20.9).
    pub fn unread_shown(&self, hidden_type: Option<u8>) -> Result<u64> {
        Ok(self
            .conn
            .prepare_cached(&format!(
                "SELECT count(*) FROM alerts a JOIN torrents t ON t.id = a.torrent_id
                 WHERE a.read_at IS NULL AND {SHOWN}"
            ))?
            .query_row(
                rusqlite::named_params! { ":hidden": flags::HIDDEN, ":type": hidden_type },
                |r| crate::get_u64(r, 0),
            )?)
    }

    /// A saved search's alerts on torrents that may be shown, newest
    /// recorded first: up to `limit` below the alert id `before`, or from
    /// the newest (as [`Store::alerts`]). `hidden_type` is as
    /// [`Store::unread_shown`] takes it; `public_only` also leaves out
    /// private torrents, every [`flags::PRIVACY`] bit (a feed, §18.7;
    /// BEP 27 private ones too, UP-0). The rule runs in SQL over
    /// the search's alerts (at most `keep` after a prune, sorted once), so
    /// a search whose matches are mostly withheld costs no lookup per row.
    pub fn shown_alerts(
        &self,
        saved_search_id: i64,
        before: Option<i64>,
        limit: usize,
        hidden_type: Option<u8>,
        public_only: bool,
    ) -> Result<Vec<Alert>> {
        let hidden = flags::HIDDEN | if public_only { flags::PRIVACY } else { 0 };
        let mut stmt = self.conn.prepare_cached(&format!(
            "SELECT a.id, a.saved_search_id, a.torrent_id, a.created_at, a.read_at
             FROM alerts a JOIN torrents t ON t.id = a.torrent_id
             WHERE a.saved_search_id = :search AND a.id < :before AND {SHOWN}
             ORDER BY a.id DESC LIMIT :limit"
        ))?;
        let rows = stmt.query_map(
            rusqlite::named_params! {
                ":search": saved_search_id,
                ":before": before.unwrap_or(i64::MAX),
                ":hidden": hidden,
                ":type": hidden_type,
                ":limit": sql_limit(limit),
            },
            alert_row,
        )?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Saved searches in all, paused ones included.
    pub fn saved_search_count(&self) -> Result<u64> {
        Ok(self
            .conn
            .query_row("SELECT count(*) FROM saved_searches", [], |r| {
                crate::get_u64(r, 0)
            })?)
    }

    /// [`Store::record_percolation`] for the percolator: each search's
    /// hits come with the AST the run matched, and a search that is
    /// paused, or whose AST is no longer that one, when the transaction
    /// runs is left alone: no alerts and no watermark move. A search
    /// paused or edited while a run was under way then gets nothing from
    /// the old query; an edited one runs the same range again with the
    /// new one.
    pub fn record_percolation_unchanged(
        &mut self,
        through: i64,
        results: &[(SearchHits<'_>, &str)],
        now: i64,
    ) -> Result<Vec<Alert>> {
        if results
            .iter()
            .any(|(r, _)| r.torrent_ids.iter().any(|&t| t > through))
        {
            return Err(Error::Invalid("hit above the percolation range"));
        }
        let tx = crate::begin_write(&mut self.conn)?;
        let mut created = Vec::new();
        for (r, ast) in results {
            let current: Option<(bool, String)> = tx
                .prepare_cached("SELECT paused, ast FROM saved_searches WHERE id = ?1")?
                .query_row([r.saved_search_id], |row| Ok((row.get(0)?, row.get(1)?)))
                .optional()?;
            if current.is_some_and(|(paused, current)| !paused && current == *ast) {
                record_hits(&tx, through, r, now, &mut created)?;
            }
        }
        tx.commit()?;
        Ok(created)
    }

    /// Up to `limit` queued promotions whose `reindex_log` entry is at or
    /// below `applied` (what the serving index has applied), oldest
    /// first. A row whose entry is gone reads as torrent 0, which matches
    /// nothing, so it's still consumed.
    pub fn promoted_due(&self, applied: i64, limit: usize) -> Result<Vec<PromotedDue>> {
        self.promoted_due_after(0, applied, limit)
    }

    /// [`Store::promoted_due`] from past the row whose `reindex_log`
    /// entry is `after` (0: the queue's head), so a reader can step past
    /// rows it's leaving queued (X-percolate-promoted-hold).
    pub fn promoted_due_after(
        &self,
        after: i64,
        applied: i64,
        limit: usize,
    ) -> Result<Vec<PromotedDue>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT CAST(substr(s.key, ?3) AS INTEGER), coalesce(r.torrent_id, 0),
                    CAST(s.value AS INTEGER)
             FROM settings s LEFT JOIN reindex_log r
                 ON r.seq = CAST(substr(s.key, ?3) AS INTEGER)
             WHERE s.key > ?1 AND s.key <= ?2 ORDER BY s.key LIMIT ?4",
        )?;
        let from = if after > 0 {
            promoted_key(after)
        } else {
            PROMOTED.to_owned()
        };
        let rows = stmt.query_map(
            params![
                from,
                promoted_key(applied),
                PROMOTED.len() as i64 + 1,
                sql_limit(limit)
            ],
            |r| {
                Ok(PromotedDue {
                    seq: r.get(0)?,
                    torrent_id: r.get(1)?,
                    searches_through: r.get(2)?,
                })
            },
        )?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Records the percolation of every queued promotion up to `seq`, in
    /// one transaction: each search's hits become alerts, checked as
    /// [`Store::record_percolation_unchanged`] checks them (a search paused
    /// or edited meanwhile gets none), and those queue rows are dropped.
    /// No watermark moves. A crash before the commit replays the rows; a
    /// hit recorded already (by the forward run or a replay) is ignored.
    pub fn record_promoted(
        &mut self,
        seq: i64,
        results: &[(SearchHits<'_>, &str)],
        now: i64,
    ) -> Result<Vec<Alert>> {
        let tx = crate::begin_write(&mut self.conn)?;
        let created = insert_promoted(&tx, results, now)?;
        tx.prepare_cached("DELETE FROM settings WHERE key > ?1 AND key <= ?2")?
            .execute(params![PROMOTED, promoted_key(seq)])?;
        tx.commit()?;
        Ok(created)
    }

    /// [`Store::record_promoted`], dropping exactly the queue rows in
    /// `seqs` rather than every row up to one: a row a search is still
    /// held on stays queued while later ones go
    /// (X-percolate-promoted-hold). Hits for a row left queued are
    /// recorded too; a replay of them adds nothing (`alerts` is unique
    /// per search and torrent), so `results` is per search, not per row.
    ///
    /// Every seq must name a queued row, each once: otherwise it's
    /// [`Error::NotFound`] and nothing is written, alerts included.
    pub fn record_promoted_rows(
        &mut self,
        seqs: &[i64],
        results: &[(SearchHits<'_>, &str)],
        now: i64,
    ) -> Result<Vec<Alert>> {
        let tx = crate::begin_write(&mut self.conn)?;
        {
            let mut delete = tx.prepare_cached("DELETE FROM settings WHERE key = ?1")?;
            for &seq in seqs {
                if seq <= 0 || delete.execute([promoted_key(seq)])? != 1 {
                    // Dropping `tx` rolls back the rows already deleted.
                    return Err(Error::NotFound("queued promotion"));
                }
            }
        }
        let created = insert_promoted(&tx, results, now)?;
        tx.commit()?;
        Ok(created)
    }
}

/// A promoted pass's hits as alerts, for searches neither paused nor
/// edited since they ran.
fn insert_promoted(
    tx: &Transaction<'_>,
    results: &[(SearchHits<'_>, &str)],
    now: i64,
) -> Result<Vec<Alert>> {
    let mut created = Vec::new();
    for (r, ast) in results {
        let current: Option<(bool, String)> = tx
            .prepare_cached("SELECT paused, ast FROM saved_searches WHERE id = ?1")?
            .query_row([r.saved_search_id], |row| Ok((row.get(0)?, row.get(1)?)))
            .optional()?;
        if current.is_some_and(|(paused, current)| !paused && current == *ast) {
            insert_alerts(tx, r, i64::MIN, now, &mut created)?;
        }
    }
    Ok(created)
}

fn record_hits(
    tx: &Transaction<'_>,
    through: i64,
    r: &SearchHits<'_>,
    now: i64,
    created: &mut Vec<Alert>,
) -> Result<()> {
    let watermark: Option<i64> = tx
        .prepare_cached("SELECT watermark FROM saved_searches WHERE id = ?1")?
        .query_row([r.saved_search_id], |row| row.get(0))
        .optional()?;
    let Some(watermark) = watermark else {
        return Ok(());
    };
    insert_alerts(tx, r, watermark, now, created)?;
    tx.prepare_cached("UPDATE saved_searches SET watermark = max(watermark, ?2) WHERE id = ?1")?
        .execute(params![r.saved_search_id, through])?;
    Ok(())
}

/// `r`'s hits above `after` that are still torrents become alerts; one
/// recorded already is left as it is.
fn insert_alerts(
    tx: &Transaction<'_>,
    r: &SearchHits<'_>,
    after: i64,
    now: i64,
    created: &mut Vec<Alert>,
) -> Result<()> {
    let mut insert = tx.prepare_cached(
        "INSERT INTO alerts (saved_search_id, torrent_id, created_at)
         SELECT ?1, id, ?3 FROM torrents WHERE id = ?2
         ON CONFLICT DO NOTHING",
    )?;
    let mut ids = r.torrent_ids.to_vec();
    ids.sort_unstable();
    ids.dedup();
    for t in ids.into_iter().filter(|&t| t > after) {
        if insert.execute(params![r.saved_search_id, t, now])? > 0 {
            created.push(Alert {
                id: tx.last_insert_rowid(),
                saved_search_id: r.saved_search_id,
                torrent_id: t,
                created_at: now,
                read_at: None,
            });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::user_data::{delete_torrent, test_store};

    const AST: &str = r#"{"op":"term","word":"nature"}"#;

    fn input(name: &str) -> SavedSearchInput<'_> {
        SavedSearchInput {
            name,
            query: "nature res:2160p",
            ast: AST,
            sort: Some("-seeders"),
            alert: true,
            rss: false,
            channels: 0,
            paused: false,
        }
    }

    fn hits(id: i64, t: &[i64]) -> SearchHits<'_> {
        SearchHits {
            saved_search_id: id,
            torrent_ids: t,
        }
    }

    #[test]
    fn the_promoted_queue_reads_in_seq_order_up_to_what_the_index_applied() {
        let mut s = test_store(0);
        let a = s.create_saved_search(&input("a"), 0, 0).unwrap();
        for seq in [100, 9, 10] {
            queue_promoted(&s.conn, seq).unwrap();
        }
        let seqs = |s: &Store, applied| -> Vec<i64> {
            let due = s.promoted_due(applied, 10).unwrap();
            due.iter().map(|d| d.seq).collect()
        };
        assert_eq!(seqs(&s, 10), vec![9, 10]);
        assert_eq!(seqs(&s, i64::MAX), vec![9, 10, 100]);
        // No `reindex_log` entry behind it: torrent 0, consumed all the same.
        let due = s.promoted_due(i64::MAX, 1).unwrap();
        assert_eq!((due[0].torrent_id, due[0].searches_through), (0, a));
        assert!(s.record_promoted(10, &[], 0).unwrap().is_empty());
        assert_eq!(seqs(&s, i64::MAX), vec![100]);
    }

    /// A promoted torrent's alert is recorded after alerts on higher
    /// torrent ids: it lists first, survives a prune that keeps the
    /// newest, and marking read through the top row covers it.
    #[test]
    fn a_promoted_alert_is_the_newest_for_listing_pruning_and_reading() {
        let mut s = test_store(10);
        let a = s.create_saved_search(&input("a"), 0, 0).unwrap();
        s.record_percolation(10, &[hits(a, &[5, 6, 7])], 1).unwrap();
        queue_promoted(&s.conn, 1).unwrap();
        let created = s.record_promoted(1, &[(hits(a, &[2]), AST)], 2).unwrap();
        assert_eq!(created.len(), 1);
        let t = |v: Vec<Alert>| v.iter().map(|x| x.torrent_id).collect::<Vec<_>>();
        assert_eq!(t(s.alerts(a, None, 10).unwrap()), [2, 7, 6, 5]);
        assert_eq!(
            t(s.shown_alerts(a, None, 10, None, false).unwrap()),
            [2, 7, 6, 5]
        );
        let top = s.alerts(a, None, 1).unwrap()[0].id;
        assert_eq!(t(s.alerts(a, Some(top), 10).unwrap()), [7, 6, 5]);
        assert_eq!(s.saved_searches().unwrap()[0].last_alert_at, Some(2));
        assert_eq!(s.mark_alerts_read(Some(a), top, 3).unwrap(), 4);
        assert_eq!(s.prune_alerts(2).unwrap(), 2);
        assert_eq!(t(s.alerts(a, None, 10).unwrap()), [2, 7]);
    }

    /// As `record_percolation_unchanged`: a search paused or edited after
    /// the match gets nothing, and the rows are consumed all the same. An
    /// Adult torrent's alert is recorded and withheld when it's read.
    /// X-percolate-promoted-hold: a reader steps past rows it leaves
    /// queued, and exactly the rows named are dropped.
    #[test]
    fn promoted_rows_can_be_stepped_past_and_dropped_one_by_one() {
        let mut s = test_store(10);
        let id = s.create_saved_search(&input("n"), 10, 0).unwrap();
        for seq in [2, 4, 6, 8] {
            queue_promoted(&s.conn, seq).unwrap();
        }
        let seqs = |v: Vec<PromotedDue>| v.into_iter().map(|r| r.seq).collect::<Vec<_>>();
        assert_eq!(seqs(s.promoted_due_after(0, 99, 10).unwrap()), [2, 4, 6, 8]);
        assert_eq!(seqs(s.promoted_due_after(4, 99, 10).unwrap()), [6, 8]);
        assert_eq!(seqs(s.promoted_due_after(3, 6, 10).unwrap()), [4, 6]);
        assert_eq!(seqs(s.promoted_due_after(2, 99, 1).unwrap()), [4]);
        assert!(s.promoted_due_after(8, 99, 10).unwrap().is_empty());
        // 2 and 6 stay; the hits for any of them are recorded.
        let created = s
            .record_promoted_rows(&[4, 8], &[(hits(id, &[3, 5]), AST)], 1)
            .unwrap();
        assert_eq!(t_ids(&created), [3, 5]);
        assert_eq!(seqs(s.promoted_due(99, 10).unwrap()), [2, 6]);
        // A replay of the same hits makes nothing twice.
        let again = s
            .record_promoted_rows(&[2, 6], &[(hits(id, &[3, 5]), AST)], 2)
            .unwrap();
        assert!(again.is_empty());
        assert!(s.promoted_due(99, 10).unwrap().is_empty());
    }

    /// A seq that names no queued row (unknown, already dropped, or given
    /// twice) fails the call and rolls all of it back: no row is dropped
    /// and no alert is made.
    #[test]
    fn record_promoted_rows_refuses_an_unknown_row_and_writes_nothing() {
        let mut s = test_store(10);
        let id = s.create_saved_search(&input("n"), 10, 0).unwrap();
        for seq in [2, 4] {
            queue_promoted(&s.conn, seq).unwrap();
        }
        let both = |s: &Store| s.promoted_due(99, 10).unwrap().len();
        for seqs in [&[3][..], &[2, 3], &[2, 2], &[0], &[4, 2, 9]] {
            let r = s.record_promoted_rows(seqs, &[(hits(id, &[3]), AST)], 1);
            assert!(matches!(r, Err(Error::NotFound(_))), "{seqs:?}: {r:?}");
            assert_eq!(both(&s), 2, "{seqs:?}: nothing dropped");
            assert!(s.alerts(id, None, 10).unwrap().is_empty(), "{seqs:?}");
        }
        // The same call with known rows goes through.
        let created = s
            .record_promoted_rows(&[2, 4], &[(hits(id, &[3]), AST)], 1)
            .unwrap();
        assert_eq!(t_ids(&created), [3]);
        assert_eq!(both(&s), 0);
    }

    #[test]
    fn record_promoted_skips_a_search_paused_or_edited_meanwhile() {
        let mut s = test_store(10);
        let paused = s.create_saved_search(&input("p"), 10, 0).unwrap();
        let edited = s.create_saved_search(&input("e"), 10, 0).unwrap();
        let adult = s.create_saved_search(&input("x"), 10, 0).unwrap();
        s.update_saved_search(
            paused,
            &SavedSearchInput {
                paused: true,
                ..input("p")
            },
            10,
            0,
        )
        .unwrap();
        queue_promoted(&s.conn, 4).unwrap();
        let old = r#"{"op":"term","word":"old"}"#;
        let created = s
            .record_promoted(
                4,
                &[
                    (hits(paused, &[3]), AST),
                    (hits(edited, &[3]), old),
                    (hits(adult, &[4]), AST),
                ],
                1,
            )
            .unwrap();
        assert_eq!(t_ids(&created), [4]);
        assert!(s.promoted_due(i64::MAX, 10).unwrap().is_empty());
        s.conn
            .execute_batch(
                "UPDATE torrents SET content_type = 11 WHERE id = 4;
                 INSERT INTO torrent_parsed (torrent_id, parser_ver, data) VALUES (4, 1, x'00');",
            )
            .unwrap();
        assert!(
            s.shown_alerts(adult, None, 10, Some(11), false)
                .unwrap()
                .is_empty()
        );
        assert_eq!(s.unread_shown(Some(11)).unwrap(), 0);
    }

    fn t_ids(v: &[Alert]) -> Vec<i64> {
        v.iter().map(|x| x.torrent_id).collect()
    }

    #[test]
    fn round_trip_and_update() {
        let mut s = test_store(0);
        let id = s
            .create_saved_search(&input("4K nature docs"), 7, 100)
            .unwrap();
        let got = s.saved_search(id).unwrap().unwrap();
        assert_eq!(
            got,
            SavedSearch {
                id,
                name: "4K nature docs".into(),
                query: "nature res:2160p".into(),
                ast: AST.into(),
                sort: Some("-seeders".into()),
                alert: true,
                rss: false,
                channels: 0,
                paused: false,
                watermark: 7,
                created_at: 100,
                updated_at: 100,
            }
        );
        let mut changed = input("Nature");
        changed.sort = None;
        changed.rss = true;
        assert!(s.update_saved_search(id, &changed, 50, 200).unwrap());
        let got = s.saved_search(id).unwrap().unwrap();
        assert_eq!(
            (
                got.name.as_str(),
                got.sort,
                got.rss,
                got.watermark,
                got.updated_at
            ),
            ("Nature", None, true, 7, 200),
            "an active search keeps its watermark"
        );
        assert!(!s.update_saved_search(999, &changed, 0, 0).unwrap());
        assert!(s.saved_search(999).unwrap().is_none());
    }

    #[test]
    fn pausing_keeps_the_channels_and_resuming_skips_the_backlog() {
        let mut s = test_store(0);
        let mut rss_only = input("a");
        rss_only.alert = false;
        rss_only.rss = true;
        rss_only.channels = 0b10;
        let id = s.create_saved_search(&rss_only, 10, 0).unwrap();
        assert_eq!(s.percolation_due(1000).unwrap().len(), 1);

        let mut paused = rss_only;
        paused.paused = true;
        s.update_saved_search(id, &paused, 20, 1).unwrap();
        let got = s.saved_search(id).unwrap().unwrap();
        assert!(got.paused);
        assert_eq!(
            (got.alert, got.rss, got.channels, got.watermark),
            (false, true, 0b10, 10)
        );
        assert!(s.percolation_due(1000).unwrap().is_empty());

        // Resuming restores RSS-only and starts from the index's watermark.
        s.update_saved_search(id, &rss_only, 500, 2).unwrap();
        let got = s.saved_search(id).unwrap().unwrap();
        assert_eq!(
            (got.paused, got.alert, got.rss, got.channels, got.watermark),
            (false, false, true, 0b10, 500)
        );
        // A resume never moves it down, and an edit while active keeps it.
        s.update_saved_search(id, &paused, 0, 3).unwrap();
        s.update_saved_search(id, &rss_only, 100, 4).unwrap();
        assert_eq!(s.saved_search(id).unwrap().unwrap().watermark, 500);
        s.update_saved_search(id, &input("a"), 900, 5).unwrap();
        assert_eq!(s.saved_search(id).unwrap().unwrap().watermark, 500);
        // A search with every channel off still percolates: the list view
        // shows its new hits (§10.8).
        let mut silent = input("a");
        silent.alert = false;
        s.update_saved_search(id, &silent, 0, 6).unwrap();
        assert_eq!(s.percolation_due(1000).unwrap().len(), 1);
    }

    #[test]
    fn constraints() {
        let mut s = test_store(0);
        let mut bad = input("x");
        bad.ast = "{not json";
        assert!(matches!(
            s.create_saved_search(&bad, 0, 0),
            Err(Error::Invalid("ast is not JSON"))
        ));
        assert!(s.create_saved_search(&input(""), 0, 0).is_err());
        assert!(s.create_saved_search(&input(" padded"), 0, 0).is_err());
        let long = "q".repeat(2049);
        let mut q = input("x");
        q.query = &long;
        assert!(s.create_saved_search(&q, 0, 0).is_err());
        let mut sort = input("x");
        sort.sort = Some("");
        assert!(s.create_saved_search(&sort, 0, 0).is_err());
        // The database's own checks back the Rust ones up.
        let err = s
            .conn
            .execute(
                "INSERT INTO saved_searches (name, query, ast, alert, rss, watermark,
                     created_at, updated_at) VALUES ('x', '', '{}', 2, 0, 0, 0, 0)",
                [],
            )
            .unwrap_err();
        assert_eq!(
            err.sqlite_error_code(),
            Some(rusqlite::ErrorCode::ConstraintViolation)
        );
        assert!(s.saved_searches().unwrap().is_empty());
    }

    #[test]
    fn percolation_records_hits_once_and_moves_the_watermark() {
        let mut s = test_store(20);
        let a = s.create_saved_search(&input("a"), 5, 0).unwrap();
        let b = s.create_saved_search(&input("b"), 5, 0).unwrap();
        assert_eq!(s.percolation_due(10).unwrap().len(), 2);
        assert!(s.percolation_due(5).unwrap().is_empty());

        // a matched 6, 8 (twice) and 4 (below its watermark); b nothing.
        let created = s
            .record_percolation(10, &[hits(a, &[8, 6, 8, 4]), hits(b, &[])], 100)
            .unwrap();
        assert_eq!(
            created.iter().map(|x| x.torrent_id).collect::<Vec<_>>(),
            [6, 8]
        );
        assert!(created.windows(2).all(|w| w[0].id < w[1].id));
        assert_eq!(s.saved_search(a).unwrap().unwrap().watermark, 10);
        assert_eq!(s.saved_search(b).unwrap().unwrap().watermark, 10);
        assert!(s.percolation_due(10).unwrap().is_empty());

        // A re-run of the same range (a crash before the caller saw the
        // result) adds nothing.
        assert!(
            s.record_percolation(10, &[hits(a, &[6, 8])], 101)
                .unwrap()
                .is_empty()
        );
        // A hit above the range is the caller's bug: nothing is written.
        assert!(matches!(
            s.record_percolation(15, &[hits(b, &[12]), hits(a, &[16])], 102),
            Err(Error::Invalid(_))
        ));
        assert_eq!(s.saved_search(b).unwrap().unwrap().watermark, 10);
        // A torrent deleted meanwhile and a search deleted meanwhile are
        // skipped.
        delete_torrent(&s, 12);
        let created = s
            .record_percolation(15, &[hits(a, &[12, 13]), hits(999, &[14])], 103)
            .unwrap();
        assert_eq!(created.len(), 1);
        assert_eq!(created[0].torrent_id, 13);

        let page = s.alerts(a, None, 2).unwrap();
        assert_eq!(
            page.iter().map(|x| x.torrent_id).collect::<Vec<_>>(),
            [13, 8]
        );
        let next = s.alerts(a, Some(page[1].id), 10).unwrap();
        assert_eq!(next.iter().map(|x| x.torrent_id).collect::<Vec<_>>(), [6]);
    }

    #[test]
    fn unread_counts_and_marking_read() {
        let mut s = test_store(30);
        let a = s.create_saved_search(&input("a"), 0, 0).unwrap();
        let b = s.create_saved_search(&input("b"), 0, 0).unwrap();
        let first = s
            .record_percolation(10, &[hits(a, &[1, 2, 3]), hits(b, &[4])], 50)
            .unwrap();
        assert_eq!(s.unread_counts().unwrap(), [(a, 3), (b, 1)]);
        assert_eq!(s.unread_shown(None).unwrap(), 4);

        // The user saw a's page up to its newest alert; one arrives after.
        let seen = first
            .iter()
            .filter(|x| x.saved_search_id == a)
            .map(|x| x.id)
            .max()
            .unwrap();
        s.record_percolation(20, &[hits(a, &[15])], 60).unwrap();
        assert_eq!(s.mark_alerts_read(Some(a), seen, 70).unwrap(), 3);
        assert_eq!(s.unread_counts().unwrap(), [(a, 1), (b, 1)]);
        let page = s.alerts(a, None, 10).unwrap();
        assert_eq!(page[0].read_at, None, "the one after the divider");
        assert!(page[1..].iter().all(|x| x.read_at == Some(70)));

        let list = s.saved_searches().unwrap();
        assert_eq!(list[0].alerts, 4);
        assert_eq!(list[0].unread, 1);
        assert_eq!(list[0].last_alert_at, Some(60));
        assert_eq!(list[1].last_alert_at, Some(50));

        assert_eq!(s.mark_alerts_read(None, i64::MAX, 80).unwrap(), 2);
        assert_eq!(s.unread_shown(None).unwrap(), 0);
        assert!(s.unread_counts().unwrap().is_empty());
    }

    /// The badge leaves out filter-hidden torrents, and the hidden type
    /// only where a torrent is classified (has a `torrent_parsed` row).
    #[test]
    fn the_shown_count_leaves_out_what_is_withheld() {
        let mut s = test_store(6);
        let a = s.create_saved_search(&input("a"), 0, 0).unwrap();
        s.record_percolation(6, &[hits(a, &[1, 2, 3, 4, 5, 6])], 0)
            .unwrap();
        let exec = |sql: &str| s.conn.execute_batch(sql).unwrap();
        // 2: hidden by a filter rule. 3: Adult and classified. 4: Adult's
        // code but no parsed row (unclassified). 5: classified, other type.
        exec(&format!(
            "UPDATE torrents SET flags = {} WHERE id = 2;
             UPDATE torrents SET content_type = 11 WHERE id IN (3, 4);
             UPDATE torrents SET content_type = 0 WHERE id = 5;
             INSERT INTO torrent_parsed (torrent_id, parser_ver, data)
                 VALUES (3, 1, x'00'), (5, 1, x'00'), (6, 1, x'00');",
            flags::HIDDEN
        ));
        assert_eq!(s.unread_shown(None).unwrap(), 5);
        assert_eq!(s.unread_shown(Some(11)).unwrap(), 4);
        // 6 has a parsed row and a NULL type: it counts as type 0 there,
        // as `Store::enrichment` reads it.
        assert_eq!(s.unread_shown(Some(0)).unwrap(), 3);
        s.mark_alerts_read(Some(a), i64::MAX, 1).unwrap();
        assert_eq!(s.unread_shown(Some(11)).unwrap(), 0);
    }

    /// The list and the feed read shown alerts in SQL: thousands of
    /// withheld ones in between cost no lookups and don't fill a page.
    #[test]
    fn shown_alerts_skip_withheld_rows_in_sql() {
        let mut s = test_store(3000);
        let a = s.create_saved_search(&input("a"), 0, 0).unwrap();
        let ids: Vec<i64> = (1..=3000).collect();
        s.record_percolation(3000, &[hits(a, &ids)], 0).unwrap();
        // Everything but 1, 2, 1500 and 3000 is hidden; 2 is Adult and
        // classified; 1500 is private-source.
        s.conn
            .execute_batch(&format!(
                "UPDATE torrents SET flags = {h} WHERE id NOT IN (1, 2, 1500, 3000);
                 UPDATE torrents SET flags = {p} WHERE id = 1500;
                 UPDATE torrents SET content_type = 11 WHERE id = 2;
                 INSERT INTO torrent_parsed (torrent_id, parser_ver, data) VALUES (2, 1, x'00');",
                h = flags::HIDDEN,
                p = flags::PRIVATE_SOURCE
            ))
            .unwrap();
        let t = |v: Vec<Alert>| v.iter().map(|x| x.torrent_id).collect::<Vec<_>>();
        assert_eq!(
            t(s.shown_alerts(a, None, 10, Some(11), false).unwrap()),
            [3000, 1500, 1]
        );
        assert_eq!(
            t(s.shown_alerts(a, None, 10, None, false).unwrap()),
            [3000, 1500, 2, 1]
        );
        // A feed leaves out private-source torrents too.
        assert_eq!(
            t(s.shown_alerts(a, None, 10, Some(11), true).unwrap()),
            [3000, 1]
        );
        // And every other privacy bit: a BEP 27 private torrent (UP-0) or
        // a remote row whose private bit isn't verified yet.
        for bit in [flags::PRIVATE, flags::REMOTE_UNVERIFIED] {
            s.conn
                .execute("UPDATE torrents SET flags = ?1 WHERE id = 3000", [bit])
                .unwrap();
            assert_eq!(
                t(s.shown_alerts(a, None, 10, Some(11), true).unwrap()),
                [1],
                "{bit}"
            );
            assert_eq!(
                t(s.shown_alerts(a, None, 10, Some(11), false).unwrap())[0],
                3000,
                "kept on the instance's own list"
            );
        }
        s.conn
            .execute("UPDATE torrents SET flags = 0 WHERE id = 3000", [])
            .unwrap();
        // Paging: below a cursor, and a limit.
        assert_eq!(
            t(s.shown_alerts(a, Some(3000), 1, Some(11), false).unwrap()),
            [1500]
        );
        assert_eq!(
            t(s.shown_alerts(a, Some(1500), 10, Some(11), false).unwrap()),
            [1]
        );
        // The same rule as the badge.
        assert_eq!(s.unread_shown(Some(11)).unwrap(), 3);
        assert_eq!(s.saved_search_count().unwrap(), 1);
    }

    /// A search paused, edited or deleted while a run was under way gets
    /// nothing from it, and keeps its watermark.
    #[test]
    fn a_guarded_record_skips_searches_changed_mid_run() {
        let mut s = test_store(10);
        let same = s.create_saved_search(&input("same"), 0, 0).unwrap();
        let paused = s.create_saved_search(&input("paused"), 0, 0).unwrap();
        let edited = s.create_saved_search(&input("edited"), 0, 0).unwrap();
        let gone = s.create_saved_search(&input("gone"), 0, 0).unwrap();
        let mut p = input("paused");
        p.paused = true;
        s.update_saved_search(paused, &p, 0, 1).unwrap();
        let mut e = input("edited");
        e.ast = r#"{"op":"term","word":"other"}"#;
        s.update_saved_search(edited, &e, 0, 1).unwrap();
        s.delete_saved_search(gone).unwrap();
        let ids = [3, 5];
        let created = s
            .record_percolation_unchanged(
                10,
                &[
                    (hits(same, &ids), AST),
                    (hits(paused, &ids), AST),
                    (hits(edited, &ids), AST),
                    (hits(gone, &ids), AST),
                ],
                9,
            )
            .unwrap();
        assert_eq!(created.len(), 2);
        assert!(created.iter().all(|a| a.saved_search_id == same));
        assert_eq!(s.saved_search(same).unwrap().unwrap().watermark, 10);
        for id in [paused, edited] {
            assert_eq!(s.saved_search(id).unwrap().unwrap().watermark, 0);
            assert!(s.alerts(id, None, 10).unwrap().is_empty());
        }
        // The edited one, recorded with its new AST, moves on.
        let created = s
            .record_percolation_unchanged(10, &[(hits(edited, &[7]), e.ast)], 9)
            .unwrap();
        assert_eq!(created.len(), 1);
        assert!(matches!(
            s.record_percolation_unchanged(5, &[(hits(same, &[6]), AST)], 9),
            Err(Error::Invalid(_))
        ));
    }

    #[test]
    fn pruning_keeps_the_newest() {
        let mut s = test_store(30);
        let a = s.create_saved_search(&input("a"), 0, 0).unwrap();
        let b = s.create_saved_search(&input("b"), 0, 0).unwrap();
        let ids: Vec<i64> = (1..=10).collect();
        s.record_percolation(10, &[hits(a, &ids), hits(b, &[1, 2])], 0)
            .unwrap();
        assert_eq!(s.prune_alerts(3).unwrap(), 7);
        let left: Vec<i64> = s
            .alerts(a, None, 99)
            .unwrap()
            .iter()
            .map(|x| x.torrent_id)
            .collect();
        assert_eq!(left, [10, 9, 8]);
        assert_eq!(s.alerts(b, None, 99).unwrap().len(), 2);
        assert_eq!(s.prune_alerts(3).unwrap(), 0);
        // "Keep everything" keeps everything (a cast would make it -1).
        assert_eq!(s.prune_alerts(usize::MAX).unwrap(), 0);
        assert_eq!(s.alerts(a, None, usize::MAX).unwrap().len(), 3);
        assert_eq!(s.prune_alerts(0).unwrap(), 5);
    }

    #[test]
    fn deletes_cascade_and_ids_are_never_reused() {
        let mut s = test_store(10);
        let a = s.create_saved_search(&input("a"), 0, 0).unwrap();
        let created = s.record_percolation(5, &[hits(a, &[1, 2])], 0).unwrap();
        s.conn
            .execute(
                "INSERT INTO api_keys (kind, name, prefix, hash, scopes, saved_search_id,
                     created_at) VALUES (1, 'feed', 'lsf_abcdefgh', zeroblob(32), 0, ?1, 0)",
                [a],
            )
            .unwrap();

        // A deleted torrent takes its alert with it.
        delete_torrent(&s, 1);
        assert_eq!(s.alerts(a, None, 10).unwrap().len(), 1);

        assert!(s.delete_saved_search(a).unwrap());
        assert!(!s.delete_saved_search(a).unwrap());
        let left: i64 = s
            .conn
            .query_row(
                "SELECT (SELECT count(*) FROM alerts) + (SELECT count(*) FROM api_keys)",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(left, 0, "alerts and feed tokens go with it");

        let b = s.create_saved_search(&input("b"), 0, 0).unwrap();
        assert!(b > a);
        let again = s.record_percolation(5, &[hits(b, &[2])], 0).unwrap();
        assert!(again[0].id > created[1].id);
    }
}
