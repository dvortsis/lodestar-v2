//! The canonical store: SQLite is the source of truth (architecture §7).
//! Search indexes and hot columns are derived from it and can always be
//! rebuilt.
//!
//! Scope so far: schema and migrations, connection setup, the file-tree
//! blob codec with zstd dictionaries, batched writes and reads, source
//! provenance and the shared id sequence (§18.5), Insights counters (§7)
//! and the persisted fetch queue (§5). A single [`Store`] owns the one
//! writer connection; the pipeline's writer task drives it.

mod compress;
pub mod filetree;
mod insights;
mod schema;
mod sources;

use std::fmt;
use std::io;
use std::path::Path;

use lodestar_metainfo::{
    FileAttrs, Info, InfoHash, InfoHashV2, MetaVersion, Text, TextEncoding, info_without_pieces,
};
use rusqlite::{Connection, OptionalExtension, params};

pub use compress::{BlobCodec, Codec, Compressed};
pub use filetree::{CodecError, FileTree};
pub use insights::{CohortCount, Counter, Event};
pub use rusqlite;
pub use sources::{MAX_SOURCES, QueuedKey, SourceId, SourceRow, SourceState};

/// Bits of the `torrents.flags` column. Values are persisted: append only.
pub mod flags {
    pub const PRIVATE: i64 = 1 << 0;
    /// Has a v2 info hash (pure v2 or hybrid).
    pub const V2: i64 = 1 << 1;
    pub const HYBRID: i64 = 1 << 2;
    pub const PADDING: i64 = 1 << 3;
    pub const SYMLINKS: i64 = 1 << 4;
    /// Came from a `.torrent` import rather than the DHT.
    pub const IMPORTED: i64 = 1 << 5;
    /// First seen during crawler warm-up: `discovered_at` only means "when
    /// we first saw it" (architecture §6, `age_confidence`).
    pub const BACKFILL: i64 = 1 << 6;
    /// The file list is incomplete (bitmagnet import above its threshold).
    pub const FILES_PARTIAL: i64 = 1 << 7;
    pub const SINGLE_FILE: i64 = 1 << 8;
    /// Metainfo parsing reported warnings.
    pub const WARNINGS: i64 = 1 << 9;
}

#[derive(Debug)]
#[non_exhaustive]
pub enum Error {
    Sqlite(rusqlite::Error),
    Codec(CodecError),
    Compression(io::Error),
    SchemaTooNew {
        found: i64,
        supported: i64,
    },
    Corrupt(&'static str),
    /// All 64 source ids are taken.
    TooManySources,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Sqlite(e) => write!(f, "sqlite: {e}"),
            Error::Codec(e) => e.fmt(f),
            Error::Compression(e) => write!(f, "compression: {e}"),
            Error::SchemaTooNew { found, supported } => write!(
                f,
                "database schema v{found} is newer than this build supports (v{supported})"
            ),
            Error::Corrupt(what) => write!(f, "corrupt store: {what}"),
            Error::TooManySources => write!(f, "all {MAX_SOURCES} source ids are in use"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Sqlite(e) => Some(e),
            Error::Codec(e) => Some(e),
            Error::Compression(e) => Some(e),
            _ => None,
        }
    }
}

impl From<rusqlite::Error> for Error {
    fn from(e: rusqlite::Error) -> Self {
        Error::Sqlite(e)
    }
}

impl From<CodecError> for Error {
    fn from(e: CodecError) -> Self {
        Error::Codec(e)
    }
}

impl From<io::Error> for Error {
    fn from(e: io::Error) -> Self {
        Error::Compression(e)
    }
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoreConfig {
    /// SQLite `mmap_size`; size it to available RAM.
    pub mmap_size: u64,
    /// zstd level for blobs. Writes are tens per second, so a high level
    /// costs little and buys size.
    pub compression_level: i32,
    /// Keep the info dictionary minus `pieces` (off by default, §3).
    pub archive_raw: bool,
}

impl Default for StoreConfig {
    fn default() -> Self {
        StoreConfig {
            mmap_size: 256 << 20,
            compression_level: 19,
            archive_raw: false,
        }
    }
}

/// A torrent to insert.
#[derive(Debug, Clone, Copy)]
pub struct NewTorrent<'a> {
    pub info: &'a Info,
    /// The raw info dictionary, needed only when `archive_raw` is on.
    pub raw_info: Option<&'a [u8]>,
    pub discovered_at: i64,
    pub metadata_at: i64,
    /// Provenance flags the metainfo can't know ([`flags::IMPORTED`],
    /// [`flags::BACKFILL`], [`flags::FILES_PARTIAL`]).
    pub extra_flags: i64,
    /// The source that found it. An existing torrent gains this source's
    /// bit in `sources`.
    pub source: SourceId,
}

/// Everything one writer transaction commits (§7 write path): torrents
/// plus the Insights counters accumulated since the last flush.
#[derive(Debug, Clone, Copy, Default)]
pub struct WriteBatch<'a> {
    pub torrents: &'a [NewTorrent<'a>],
    /// Rediscoveries of stored torrents, applied after `torrents`.
    pub touches: &'a [Touch],
    pub counters: &'a [Counter],
    pub cohorts: &'a [CohortCount],
    pub events: &'a [Event],
}

/// A stored torrent seen again: its swarm hash and the sources (a bitset)
/// that saw it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Touch {
    pub hash: InfoHash,
    pub sources: u64,
}

/// What [`Store::write`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Written {
    /// One per torrent, in order.
    pub torrents: Vec<Inserted>,
    /// One per touch: the torrent's id, or `None` if it isn't stored.
    pub touched: Vec<Option<i64>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Inserted {
    New(i64),
    /// Already stored under one of its swarm hashes.
    Existing(i64),
}

impl Inserted {
    pub fn id(self) -> i64 {
        match self {
            Inserted::New(id) | Inserted::Existing(id) => id,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TorrentRow {
    pub id: i64,
    pub info_hash: InfoHash,
    pub info_hash_v2: Option<InfoHashV2>,
    pub name: String,
    /// Raw name bytes when they aren't plain UTF-8.
    pub name_raw: Option<Text>,
    pub size: u64,
    pub piece_len: u64,
    pub piece_count: u64,
    /// Files excluding padding.
    pub file_count: u64,
    pub info_size: u64,
    pub flags: i64,
    pub discovered_at: i64,
    pub metadata_at: i64,
    pub last_seen_at: i64,
    /// Bitset over [`SourceId`]s; 0 for torrents stored before provenance.
    pub sources: u64,
    pub first_source: Option<SourceId>,
}

const FILE_DICT_SETTING: &str = "file_dict";

/// The writer connection plus blob codec.
#[derive(Debug)]
pub struct Store {
    conn: Connection,
    codec: BlobCodec,
    config: StoreConfig,
}

impl Store {
    pub fn open(path: impl AsRef<Path>, config: StoreConfig) -> Result<Store> {
        let path = path.as_ref();
        let conn = Connection::open(path)?;
        Store::init(conn, Some(path), config)
    }

    /// A private in-memory database, for tests.
    pub fn open_in_memory(config: StoreConfig) -> Result<Store> {
        Store::init(Connection::open_in_memory()?, None, config)
    }

    /// A read-only connection beside the writer, for status and query
    /// handlers (WAL lets readers run alongside it). It can't migrate, so
    /// the schema must already be this build's.
    pub fn open_reader(path: impl AsRef<Path>, config: StoreConfig) -> Result<Store> {
        let conn = Connection::open_with_flags(
            path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        conn.pragma_update(None, "mmap_size", config.mmap_size as i64)?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        schema::check_current(&conn)?;
        Store::load(conn, config)
    }

    fn init(mut conn: Connection, path: Option<&Path>, config: StoreConfig) -> Result<Store> {
        schema::configure(&conn, config.mmap_size)?;
        schema::migrate(&mut conn, path)?;
        Store::load(conn, config)
    }

    fn load(conn: Connection, config: StoreConfig) -> Result<Store> {
        let mut codec = BlobCodec::new(config.compression_level);
        {
            let mut stmt = conn.prepare("SELECT id, data FROM zstd_dicts")?;
            let rows =
                stmt.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, Vec<u8>>(1)?)))?;
            for row in rows {
                let (id, data) = row?;
                codec.add_dict(id, data);
            }
        }
        let active: Option<i64> = conn
            .query_row(
                "SELECT value FROM settings WHERE key = ?1",
                [FILE_DICT_SETTING],
                |r| r.get(0),
            )
            .optional()?;
        if active.is_some_and(|id| codec_has(&conn, id).ok() != Some(true)) {
            return Err(Error::Corrupt("active file dictionary is missing"));
        }
        codec.set_active(active);
        Ok(Store {
            conn,
            codec,
            config,
        })
    }

    pub fn connection(&self) -> &Connection {
        &self.conn
    }

    pub fn active_file_dict(&self) -> Option<i64> {
        self.codec.active()
    }

    /// Inserts torrents in one transaction; see [`Store::write`].
    pub fn insert(&mut self, batch: &[NewTorrent<'_>]) -> Result<Vec<Inserted>> {
        Ok(self
            .write(&WriteBatch {
                torrents: batch,
                ..WriteBatch::default()
            })?
            .torrents)
    }

    /// Commits a batch in one transaction. Torrents already stored under any
    /// of their swarm hashes (v1 or truncated v2) keep their data and only
    /// gain the new source's bit. New torrents take their ids from the
    /// shared sequence. Touches add source bits to stored torrents. Counters
    /// are added to what's stored.
    pub fn write(&mut self, batch: &WriteBatch<'_>) -> Result<Written> {
        let Store {
            conn,
            codec,
            config,
        } = self;
        let tx = conn.transaction()?;
        let mut out = Vec::with_capacity(batch.torrents.len());
        for t in batch.torrents {
            let info = t.info;
            let bit = t.source.bit();
            if let Some(id) = find_any(&tx, info.swarm_hashes())? {
                add_sources(&tx, id, bit as u64)?;
                out.push(Inserted::Existing(id));
                continue;
            }
            let blob = filetree::encode(&info.files, info.path_encoding)?;
            let blob = codec.compress(&blob)?;
            let name = info.name.decode();
            let plain_name =
                info.name.encoding == TextEncoding::UTF8 && name.as_bytes() == info.name.raw;
            let file_count = info.files.iter().filter(|f| !f.is_padding()).count();
            let id = sources::next_id(&tx)?;
            tx.prepare_cached(
                "INSERT INTO torrents (id, info_hash, info_hash_v2, name, name_raw, name_enc,
                     size, piece_len, piece_count, file_count, info_size, flags,
                     discovered_at, metadata_at, last_seen_at, sources, first_source)
                 VALUES (?14, ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?12,
                     ?15, ?16)",
            )?
            .execute(params![
                &info.primary_hash().0[..],
                info.v2.as_ref().map(|h| &h.0[..]),
                name,
                (!plain_name).then_some(&info.name.raw[..]),
                (!plain_name).then_some(info.name.encoding.id()),
                to_i64(info.total_size)?,
                to_i64(info.piece_length)?,
                to_i64(info.piece_count)?,
                file_count as i64,
                info.info_size as i64,
                derive_flags(info) | t.extra_flags,
                t.discovered_at,
                t.metadata_at,
                id,
                bit,
                t.source.get(),
            ])?;
            tx.prepare_cached(
                "INSERT INTO torrent_files (torrent_id, codec, dict_id, data) VALUES (?1, ?2, ?3, ?4)",
            )?
            .execute(params![id, blob.codec as u8, blob.dict_id, blob.data])?;
            if let Some(extra) = &info.extra {
                tx.prepare_cached("INSERT INTO torrent_extra (torrent_id, data) VALUES (?1, ?2)")?
                    .execute(params![id, extra])?;
            }
            if config.archive_raw {
                if let Some(raw) = t.raw_info {
                    let stripped = info_without_pieces(raw)
                        .map_err(|_| Error::Corrupt("raw info does not parse"))?;
                    tx.prepare_cached(
                        "INSERT INTO torrent_raw (torrent_id, data) VALUES (?1, ?2)",
                    )?
                    .execute(params![id, stripped])?;
                }
            }
            out.push(Inserted::New(id));
        }
        let mut touched = Vec::with_capacity(batch.touches.len());
        for t in batch.touches {
            let id = find_any(&tx, std::iter::once(t.hash))?;
            if let Some(id) = id {
                add_sources(&tx, id, t.sources)?;
            }
            touched.push(id);
        }
        insights::write(&tx, batch.counters, batch.cohorts, batch.events)?;
        tx.commit()?;
        Ok(Written {
            torrents: out,
            touched,
        })
    }

    /// Calls `f` with every 20-byte swarm hash stored: the primary hash of
    /// each torrent, plus the truncated v2 hash of hybrids.
    pub fn for_each_swarm_hash(&self, mut f: impl FnMut(InfoHash)) -> Result<()> {
        let mut stmt = self
            .conn
            .prepare("SELECT info_hash, substr(info_hash_v2, 1, 20) FROM torrents")?;
        let mut rows = stmt.query([])?;
        while let Some(r) = rows.next()? {
            for col in 0..2 {
                let h = r
                    .get_ref(col)?
                    .as_blob_or_null()
                    .map_err(|_| Error::Corrupt("hash type"))?;
                if let Some(h) = h {
                    let h: [u8; 20] = h.try_into().map_err(|_| Error::Corrupt("hash length"))?;
                    f(InfoHash(h));
                }
            }
        }
        Ok(())
    }

    pub fn setting_i64(&self, key: &str) -> Result<Option<i64>> {
        Ok(self
            .conn
            .query_row("SELECT value FROM settings WHERE key = ?1", [key], |r| {
                r.get(0)
            })
            .optional()?)
    }

    pub fn set_setting_i64(&mut self, key: &str, value: i64) -> Result<()> {
        self.conn.execute(
            "INSERT INTO settings (key, value) VALUES (?1, ?2)
             ON CONFLICT (key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )?;
        Ok(())
    }

    /// Looks a torrent up by any 20-byte swarm hash.
    pub fn find(&self, hash: &InfoHash) -> Result<Option<i64>> {
        find_any(&self.conn, std::iter::once(*hash))
    }

    pub fn torrent(&self, id: i64) -> Result<Option<TorrentRow>> {
        let row = self
            .conn
            .prepare_cached(
                "SELECT id, info_hash, info_hash_v2, name, name_raw, name_enc, size, piece_len,
                        piece_count, file_count, info_size, flags, discovered_at, metadata_at,
                        last_seen_at, sources, first_source
                 FROM torrents WHERE id = ?1",
            )?
            .query_row([id], |r| {
                Ok((
                    TorrentRow {
                        id: r.get(0)?,
                        info_hash: InfoHash([0; 20]),
                        info_hash_v2: None,
                        name: r.get(3)?,
                        name_raw: None,
                        size: get_u64(r, 6)?,
                        piece_len: get_u64(r, 7)?,
                        piece_count: get_u64(r, 8)?,
                        file_count: get_u64(r, 9)?,
                        info_size: get_u64(r, 10)?,
                        flags: r.get(11)?,
                        discovered_at: r.get(12)?,
                        metadata_at: r.get(13)?,
                        last_seen_at: r.get(14)?,
                        sources: r.get::<_, i64>(15)? as u64,
                        first_source: None,
                    },
                    r.get::<_, Vec<u8>>(1)?,
                    r.get::<_, Option<Vec<u8>>>(2)?,
                    r.get::<_, Option<Vec<u8>>>(4)?,
                    r.get::<_, Option<u8>>(5)?,
                    r.get::<_, Option<i64>>(16)?,
                ))
            })
            .optional()?;
        let Some((mut row, hash, hash_v2, name_raw, name_enc, first_source)) = row else {
            return Ok(None);
        };
        row.first_source = first_source.map(SourceId::from_sql).transpose()?;
        row.info_hash = InfoHash(
            hash.try_into()
                .map_err(|_| Error::Corrupt("info_hash length"))?,
        );
        row.info_hash_v2 = hash_v2
            .map(|h| h.try_into().map(InfoHashV2))
            .transpose()
            .map_err(|_| Error::Corrupt("info_hash_v2 length"))?;
        row.name_raw = match (name_raw, name_enc) {
            (Some(raw), Some(enc)) => Some(Text {
                raw,
                encoding: TextEncoding::from_id(enc).ok_or(Error::Corrupt("name_enc"))?,
            }),
            (None, None) => None,
            _ => return Err(Error::Corrupt("name_raw without name_enc")),
        };
        Ok(Some(row))
    }

    pub fn files(&mut self, id: i64) -> Result<Option<FileTree>> {
        let row = self
            .conn
            .prepare_cached("SELECT codec, dict_id, data FROM torrent_files WHERE torrent_id = ?1")?
            .query_row([id], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, Option<i64>>(1)?,
                    r.get::<_, Vec<u8>>(2)?,
                ))
            })
            .optional()?;
        let Some((codec, dict_id, data)) = row else {
            return Ok(None);
        };
        let codec = Codec::from_id(codec).ok_or(Error::Corrupt("unknown blob codec"))?;
        let raw = self.codec.decompress(codec, dict_id, &data)?;
        Ok(Some(filetree::decode(&raw)?))
    }

    /// Unknown info keys, as a canonical bencode dictionary.
    pub fn extra(&self, id: i64) -> Result<Option<Vec<u8>>> {
        self.blob("SELECT data FROM torrent_extra WHERE torrent_id = ?1", id)
    }

    /// The archived info dictionary minus `pieces`, if archiving was on.
    pub fn raw_info(&self, id: i64) -> Result<Option<Vec<u8>>> {
        self.blob("SELECT data FROM torrent_raw WHERE torrent_id = ?1", id)
    }

    fn blob(&self, sql: &str, id: i64) -> Result<Option<Vec<u8>>> {
        Ok(self
            .conn
            .prepare_cached(sql)?
            .query_row([id], |r| r.get(0))
            .optional()?)
    }

    /// The most recently stored torrents, newest first.
    pub fn latest(&self, limit: usize) -> Result<Vec<TorrentRow>> {
        let ids: Vec<i64> = self
            .conn
            .prepare_cached("SELECT id FROM torrents ORDER BY id DESC LIMIT ?1")?
            .query_map([limit as i64], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        ids.into_iter()
            .filter_map(|id| self.torrent(id).transpose())
            .collect()
    }

    /// Counts every row: a full scan, so not for hot paths.
    pub fn count(&self) -> Result<u64> {
        Ok(self
            .conn
            .query_row("SELECT count(*) FROM torrents", [], |r| get_u64(r, 0))?)
    }

    /// Trains a file-tree dictionary from up to `max_samples` stored blobs
    /// spread evenly over the table, and makes it the active one. Returns
    /// `None` when there are too few torrents to train on.
    pub fn train_file_dict(&mut self, max_samples: usize, dict_size: usize) -> Result<Option<i64>> {
        const MIN_SAMPLES: usize = 256;
        let total = self.count()? as usize;
        if total < MIN_SAMPLES {
            return Ok(None);
        }
        let step = (total / max_samples.max(1)).max(1) as i64;
        let rows: Vec<(i64, Option<i64>, Vec<u8>)> = self
            .conn
            .prepare(
                "SELECT codec, dict_id, data FROM torrent_files
                 WHERE torrent_id % ?1 = 0 LIMIT ?2",
            )?
            .query_map(params![step, max_samples as i64], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })?
            .collect::<Result<_, _>>()?;
        let mut samples = Vec::with_capacity(rows.len());
        for (codec, dict_id, data) in rows {
            let codec = Codec::from_id(codec).ok_or(Error::Corrupt("unknown blob codec"))?;
            samples.push(self.codec.decompress(codec, dict_id, &data)?);
        }
        if samples.len() < MIN_SAMPLES {
            return Ok(None);
        }
        let dict = compress::train(&samples, dict_size)?;
        let tx = self.conn.transaction()?;
        let id: i64 = tx.query_row(
            "INSERT INTO zstd_dicts (kind, created_at, samples, data)
             VALUES ('file_tree', unixepoch(), ?1, ?2) RETURNING id",
            params![samples.len() as i64, &dict],
            |r| r.get(0),
        )?;
        tx.execute(
            "INSERT INTO settings (key, value) VALUES (?1, ?2)
             ON CONFLICT (key) DO UPDATE SET value = excluded.value",
            params![FILE_DICT_SETTING, id],
        )?;
        tx.commit()?;
        self.codec.add_dict(id, dict);
        self.codec.set_active(Some(id));
        Ok(Some(id))
    }

    /// Rewrites blobs not yet compressed with the active dictionary, in
    /// batches of `batch` rows per transaction. Returns how many changed.
    pub fn recompress_files(&mut self, batch: usize) -> Result<u64> {
        let active = self.codec.active();
        let mut last_id = 0i64;
        let mut changed = 0u64;
        loop {
            let rows: Vec<(i64, i64, Option<i64>, Vec<u8>)> = self
                .conn
                .prepare_cached(
                    "SELECT torrent_id, codec, dict_id, data FROM torrent_files
                     WHERE torrent_id > ?1 AND (codec != 1 OR dict_id IS NOT ?2)
                     ORDER BY torrent_id LIMIT ?3",
                )?
                .query_map(params![last_id, active, batch as i64], |r| {
                    Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
                })?
                .collect::<Result<_, _>>()?;
            let Some(&(last, ..)) = rows.last() else {
                return Ok(changed);
            };
            last_id = last;
            let tx = self.conn.transaction()?;
            for (id, codec, dict_id, data) in rows {
                let codec = Codec::from_id(codec).ok_or(Error::Corrupt("unknown blob codec"))?;
                let raw = self.codec.decompress(codec, dict_id, &data)?;
                let z = self.codec.compress(&raw)?;
                if z.codec != codec || z.dict_id != dict_id {
                    tx.prepare_cached(
                        "UPDATE torrent_files SET codec = ?2, dict_id = ?3, data = ?4
                         WHERE torrent_id = ?1",
                    )?
                    .execute(params![id, z.codec as u8, z.dict_id, z.data])?;
                    changed += 1;
                }
            }
            tx.commit()?;
        }
    }

    /// Folds the WAL back into the main file.
    pub fn checkpoint(&self) -> Result<()> {
        self.conn
            .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()))?;
        Ok(())
    }
}

/// Sets source bits, writing only when one is new.
fn add_sources(conn: &Connection, id: i64, bits: u64) -> Result<()> {
    conn.prepare_cached(
        "UPDATE torrents SET sources = sources | ?2 WHERE id = ?1 AND sources & ?2 != ?2",
    )?
    .execute(params![id, bits as i64])?;
    Ok(())
}

fn codec_has(conn: &Connection, id: i64) -> rusqlite::Result<bool> {
    conn.query_row("SELECT 1 FROM zstd_dicts WHERE id = ?1", [id], |_| Ok(()))
        .optional()
        .map(|r| r.is_some())
}

/// Two indexed lookups. Written with `OR`, SQLite scans the whole table.
const FIND_BY_HASH: &str = "SELECT id FROM torrents WHERE info_hash = ?1
     UNION ALL
     SELECT id FROM torrents
     WHERE info_hash_v2 IS NOT NULL AND substr(info_hash_v2, 1, 20) = ?1
     LIMIT 1";

fn find_any(conn: &Connection, hashes: impl Iterator<Item = InfoHash>) -> Result<Option<i64>> {
    let mut stmt = conn.prepare_cached(FIND_BY_HASH)?;
    for h in hashes {
        if let Some(id) = stmt.query_row([&h.0[..]], |r| r.get(0)).optional()? {
            return Ok(Some(id));
        }
    }
    Ok(None)
}

fn derive_flags(info: &Info) -> i64 {
    let mut f = 0;
    let mut set = |cond: bool, bit: i64| {
        if cond {
            f |= bit;
        }
    };
    set(info.private, flags::PRIVATE);
    set(info.v2.is_some(), flags::V2);
    set(info.version == MetaVersion::Hybrid, flags::HYBRID);
    set(info.files.iter().any(|f| f.is_padding()), flags::PADDING);
    set(
        info.files
            .iter()
            .any(|f| f.attrs.contains(FileAttrs::SYMLINK)),
        flags::SYMLINKS,
    );
    set(info.single_file, flags::SINGLE_FILE);
    set(!info.warnings.is_empty(), flags::WARNINGS);
    f
}

fn to_i64(v: u64) -> Result<i64> {
    i64::try_from(v).map_err(|_| Error::Corrupt("integer out of SQLite range"))
}

/// Reads a column that must hold a non-negative integer.
fn get_u64(r: &rusqlite::Row<'_>, idx: usize) -> rusqlite::Result<u64> {
    let v: i64 = r.get(idx)?;
    u64::try_from(v).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(idx, rusqlite::types::Type::Integer, Box::new(e))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_lookup_uses_indexes() {
        let store = Store::open_in_memory(StoreConfig::default()).unwrap();
        let plan: Vec<String> = store
            .conn
            .prepare(&format!("EXPLAIN QUERY PLAN {FIND_BY_HASH}"))
            .unwrap()
            .query_map([&[0u8; 20][..]], |r| r.get(3))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert!(
            plan.iter().all(|step| !step.starts_with("SCAN")),
            "{plan:?}"
        );
        assert!(
            plan.iter().any(|step| step.contains("torrents_v2")),
            "{plan:?}"
        );
    }
}
