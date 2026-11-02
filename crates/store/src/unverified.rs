//! The unverified tier, claims and plugin state (V14, architecture §18.5;
//! `docs/decisions/2026-11-02-SRC-1.md`).
//!
//! - **Unverified items** are keys a source found with claims but no
//!   metadata yet. They take ids from `id_seq` and keep them when they
//!   become torrents ([`Store::promote`]), so index documents, hot-column
//!   slots and saved-search hits stay valid.
//! - **Claims** are what one source asserted about a key
//!   ([`claims`](crate::claims)), on an unverified item or a stored torrent.
//! - **Plugin state** is each plugin's key-value store (cursors, ETags).
//!
//! **Content safety.** Nothing here writes under a hash in `tombstones`,
//! `quarantine` or `cs_hashes`, and the schema's triggers delete the item
//! or claims under a hash that enters one later (journalling the hash for
//! the snapshot purge). The filter engine still judges every claim before
//! it reaches these calls, and again when its rules change: the re-judge
//! pages ([`Store::unverified_after`], [`Store::claims_after`]) are
//! bounded in rows and decoded bytes, and each write is its own short
//! transaction, so a sweep can stop between any two.

use std::fmt;

use rusqlite::{Connection, OptionalExtension, Transaction, params};

use crate::claims::{self, Claims};
use crate::filter::{self, Filtered};
use crate::sources::{SourceId, SourceState};
use crate::{Error, PreparedTorrent, Result, Store, begin_write, flags};

/// `unverified_items.flags`: found only through private sources (§18.7).
/// Never sent to public peer finding. A public sighting clears it.
const PRIVATE: i64 = 1;

/// Rows in one re-judge page at most.
pub const MAX_PAGE: usize = 1024;
/// Decoded claim text in one re-judge page at most. A page always holds
/// at least one row, so a page holds less than this plus one row's
/// [`claims::MAX_PLAIN`].
pub const PAGE_TEXT: usize = 8 << 20;
/// Rows one purge step looks at, at most.
pub const MAX_PURGE_STEP: usize = 4096;

/// Bytes in a plugin state key (as `lodestar_sources::state`).
pub const STATE_MAX_KEY: usize = 128;
/// Bytes in a plugin state value.
pub const STATE_MAX_VALUE: usize = 64 * 1024;
/// Keys per plugin.
pub const STATE_MAX_KEYS: usize = 1024;

/// Which network a key belongs to (§18.6). Values are persisted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Network {
    /// Clearnet BitTorrent: the key is a 20-byte swarm hash (a v1 hash or
    /// a truncated v2 one).
    Bt = 0,
}

impl Network {
    fn from_sql(v: i64) -> Result<Network> {
        match v {
            0 => Ok(Network::Bt),
            _ => Err(Error::Corrupt("unknown network")),
        }
    }
}

/// A key a source found, for [`Store::put_unverified`].
#[derive(Debug, Clone, Copy)]
pub struct NewUnverified<'a> {
    pub network: Network,
    pub key: &'a [u8],
    pub source: SourceId,
    /// Found through a private source (§18.7).
    pub private_source: bool,
    /// Already judged by the filter engine. A new item needs them: bare
    /// keys wait in the verification queue, not here (§18.5).
    pub claims: Option<&'a Claims>,
    pub at: i64,
}

/// What [`Store::put_unverified`] did with one key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Put {
    /// A new unverified item, with this id.
    New(i64),
    /// An unverified item already held the key: it gained the source, and
    /// its claims when they're the source's own or it had none.
    Existing(i64),
    /// A stored torrent holds the key: it gained the source's bit, and
    /// the claims went to `torrent_claims`.
    Torrent(i64),
    /// The key is tombstoned, quarantined or on the CS-1 list: nothing
    /// was written.
    Held,
    /// A new key with no claims: nothing was written.
    Bare,
}

/// An unverified item, for the re-judge sweep and the verification queue.
/// Its `Debug` holds no claim text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnverifiedItem {
    pub id: i64,
    pub network: Network,
    pub key: Vec<u8>,
    /// The source whose claims these are, or one that found it.
    pub source: SourceId,
    /// Every source that found it, a bitset over [`SourceId`]s.
    pub sources: u64,
    pub private_source: bool,
    /// `None` when the item has none (they were purged), or they don't
    /// decode (`unreadable`).
    pub claims: Option<Claims>,
    /// The stored claims don't decode: drop the item, it can't be judged.
    pub unreadable: bool,
    pub first_at: i64,
    pub last_at: i64,
}

/// A torrent's claims from one source, for the re-judge sweep.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredClaims {
    pub torrent_id: i64,
    pub source: SourceId,
    /// `None` when they don't decode: drop them.
    pub claims: Option<Claims>,
}

/// An unverified item the rules now hold back, or that can't be judged.
#[derive(Debug, Clone, Copy)]
pub struct RefiledUnverified<'a> {
    pub id: i64,
    /// The decision, written as ingest writes one (a filter hit, and a
    /// hold on the hashes when `held` is set). `None` only removes it.
    pub filtered: Option<Filtered<'a>>,
}

/// A torrent's claims the rules now hold back, or that can't be judged.
#[derive(Debug, Clone, Copy)]
pub struct RefiledClaims<'a> {
    pub torrent_id: i64,
    pub source: SourceId,
    pub filtered: Option<Filtered<'a>>,
}

/// What [`Store::promote`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Promoted {
    /// Stored as a torrent under the item's own id.
    Kept(i64),
    /// A torrent already held one of its hashes: it gained the item's
    /// sources and claims, and the item is gone. Its id isn't reused.
    Merged(i64),
    /// One of its hashes is held: nothing was stored, and the item is
    /// gone (its hash journalled for the snapshot purge).
    Held,
    /// No such unverified item.
    Gone,
}

/// Where a chunked [`Store::purge_source`] goes on from. Start with
/// [`PurgeCursor::START`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PurgeCursor {
    stage: Stage,
    after: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stage {
    Unverified,
    Claims,
    Torrents,
    State,
}

impl PurgeCursor {
    pub const START: PurgeCursor = PurgeCursor {
        stage: Stage::Unverified,
        after: 0,
    };
}

/// One step of [`Store::purge_source`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PurgeStep {
    /// Where to go on from; `None` when the purge is done.
    pub next: Option<PurgeCursor>,
    /// Unverified items only this source found, deleted.
    pub unverified_deleted: usize,
    /// Unverified items others found too, kept without this source.
    pub unverified_kept: usize,
    /// Claims of stored torrents deleted.
    pub claims_deleted: usize,
    /// Torrents only this source found, deleted: the search index drops
    /// them through `reindex_log`, as [`Store::delete_torrents`] does.
    pub torrents_deleted: Vec<i64>,
    /// Torrents others found too, kept without this source's bit.
    pub torrents_kept: usize,
    /// Plugin state keys deleted.
    pub state_deleted: usize,
}

impl Store {
    /// Records keys sources found, in one transaction (see [`Put`]).
    /// Claims must be within the [`claims`] limits (`Error::Invalid`
    /// otherwise, and nothing is written).
    pub fn put_unverified(&mut self, items: &[NewUnverified<'_>]) -> Result<Vec<Put>> {
        let level = self.config.compression_level;
        let blobs = items
            .iter()
            .map(|i| i.claims.map(|c| encode(c, level)).transpose())
            .collect::<Result<Vec<_>>>()?;
        for i in items {
            check_key(i.network, i.key)?;
        }
        let tx = begin_write(&mut self.conn)?;
        let mut out = Vec::with_capacity(items.len());
        for (item, blob) in items.iter().zip(&blobs) {
            out.push(put_one(&tx, item, blob.as_deref())?);
        }
        tx.commit()?;
        Ok(out)
    }

    /// Stores `source`'s claims on a torrent, replacing its earlier ones.
    /// False when the torrent isn't stored or one of its hashes is held.
    pub fn put_claims(
        &mut self,
        torrent_id: i64,
        source: SourceId,
        claims: &Claims,
    ) -> Result<bool> {
        let blob = encode(claims, self.config.compression_level)?;
        let tx = begin_write(&mut self.conn)?;
        let Some(hashes) = torrent_hashes(&tx, torrent_id)? else {
            return Ok(false);
        };
        if held(&tx, &hashes)? {
            return Ok(false);
        }
        write_claims(&tx, torrent_id, source, &blob)?;
        tx.commit()?;
        Ok(true)
    }

    /// A torrent's claims, by source. Claims that don't decode are left
    /// out.
    pub fn torrent_claims(&self, torrent_id: i64) -> Result<Vec<(SourceId, Claims)>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT source_id, data FROM torrent_claims WHERE torrent_id = ?1 ORDER BY source_id",
        )?;
        let rows = stmt.query_map([torrent_id], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, Vec<u8>>(1)?))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (source, data) = row?;
            if let Ok(c) = claims::decode(&data) {
                out.push((SourceId::from_sql(source)?, c));
            }
        }
        Ok(out)
    }

    /// The unverified item under a key.
    pub fn unverified(&self, network: Network, key: &[u8]) -> Result<Option<UnverifiedItem>> {
        let mut stmt = self.conn.prepare_cached(&format!(
            "SELECT {ITEM_COLUMNS} FROM unverified_items WHERE network = ?1 AND key = ?2"
        ))?;
        let row = stmt
            .query_row(params![network as i64, key], read_item)
            .optional()?;
        row.map(|r| r.into_item()).transpose()
    }

    /// Unverified items after `after` (by id): one keyset page of the
    /// re-judge sweep or the verification queue. At most `limit` (capped
    /// at [`MAX_PAGE`]) rows, and no more decoded text than [`PAGE_TEXT`]
    /// past the first row.
    pub fn unverified_after(&self, after: i64, limit: usize) -> Result<Vec<UnverifiedItem>> {
        let mut stmt = self.conn.prepare_cached(&format!(
            "SELECT {ITEM_COLUMNS} FROM unverified_items WHERE id > ?1 ORDER BY id LIMIT ?2"
        ))?;
        let rows = stmt.query_map(params![after, limit.clamp(1, MAX_PAGE) as i64], read_item)?;
        let mut out = Vec::new();
        let mut text = 0;
        for row in rows {
            let item = row?.into_item()?;
            text += item.claims.as_ref().map_or(0, Claims::text_len);
            out.push(item);
            if text >= PAGE_TEXT {
                break;
            }
        }
        Ok(out)
    }

    /// Torrent claims after `after` (`(torrent_id, source)`, exclusive):
    /// one keyset page of the re-judge sweep, bounded as
    /// [`unverified_after`](Store::unverified_after).
    pub fn claims_after(&self, after: (i64, SourceId), limit: usize) -> Result<Vec<StoredClaims>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT torrent_id, source_id, data FROM torrent_claims
             WHERE (torrent_id, source_id) > (?1, ?2)
             ORDER BY torrent_id, source_id LIMIT ?3",
        )?;
        let rows = stmt.query_map(
            params![after.0, after.1.get(), limit.clamp(1, MAX_PAGE) as i64],
            |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, Vec<u8>>(2)?,
                ))
            },
        )?;
        let mut out = Vec::new();
        let mut text = 0;
        for row in rows {
            let (torrent_id, source, data) = row?;
            let claims = claims::decode(&data).ok();
            text += claims.as_ref().map_or(0, Claims::text_len);
            out.push(StoredClaims {
                torrent_id,
                source: SourceId::from_sql(source)?,
                claims,
            });
            if text >= PAGE_TEXT {
                break;
            }
        }
        Ok(out)
    }

    /// Removes unverified items the rules now hold back (or that can't be
    /// judged), in one transaction, writing each decision as ingest would.
    /// Every removed key is journalled for the snapshot purge.
    ///
    /// `current` is asked inside the transaction whether the rules the
    /// items were judged under are still the engine's; when they aren't,
    /// nothing is written and this returns `None`. Items already gone are
    /// skipped. Returns the ids removed.
    pub fn refile_unverified(
        &mut self,
        refiled: &[RefiledUnverified<'_>],
        current: impl FnOnce() -> bool,
    ) -> Result<Option<Vec<i64>>> {
        let tx = begin_write(&mut self.conn)?;
        if !current() {
            return Ok(None);
        }
        let mut done = Vec::new();
        for r in refiled {
            let exists = tx
                .prepare_cached("SELECT 1 FROM unverified_items WHERE id = ?1")?
                .exists([r.id])?;
            if !exists {
                continue;
            }
            if let Some(f) = &r.filtered {
                filter::write(&tx, std::slice::from_ref(f))?;
            }
            delete_item(&tx, r.id, true)?;
            done.push(r.id);
        }
        tx.commit()?;
        if !done.is_empty() {
            self.scrub_wal();
        }
        Ok(Some(done))
    }

    /// Removes torrent claims the rules now hold back (or that can't be
    /// judged), as [`refile_unverified`](Store::refile_unverified) does
    /// for items. The torrents themselves are judged on their verified
    /// metadata, not here. Returns how many were removed, or `None` when
    /// the rules moved on.
    pub fn refile_claims(
        &mut self,
        refiled: &[RefiledClaims<'_>],
        current: impl FnOnce() -> bool,
    ) -> Result<Option<usize>> {
        let tx = begin_write(&mut self.conn)?;
        if !current() {
            return Ok(None);
        }
        let mut done = 0;
        for r in refiled {
            let n = tx
                .prepare_cached(
                    "DELETE FROM torrent_claims WHERE torrent_id = ?1 AND source_id = ?2",
                )?
                .execute(params![r.torrent_id, r.source.get()])?;
            if n == 0 {
                continue;
            }
            if let Some(f) = &r.filtered {
                filter::write(&tx, std::slice::from_ref(f))?;
            }
            done += n;
        }
        tx.commit()?;
        if done > 0 {
            self.scrub_wal();
        }
        Ok(Some(done))
    }

    /// Stores an unverified item's verified metadata as a torrent, in one
    /// transaction (see [`Promoted`]). The torrent keeps the item's id and
    /// gains its sources and claims; `t.source` is added too. It stays
    /// private-source if the item was and `t` says so. The id goes into
    /// `reindex_log`: it's below the search index's id watermark.
    ///
    /// `Error::Invalid` when `t`'s hashes don't include the item's key.
    pub fn promote(&mut self, id: i64, t: &PreparedTorrent<'_>) -> Result<Promoted> {
        let p = t.torrent;
        if p.files.dict_id.is_some_and(|d| !self.codec.has_dict(d)) {
            return Err(Error::Corrupt("prepared blob names an unknown dictionary"));
        }
        let tx = begin_write(&mut self.conn)?;
        let row = tx
            .prepare_cached(&format!(
                "SELECT {ITEM_COLUMNS} FROM unverified_items WHERE id = ?1"
            ))?
            .query_row([id], read_item)
            .optional()?;
        let Some(row) = row else {
            return Ok(Promoted::Gone);
        };
        let hashes: Vec<[u8; 20]> = p.swarm_hashes().map(|h| h.0).collect();
        if row.network != Network::Bt as i64 || !hashes.iter().any(|h| h[..] == row.key[..]) {
            return Err(Error::Invalid(
                "promotion of a key the torrent doesn't hold",
            ));
        }
        if held(&tx, &hashes)? {
            delete_item(&tx, id, true)?;
            tx.commit()?;
            self.scrub_wal();
            return Ok(Promoted::Held);
        }
        let item_source = SourceId::from_sql(row.source)?;
        let bits = (row.sources as u64) | t.source.bit() as u64;
        let public = row.flags & PRIVATE == 0 || t.extra_flags & flags::PRIVATE_SOURCE == 0;
        let out = if let Some(existing) = crate::find_any(&tx, p.swarm_hashes())? {
            crate::add_sources(&tx, existing, bits)?;
            if public {
                crate::make_public(&tx, existing)?;
            }
            if let Some(blob) = &row.claims {
                tx.prepare_cached(
                    "INSERT INTO torrent_claims (torrent_id, source_id, data) VALUES (?1, ?2, ?3)
                     ON CONFLICT DO NOTHING",
                )?
                .execute(params![existing, item_source.get(), blob])?;
            }
            Promoted::Merged(existing)
        } else {
            let mut fl = p.flags | t.extra_flags;
            if public {
                fl &= !flags::PRIVATE_SOURCE;
            } else {
                fl |= flags::PRIVATE_SOURCE;
            }
            let discovered = row.first_at.min(t.discovered_at);
            // `insert_new` takes the next id from `id_seq`: point it at the
            // item's id for this one insert, then put it back. The item's
            // id came from the sequence and was never a torrent's.
            let next: i64 = tx.query_row("SELECT next FROM id_seq", [], |r| r.get(0))?;
            tx.execute("UPDATE id_seq SET next = ?1", [id])?;
            let got = crate::insert_new(&tx, p, fl, discovered, t.metadata_at, item_source)?;
            tx.execute("UPDATE id_seq SET next = ?1", [next])?;
            if got != id {
                return Err(Error::Corrupt("promotion took another id"));
            }
            crate::add_sources(&tx, id, bits)?;
            if let Some(blob) = &row.claims {
                write_claims(&tx, id, item_source, blob)?;
            }
            tx.prepare_cached("INSERT INTO reindex_log (torrent_id) VALUES (?1)")?
                .execute([id])?;
            Promoted::Kept(id)
        };
        // A promotion isn't a deletion: nothing is journalled.
        delete_item(&tx, id, false)?;
        tx.commit()?;
        Ok(out)
    }

    /// One step of purging `source` (§18.1: items only it found go, with
    /// all its claims and state; items others found too lose its bit).
    /// Each step is one transaction over at most `limit` (capped at
    /// [`MAX_PURGE_STEP`]) rows, so a purge of millions runs as a job that
    /// can stop between steps. Deleted keys and torrents are journalled
    /// for the snapshot purge.
    ///
    /// The source must be disabled or purged first (`Error::Invalid`
    /// otherwise), so nothing it finds meanwhile slips past the purge.
    pub fn purge_source(
        &mut self,
        source: SourceId,
        from: PurgeCursor,
        limit: usize,
    ) -> Result<PurgeStep> {
        let limit = limit.clamp(1, MAX_PURGE_STEP);
        let bit = source.bit();
        let tx = begin_write(&mut self.conn)?;
        let state: Option<i64> = tx
            .query_row(
                "SELECT state FROM sources WHERE id = ?1",
                [source.get()],
                |r| r.get(0),
            )
            .optional()?;
        match state {
            None => return Err(Error::NotFound("source")),
            Some(s) if s == SourceState::Disabled as i64 || s == SourceState::Purged as i64 => {}
            Some(_) => return Err(Error::Invalid("purge of a source that isn't disabled")),
        }
        let mut step = PurgeStep::default();
        let mut scanned = 0;
        let mut last = from.after;
        match from.stage {
            Stage::Unverified => {
                let rows: Vec<(i64, i64, i64)> = tx
                    .prepare_cached(
                        "SELECT id, sources, source_id FROM unverified_items
                         WHERE id > ?1 ORDER BY id LIMIT ?2",
                    )?
                    .query_map(params![from.after, limit as i64], |r| {
                        Ok((r.get(0)?, r.get(1)?, r.get(2)?))
                    })?
                    .collect::<rusqlite::Result<_>>()?;
                for (id, sources, owner) in rows {
                    scanned += 1;
                    last = id;
                    if sources & bit == 0 {
                        continue;
                    }
                    let rest = sources & !bit;
                    if rest == 0 {
                        delete_item(&tx, id, true)?;
                        step.unverified_deleted += 1;
                    } else if owner == i64::from(source.get()) {
                        tx.prepare_cached(
                            "UPDATE unverified_items
                             SET sources = ?2, source_id = ?3, claims = NULL WHERE id = ?1",
                        )?
                        .execute(params![
                            id,
                            rest,
                            rest.trailing_zeros()
                        ])?;
                        step.unverified_kept += 1;
                    } else {
                        tx.prepare_cached(
                            "UPDATE unverified_items SET sources = ?2 WHERE id = ?1",
                        )?
                        .execute(params![id, rest])?;
                        step.unverified_kept += 1;
                    }
                }
            }
            Stage::Claims => {
                scanned = tx
                    .prepare_cached(
                        "DELETE FROM torrent_claims WHERE rowid IN (
                             SELECT rowid FROM torrent_claims WHERE source_id = ?1 LIMIT ?2)",
                    )?
                    .execute(params![source.get(), limit as i64])?;
                step.claims_deleted = scanned;
            }
            Stage::Torrents => {
                let rows: Vec<(i64, i64)> = tx
                    .prepare_cached(
                        "SELECT id, sources FROM torrents WHERE id > ?1 ORDER BY id LIMIT ?2",
                    )?
                    .query_map(params![from.after, limit as i64], |r| {
                        Ok((r.get(0)?, r.get(1)?))
                    })?
                    .collect::<rusqlite::Result<_>>()?;
                // Cached: the delete carries Insights' trigger (V11), so
                // the rollups count it as removed, as any deletion.
                let mut delete = tx.prepare_cached("DELETE FROM torrents WHERE id = ?1")?;
                let mut log =
                    tx.prepare_cached("INSERT INTO reindex_log (torrent_id) VALUES (?1)")?;
                let mut clear =
                    tx.prepare_cached("UPDATE torrents SET sources = sources & ~?2 WHERE id = ?1")?;
                for (id, sources) in rows {
                    scanned += 1;
                    last = id;
                    if sources & bit == 0 {
                        continue;
                    }
                    if sources == bit {
                        delete.execute([id])?;
                        log.execute([id])?;
                        step.torrents_deleted.push(id);
                    } else {
                        clear.execute(params![id, bit])?;
                        step.torrents_kept += 1;
                    }
                }
            }
            Stage::State => {
                step.state_deleted = tx
                    .prepare_cached("DELETE FROM source_state WHERE source_id = ?1")?
                    .execute([source.get()])?;
            }
        }
        step.next = if from.stage == Stage::State {
            None
        } else if scanned < limit {
            let stage = match from.stage {
                Stage::Unverified => Stage::Claims,
                Stage::Claims => Stage::Torrents,
                _ => Stage::State,
            };
            Some(PurgeCursor { stage, after: 0 })
        } else {
            Some(PurgeCursor {
                stage: from.stage,
                after: last,
            })
        };
        tx.commit()?;
        if step.unverified_deleted + step.unverified_kept + step.claims_deleted > 0
            || !step.torrents_deleted.is_empty()
        {
            self.scrub_wal();
        }
        Ok(step)
    }

    /// A plugin's state value.
    pub fn plugin_state(&self, source: SourceId, key: &str) -> Result<Option<Vec<u8>>> {
        check_state(key, None)?;
        Ok(self
            .conn
            .prepare_cached("SELECT value FROM source_state WHERE source_id = ?1 AND key = ?2")?
            .query_row(params![source.get(), key], |r| r.get(0))
            .optional()?)
    }

    /// Sets (or with `None` removes) a plugin's state value.
    /// `Error::Invalid` for a key or value over the limits, or a new key
    /// past [`STATE_MAX_KEYS`].
    pub fn set_plugin_state(
        &mut self,
        source: SourceId,
        key: &str,
        value: Option<&[u8]>,
    ) -> Result<()> {
        self.swap_plugin_state(source, key, None, value, false)
            .map(|_| ())
    }

    /// Compare-and-set: writes `new` (or removes the key for `None`) only
    /// when the current value is `expected` (`None`: absent). Whether it
    /// wrote.
    pub fn compare_and_set_plugin_state(
        &mut self,
        source: SourceId,
        key: &str,
        expected: Option<&[u8]>,
        new: Option<&[u8]>,
    ) -> Result<bool> {
        self.swap_plugin_state(source, key, expected, new, true)
    }

    fn swap_plugin_state(
        &mut self,
        source: SourceId,
        key: &str,
        expected: Option<&[u8]>,
        new: Option<&[u8]>,
        compare: bool,
    ) -> Result<bool> {
        check_state(key, new)?;
        let tx = begin_write(&mut self.conn)?;
        let current: Option<Vec<u8>> = tx
            .prepare_cached("SELECT value FROM source_state WHERE source_id = ?1 AND key = ?2")?
            .query_row(params![source.get(), key], |r| r.get(0))
            .optional()?;
        if compare && current.as_deref() != expected {
            return Ok(false);
        }
        match new {
            Some(v) => {
                if current.is_none() {
                    let n: i64 = tx
                        .prepare_cached("SELECT count(*) FROM source_state WHERE source_id = ?1")?
                        .query_row([source.get()], |r| r.get(0))?;
                    if n >= STATE_MAX_KEYS as i64 {
                        return Err(Error::Invalid("plugin state is full"));
                    }
                }
                tx.prepare_cached(
                    "INSERT INTO source_state (source_id, key, value) VALUES (?1, ?2, ?3)
                     ON CONFLICT DO UPDATE SET value = excluded.value",
                )?
                .execute(params![source.get(), key, v])?;
            }
            None => {
                tx.prepare_cached("DELETE FROM source_state WHERE source_id = ?1 AND key = ?2")?
                    .execute(params![source.get(), key])?;
            }
        }
        tx.commit()?;
        Ok(true)
    }
}

const ITEM_COLUMNS: &str = "id, network, key, source_id, sources, flags, claims, first_at, last_at";

/// An `unverified_items` row as stored.
struct RawItem {
    id: i64,
    network: i64,
    key: Vec<u8>,
    source: i64,
    sources: i64,
    flags: i64,
    claims: Option<Vec<u8>>,
    first_at: i64,
    last_at: i64,
}

fn read_item(r: &rusqlite::Row<'_>) -> rusqlite::Result<RawItem> {
    Ok(RawItem {
        id: r.get(0)?,
        network: r.get(1)?,
        key: r.get(2)?,
        source: r.get(3)?,
        sources: r.get(4)?,
        flags: r.get(5)?,
        claims: r.get(6)?,
        first_at: r.get(7)?,
        last_at: r.get(8)?,
    })
}

impl RawItem {
    fn into_item(self) -> Result<UnverifiedItem> {
        let (claims, unreadable) = match &self.claims {
            None => (None, false),
            Some(b) => match claims::decode(b) {
                Ok(c) => (Some(c), false),
                Err(_) => (None, true),
            },
        };
        Ok(UnverifiedItem {
            id: self.id,
            network: Network::from_sql(self.network)?,
            key: self.key,
            source: SourceId::from_sql(self.source)?,
            sources: self.sources as u64,
            private_source: self.flags & PRIVATE != 0,
            claims,
            unreadable,
            first_at: self.first_at,
            last_at: self.last_at,
        })
    }
}

fn encode(c: &Claims, level: i32) -> Result<Vec<u8>> {
    claims::encode(c, level).map_err(|_| Error::Invalid("claims over a limit"))
}

fn check_key(network: Network, key: &[u8]) -> Result<()> {
    match network {
        Network::Bt if key.len() == 20 => Ok(()),
        Network::Bt => Err(Error::Invalid("a BitTorrent key is 20 bytes")),
    }
}

fn check_state(key: &str, value: Option<&[u8]>) -> Result<()> {
    if key.is_empty() || key.len() > STATE_MAX_KEY {
        return Err(Error::Invalid("plugin state key"));
    }
    if value.is_some_and(|v| v.len() > STATE_MAX_VALUE) {
        return Err(Error::Invalid("plugin state value"));
    }
    Ok(())
}

fn put_one(tx: &Transaction<'_>, item: &NewUnverified<'_>, blob: Option<&[u8]>) -> Result<Put> {
    let key = item.key;
    let swarm: [u8; 20] = key.try_into().map_err(|_| Error::Invalid("key length"))?;
    if held(tx, &[swarm])? {
        return Ok(Put::Held);
    }
    let bit = item.source.bit();
    if let Some(id) = crate::find_any(tx, std::iter::once(lodestar_metainfo::InfoHash(swarm)))? {
        crate::add_sources(tx, id, bit as u64)?;
        if !item.private_source {
            crate::make_public(tx, id)?;
        }
        if let Some(blob) = blob {
            write_claims(tx, id, item.source, blob)?;
        }
        return Ok(Put::Torrent(id));
    }
    let existing: Option<(i64, i64, bool)> = tx
        .prepare_cached(
            "SELECT id, source_id, claims IS NULL FROM unverified_items
             WHERE network = ?1 AND key = ?2",
        )?
        .query_row(params![item.network as i64, key], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })
        .optional()?;
    // A public sighting clears the private mark; a private one never sets it.
    let keep_private = if item.private_source { PRIVATE } else { 0 };
    match existing {
        Some((id, owner, bare)) => {
            let take = blob.is_some() && (bare || owner == i64::from(item.source.get()));
            tx.prepare_cached(
                "UPDATE unverified_items SET
                     sources = sources | ?2,
                     flags = flags & ?3,
                     last_at = max(last_at, ?4),
                     source_id = CASE WHEN ?5 THEN ?6 ELSE source_id END,
                     claims = CASE WHEN ?5 THEN ?7 ELSE claims END
                 WHERE id = ?1",
            )?
            .execute(params![
                id,
                bit,
                keep_private,
                item.at,
                take,
                item.source.get(),
                blob
            ])?;
            Ok(Put::Existing(id))
        }
        None => {
            let Some(blob) = blob else {
                return Ok(Put::Bare);
            };
            let id = crate::sources::next_id(tx)?;
            tx.prepare_cached(
                "INSERT INTO unverified_items
                     (id, network, key, source_id, sources, flags, claims, first_at, last_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?8)",
            )?
            .execute(params![
                id,
                item.network as i64,
                key,
                item.source.get(),
                bit,
                keep_private,
                blob,
                item.at
            ])?;
            Ok(Put::New(id))
        }
    }
}

fn write_claims(conn: &Connection, torrent_id: i64, source: SourceId, blob: &[u8]) -> Result<()> {
    conn.prepare_cached(
        "INSERT INTO torrent_claims (torrent_id, source_id, data) VALUES (?1, ?2, ?3)
         ON CONFLICT DO UPDATE SET data = excluded.data",
    )?
    .execute(params![torrent_id, source.get(), blob])?;
    Ok(())
}

/// Deletes an unverified item, journalling its key when `journal` (a
/// deletion, not a promotion).
fn delete_item(conn: &Connection, id: i64, journal: bool) -> Result<()> {
    if journal {
        conn.prepare_cached(
            "INSERT INTO snapshot_deletions (hash, at)
             SELECT key, unixepoch() FROM unverified_items WHERE id = ?1 AND network = 0
             ON CONFLICT (hash) DO UPDATE SET at = max(at, excluded.at)",
        )?
        .execute([id])?;
    }
    conn.prepare_cached("DELETE FROM unverified_items WHERE id = ?1")?
        .execute([id])?;
    Ok(())
}

fn torrent_hashes(conn: &Connection, id: i64) -> Result<Option<Vec<[u8; 20]>>> {
    let row: Option<(Vec<u8>, Option<Vec<u8>>)> = conn
        .prepare_cached("SELECT info_hash, info_hash_v2 FROM torrents WHERE id = ?1")?
        .query_row([id], |r| Ok((r.get(0)?, r.get(1)?)))
        .optional()?;
    let Some((h, v2)) = row else { return Ok(None) };
    let mut out = vec![<[u8; 20]>::try_from(h).map_err(|_| Error::Corrupt("hash length"))?];
    if let Some(v2) = v2 {
        let t: [u8; 20] = v2
            .get(..20)
            .and_then(|t| t.try_into().ok())
            .ok_or(Error::Corrupt("v2 hash length"))?;
        if t != out[0] {
            out.push(t);
        }
    }
    Ok(Some(out))
}

/// Whether any of the hashes is tombstoned, on the CS-1 list or
/// quarantined.
fn held(conn: &Connection, hashes: &[[u8; 20]]) -> Result<bool> {
    let mut stmt = conn.prepare_cached(
        "SELECT 1 FROM tombstones WHERE hash = ?1
         UNION ALL SELECT 1 FROM cs_hashes WHERE hash = ?1
         UNION ALL SELECT 1 FROM quarantine WHERE hash = ?1",
    )?;
    for h in hashes {
        if stmt.exists([&h[..]])? {
            return Ok(true);
        }
    }
    Ok(false)
}

impl fmt::Display for Network {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Network::Bt => "bt",
        })
    }
}
