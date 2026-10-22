//! Saved searches and their alerts (architecture §8 "Saved searches →
//! alerts", §10.8).
//!
//! The percolator (B9) runs each saved search over the torrents above its
//! `watermark` after an index commit, then records the hits and the new
//! watermark in one transaction ([`Store::record_percolation`]), so a
//! crash between the two can only re-run a range, never skip one; a
//! repeated hit is ignored. The store keeps the query text and AST as the
//! API gives them: parsing and compiling belong to the search crate.

use rusqlite::{OptionalExtension, Transaction, params};

use crate::user_data::{check_name, check_text, sql_limit, write_tx};
use crate::{Error, Result, Store};

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
                      ORDER BY a.torrent_id DESC LIMIT 1)
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
        let tx = write_tx(&mut self.conn)?;
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
        let tx = write_tx(&mut self.conn)?;
        let mut created = Vec::new();
        for r in results {
            record_hits(&tx, through, r, now, &mut created)?;
        }
        tx.commit()?;
        Ok(created)
    }

    /// A saved search's alerts, newest first: up to `limit` below the
    /// torrent id `before` (the previous page's last), or from the newest.
    pub fn alerts(
        &self,
        saved_search_id: i64,
        before: Option<i64>,
        limit: usize,
    ) -> Result<Vec<Alert>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT id, saved_search_id, torrent_id, created_at, read_at FROM alerts
             WHERE saved_search_id = ?1 AND torrent_id < ?2
             ORDER BY torrent_id DESC LIMIT ?3",
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

    /// Unread alerts across every saved search (the nav badge).
    pub fn unread_total(&self) -> Result<u64> {
        Ok(self.conn.query_row(
            "SELECT count(*) FROM alerts WHERE read_at IS NULL",
            [],
            |r| crate::get_u64(r, 0),
        )?)
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
        let tx = write_tx(&mut self.conn)?;
        let ids: Vec<i64> = {
            let mut stmt = tx.prepare("SELECT id FROM saved_searches")?;
            stmt.query_map([], |r| r.get(0))?
                .collect::<rusqlite::Result<_>>()?
        };
        let mut deleted = 0;
        {
            let mut stmt = tx.prepare_cached(
                "DELETE FROM alerts WHERE saved_search_id = ?1 AND torrent_id <= (
                     SELECT torrent_id FROM alerts WHERE saved_search_id = ?1
                     ORDER BY torrent_id DESC LIMIT 1 OFFSET ?2)",
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
    let mut insert = tx.prepare_cached(
        "INSERT INTO alerts (saved_search_id, torrent_id, created_at)
         SELECT ?1, id, ?3 FROM torrents WHERE id = ?2
         ON CONFLICT DO NOTHING",
    )?;
    let mut ids = r.torrent_ids.to_vec();
    ids.sort_unstable();
    ids.dedup();
    for t in ids.into_iter().filter(|&t| t > watermark) {
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
    tx.prepare_cached("UPDATE saved_searches SET watermark = max(watermark, ?2) WHERE id = ?1")?
        .execute(params![r.saved_search_id, through])?;
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
        let next = s.alerts(a, Some(8), 10).unwrap();
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
        assert_eq!(s.unread_total().unwrap(), 4);

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
        assert_eq!(s.unread_total().unwrap(), 0);
        assert!(s.unread_counts().unwrap().is_empty());
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
