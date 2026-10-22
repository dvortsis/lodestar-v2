//! Reporting tables that Insights reads (architecture §7, §10.9.5):
//! searchable time per hour, the opt-in query log, and when a torrent's
//! identity was resolved.
//!
//! **Searchable time** (`index_watermark_hour`). The index commits in id
//! order, so the highest id searchable at the end of each hour is enough:
//! a torrent's searchable hour is the first hour whose `last_id` reaches
//! its id. Both columns only grow ([`Store::record_index_watermark`]), so
//! that hour is well defined. Hour 0 holds what was indexed before the
//! table existed; those torrents' hour is unknown.
//!
//! **The query log** is off unless the user turns it on (§10.9.5: off by
//! default, local, retained 30 days, clearable). It holds the time, the
//! query, the result count and the latency, never an address, key or
//! token: there's no column for one, and links in a query (a pasted
//! magnet or tracker URL can carry a passkey) are replaced before writing.

use rusqlite::{OptionalExtension, params};

use crate::user_data::write_tx;
use crate::{Error, Result, Store};

/// Whether the query log is on: the setting's value, passed in by the
/// caller on every append, so nothing is written unless it says `On`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum QueryLogging {
    #[default]
    Off,
    On,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QueryLogEntry<'a> {
    pub at: i64,
    /// The query as run; links in it are replaced, and it's cut to 2,048
    /// bytes.
    pub query: &'a str,
    /// Matches; `None` when the search failed or was cancelled.
    pub results: Option<u64>,
    pub latency_us: u64,
}

/// One row of "top queries" (§10.9.5 Serve).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueryCount {
    pub query: String,
    pub count: u64,
    /// How many of those runs found nothing.
    pub zero: u64,
}

/// A torrent whose identity enrichment resolved, and when.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Resolved {
    pub torrent_id: i64,
    pub at: i64,
}

const MAX_QUERY: usize = 2048;

/// What the query log stores for `query`: every word that is or holds a
/// link becomes `[link]`, then the text is cut at a character boundary.
fn scrub_query(query: &str) -> String {
    let mut out = String::with_capacity(query.len().min(MAX_QUERY));
    for (i, word) in query.split(' ').enumerate() {
        if i > 0 {
            out.push(' ');
        }
        let lower = word.to_ascii_lowercase();
        if lower.contains("://") || lower.contains("magnet:") {
            out.push_str("[link]");
        } else {
            out.push_str(word);
        }
    }
    if out.len() > MAX_QUERY {
        let mut cut = MAX_QUERY;
        while !out.is_char_boundary(cut) {
            cut -= 1;
        }
        out.truncate(cut);
    }
    out
}

impl Store {
    /// Records that every torrent id up to `last_id` was searchable by the
    /// end of `hour` (unix time / 3600, at least 1). Both columns only
    /// grow: an id at or below one already recorded changes nothing, and
    /// an hour before the latest recorded (a clock set back) is counted in
    /// the latest instead. Returns whether it wrote.
    pub fn record_index_watermark(&mut self, hour: i64, last_id: i64) -> Result<bool> {
        if hour < 1 || last_id < 0 {
            return Err(Error::Invalid(
                "hour must be at least 1 and last_id not negative",
            ));
        }
        let tx = write_tx(&mut self.conn)?;
        let (max_hour, max_id): (i64, i64) = tx.query_row(
            "SELECT coalesce(max(hour), 0), coalesce(max(last_id), 0) FROM index_watermark_hour",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        if last_id <= max_id {
            return Ok(false);
        }
        tx.execute(
            "INSERT INTO index_watermark_hour (hour, last_id) VALUES (?1, ?2)
             ON CONFLICT (hour) DO UPDATE SET last_id = excluded.last_id",
            params![hour.max(max_hour).max(1), last_id],
        )?;
        tx.commit()?;
        Ok(true)
    }

    /// The hour a torrent id became searchable: `None` if it isn't yet, or
    /// if it was indexed before hourly tracking started (hour 0).
    pub fn searchable_hour(&self, id: i64) -> Result<Option<i64>> {
        let hour: Option<i64> = self
            .conn
            .prepare_cached(
                "SELECT hour FROM index_watermark_hour WHERE last_id >= ?1
                 ORDER BY last_id LIMIT 1",
            )?
            .query_row([id], |r| r.get(0))
            .optional()?;
        Ok(hour.filter(|&h| h > 0))
    }

    /// `(hour, last_id)` for the recorded hours in `from..=to`, in order,
    /// plus the last recorded row before `from` (hour 0 included), which
    /// bounds the first hour's range from below. The ids that became
    /// searchable in hour `h` are those above the previous row's
    /// `last_id`, up to `h`'s: `lodestar rollup --repair` counts them.
    pub fn index_watermarks(&self, from: i64, to: i64) -> Result<Vec<(i64, i64)>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT hour, last_id FROM (
                 SELECT hour, last_id FROM index_watermark_hour WHERE hour < ?1
                 ORDER BY hour DESC LIMIT 1)
             UNION ALL
             SELECT hour, last_id FROM index_watermark_hour WHERE hour BETWEEN ?1 AND ?2
             ORDER BY hour",
        )?;
        let rows = stmt.query_map(params![from, to], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Appends searches to the query log if `logging` is on; writes
    /// nothing otherwise. Returns how many rows were written.
    pub fn log_queries(
        &mut self,
        logging: QueryLogging,
        entries: &[QueryLogEntry<'_>],
    ) -> Result<usize> {
        if logging == QueryLogging::Off || entries.is_empty() {
            return Ok(0);
        }
        let tx = write_tx(&mut self.conn)?;
        {
            let mut stmt = tx.prepare_cached(
                "INSERT INTO query_log (at, query, results, latency_us) VALUES (?1, ?2, ?3, ?4)",
            )?;
            for e in entries {
                stmt.execute(params![
                    e.at,
                    scrub_query(e.query),
                    e.results.map(|n| n.min(i64::MAX as u64) as i64),
                    e.latency_us.min(i64::MAX as u64) as i64
                ])?;
            }
        }
        tx.commit()?;
        Ok(entries.len())
    }

    /// Deletes queries logged before `before`, then the oldest beyond the
    /// newest `max_rows`. Returns how many were deleted.
    pub fn prune_query_log(&mut self, before: i64, max_rows: u64) -> Result<usize> {
        let tx = write_tx(&mut self.conn)?;
        let mut deleted = tx.execute("DELETE FROM query_log WHERE at < ?1", [before])?;
        deleted += tx.execute(
            "DELETE FROM query_log WHERE id IN (
                 SELECT id FROM query_log ORDER BY at DESC, id DESC LIMIT -1 OFFSET ?1)",
            [max_rows.min(i64::MAX as u64) as i64],
        )?;
        tx.commit()?;
        if deleted > 0 {
            self.after_user_delete();
        }
        Ok(deleted)
    }

    /// Deletes the whole query log ("Clear usage data").
    pub fn clear_query_log(&mut self) -> Result<usize> {
        let deleted = self.conn.execute("DELETE FROM query_log", [])?;
        if deleted > 0 {
            self.after_user_delete();
        }
        Ok(deleted)
    }

    pub fn query_log_len(&self) -> Result<u64> {
        Ok(self
            .conn
            .query_row("SELECT count(*) FROM query_log", [], |r| {
                crate::get_u64(r, 0)
            })?)
    }

    /// The most frequent queries logged in `from..to`, most frequent first
    /// (ties by text); with `zero_only`, only runs that found nothing.
    pub fn top_queries(
        &self,
        from: i64,
        to: i64,
        zero_only: bool,
        limit: usize,
    ) -> Result<Vec<QueryCount>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT query, count(*) AS n, coalesce(sum(results = 0), 0) FROM query_log
             WHERE at >= ?1 AND at < ?2 AND (?3 = 0 OR results = 0)
             GROUP BY query ORDER BY n DESC, query LIMIT ?4",
        )?;
        let rows = stmt.query_map(params![from, to, zero_only, limit as i64], |r| {
            Ok(QueryCount {
                query: r.get(0)?,
                count: crate::get_u64(r, 1)?,
                zero: crate::get_u64(r, 2)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Records when enrichment resolved each torrent's identity (it was
    /// ambiguous or unresolved before, or its title changed). Torrents with
    /// no enrichment record are skipped. Returns how many were set.
    pub fn set_resolved_at(&mut self, resolved: &[Resolved]) -> Result<usize> {
        let tx = write_tx(&mut self.conn)?;
        let mut done = 0;
        {
            let mut stmt = tx.prepare_cached(
                "UPDATE torrent_parsed SET resolved_at = ?2 WHERE torrent_id = ?1",
            )?;
            for r in resolved {
                done += stmt.execute(params![r.torrent_id, r.at])?;
            }
        }
        tx.commit()?;
        Ok(done)
    }

    /// Torrents by when they were resolved, newest first: up to `limit`
    /// before the previous page's last `(at, torrent_id)`, or from the
    /// newest (Discover's "Recently resolved").
    pub fn recently_resolved(
        &self,
        before: Option<(i64, i64)>,
        limit: usize,
    ) -> Result<Vec<Resolved>> {
        let (at, id) = before.unwrap_or((i64::MAX, i64::MAX));
        let mut stmt = self.conn.prepare_cached(
            "SELECT torrent_id, resolved_at FROM torrent_parsed
             WHERE resolved_at IS NOT NULL AND (resolved_at, torrent_id) < (?1, ?2)
             ORDER BY resolved_at DESC, torrent_id DESC LIMIT ?3",
        )?;
        let rows = stmt.query_map(params![at, id, limit as i64], |r| {
            Ok(Resolved {
                torrent_id: r.get(0)?,
                at: r.get(1)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::user_data::test_store;

    #[test]
    fn watermarks_only_grow() {
        let mut s = test_store(0);
        assert!(s.record_index_watermark(100, 10).unwrap());
        assert!(s.record_index_watermark(100, 25).unwrap());
        assert!(!s.record_index_watermark(100, 20).unwrap(), "never down");
        assert!(!s.record_index_watermark(101, 25).unwrap(), "nothing new");
        assert!(s.record_index_watermark(102, 40).unwrap());
        // A clock set back: counted in the latest hour.
        assert!(s.record_index_watermark(90, 50).unwrap());
        assert_eq!(
            s.index_watermarks(1, 200).unwrap(),
            [(0, 0), (100, 25), (102, 50)]
        );
        assert!(matches!(
            s.record_index_watermark(0, 60),
            Err(Error::Invalid(_))
        ));
        assert!(s.record_index_watermark(1, -1).is_err());
    }

    #[test]
    fn searchable_hour_at_the_boundaries() {
        let mut s = test_store(0);
        s.record_index_watermark(100, 10).unwrap();
        s.record_index_watermark(103, 20).unwrap();
        for (id, hour) in [
            (1, Some(100)),
            (10, Some(100)),
            (11, Some(103)),
            (20, Some(103)),
            (21, None),
        ] {
            assert_eq!(s.searchable_hour(id).unwrap(), hour, "id {id}");
        }
        assert_eq!(
            s.index_watermarks(101, 103).unwrap(),
            [(100, 10), (103, 20)]
        );
        assert_eq!(s.index_watermarks(104, 200).unwrap(), [(103, 20)]);
    }

    #[test]
    fn torrents_indexed_before_tracking_have_no_hour() {
        // The migration seeds hour 0 with the highest id stored then.
        let mut s = test_store(0);
        s.conn
            .execute(
                "UPDATE index_watermark_hour SET last_id = 7 WHERE hour = 0",
                [],
            )
            .unwrap();
        s.record_index_watermark(100, 9).unwrap();
        assert_eq!(s.searchable_hour(7).unwrap(), None);
        assert_eq!(s.searchable_hour(8).unwrap(), Some(100));
        assert!(!s.record_index_watermark(101, 5).unwrap());
    }

    fn entry(at: i64, query: &str, results: Option<u64>) -> QueryLogEntry<'_> {
        QueryLogEntry {
            at,
            query,
            results,
            latency_us: 1200,
        }
    }

    #[test]
    fn nothing_is_logged_while_off() {
        let mut s = test_store(0);
        let e = [entry(1, "dune", Some(3))];
        assert_eq!(s.log_queries(QueryLogging::default(), &e).unwrap(), 0);
        assert_eq!(s.log_queries(QueryLogging::Off, &e).unwrap(), 0);
        assert_eq!(s.query_log_len().unwrap(), 0);
        assert_eq!(s.log_queries(QueryLogging::On, &e).unwrap(), 1);
        assert_eq!(s.query_log_len().unwrap(), 1);
    }

    #[test]
    fn links_are_replaced_and_queries_cut() {
        assert_eq!(
            scrub_query("dune magnet:?xt=urn:btih:abc&tr=http://t/announce?passkey=s3cret 2160p"),
            "dune [link] 2160p"
        );
        assert_eq!(scrub_query("a HTTPS://x/y?k=1 b"), "a [link] b");
        assert_eq!(scrub_query("Re:Zero s:2"), "Re:Zero s:2");
        let long = "é".repeat(1500);
        let cut = scrub_query(&long);
        assert!(cut.len() <= 2048 && cut.len() >= 2047);
        assert!(long.starts_with(&cut));

        let mut s = test_store(0);
        s.log_queries(
            QueryLogging::On,
            &[entry(1, "x udp://tracker:80/?passkey=s3cret", Some(0))],
        )
        .unwrap();
        let q: String = s
            .conn
            .query_row("SELECT query FROM query_log", [], |r| r.get(0))
            .unwrap();
        assert_eq!(q, "x [link]");
    }

    #[test]
    fn pruning_bounds_the_log() {
        let mut s = test_store(0);
        let entries: Vec<_> = (0..10).map(|i| entry(i * 100, "q", Some(1))).collect();
        s.log_queries(QueryLogging::On, &entries).unwrap();
        assert_eq!(s.prune_query_log(300, u64::MAX).unwrap(), 3);
        assert_eq!(s.query_log_len().unwrap(), 7);
        assert_eq!(s.prune_query_log(0, 4).unwrap(), 3);
        let oldest: i64 = s
            .conn
            .query_row("SELECT min(at) FROM query_log", [], |r| r.get(0))
            .unwrap();
        assert_eq!(oldest, 600, "the newest are kept");
        assert_eq!(s.prune_query_log(0, 4).unwrap(), 0);
        assert_eq!(s.clear_query_log().unwrap(), 4);
        assert_eq!(s.query_log_len().unwrap(), 0);
    }

    #[test]
    fn top_queries() {
        let mut s = test_store(0);
        s.log_queries(
            QueryLogging::On,
            &[
                entry(10, "dune", Some(5)),
                entry(11, "dune", Some(0)),
                entry(12, "dune", Some(4)),
                entry(13, "zzz", Some(0)),
                entry(14, "zzz", Some(0)),
                entry(15, "alien", None),
                entry(99, "late", Some(0)),
            ],
        )
        .unwrap();
        let top = s.top_queries(10, 99, false, 10).unwrap();
        let summary: Vec<_> = top
            .iter()
            .map(|q| (q.query.as_str(), q.count, q.zero))
            .collect();
        assert_eq!(summary, [("dune", 3, 1), ("zzz", 2, 2), ("alien", 1, 0)]);
        let zero = s.top_queries(0, 100, true, 1).unwrap();
        assert_eq!(
            zero,
            [QueryCount {
                query: "zzz".into(),
                count: 2,
                zero: 2
            }]
        );
    }

    #[test]
    fn recently_resolved_pages_newest_first() {
        let mut s = test_store(5);
        for id in 1..=4 {
            s.conn
                .execute(
                    "INSERT INTO torrent_parsed (torrent_id, parser_ver, data) VALUES (?1, 1, x'')",
                    [id],
                )
                .unwrap();
        }
        let set = [(1, 50), (2, 70), (3, 70), (5, 90)]
            .map(|(torrent_id, at)| Resolved { torrent_id, at });
        assert_eq!(s.set_resolved_at(&set).unwrap(), 3, "5 has no record");
        let page = s.recently_resolved(None, 2).unwrap();
        assert_eq!(
            page,
            [
                Resolved {
                    torrent_id: 3,
                    at: 70
                },
                Resolved {
                    torrent_id: 2,
                    at: 70
                }
            ]
        );
        let rest = s.recently_resolved(Some((70, 2)), 10).unwrap();
        assert_eq!(
            rest,
            [Resolved {
                torrent_id: 1,
                at: 50
            }]
        );
        // Re-enrichment keeps the time.
        s.conn
            .execute(
                "UPDATE torrent_parsed SET data = x'01' WHERE torrent_id = 1",
                [],
            )
            .unwrap();
        assert_eq!(s.recently_resolved(Some((70, 2)), 10).unwrap(), rest);
    }
}
