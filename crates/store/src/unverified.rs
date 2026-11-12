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
//! **Usenet items** (UN-0, `docs/decisions/2026-11-12-UN-0.md`) live in the
//! same table under [`Network::Usenet`], keyed by [`UsenetKey`]. The store
//! enforces their rules whatever the caller passes: they're always
//! private-source (§18.7), never offered for verification or promoted
//! (`Error::NeverVerified`), never looked up as a swarm hash, capped at
//! [`USENET_CAP`] oldest first, and purged as a tier
//! ([`Store::purge_network`], §18.6).
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
use sha2::{Digest, Sha256};

use crate::claims::{self, Claims};
use crate::filter::{self, Filtered};
use crate::insights::SearchableCodes;
use crate::sources::{SourceId, SourceState};
use crate::{Error, PreparedTorrent, Result, Store, begin_write, flags};

/// `unverified_items.flags`: found only through private sources (§18.7).
/// Never sent to public peer finding. A public sighting clears it.
pub(crate) const PRIVATE: i64 = 1;
/// `unverified_items.flags`, and `torrent_claims.flags` as 1: the claims
/// came through a private source (a private indexer's names, tracker URLs
/// with passkeys). Unlike [`PRIVATE`], a public sighting of the key
/// doesn't clear it: only public claims replacing them do. Private claims
/// never leave the instance (§18.7).
const CLAIMS_PRIVATE: i64 = 2;

/// Rows in one re-judge page at most.
pub const MAX_PAGE: usize = 1024;
/// Decoded claims in one re-judge page at most, by
/// [`Claims::decoded_size`]. A page always holds at least one row, so it
/// holds less than this plus one row's largest ([`MAX_ROW`]): under
/// 9.5 MiB of claims.
pub const PAGE_BYTES: usize = 8 << 20;
/// The largest [`Claims::decoded_size`] one row can have: all the text,
/// the external reference, and every entry at its count limit.
pub const MAX_ROW: usize = std::mem::size_of::<Claims>()
    + claims::MAX_TOTAL
    + claims::MAX_EXTERNAL_REF
    + claims::MAX_FILES * std::mem::size_of::<claims::ClaimedFile>()
    + claims::MAX_TRACKERS * std::mem::size_of::<String>()
    + claims::MAX_IDS * std::mem::size_of::<claims::ExternalId>();
/// Rows one purge step looks at, at most.
pub const MAX_PURGE_STEP: usize = 4096;

/// Bytes in a plugin state key (as `lodestar_sources::state`).
pub const STATE_MAX_KEY: usize = 128;
/// Bytes in a plugin state value.
pub const STATE_MAX_VALUE: usize = 64 * 1024;
/// Keys per plugin.
pub const STATE_MAX_KEYS: usize = 1024;

/// Usenet items kept at most (plan Q13, a §2.26 number the runner sets):
/// past it, the oldest (lowest id, first found) go first. Each write of
/// new usenet items evicts in the same transaction, at most
/// [`MAX_PURGE_STEP`] rows.
pub const USENET_CAP: u64 = 1_000_000;
/// Usenet items one [`Store::put_unverified`] call may hold: with the cap
/// at least this, one batch's eviction always brings the tier back under
/// [`USENET_CAP`], and never evicts the batch's own new items.
pub const MAX_USENET_BATCH: usize = MAX_PURGE_STEP;
const _: () = assert!(USENET_CAP >= MAX_USENET_BATCH as u64);
/// Bytes in each part of a [`UsenetKey`] at most: an indexer id or a
/// Newznab GUID longer than this is refused, not hashed.
pub const USENET_MAX_PART: usize = 1024;
/// The domain tag that starts every [`UsenetKey`] preimage.
const USENET_KEY_TAG: &[u8] = b"lodestar.usenet.v1";

/// Which network a key belongs to (§18.6). Values are persisted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Network {
    /// Clearnet BitTorrent: the key is a 20-byte swarm hash (a v1 hash or
    /// a truncated v2 one).
    Bt = 0,
    /// Usenet (v0.7, UN-0): the key is a 32-byte [`UsenetKey`]. Items are
    /// always private-source and never verified; there's no info hash.
    Usenet = 4,
}

impl Network {
    fn from_sql(v: i64) -> Result<Network> {
        match v {
            0 => Ok(Network::Bt),
            4 => Ok(Network::Usenet),
            _ => Err(Error::Corrupt("unknown network")),
        }
    }

    /// The name search's `network:` field uses (§18.6).
    pub fn as_str(self) -> &'static str {
        match self {
            Network::Bt => "bt",
            Network::Usenet => "usenet",
        }
    }
}

/// A usenet item's key (UN-0): SHA-256 over the indexer's id and the
/// item's Newznab GUID, each length-prefixed, so no two pairs share a
/// preimage. The preimage is
///
/// ```text
/// "lodestar.usenet.v1" || u32be(len(indexer)) || indexer || u32be(len(guid)) || guid
/// ```
///
/// with both parts as the indexer sent them (no case folding or trimming).
/// The NZB link isn't part of it: it embeds the indexer's API key.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct UsenetKey([u8; 32]);

impl UsenetKey {
    /// `Error::Invalid` for an empty part or one over [`USENET_MAX_PART`].
    pub fn new(indexer: &str, guid: &str) -> Result<UsenetKey> {
        let (i, g) = (indexer.as_bytes(), guid.as_bytes());
        for part in [i, g] {
            if part.is_empty() || part.len() > USENET_MAX_PART {
                return Err(Error::Invalid("a usenet key part is 1 to 1024 bytes"));
            }
        }
        let mut h = Sha256::new();
        h.update(USENET_KEY_TAG);
        for part in [i, g] {
            // Fits: bounded by USENET_MAX_PART above.
            h.update((part.len() as u32).to_be_bytes());
            h.update(part);
        }
        Ok(UsenetKey(h.finalize().into()))
    }

    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl fmt::Debug for UsenetKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("UsenetKey(")?;
        for b in &self.0[..4] {
            write!(f, "{b:02x}")?;
        }
        f.write_str("…)")
    }
}

/// A key a source found, for [`Store::put_unverified`].
#[derive(Debug, Clone, Copy)]
pub struct NewUnverified<'a> {
    pub network: Network,
    pub key: &'a [u8],
    pub source: SourceId,
    /// Found through a private source (§18.7). Ignored for
    /// [`Network::Usenet`]: a usenet item is always private-source.
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
    /// The claims came through a private source.
    pub claims_private: bool,
    /// `None` when the item has none (they were purged), or they don't
    /// decode (`unreadable`).
    pub claims: Option<Claims>,
    /// The stored claims don't decode: drop the item, it can't be judged.
    pub unreadable: bool,
    pub first_at: i64,
    pub last_at: i64,
}

/// An unverified item's key, for the verification cursor (SRC-2): no
/// claims are read or decoded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingKey {
    pub id: i64,
    pub network: Network,
    pub key: Vec<u8>,
    /// The source whose claims the item holds.
    pub source: SourceId,
    pub private_source: bool,
}

/// A torrent's claims from one source, for the re-judge sweep.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredClaims {
    pub torrent_id: i64,
    pub source: SourceId,
    /// They came through a private source (§18.7): never exported or
    /// served beyond this instance.
    pub private_source: bool,
    /// `None` when they don't decode: drop them.
    pub claims: Option<Claims>,
}

/// What [`Store::refile_unverified`] did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UnverifiedRefiled {
    /// Unverified items removed.
    pub ids: Vec<i64>,
    /// Torrents a held decision deleted: a crawled torrent under the same
    /// hash, not yet merged with the item. Logged for the search index.
    pub torrents_deleted: Vec<i64>,
}

/// What [`Store::refile_claims`] did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ClaimsRefiled {
    /// Claim rows removed.
    pub claims: usize,
    /// Torrents a held decision deleted, logged for the search index as
    /// [`Store::delete_torrents`] does.
    pub torrents_deleted: Vec<i64>,
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
    /// Persisted fetch-queue keys of this source dropped.
    pub queue_deleted: usize,
}

/// What [`Store::purge_source`] would remove for a source if it ran now,
/// counted with the same rules and changing nothing
/// ([`Store::purge_preview`]). Each count matches the sum of the
/// [`PurgeStep`] field of the same name over a whole purge run while
/// nothing else writes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PurgePreview {
    pub unverified_deleted: u64,
    pub unverified_kept: u64,
    pub claims_deleted: u64,
    pub torrents_deleted: u64,
    pub torrents_kept: u64,
    pub state_deleted: u64,
    pub queue_deleted: u64,
}

/// One step of [`Store::purge_network`]. Start with `after` 0, then pass
/// `next` back until it's `None`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct NetworkPurgeStep {
    /// The id to go on after; `None` when the tier is empty past it.
    pub next: Option<i64>,
    /// Unverified items deleted in this step.
    pub deleted: usize,
}

/// What a purge of one source does to a row its bit may be set on. The
/// one rule [`Store::purge_source`] applies and [`Store::purge_preview`]
/// counts, so the two can't drift apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Fate {
    /// Not this source's: left as it is.
    Untouched,
    /// Only this source found it: deleted.
    Delete,
    /// Others found it too: kept, without this source's bit.
    Keep,
}

fn fate(sources: i64, bit: i64) -> Fate {
    if sources & bit == 0 {
        Fate::Untouched
    } else if sources & !bit == 0 {
        Fate::Delete
    } else {
        Fate::Keep
    }
}

/// The rows a purge reads, in pages, and the tables it clears by source:
/// the same text in the purge and the preview.
macro_rules! unverified_page {
    () => {
        "SELECT id, sources, source_id FROM unverified_items WHERE id > ?1 ORDER BY id LIMIT ?2"
    };
}
macro_rules! torrents_page {
    () => {
        "SELECT id, sources FROM torrents WHERE id > ?1 ORDER BY id LIMIT ?2"
    };
}
macro_rules! claims_of {
    () => {
        "torrent_claims WHERE source_id = ?1"
    };
}
macro_rules! state_of {
    () => {
        "source_state WHERE source_id = ?1"
    };
}
macro_rules! queue_of {
    () => {
        "fetch_queue WHERE source = ?1"
    };
}

impl Store {
    /// Records keys sources found, in one transaction (see [`Put`]).
    /// Claims must be within the [`claims`] limits (`Error::Invalid`
    /// otherwise, and nothing is written).
    ///
    /// A [`Network::Usenet`] item is stored private-source whatever
    /// `private_source` says, and never matched against torrents or holds
    /// by hash (it has none). More than [`MAX_USENET_BATCH`] usenet items
    /// in one call is `Error::Invalid`. A call that adds usenet items drops
    /// the oldest past [`USENET_CAP`] in its transaction.
    pub fn put_unverified(&mut self, items: &[NewUnverified<'_>]) -> Result<Vec<Put>> {
        self.put_unverified_capped(items, USENET_CAP)
    }

    fn put_unverified_capped(
        &mut self,
        items: &[NewUnverified<'_>],
        usenet_cap: u64,
    ) -> Result<Vec<Put>> {
        let usenet = items
            .iter()
            .filter(|i| i.network == Network::Usenet)
            .count();
        if usenet > MAX_USENET_BATCH {
            return Err(Error::Invalid("too many usenet items in one batch"));
        }
        let level = self.config.compression_level;
        let blobs = items
            .iter()
            .map(|i| i.claims.map(|c| encode(c, level)).transpose())
            .collect::<Result<Vec<_>>>()?;
        for i in items {
            check_key(i.network, i.key)?;
        }
        let tx = begin_write(&mut self.conn)?;
        let mut sources: Vec<SourceId> = items.iter().map(|i| i.source).collect();
        sources.sort_unstable();
        sources.dedup();
        for s in sources {
            check_running(&tx, s)?;
        }
        let mut out = Vec::with_capacity(items.len());
        let mut added = false;
        for (item, blob) in items.iter().zip(&blobs) {
            let put = put_one(&tx, item, blob.as_deref(), self.adult_mark)?;
            added |= item.network == Network::Usenet && matches!(put, Put::New(_));
            out.push(put);
        }
        if added {
            cap_network(&tx, Network::Usenet, usenet_cap)?;
        }
        tx.commit()?;
        Ok(out)
    }

    /// Stores `source`'s claims on a torrent, replacing its earlier ones;
    /// `private_source` when they came through a private source (§18.7).
    /// False when the torrent isn't stored or one of its hashes is held.
    pub fn put_claims(
        &mut self,
        torrent_id: i64,
        source: SourceId,
        claims: &Claims,
        private_source: bool,
    ) -> Result<bool> {
        let blob = encode(claims, self.config.compression_level)?;
        let tx = begin_write(&mut self.conn)?;
        check_running(&tx, source)?;
        let Some(hashes) = torrent_hashes(&tx, torrent_id)? else {
            return Ok(false);
        };
        if held(&tx, &hashes)? {
            return Ok(false);
        }
        write_claims(&tx, torrent_id, source, &blob, private_source)?;
        tx.commit()?;
        Ok(true)
    }

    /// A torrent's claims, by source. Claims that don't decode are left
    /// out.
    pub fn torrent_claims(&self, torrent_id: i64) -> Result<Vec<StoredClaims>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT torrent_id, source_id, flags, data FROM torrent_claims
             WHERE torrent_id = ?1 ORDER BY source_id",
        )?;
        let rows = stmt.query_map([torrent_id], read_claims)?;
        let mut out = Vec::new();
        for row in rows {
            let c = row??;
            if c.claims.is_some() {
                out.push(c);
            }
        }
        Ok(out)
    }

    /// Writes the holds a source's claims earned (SRC-2: a filter hit and
    /// a tombstone or quarantine each) and deletes any torrent stored under
    /// their hashes, in one transaction, so nothing is ever left stored
    /// under a held hash. Returns the torrents deleted, logged for the
    /// search index as [`Store::delete_torrents`] does.
    pub fn hold_claims(&mut self, holds: &[Filtered<'_>]) -> Result<Vec<i64>> {
        let tx = begin_write(&mut self.conn)?;
        filter::write(&tx, holds)?;
        let mut ids = Vec::new();
        for f in holds.iter().filter(|f| f.held.is_some()) {
            for h in f.hashes {
                let found = crate::find_any(&tx, std::iter::once(lodestar_metainfo::InfoHash(*h)))?;
                if let Some(id) = found
                    && !ids.contains(&id)
                {
                    tx.prepare_cached("DELETE FROM torrents WHERE id = ?1")?
                        .execute([id])?;
                    tx.prepare_cached("INSERT INTO reindex_log (torrent_id) VALUES (?1)")?
                        .execute([id])?;
                    ids.push(id);
                }
            }
        }
        tx.commit()?;
        if !ids.is_empty() {
            self.scrub_wal();
        }
        Ok(ids)
    }

    /// Whether any clearnet BitTorrent unverified item is stored: the
    /// writer's duplicate check is skipped for a batch when none is
    /// (SRC-2), so a store with no sources pays one cheap read per batch,
    /// not a probe per insert. Usenet items don't count: the writer looks
    /// up swarm hashes, and a usenet key is never one (UN-0).
    pub fn has_unverified(&self) -> Result<bool> {
        Ok(self
            .conn
            .prepare_cached("SELECT EXISTS (SELECT 1 FROM unverified_items WHERE network = ?1)")?
            .query_row([Network::Bt as i64], |r| r.get(0))?)
    }

    /// A stored torrent's swarm hashes (its v1 hash, and a hybrid's
    /// truncated v2 one), for judging its claims again (SRC-2).
    pub fn torrent_swarm_hashes(&self, torrent_id: i64) -> Result<Option<Vec<[u8; 20]>>> {
        torrent_hashes(&self.conn, torrent_id)
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
    /// at [`MAX_PAGE`]) rows, and no more decoded claims than [`PAGE_BYTES`]
    /// past the first row.
    pub fn unverified_after(&self, after: i64, limit: usize) -> Result<Vec<UnverifiedItem>> {
        let mut stmt = self.conn.prepare_cached(&format!(
            "SELECT {ITEM_COLUMNS} FROM unverified_items WHERE id > ?1 ORDER BY id LIMIT ?2"
        ))?;
        let rows = stmt.query_map(params![after, limit.clamp(1, MAX_PAGE) as i64], read_item)?;
        let mut out = Vec::new();
        let mut bytes = 0;
        for row in rows {
            let item = row?.into_item()?;
            bytes += item.claims.as_ref().map_or(0, Claims::decoded_size);
            out.push(item);
            if bytes >= PAGE_BYTES {
                break;
            }
        }
        Ok(out)
    }

    /// Keys of unverified items after `after` (by id), at most `limit`
    /// (capped at [`MAX_PAGE`]), whose claims' source is enabled or paused:
    /// a disabled or purged source's items aren't verified. Their claims
    /// aren't read (SRC-2's verification cursor). Only clearnet
    /// BitTorrent keys: a usenet item is never verified, so its key never
    /// reaches peer finding (UN-0).
    pub fn pending_keys_after(&self, after: i64, limit: usize) -> Result<Vec<PendingKey>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT u.id, u.network, u.key, u.source_id, u.flags
             FROM unverified_items u JOIN sources s ON s.id = u.source_id
             WHERE u.id > ?1 AND s.state IN (?2, ?3) AND u.network = ?5
             ORDER BY u.id LIMIT ?4",
        )?;
        let rows = stmt.query_map(
            params![
                after,
                SourceState::Enabled as i64,
                SourceState::Paused as i64,
                limit.clamp(1, MAX_PAGE) as i64,
                Network::Bt as i64
            ],
            |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, Vec<u8>>(2)?,
                    r.get::<_, i64>(3)?,
                    r.get::<_, i64>(4)?,
                ))
            },
        )?;
        let mut out = Vec::new();
        for row in rows {
            let (id, network, key, source, flags) = row?;
            let network = Network::from_sql(network)?;
            out.push(PendingKey {
                id,
                network,
                key,
                source: SourceId::from_sql(source)?,
                private_source: forced_private(network, flags),
            });
        }
        Ok(out)
    }

    /// Torrent claims after `after` (`(torrent_id, source)`, exclusive):
    /// one keyset page of the re-judge sweep, bounded as
    /// [`unverified_after`](Store::unverified_after).
    pub fn claims_after(&self, after: (i64, SourceId), limit: usize) -> Result<Vec<StoredClaims>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT torrent_id, source_id, flags, data FROM torrent_claims
             WHERE (torrent_id, source_id) > (?1, ?2)
             ORDER BY torrent_id, source_id LIMIT ?3",
        )?;
        let rows = stmt.query_map(
            params![after.0, after.1.get(), limit.clamp(1, MAX_PAGE) as i64],
            read_claims,
        )?;
        let mut out = Vec::new();
        let mut bytes = 0;
        for row in rows {
            let c = row??;
            bytes += c.claims.as_ref().map_or(0, Claims::decoded_size);
            out.push(c);
            if bytes >= PAGE_BYTES {
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
    /// skipped.
    ///
    /// A decision that holds also deletes a torrent stored under the item's
    /// key or the decision's hashes (crawled before SRC-2's writer merged
    /// the two), as [`refile_claims`](Store::refile_claims) does: nothing
    /// stays stored under a held hash.
    pub fn refile_unverified(
        &mut self,
        refiled: &[RefiledUnverified<'_>],
        current: impl FnOnce() -> bool,
    ) -> Result<Option<UnverifiedRefiled>> {
        let tx = begin_write(&mut self.conn)?;
        if !current() {
            return Ok(None);
        }
        let mut done = UnverifiedRefiled::default();
        for r in refiled {
            let key: Option<(i64, Vec<u8>)> = tx
                .prepare_cached("SELECT network, key FROM unverified_items WHERE id = ?1")?
                .query_row([r.id], |row| Ok((row.get(0)?, row.get(1)?)))
                .optional()?;
            let Some((network, key)) = key else {
                continue;
            };
            if let Some(f) = &r.filtered {
                filter::write(&tx, std::slice::from_ref(f))?;
                if f.held.is_some() {
                    let mut hashes: Vec<[u8; 20]> = f.hashes.to_vec();
                    if network == Network::Bt as i64
                        && let Ok(k) = <[u8; 20]>::try_from(key.as_slice())
                    {
                        hashes.push(k);
                    }
                    for h in hashes {
                        let found =
                            crate::find_any(&tx, std::iter::once(lodestar_metainfo::InfoHash(h)))?;
                        if let Some(tid) = found
                            && !done.torrents_deleted.contains(&tid)
                        {
                            tx.prepare_cached("DELETE FROM torrents WHERE id = ?1")?
                                .execute([tid])?;
                            tx.prepare_cached("INSERT INTO reindex_log (torrent_id) VALUES (?1)")?
                                .execute([tid])?;
                            done.torrents_deleted.push(tid);
                        }
                    }
                }
            }
            delete_item(&tx, r.id, true)?;
            done.ids.push(r.id);
        }
        tx.commit()?;
        if !done.ids.is_empty() {
            self.scrub_wal();
        }
        Ok(Some(done))
    }

    /// Removes torrent claims the rules now hold back (or that can't be
    /// judged), as [`refile_unverified`](Store::refile_unverified) does
    /// for items. A decision that holds (a tombstone or quarantine) also
    /// deletes the torrent, as the stored-torrent refile does: nothing
    /// stays stored under a held hash. `None` when the rules moved on.
    pub fn refile_claims(
        &mut self,
        refiled: &[RefiledClaims<'_>],
        current: impl FnOnce() -> bool,
    ) -> Result<Option<ClaimsRefiled>> {
        let tx = begin_write(&mut self.conn)?;
        if !current() {
            return Ok(None);
        }
        let mut done = ClaimsRefiled::default();
        for r in refiled {
            let n = tx
                .prepare_cached(
                    "DELETE FROM torrent_claims WHERE torrent_id = ?1 AND source_id = ?2",
                )?
                .execute(params![r.torrent_id, r.source.get()])?;
            if n == 0 {
                continue;
            }
            done.claims += n;
            let Some(f) = &r.filtered else { continue };
            filter::write(&tx, std::slice::from_ref(f))?;
            if f.held.is_some()
                && tx
                    .prepare_cached("DELETE FROM torrents WHERE id = ?1")?
                    .execute([r.torrent_id])?
                    > 0
            {
                tx.prepare_cached("INSERT INTO reindex_log (torrent_id) VALUES (?1)")?
                    .execute([r.torrent_id])?;
                done.torrents_deleted.push(r.torrent_id);
            }
        }
        tx.commit()?;
        if done.claims > 0 {
            self.scrub_wal();
        }
        Ok(Some(done))
    }

    /// Stores an unverified item's verified metadata as a torrent, in one
    /// transaction (see [`Promoted`]). The torrent keeps the item's id and
    /// gains its sources and claims; `t.source` is added too. It stays
    /// private-source if the item was and `t` says so. The id goes into
    /// `reindex_log`: it's below the search index's id watermark. A kept
    /// id is also queued for the percolator (SRC-2b,
    /// [`Store::promoted_due`]).
    ///
    /// `Error::Invalid` when `t`'s hashes don't include the item's key, or
    /// `t` isn't verified metadata (§18.5: only that promotes).
    /// `Error::NeverVerified` for a usenet item, which nothing promotes
    /// (UN-0); it's left as it was.
    ///
    /// **Insights** (SRC-2): a kept id at or below the searchable watermark
    /// is counted as newly searchable at `at` under `codes`, in this
    /// transaction ([`crate::insights::count_promoted`]), so a promoted
    /// torrent deleted later leaves the rollups consistent.
    pub fn promote(
        &mut self,
        id: i64,
        t: &PreparedTorrent<'_>,
        codes: SearchableCodes,
        at: i64,
    ) -> Result<Promoted> {
        let p = t.torrent;
        // A usenet item is refused first, with its own error, whatever
        // `t` is; checked again in the transaction below.
        let network: Option<i64> = self
            .conn
            .prepare_cached("SELECT network FROM unverified_items WHERE id = ?1")?
            .query_row([id], |r| r.get(0))
            .optional()?;
        if network == Some(Network::Usenet as i64) {
            return Err(Error::NeverVerified);
        }
        if !t.verified {
            return Err(Error::Invalid("promotion needs verified metadata"));
        }
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
        if row.network == Network::Usenet as i64 {
            return Err(Error::NeverVerified);
        }
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
        // A disabled or purged source adds nothing (SRC-14b). With no live
        // finder left the item is the purge's: the torrent goes through
        // the usual insert, which refuses it the same way.
        let stopped = crate::sources::stopped_mask(&tx)?;
        let bits = ((row.sources as u64) | t.source.bit() as u64) & !stopped;
        if bits == 0 {
            return Ok(Promoted::Gone);
        }
        // The item's claims are a stopped source's: they're not carried.
        let row_claims = row
            .claims
            .as_ref()
            .filter(|_| stopped & item_source.bit() as u64 == 0);
        // The item's public mark may be a stopped finder's sighting: then
        // it's treated as private, so only a live public sighting (this
        // fetch, or a later one) makes it public (SRC-14b).
        let item_public = row.flags & PRIVATE == 0
            && (row.sources as u64) & stopped == 0
            && stopped & item_source.bit() as u64 == 0;
        let public = item_public || t.extra_flags & flags::PRIVATE_SOURCE == 0;
        let claims_private = i64::from(row.flags & CLAIMS_PRIVATE != 0);
        let out = if let Some(existing) = crate::find_any(&tx, p.swarm_hashes())? {
            crate::add_sources(&tx, existing, bits)?;
            if public {
                crate::make_public(&tx, existing)?;
            }
            // As `commit_one`: an import's partial file list gets the
            // fetched one.
            if t.extra_flags & flags::FILES_PARTIAL == 0 {
                crate::upgrade_partial(&tx, existing, t)?;
            }
            if let Some(blob) = row_claims {
                tx.prepare_cached(
                    "INSERT INTO torrent_claims (torrent_id, source_id, flags, data)
                     VALUES (?1, ?2, ?3, ?4) ON CONFLICT DO NOTHING",
                )?
                .execute(params![
                    existing,
                    item_source.get(),
                    claims_private,
                    blob
                ])?;
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
            if id <= 0 || id >= next {
                return Err(Error::Corrupt("unverified id outside the issued sequence"));
            }
            tx.execute("UPDATE id_seq SET next = ?1", [id])?;
            // The first finder, or the lowest live one when it's stopped.
            let first = if stopped & item_source.bit() as u64 == 0 {
                item_source
            } else {
                SourceId::new(bits.trailing_zeros() as u8).expect("bit under 64")
            };
            let got = crate::insert_new(&tx, p, fl, discovered, t.metadata_at, first)?;
            tx.execute("UPDATE id_seq SET next = ?1", [next])?;
            if got != id {
                return Err(Error::Corrupt("promotion took another id"));
            }
            crate::add_sources(&tx, id, bits)?;
            if let Some(blob) = row_claims {
                write_claims(&tx, id, item_source, blob, claims_private != 0)?;
            }
            tx.prepare_cached("INSERT INTO reindex_log (torrent_id) VALUES (?1)")?
                .execute([id])?;
            crate::saved::queue_promoted(&tx, tx.last_insert_rowid())?;
            crate::insights::count_promoted(&tx, id, at, codes)?;
            Promoted::Kept(id)
        };
        // Found by an always-adult source and not stored adult yet: marked
        // in this transaction, so the reclassification after it is retried.
        if let Promoted::Kept(t) | Promoted::Merged(t) = out {
            crate::mark_adult_found(&tx, self.adult_mark, t)?;
        }
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
                    .prepare_cached(unverified_page!())?
                    .query_map(params![from.after, limit as i64], |r| {
                        Ok((r.get(0)?, r.get(1)?, r.get(2)?))
                    })?
                    .collect::<rusqlite::Result<_>>()?;
                for (id, sources, owner) in rows {
                    scanned += 1;
                    last = id;
                    let rest = sources & !bit;
                    match fate(sources, bit) {
                        Fate::Untouched => continue,
                        Fate::Delete => {
                            delete_item(&tx, id, true)?;
                            step.unverified_deleted += 1;
                            continue;
                        }
                        Fate::Keep => {}
                    }
                    if owner == i64::from(source.get()) {
                        tx.prepare_cached(
                            "UPDATE unverified_items
                             SET sources = ?2, source_id = ?3, claims = NULL, flags = flags & ~2
                             WHERE id = ?1",
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
                    .prepare_cached(concat!(
                        "DELETE FROM torrent_claims WHERE rowid IN (SELECT rowid FROM ",
                        claims_of!(),
                        " LIMIT ?2)"
                    ))?
                    .execute(params![source.get(), limit as i64])?;
                step.claims_deleted = scanned;
            }
            Stage::Torrents => {
                let rows: Vec<(i64, i64)> = tx
                    .prepare_cached(torrents_page!())?
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
                    match fate(sources, bit) {
                        Fate::Untouched => {}
                        Fate::Delete => {
                            delete.execute([id])?;
                            log.execute([id])?;
                            step.torrents_deleted.push(id);
                        }
                        Fate::Keep => {
                            clear.execute(params![id, bit])?;
                            step.torrents_kept += 1;
                        }
                    }
                }
            }
            Stage::State => {
                step.state_deleted = tx
                    .prepare_cached(concat!("DELETE FROM ", state_of!()))?
                    .execute([source.get()])?;
                // Keys it queued before it was disabled would otherwise be
                // stored under its bit after the purge. The pipeline's
                // in-memory queue is SRC-14's to drop.
                step.queue_deleted = tx
                    .prepare_cached(concat!("DELETE FROM ", queue_of!()))?
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

    /// What purging `source` would remove now ([`PurgePreview`]),
    /// changing nothing. It reads the rows the purge reads, in the same
    /// pages, and judges each with the same rule (`fate`), so the two
    /// agree. Each page is its own short read, never one snapshot held
    /// across the scan, so a purge step's WAL checkpoint never waits on a
    /// preview of millions of rows. The counts are therefore exact only
    /// while nothing writes, which is already the case they promise:
    /// unlike [`Store::purge_source`] the preview doesn't need the source
    /// off, and an enabled source can still find more before a purge turns
    /// it off. It reads every unverified item and torrent once, as a purge
    /// does: run it on a reader ([`Store::open_reader`]) to keep the
    /// writer free. `Error::NotFound` for an unknown source.
    pub fn purge_preview(&self, source: SourceId) -> Result<PurgePreview> {
        let bit = source.bit();
        let conn = &self.conn;
        let known: Option<i64> = conn
            .query_row("SELECT 1 FROM sources WHERE id = ?1", [source.get()], |r| {
                r.get(0)
            })
            .optional()?;
        if known.is_none() {
            return Err(Error::NotFound("source"));
        }
        let mut preview = PurgePreview::default();
        let tally = |sources: i64, deleted: &mut u64, kept: &mut u64| match fate(sources, bit) {
            Fate::Untouched => {}
            Fate::Delete => *deleted += 1,
            Fate::Keep => *kept += 1,
        };
        let page = MAX_PURGE_STEP as i64;
        let mut unverified = conn.prepare_cached(unverified_page!())?;
        let mut after = 0;
        loop {
            let rows: Vec<(i64, i64)> = unverified
                .query_map(params![after, page], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect::<rusqlite::Result<_>>()?;
            for &(_, sources) in &rows {
                tally(
                    sources,
                    &mut preview.unverified_deleted,
                    &mut preview.unverified_kept,
                );
            }
            match rows.last() {
                Some(&(id, _)) if rows.len() as i64 == page => after = id,
                _ => break,
            }
        }
        drop(unverified);
        let mut torrents = conn.prepare_cached(torrents_page!())?;
        let mut after = 0;
        loop {
            let rows: Vec<(i64, i64)> = torrents
                .query_map(params![after, page], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect::<rusqlite::Result<_>>()?;
            for &(_, sources) in &rows {
                tally(
                    sources,
                    &mut preview.torrents_deleted,
                    &mut preview.torrents_kept,
                );
            }
            match rows.last() {
                Some(&(id, _)) if rows.len() as i64 == page => after = id,
                _ => break,
            }
        }
        drop(torrents);
        let count = |sql: &str| -> Result<u64> {
            Ok(conn.query_row(sql, [source.get()], |r| r.get::<_, i64>(0))? as u64)
        };
        preview.claims_deleted = count(concat!("SELECT count(*) FROM ", claims_of!()))?;
        preview.state_deleted = count(concat!("SELECT count(*) FROM ", state_of!()))?;
        preview.queue_deleted = count(concat!("SELECT count(*) FROM ", queue_of!()))?;
        Ok(preview)
    }

    /// One step of purging a network tier's unverified items (§18.6's
    /// one-click purge per tier): at most `limit` (capped at
    /// [`MAX_PURGE_STEP`]) items with ids after `after`, in one
    /// transaction, whatever sources found them. Clearnet BitTorrent isn't
    /// a tier this purges (`Error::Invalid`): its items are the
    /// verification queue's, and its torrents are the store's.
    ///
    /// The store doesn't know which sources write a tier (that's their
    /// manifests'), so the caller disables them first, as for
    /// [`Store::purge_source`]: an item a running source writes behind the
    /// cursor would survive the purge. Deleted items aren't journalled for
    /// the snapshot purge (`docs/decisions/2026-11-12-UN-0.md`).
    pub fn purge_network(
        &mut self,
        network: Network,
        after: i64,
        limit: usize,
    ) -> Result<NetworkPurgeStep> {
        if network == Network::Bt {
            return Err(Error::Invalid("the clearnet tier isn't purged by network"));
        }
        let limit = limit.clamp(1, MAX_PURGE_STEP);
        let tx = begin_write(&mut self.conn)?;
        let ids: Vec<i64> = tx
            .prepare_cached(
                "SELECT id FROM unverified_items WHERE network = ?1 AND id > ?2
                 ORDER BY id LIMIT ?3",
            )?
            .query_map(params![network as i64, after, limit as i64], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        for &id in &ids {
            // Not journalled: the snapshot journal holds swarm hashes only.
            delete_item(&tx, id, false)?;
        }
        tx.commit()?;
        if !ids.is_empty() {
            self.scrub_wal();
        }
        Ok(NetworkPurgeStep {
            next: (ids.len() == limit).then(|| ids[ids.len() - 1]),
            deleted: ids.len(),
        })
    }

    /// Unverified items stored under `network`: the count a tier purge
    /// would remove now, and what [`USENET_CAP`] bounds.
    pub fn unverified_count(&self, network: Network) -> Result<u64> {
        let n: i64 = self
            .conn
            .prepare_cached("SELECT count(*) FROM unverified_items WHERE network = ?1")?
            .query_row([network as i64], |r| r.get(0))?;
        Ok(n as u64)
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
        let network = Network::from_sql(self.network)?;
        Ok(UnverifiedItem {
            id: self.id,
            network,
            key: self.key,
            source: SourceId::from_sql(self.source)?,
            sources: self.sources as u64,
            private_source: forced_private(network, self.flags),
            claims_private: self.flags & CLAIMS_PRIVATE != 0
                || (network == Network::Usenet && claims.is_some()),
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

/// Refuses writes from a source that's disabled or purged: a poll still in
/// flight when the user disabled it, or one finishing after its purge
/// passed, mustn't put back what the purge removed. Checked inside the
/// write's transaction, so a purge (which needs the source disabled
/// first) and a write never interleave. A paused source may still finish.
fn check_running(conn: &Connection, source: SourceId) -> Result<()> {
    let state: Option<i64> = conn
        .prepare_cached("SELECT state FROM sources WHERE id = ?1")?
        .query_row([source.get()], |r| r.get(0))
        .optional()?;
    match state {
        None => Err(Error::NotFound("source")),
        Some(s) if s == SourceState::Disabled as i64 || s == SourceState::Purged as i64 => {
            Err(Error::Invalid("a write from a disabled source"))
        }
        Some(_) => Ok(()),
    }
}

fn check_key(network: Network, key: &[u8]) -> Result<()> {
    match network {
        Network::Bt if key.len() == 20 => Ok(()),
        Network::Bt => Err(Error::Invalid("a BitTorrent key is 20 bytes")),
        Network::Usenet if key.len() == 32 => Ok(()),
        Network::Usenet => Err(Error::Invalid("a usenet key is 32 bytes")),
    }
}

/// Whether a stored item is private-source: its flag, or always for a
/// usenet item (§18.7), so even a row written outside this module reads
/// back private.
fn forced_private(network: Network, flags: i64) -> bool {
    network == Network::Usenet || flags & PRIVATE != 0
}

/// Drops `network`'s oldest unverified items (lowest id: first found)
/// past `cap`, at most [`MAX_PURGE_STEP`] in this call. An old item seen
/// again in the same batch is still old: it can go, though the call
/// reported it as [`Put::Existing`]. Usenet items have
/// no swarm hash, so nothing is journalled.
fn cap_network(tx: &Transaction<'_>, network: Network, cap: u64) -> Result<usize> {
    let n: i64 = tx
        .prepare_cached("SELECT count(*) FROM unverified_items WHERE network = ?1")?
        .query_row([network as i64], |r| r.get(0))?;
    let excess = (n as u64).saturating_sub(cap).min(MAX_PURGE_STEP as u64);
    if excess == 0 {
        return Ok(0);
    }
    Ok(tx
        .prepare_cached(
            "DELETE FROM unverified_items WHERE id IN (
                 SELECT id FROM unverified_items WHERE network = ?1 ORDER BY id LIMIT ?2)",
        )?
        .execute(params![network as i64, excess as i64])?)
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

fn put_one(
    tx: &Transaction<'_>,
    item: &NewUnverified<'_>,
    blob: Option<&[u8]>,
    adult_mark: crate::AdultMark,
) -> Result<Put> {
    let key = item.key;
    let bit = item.source.bit();
    // A usenet item is always private-source (§18.7), whatever the caller
    // says: its claims come from an account source.
    let private_source = item.private_source || item.network == Network::Usenet;
    // Only a swarm hash is looked up among holds and torrents: a usenet
    // key is neither, and is never treated as one.
    if item.network == Network::Bt
        && let Some(put) = put_bt_torrent(tx, item, blob, adult_mark)?
    {
        return Ok(put);
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
    // A public sighting clears the item's private mark; a private one
    // never sets it. The claims' mark follows whichever claims are kept.
    let keep_private = if private_source { PRIVATE } else { 0 };
    let claims_private = if private_source { CLAIMS_PRIVATE } else { 0 };
    match existing {
        Some((id, owner, bare)) => {
            let take = blob.is_some() && (bare || owner == i64::from(item.source.get()));
            tx.prepare_cached(
                "UPDATE unverified_items SET
                     sources = sources | ?2,
                     flags = (flags & ?3)
                         | CASE WHEN ?5 THEN ?8 ELSE flags & 2 END,
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
                blob,
                claims_private
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
                keep_private | claims_private,
                blob,
                item.at
            ])?;
            Ok(Put::New(id))
        }
    }
}

/// A BitTorrent key's held and stored-torrent cases of [`put_one`]:
/// `None` when neither applies and the key goes to `unverified_items`.
fn put_bt_torrent(
    tx: &Transaction<'_>,
    item: &NewUnverified<'_>,
    blob: Option<&[u8]>,
    adult_mark: crate::AdultMark,
) -> Result<Option<Put>> {
    let swarm: [u8; 20] = item
        .key
        .try_into()
        .map_err(|_| Error::Invalid("key length"))?;
    if held(tx, &[swarm])? {
        return Ok(Some(Put::Held));
    }
    let Some(id) = crate::find_any(tx, std::iter::once(lodestar_metainfo::InfoHash(swarm)))? else {
        return Ok(None);
    };
    crate::add_sources(tx, id, item.source.bit() as u64)?;
    // An always-adult source's bit, in this transaction: marked for
    // the backfill, so the reclassification after it is retried if
    // it never commits (X-source-adult's crash window, SRC-13b).
    crate::mark_adult_found(tx, adult_mark, id)?;
    if !item.private_source {
        crate::make_public(tx, id)?;
    }
    if let Some(blob) = blob {
        write_claims(tx, id, item.source, blob, item.private_source)?;
    }
    Ok(Some(Put::Torrent(id)))
}

fn write_claims(
    conn: &Connection,
    torrent_id: i64,
    source: SourceId,
    blob: &[u8],
    private_source: bool,
) -> Result<()> {
    conn.prepare_cached(
        "INSERT INTO torrent_claims (torrent_id, source_id, flags, data) VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT DO UPDATE SET flags = excluded.flags, data = excluded.data",
    )?
    .execute(params![
        torrent_id,
        source.get(),
        i64::from(private_source),
        blob
    ])?;
    Ok(())
}

fn read_claims(r: &rusqlite::Row<'_>) -> rusqlite::Result<Result<StoredClaims>> {
    let (torrent_id, source, flags, data): (i64, i64, i64, Vec<u8>) =
        (r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?);
    Ok(SourceId::from_sql(source).map(|source| StoredClaims {
        torrent_id,
        source,
        private_source: flags & 1 != 0,
        claims: claims::decode(&data).ok(),
    }))
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
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::StoreConfig;

    const T: i64 = 1_700_000_000;

    fn claims() -> Claims {
        Claims {
            name: Some("x".into()),
            ..Claims::default()
        }
    }

    fn usenet<'a>(k: &'a UsenetKey, source: SourceId, c: &'a Claims, at: i64) -> NewUnverified<'a> {
        NewUnverified {
            network: Network::Usenet,
            key: k.as_bytes(),
            source,
            private_source: false,
            claims: Some(c),
            at,
        }
    }

    fn ids(store: &Store) -> Vec<i64> {
        let mut stmt = store
            .conn
            .prepare("SELECT id FROM unverified_items WHERE network = 4 ORDER BY id")
            .unwrap();
        stmt.query_map([], |r| r.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap()
    }

    #[test]
    fn networks_decode_and_name_themselves() {
        assert_eq!(Network::from_sql(0).unwrap(), Network::Bt);
        assert_eq!(Network::from_sql(4).unwrap(), Network::Usenet);
        for v in [-1, 1, 2, 3, 5, 15, 16, i64::MAX] {
            assert!(matches!(
                Network::from_sql(v),
                Err(Error::Corrupt("unknown network"))
            ));
        }
        // As search's `network:` values (`lodestar_search::query::Network`).
        assert_eq!(Network::Bt.as_str(), "bt");
        assert_eq!(Network::Usenet.as_str(), "usenet");
        assert_eq!(Network::Usenet as i64, 4);
    }

    #[test]
    fn the_key_preimage_is_as_documented() {
        let mut h = Sha256::new();
        h.update(b"lodestar.usenet.v1\0\0\0\x017\0\0\0\x03abc");
        let want: [u8; 32] = h.finalize().into();
        assert_eq!(UsenetKey::new("7", "abc").unwrap().as_bytes(), &want);
    }

    #[test]
    fn the_cap_evicts_the_oldest_first() {
        let mut store = Store::open_in_memory(StoreConfig::default()).unwrap();
        let bt_source = store.register_source("dht", "1", T).unwrap();
        let src = store.register_source("prowlarr", "1", T).unwrap();
        let c = claims();
        let keys: Vec<UsenetKey> = (0..8)
            .map(|n| UsenetKey::new("1", &format!("g{n}")).unwrap())
            .collect();
        // A Bt item in between counts for nothing and is never evicted.
        let bt = [5u8; 20];
        let first: Vec<i64> = store
            .put_unverified_capped(
                &[
                    usenet(&keys[0], src, &c, T),
                    usenet(&keys[1], src, &c, T),
                    NewUnverified {
                        network: Network::Bt,
                        key: &bt,
                        source: bt_source,
                        private_source: false,
                        claims: Some(&c),
                        at: T,
                    },
                    usenet(&keys[2], src, &c, T),
                ],
                3,
            )
            .unwrap()
            .into_iter()
            .filter_map(|p| match p {
                Put::New(id) => Some(id),
                _ => None,
            })
            .collect();
        assert_eq!(ids(&store).len(), 3);
        // Two more: the two oldest go, in the same call.
        store
            .put_unverified_capped(
                &[
                    usenet(&keys[3], src, &c, T + 1),
                    usenet(&keys[4], src, &c, T + 1),
                ],
                3,
            )
            .unwrap();
        let kept = ids(&store);
        assert_eq!(kept.len(), 3);
        assert!(!kept.contains(&first[0]) && !kept.contains(&first[1]));
        assert!(kept.contains(&first[3]), "the third usenet item stays");
        assert!(store.unverified(Network::Bt, &bt).unwrap().is_some());
        // Seeing a kept item again adds nothing, so evicts nothing.
        store
            .put_unverified_capped(&[usenet(&keys[2], src, &c, T + 2)], 3)
            .unwrap();
        assert_eq!(ids(&store), kept);
        // A cap lowered under the stored count converges, at most a step
        // per call.
        store
            .put_unverified_capped(&[usenet(&keys[5], src, &c, T + 3)], 1)
            .unwrap();
        assert_eq!(ids(&store).len(), 1);
        assert!(
            store
                .unverified(Network::Usenet, keys[5].as_bytes())
                .unwrap()
                .is_some(),
            "the newest is kept"
        );
    }

    #[test]
    fn the_cap_step_is_bounded() {
        let mut store = Store::open_in_memory(StoreConfig::default()).unwrap();
        let src = store.register_source("prowlarr", "1", T).unwrap();
        let c = claims();
        let keys: Vec<UsenetKey> = (0..MAX_PURGE_STEP + 10)
            .map(|n| UsenetKey::new("1", &format!("g{n}")).unwrap())
            .collect();
        for chunk in keys.chunks(MAX_USENET_BATCH) {
            let items: Vec<_> = chunk.iter().map(|k| usenet(k, src, &c, T)).collect();
            store.put_unverified_capped(&items, u64::MAX).unwrap();
        }
        let tx = begin_write(&mut store.conn).unwrap();
        assert_eq!(
            cap_network(&tx, Network::Usenet, 0).unwrap(),
            MAX_PURGE_STEP
        );
        assert_eq!(cap_network(&tx, Network::Usenet, 0).unwrap(), 10);
        assert_eq!(cap_network(&tx, Network::Usenet, 0).unwrap(), 0);
    }

    /// A store upgraded from V13 to V14 through the real migration path
    /// (as a released v0.6 store is) takes usenet rows under its CHECKs,
    /// with no migration of UN-0's own.
    #[test]
    fn an_upgraded_v14_schema_accepts_usenet_rows() {
        let v14 = crate::schema::MIGRATIONS
            .iter()
            .position(|(v, _)| *v == 14)
            .unwrap();
        let mut conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE schema_migrations (
                 version INTEGER PRIMARY KEY, applied_at INTEGER NOT NULL) STRICT;",
        )
        .unwrap();
        for (v, sql) in &crate::schema::MIGRATIONS[..v14] {
            conn.execute_batch(sql).unwrap();
            conn.execute("INSERT INTO schema_migrations VALUES (?1, 0)", [v])
                .unwrap();
        }
        // V13 data: a registered source.
        conn.execute(
            "INSERT INTO sources (id, plugin, version, state, added_at) VALUES (1, 'p', '1', 0, 0)",
            [],
        )
        .unwrap();
        crate::schema::migrate(&mut conn, None).unwrap();
        let version: i64 = conn
            .query_row("SELECT max(version) FROM schema_migrations", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert!(version >= 14);
        let k = UsenetKey::new("1", "g").unwrap();
        let blob = claims::encode(&claims(), 3).unwrap();
        conn.execute(
            "INSERT INTO unverified_items (id, network, key, source_id, sources, flags, claims,
                 first_at, last_at) VALUES (8, ?1, ?2, 1, 2, 3, ?3, 0, 0)",
            params![Network::Usenet as i64, &k.as_bytes()[..], blob],
        )
        .unwrap();
        let item = conn
            .query_row(
                &format!("SELECT {ITEM_COLUMNS} FROM unverified_items"),
                [],
                read_item,
            )
            .unwrap()
            .into_item()
            .unwrap();
        assert_eq!(item.network, Network::Usenet);
        assert!(item.private_source);
        assert_eq!(item.claims, Some(claims()));
    }
}
