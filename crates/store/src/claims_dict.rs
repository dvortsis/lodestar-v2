//! The trained zstd dictionary for identification answers (X-claims-size,
//! `docs/decisions/2026-11-27-X-claims-size.md`).
//!
//! An answer's claims blob is a few hundred bytes, mostly the same layout,
//! scheme names and language tags in every row, which zstd can't share
//! across rows compressed one at a time. A dictionary trained on the
//! store's own answers supplies that, about halving each blob.
//!
//! - **Stored as the file trees' are:** a row in `zstd_dicts`, kind
//!   [`KIND`], and the settings row [`SETTING`] naming the active one. Both
//!   are written in one short transaction ([`Store::install_claims_dict`]),
//!   so the setting never names a dictionary that isn't there, and a store
//!   whose setting does is refused at open, as for the file trees.
//! - **Trained once** ([`Store::train_claims_dict`]): when there's no active
//!   dictionary and at least [`MIN_ROWS`] answers (version 3 rows of an
//!   identify source) exist, from at most [`MAX_SAMPLES`] of them and
//!   [`MAX_SAMPLE_BYTES`] in all, outside any transaction. Once the setting
//!   is there nothing trains again, restarts included. There's no
//!   retraining: one would take a new id.
//! - **Used for answers only:** [`ClaimsDicts::encode`] writes codec 2 for
//!   claims with a details section once a dictionary is active, and codec
//!   0 or 1 for everything else, as before. Rows written before it keep
//!   their codec 1 blobs: nothing rewrites them.
//! - **Never deleted or replaced:** a blob names its dictionary by id, and
//!   a codec 2 blob whose dictionary isn't found fails closed
//!   ([`ClaimsError::Dictionary`]): it reads as unreadable, as a corrupt
//!   row does, never through another dictionary.
//! - **Readers find a new dictionary themselves:** a connection opened
//!   before the dictionary was trained loads it from `zstd_dicts` the
//!   first time a blob names it.

use std::collections::HashMap;
use std::sync::Mutex;

use rusqlite::{Connection, OptionalExtension, params};
use zstd::bulk::{Compressor, Decompressor};
use zstd::zstd_safe::CParameter;

use crate::claims::{self, Claims, ClaimsError, VERSION_DETAILS};
use crate::identify_sweep::TERMS_KEY;
use crate::{Error, Result, Store};

/// `zstd_dicts.kind` of a claims dictionary.
pub const KIND: &str = "claims";
/// The settings row naming the active claims dictionary.
pub const SETTING: &str = "claims_dict";
/// Answers stored before a dictionary is trained.
pub const MIN_ROWS: usize = 1_000;
/// The most answers a dictionary is trained from.
pub const MAX_SAMPLES: usize = 4_000;
/// The most sample bytes a dictionary is trained from (memory and time).
pub const MAX_SAMPLE_BYTES: usize = 4 << 20;
/// A blob's plain body larger than this isn't a sample.
pub const MAX_SAMPLE: usize = 16 << 10;
/// The dictionary's target size: the file trees' (architecture §3).
pub const DICT_SIZE: usize = 112 << 10;
/// Fewer readable samples than this (big or unreadable rows skipped)
/// train nothing.
const MIN_SAMPLES: usize = 256;

/// What [`Store::train_claims_dict`] trained.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Trained {
    /// The `zstd_dicts` id.
    pub id: i64,
    /// Answers it was trained from.
    pub samples: usize,
    /// The dictionary's size in bytes.
    pub bytes: usize,
}

/// A connection's claims dictionaries: the active one's compressor and
/// every decompressor it has needed.
pub(crate) struct ClaimsDicts {
    active: Option<i64>,
    compressor: Option<(i64, i32, Compressor<'static>)>,
    decompressors: Mutex<HashMap<i64, Decompressor<'static>>>,
}

impl std::fmt::Debug for ClaimsDicts {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClaimsDicts")
            .field("active", &self.active)
            .finish_non_exhaustive()
    }
}

/// `zstd_dicts.data` of claims dictionary `id`, if there's one.
fn dict_bytes(conn: &Connection, id: i64) -> rusqlite::Result<Option<Vec<u8>>> {
    conn.prepare_cached("SELECT data FROM zstd_dicts WHERE id = ?1 AND kind = ?2")?
        .query_row(params![id, KIND], |r| r.get(0))
        .optional()
}

fn active_setting(conn: &Connection) -> rusqlite::Result<Option<i64>> {
    conn.query_row(
        "SELECT value FROM settings WHERE key = ?1",
        [SETTING],
        |r| r.get(0),
    )
    .optional()
}

impl ClaimsDicts {
    /// The active dictionary as stored. A setting naming a dictionary
    /// that isn't there is corruption, as for the file trees.
    pub(crate) fn load(conn: &Connection) -> Result<ClaimsDicts> {
        let active = active_setting(conn)?;
        if let Some(id) = active
            && dict_bytes(conn, id)?.is_none()
        {
            return Err(Error::Corrupt("the active claims dictionary is missing"));
        }
        Ok(ClaimsDicts {
            active,
            compressor: None,
            decompressors: Mutex::new(HashMap::new()),
        })
    }

    pub(crate) fn active(&self) -> Option<i64> {
        self.active
    }

    /// The stored blob for `c` at zstd `level`: codec 2 with the active
    /// dictionary for claims with a details section, the plain encoding
    /// otherwise.
    pub(crate) fn encode(
        &mut self,
        conn: &Connection,
        c: &Claims,
        level: i32,
    ) -> std::result::Result<Vec<u8>, ClaimsError> {
        let Some(id) = self.active.filter(|_| c.version() == VERSION_DETAILS) else {
            return claims::encode(c, level);
        };
        if self
            .compressor
            .as_ref()
            .is_none_or(|(i, l, _)| (*i, *l) != (id, level))
        {
            // A dictionary that can't be read or built only costs the
            // saving: the claims are written as before.
            let Some(z) = active_compressor(conn, id, level) else {
                return claims::encode(c, level);
            };
            self.compressor = Some((id, level, z));
        }
        let (_, _, z) = self.compressor.as_mut().expect("just set");
        claims::encode_dict(c, id, z)
    }

    /// Reads a stored blob of any codec, loading a dictionary it names
    /// from `conn` the first time. Fails closed on a dictionary that isn't
    /// there.
    pub(crate) fn decode(
        &self,
        conn: &Connection,
        blob: &[u8],
    ) -> std::result::Result<Claims, ClaimsError> {
        if claims::dictionary_of(blob).is_none() {
            return claims::decode(blob);
        }
        let mut map = self.decompressors.lock().unwrap_or_else(|e| e.into_inner());
        claims::decode_with(
            blob,
            &mut Lazy {
                conn,
                map: &mut map,
            },
        )
    }
}

fn active_compressor(conn: &Connection, id: i64, level: i32) -> Option<Compressor<'static>> {
    compressor(&dict_bytes(conn, id).ok()??, level).ok()
}

/// A compressor for [`claims::encode_dict`] with `dict` at zstd `level`:
/// the blob names its dictionary, so the frame carries no dictionary id
/// and no checksum, and it declares its size.
pub fn compressor(dict: &[u8], level: i32) -> std::io::Result<Compressor<'static>> {
    let mut z = Compressor::with_dictionary(level, dict)?;
    z.set_parameter(CParameter::DictIdFlag(false))?;
    z.set_parameter(CParameter::ChecksumFlag(false))?;
    z.set_parameter(CParameter::ContentSizeFlag(true))?;
    Ok(z)
}

/// Decompressors by dictionary id, each built from `zstd_dicts` when a
/// blob first names it.
struct Lazy<'a> {
    conn: &'a Connection,
    map: &'a mut HashMap<i64, Decompressor<'static>>,
}

impl claims::Inflate for Lazy<'_> {
    fn inflate(
        &mut self,
        id: i64,
        frame: &[u8],
        capacity: usize,
    ) -> Option<std::io::Result<Vec<u8>>> {
        if !self.map.contains_key(&id) {
            // Not found, or the lookup failed: the blob is refused.
            let dict = dict_bytes(self.conn, id).ok()??;
            self.map
                .insert(id, Decompressor::with_dictionary(&dict).ok()?);
        }
        Some(self.map.get_mut(&id)?.decompress(frame, capacity))
    }
}

/// Trains a dictionary of at most `max_size` bytes from `samples` (plain
/// bodies).
pub fn train(samples: &[Vec<u8>], max_size: usize) -> std::io::Result<Vec<u8>> {
    zstd::dict::from_samples(samples, max_size)
}

impl Store {
    /// The active claims dictionary's id, if one is trained.
    pub fn active_claims_dict(&self) -> Option<i64> {
        self.claims_dicts.active()
    }

    /// The answers a claims dictionary would be trained from: `None` when
    /// one is active already or fewer than [`MIN_ROWS`] answers are stored.
    /// Reads only, through the `(source_id, …)` indexes of the identify
    /// sources (those with a terms record), and stops after
    /// [`MAX_SAMPLES`] rows or [`MAX_SAMPLE_BYTES`].
    pub fn claims_dict_samples(&self) -> Result<Option<Vec<Vec<u8>>>> {
        if self.claims_dicts.active().is_some() || active_setting(&self.conn)?.is_some() {
            return Ok(None);
        }
        let version = [VERSION_DETAILS];
        let stored: i64 = self
            .conn
            .prepare_cached(
                "SELECT count(*) FROM (SELECT 1 FROM torrent_claims
             WHERE source_id IN (SELECT source_id FROM source_state WHERE key = ?1)
               AND substr(data, 1, 1) = ?2 LIMIT ?3)",
            )?
            .query_row(params![TERMS_KEY, &version[..], MIN_ROWS as i64], |r| {
                r.get(0)
            })?;
        if (stored as usize) < MIN_ROWS {
            return Ok(None);
        }
        let mut stmt = self.conn.prepare_cached(
            "SELECT data FROM torrent_claims
             WHERE source_id IN (SELECT source_id FROM source_state WHERE key = ?1)
               AND substr(data, 1, 1) = ?2 LIMIT ?3",
        )?;
        let mut rows = stmt.query(params![TERMS_KEY, &version[..], MAX_SAMPLES as i64])?;
        let (mut samples, mut bytes) = (Vec::new(), 0usize);
        while let Some(r) = rows.next()? {
            let data = r.get_ref(0)?.as_blob().map_err(rusqlite::Error::from)?;
            // Unreadable rows aren't samples; the plain body is.
            let Ok(c) = self.claims_dicts.decode(&self.conn, data) else {
                continue;
            };
            let Ok(plain) = claims::encode_plain(&c) else {
                continue;
            };
            if plain.len() > MAX_SAMPLE {
                continue;
            }
            if bytes + plain.len() > MAX_SAMPLE_BYTES {
                break;
            }
            bytes += plain.len();
            samples.push(plain);
        }
        Ok((samples.len() >= MIN_SAMPLES).then_some(samples))
    }

    /// Stores `dict`, trained from `samples` answers, and makes it the
    /// active claims dictionary, in one transaction. `None`, storing
    /// nothing, when another connection made one active first.
    pub fn install_claims_dict(&mut self, samples: usize, dict: &[u8]) -> Result<Option<i64>> {
        let tx = crate::begin_write(&mut self.conn)?;
        if active_setting(&tx)?.is_some() {
            return Ok(None);
        }
        let id: i64 = tx.query_row(
            "INSERT INTO zstd_dicts (kind, created_at, samples, data)
             VALUES (?1, unixepoch(), ?2, ?3) RETURNING id",
            params![KIND, samples as i64, dict],
            |r| r.get(0),
        )?;
        tx.execute(
            "INSERT INTO settings (key, value) VALUES (?1, ?2)",
            params![SETTING, id],
        )?;
        tx.commit()?;
        self.claims_dicts.active = Some(id);
        self.claims_dicts.compressor = None;
        Ok(Some(id))
    }

    /// Trains the claims dictionary once it's due
    /// ([`claims_dict_samples`](Store::claims_dict_samples)), outside any
    /// transaction, and installs it. `None` when it isn't due.
    pub fn train_claims_dict(&mut self) -> Result<Option<Trained>> {
        let Some(samples) = self.claims_dict_samples()? else {
            return Ok(None);
        };
        let dict = train(&samples, DICT_SIZE)?;
        Ok(self
            .install_claims_dict(samples.len(), &dict)?
            .map(|id| Trained {
                id,
                samples: samples.len(),
                bytes: dict.len(),
            }))
    }

    /// Reads a stored claims blob of any codec with this store's
    /// dictionaries.
    pub fn decode_claims(&self, blob: &[u8]) -> std::result::Result<Claims, ClaimsError> {
        self.claims_dicts.decode(&self.conn, blob)
    }
}
