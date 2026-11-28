//! Worker provenance and purge-by-worker (WK-12, architecture §19.4 and
//! §19.7, plan Q10; `docs/decisions/2026-11-28-WK-12.md`).
//!
//! `torrent_worker` (V17) names the remote worker whose record wrote a
//! torrent's info-derived columns: a new row, or the partial import its
//! record upgraded. It's written in the commit's own transaction
//! ([`WriteBatch::prepared_by`](crate::WriteBatch::prepared_by),
//! [`Store::promote_from`]), so a crash can't leave a remote row the purge
//! can't find. The built-in worker (0) writes nothing.
//!
//! **Purge-by-worker** ([`Store::purge_worker_step`]) deletes those
//! torrents a bounded step at a time, each step one write transaction, so
//! WK-13's job can run it over millions of rows without holding the
//! writer up, and stop between steps. A deletion goes the way every other
//! one does:
//!
//! - `reindex_log` gets the id in the same transaction, so the search
//!   indexer drops the document;
//! - V11's `torrents_removed` trigger counts it in the rollups as removed;
//! - V12's `snapshot_torrent_deleted` trigger journals its hashes, so the
//!   snapshot purge removes it from every snapshot too (owner §2.21);
//! - its child rows go by `ON DELETE CASCADE` (files, extra, parsed
//!   record, claims, collection entries, its `torrent_worker` row), and
//!   `secure_delete` zeroes the pages (owner §2.17).
//!
//! Nothing else is touched: holds (tombstones, quarantine, `cs_hashes`
//! and the worker's floor reports, which [`Store::purge_worker_floor`]
//! removes), the fetch queue (which records no holder), the worker's
//! Insights rows (WK-13's) and every torrent without that worker's row.
//! A purged key stays in the have-filter, so it comes back only through
//! rediscovery, fetched and judged again by a worker in good standing.

use rusqlite::{Connection, OptionalExtension, params};

use crate::{Error, Result, Store, begin_write};

/// The most torrents one [`Store::purge_worker_step`] deletes: a step holds
/// the write lock for at most about 250 ms on a typical machine (measured
/// in the decisions file), since the writer's latency comes first.
pub const MAX_WORKER_PURGE_STEP: usize = 1024;

/// One worker's stored torrents, for the purge preview and Settings.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct WorkerTorrents {
    /// Torrents whose metadata the worker's records wrote.
    pub torrents: u64,
    /// Of those, the ones that hold a source's claims, which a purge
    /// deletes with them (a source that reports the key again stores its
    /// claims as an unverified item).
    pub with_claims: u64,
}

/// What one [`Store::purge_worker_step`] did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WorkerPurgeStep {
    /// The torrents deleted, in id order. Ids are never reused, so
    /// hot-column slots left behind are never read again.
    pub deleted: Vec<i64>,
    /// Nothing of the worker's is left: the purge is complete (until a
    /// commit records another, which WK-13's standing refuses).
    pub done: bool,
}

/// Records that `worker` prepared torrent `id`, in the commit's
/// transaction. Does nothing for the built-in worker (0) or a torrent
/// that wasn't stored. A later record of another worker replaces it: the
/// row follows whoever last wrote the info-derived columns.
pub(crate) fn record(tx: &Connection, id: Option<i64>, worker: u32) -> Result<()> {
    let Some(id) = id.filter(|_| worker != 0) else {
        return Ok(());
    };
    tx.prepare_cached(
        "INSERT INTO torrent_worker (torrent_id, worker) VALUES (?1, ?2)
         ON CONFLICT (torrent_id) DO UPDATE SET worker = excluded.worker",
    )?
    .execute(params![id, i64::from(worker)])?;
    Ok(())
}

impl Store {
    /// The remote worker whose record wrote torrent `id`'s metadata;
    /// `None` for the built-in worker, an import, a row from before V17,
    /// or a torrent that isn't stored.
    pub fn torrent_worker(&self, id: i64) -> Result<Option<u32>> {
        let w: Option<i64> = self
            .conn
            .prepare_cached("SELECT worker FROM torrent_worker WHERE torrent_id = ?1")?
            .query_row([id], |r| r.get(0))
            .optional()?;
        w.map(|w| u32::try_from(w).map_err(|_| Error::Corrupt("worker id out of range")))
            .transpose()
    }

    /// How many torrents `worker`'s records wrote, and how many of them
    /// hold claims (see [`WorkerTorrents`]). 0 for the built-in worker.
    pub fn worker_torrents(&self, worker: u32) -> Result<WorkerTorrents> {
        let (torrents, with_claims): (i64, i64) = self
            .conn
            .prepare_cached(
                "SELECT count(*),
                        count(*) FILTER (WHERE EXISTS (
                            SELECT 1 FROM torrent_claims c WHERE c.torrent_id = w.torrent_id))
                 FROM torrent_worker w WHERE w.worker = ?1",
            )?
            .query_row([i64::from(worker)], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok(WorkerTorrents {
            torrents: torrents as u64,
            with_claims: with_claims as u64,
        })
    }

    /// One step of purge-by-worker: deletes up to `limit` (capped at
    /// [`MAX_WORKER_PURGE_STEP`]) of the torrents `worker`'s records
    /// wrote, lowest ids first, in one write transaction (see the module
    /// docs for what goes with them). Needs no cursor: the deleted rows'
    /// `torrent_worker` rows go with them, so each step takes the next,
    /// and a purge stopped between steps carries on where it was. Running
    /// it again after it's `done` deletes nothing.
    ///
    /// `Error::Invalid` for the built-in worker (0), whose torrents have
    /// no rows: purging "worker 0" must never read as purging them.
    pub fn purge_worker_step(&mut self, worker: u32, limit: usize) -> Result<WorkerPurgeStep> {
        if worker == 0 {
            return Err(Error::Invalid("purge of the built-in worker"));
        }
        let limit = limit.clamp(1, MAX_WORKER_PURGE_STEP);
        let tx = begin_write(&mut self.conn)?;
        let ids: Vec<i64> = tx
            .prepare_cached(
                "SELECT torrent_id FROM torrent_worker WHERE worker = ?1
                 ORDER BY torrent_id LIMIT ?2",
            )?
            .query_map(params![i64::from(worker), limit as i64 + 1], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        let done = ids.len() <= limit;
        let ids = &ids[..ids.len().min(limit)];
        {
            // Cached: the delete carries Insights' trigger (V11) and the
            // snapshot journal's (V12), which SQLite would otherwise
            // compile again for every row.
            let mut delete = tx.prepare_cached("DELETE FROM torrents WHERE id = ?1")?;
            let mut log = tx.prepare_cached("INSERT INTO reindex_log (torrent_id) VALUES (?1)")?;
            for id in ids {
                if delete.execute([id])? != 1 {
                    // `ON DELETE CASCADE` keeps every row's torrent.
                    return Err(Error::Corrupt("torrent_worker row without its torrent"));
                }
                log.execute([id])?;
            }
        }
        tx.commit()?;
        if !ids.is_empty() {
            self.scrub_wal();
        }
        Ok(WorkerPurgeStep {
            deleted: ids.to_vec(),
            done,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> Store {
        Store::open_in_memory(crate::StoreConfig::default()).unwrap()
    }

    fn torrent(s: &Store, id: i64) {
        s.conn
            .execute(
                "INSERT INTO torrents (id, info_hash, name, size, piece_len, piece_count,
                     file_count, info_size, flags, discovered_at, metadata_at, last_seen_at, sources)
                 VALUES (?1, randomblob(20), 'n', 1, 16384, 1, 1, 100, 0, 0, 0, 0, 1)",
                [id],
            )
            .unwrap();
    }

    #[test]
    fn the_built_in_worker_records_nothing() {
        let s = store();
        torrent(&s, 1);
        record(&s.conn, Some(1), 0).unwrap();
        record(&s.conn, None, 7).unwrap();
        let n: i64 = s
            .conn
            .query_row("SELECT count(*) FROM torrent_worker", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 0);
        assert_eq!(s.torrent_worker(1).unwrap(), None);
    }

    #[test]
    fn a_later_record_replaces_the_worker() {
        let s = store();
        torrent(&s, 1);
        record(&s.conn, Some(1), 7).unwrap();
        record(&s.conn, Some(1), u32::MAX).unwrap();
        assert_eq!(s.torrent_worker(1).unwrap(), Some(u32::MAX));
    }

    #[test]
    fn the_schema_refuses_worker_zero_and_orphans() {
        let s = store();
        torrent(&s, 1);
        let zero = s.conn.execute(
            "INSERT INTO torrent_worker (torrent_id, worker) VALUES (1, 0)",
            [],
        );
        assert!(zero.is_err(), "CHECK refuses 0");
        let big = s.conn.execute(
            "INSERT INTO torrent_worker (torrent_id, worker) VALUES (1, 4294967296)",
            [],
        );
        assert!(big.is_err(), "CHECK refuses ids past u32");
        let orphan = s.conn.execute(
            "INSERT INTO torrent_worker (torrent_id, worker) VALUES (2, 7)",
            [],
        );
        assert!(orphan.is_err(), "the foreign key refuses a missing torrent");
    }

    #[test]
    fn purging_worker_zero_is_refused() {
        let mut s = store();
        assert!(matches!(s.purge_worker_step(0, 10), Err(Error::Invalid(_))));
    }

    #[test]
    fn steps_are_bounded_and_say_when_done() {
        let mut s = store();
        for id in 1..=5 {
            torrent(&s, id);
            record(&s.conn, Some(id), 7).unwrap();
        }
        let a = s.purge_worker_step(7, 2).unwrap();
        assert_eq!((a.deleted, a.done), (vec![1, 2], false));
        let b = s.purge_worker_step(7, 2).unwrap();
        assert_eq!((b.deleted, b.done), (vec![3, 4], false));
        let c = s.purge_worker_step(7, 2).unwrap();
        assert_eq!((c.deleted, c.done), (vec![5], true));
        let d = s.purge_worker_step(7, 2).unwrap();
        assert_eq!((d.deleted, d.done), (vec![], true));
        // An exact fit says done on its own step.
        for id in 6..=7 {
            torrent(&s, id);
            record(&s.conn, Some(id), 7).unwrap();
        }
        let e = s.purge_worker_step(7, 2).unwrap();
        assert_eq!((e.deleted, e.done), (vec![6, 7], true));
        // A limit of 0 still makes progress.
        torrent(&s, 8);
        record(&s.conn, Some(8), 7).unwrap();
        assert_eq!(s.purge_worker_step(7, 0).unwrap().deleted, vec![8]);
    }
}
