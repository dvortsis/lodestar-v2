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

use crate::claims::{self, Claims, ClaimsError, Inflated, VERSION_DETAILS};
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
/// After a failed training, how long the next waits: training reads and
/// trains from scratch, and the writer waits for it.
pub const TRAIN_RETRY: std::time::Duration = std::time::Duration::from_secs(3600);

/// What [`Store::sample_claims`] found.
enum Sampled {
    NotDue,
    /// Due, but too few answers made usable samples.
    Short,
    Ready(Vec<Vec<u8>>),
}

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
    /// A trained dictionary whose install failed (a busy store, say), with
    /// its sample count: the next attempt installs it without training.
    pending: Option<(usize, Vec<u8>)>,
    /// No training before this, after one failed.
    retry_at: Option<std::time::Instant>,
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
            pending: None,
            retry_at: None,
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
        match claims::encode_dict(c, id, z, level) {
            // zstd failing costs the saving, not the answer.
            Err(ClaimsError::Compression) => claims::encode(c, level),
            r => r,
        }
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
    fn inflate(&mut self, id: i64, frame: &[u8], capacity: usize) -> Inflated {
        if !self.map.contains_key(&id) {
            let dict = match dict_bytes(self.conn, id) {
                Ok(Some(dict)) => dict,
                // Not stored: the blob is refused.
                Ok(None) => return Inflated::Unknown,
                // A store error isn't a verdict on the blob.
                Err(_) => return Inflated::Unavailable,
            };
            // Stored bytes zstd won't take are a corrupt dictionary: its
            // blobs read as unreadable, as any corrupt row does.
            let Ok(d) = Decompressor::with_dictionary(&dict) else {
                return Inflated::Unknown;
            };
            self.map.insert(id, d);
        }
        match self
            .map
            .get_mut(&id)
            .expect("just inserted")
            .decompress(frame, capacity)
        {
            Ok(plain) => Inflated::Plain(plain),
            Err(_) => Inflated::Failed,
        }
    }
}

/// The error for a read that met a dictionary it couldn't look up: the
/// store's fault, not the row's (as a missing file-tree dictionary is,
/// `compress::BlobCodec::decompress`), so the batch is tried again.
pub(crate) fn unavailable() -> Error {
    Error::Compression(std::io::Error::new(
        std::io::ErrorKind::NotFound,
        "claims dictionary unavailable",
    ))
}

/// `c`'s layout with nothing a source supplied: what the dictionary is
/// trained on.
///
/// - **Kept, as the structure the encoding has anyway:** which fields and
///   sections are present, how many entries each list has, every text's
///   length in bytes and characters, the id schemes (the fixed list
///   [`claims::ENRICHMENT_SCHEMES`]) and each id's fixed letters (`tt`,
///   `Q`, `OL`…`M`, `r`/`m`, an MBID's dashes), and the primary language
///   subtag of a language tag when it's a 2–3 letter code (ISO 639).
/// - **Replaced:** every title, genre, category and newsgroup character by
///   one fixed character of the same class (`a`, `A`, `0`, a space, `-`
///   for other ASCII, and for any other character the first one of its
///   64-character Unicode block plus 0x21, so only its script survives);
///   every id digit and MBID hex digit by `1`; the year by 2000, the match
///   confidence by 50, a posting time by 0; any other language tag (a
///   region, a script, a private-use part) by its primary subtag or
///   `und`. Entries that become equal are kept once.
///
/// A placeholder records shape, never content: no title, genre or id can
/// be read back from it.
pub fn template(c: &Claims) -> Claims {
    fn text(s: &str, lower: bool) -> String {
        s.chars()
            .map(|ch| match ch {
                'a'..='z' => 'a',
                'A'..='Z' if lower => 'a',
                'A'..='Z' => 'A',
                '0'..='9' => '0',
                ' ' => ' ',
                _ if ch.is_ascii() => '-',
                _ => char::from_u32((ch as u32 & !0x3F) | 0x21).unwrap_or('-'),
            })
            .collect()
    }
    fn id(scheme: &str, v: &str) -> String {
        v.chars()
            .map(|ch| match ch {
                '0'..='9' => '1',
                'a'..='f' if scheme == "mbid" => '1',
                _ => ch,
            })
            .collect()
    }
    fn language(tag: &str) -> String {
        match tag.split('-').next() {
            Some(p) if (2..=3).contains(&p.len()) && p.bytes().all(|b| b.is_ascii_lowercase()) => {
                p.to_owned()
            }
            _ => "und".to_owned(),
        }
    }
    fn dedup<T: PartialEq>(v: Vec<T>) -> Vec<T> {
        let mut out: Vec<T> = Vec::with_capacity(v.len());
        for x in v {
            if !out.contains(&x) {
                out.push(x);
            }
        }
        out
    }
    let mut t = Claims {
        ids: c
            .ids
            .iter()
            .map(|i| claims::ExternalId {
                scheme: i.scheme.clone(),
                value: id(&i.scheme, &i.value),
            })
            .collect(),
        ..Claims::default()
    };
    t.provenance.confidence = c.provenance.confidence;
    t.details = c.details.as_ref().map(|d| {
        Box::new(claims::Details {
            source_category: d.source_category.as_deref().map(|s| text(s, false)),
            posted_at: d.posted_at.map(|_| 0),
            newsgroups: dedup(d.newsgroups.iter().map(|g| text(g, true)).collect()),
            alt_titles: dedup(
                d.alt_titles
                    .iter()
                    .map(|a| claims::AltTitle {
                        language: language(&a.language),
                        title: text(&a.title, false),
                    })
                    .collect(),
            ),
            canonical_title: d.canonical_title.as_deref().map(|s| text(s, false)),
            genres: dedup(d.genres.iter().map(|g| text(g, true)).collect()),
            year: d.year.map(|_| 2000),
            match_confidence: d.match_confidence.map(|_| 50),
        })
    });
    t
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
        Ok(match self.sample_claims()? {
            Sampled::Ready(s) => Some(s),
            Sampled::NotDue | Sampled::Short => None,
        })
    }

    /// [`claims_dict_samples`](Store::claims_dict_samples), telling a
    /// store that isn't due from one whose answers gave too few usable
    /// samples.
    fn sample_claims(&self) -> Result<Sampled> {
        if self.claims_dicts.active().is_some() || active_setting(&self.conn)?.is_some() {
            return Ok(Sampled::NotDue);
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
            return Ok(Sampled::NotDue);
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
            // Never the answer itself: its layout with every value the
            // source supplied replaced (`template`), so the dictionary,
            // kept for good, holds nothing a purge, a refile or a terms
            // limit must remove.
            let Ok(plain) = claims::encode_plain(&template(&c)) else {
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
        Ok(if samples.len() >= MIN_SAMPLES {
            Sampled::Ready(samples)
        } else {
            Sampled::Short
        })
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
    /// transaction, and installs it. `None` when it isn't due, or another
    /// connection installed one first (this one then uses it).
    ///
    /// A failed training waits [`TRAIN_RETRY`] before the next. A failed
    /// install keeps the trained dictionary, and the next call installs it
    /// without training again. Either error is returned for the caller to
    /// report.
    pub fn train_claims_dict(&mut self) -> Result<Option<Trained>> {
        self.train_claims_dict_with(|s| train(s, DICT_SIZE))
    }

    /// [`train_claims_dict`](Store::train_claims_dict) with its trainer.
    fn train_claims_dict_with(
        &mut self,
        trainer: impl FnOnce(&[Vec<u8>]) -> std::io::Result<Vec<u8>>,
    ) -> Result<Option<Trained>> {
        if self.claims_dicts.active.is_some() || self.adopt_claims_dict()? {
            return Ok(None);
        }
        let (samples, dict) = match self.claims_dicts.pending.take() {
            Some(p) => p,
            None => {
                if self
                    .claims_dicts
                    .retry_at
                    .is_some_and(|at| std::time::Instant::now() < at)
                {
                    return Ok(None);
                }
                let samples = match self.sample_claims()? {
                    Sampled::Ready(s) => s,
                    Sampled::NotDue => return Ok(None),
                    // Due, but the answers gave too few usable samples:
                    // sampling again at every flush would read them all
                    // again for nothing.
                    Sampled::Short => {
                        self.claims_dicts.retry_at = Some(std::time::Instant::now() + TRAIN_RETRY);
                        return Ok(None);
                    }
                };
                match trainer(&samples) {
                    Ok(dict) => (samples.len(), dict),
                    Err(e) => {
                        self.claims_dicts.retry_at = Some(std::time::Instant::now() + TRAIN_RETRY);
                        return Err(e.into());
                    }
                }
            }
        };
        match self.install_claims_dict(samples, &dict) {
            Ok(Some(id)) => Ok(Some(Trained {
                id,
                samples,
                bytes: dict.len(),
            })),
            Ok(None) => {
                self.adopt_claims_dict()?;
                Ok(None)
            }
            Err(e) => {
                self.claims_dicts.pending = Some((samples, dict));
                Err(e)
            }
        }
    }

    /// Takes up the dictionary another connection made active: true when
    /// there's one.
    fn adopt_claims_dict(&mut self) -> Result<bool> {
        let Some(id) = active_setting(&self.conn)? else {
            return Ok(false);
        };
        if dict_bytes(&self.conn, id)?.is_none() {
            return Err(Error::Corrupt("the active claims dictionary is missing"));
        }
        self.claims_dicts.active = Some(id);
        self.claims_dicts.compressor = None;
        self.claims_dicts.pending = None;
        Ok(true)
    }

    /// Reads a stored claims blob of any codec with this store's
    /// dictionaries.
    pub fn decode_claims(&self, blob: &[u8]) -> std::result::Result<Claims, ClaimsError> {
        self.claims_dicts.decode(&self.conn, blob)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::StoreConfig;
    use crate::claims::{Details, ExternalId};

    /// A training that fails is reported, and the next waits
    /// [`TRAIN_RETRY`], so the writer isn't held up by it at every flush.
    #[test]
    fn a_failed_training_waits_before_the_next() {
        let mut store = Store::open_in_memory(StoreConfig::default()).unwrap();
        store.register_source("dht", "1", 1).unwrap();
        let source = store.register_source("wikidata", "1", 1).unwrap();
        store
            .set_plugin_state(
                source,
                TERMS_KEY,
                Some(&[1, 0b110, 0, 0, 0, 0, 0, 0xed, 0x4e, 0]),
            )
            .unwrap();
        for i in 1..=MIN_ROWS as i64 {
            let mut hash = [0u8; 20];
            hash[..8].copy_from_slice(&i.to_be_bytes());
            store
                .conn
                .execute(
                    "INSERT INTO torrents (id, info_hash, name, size, piece_len, piece_count,
                         file_count, info_size, flags, discovered_at, metadata_at, last_seen_at)
                     VALUES (?1, ?2, 't', 0, 0, 0, 0, 0, 0, 0, 0, 0)",
                    params![i, &hash[..]],
                )
                .unwrap();
            let c = Claims {
                ids: vec![ExternalId {
                    scheme: "wikidata".into(),
                    value: format!("Q{i}"),
                }],
                details: Some(Box::new(Details {
                    canonical_title: Some(format!("Work {i}")),
                    ..Details::default()
                })),
                ..Claims::default()
            };
            assert!(store.put_claims_at(i, source, &c, false, 1).unwrap());
        }
        let calls = std::cell::Cell::new(0);
        let refuse = |_: &[Vec<u8>]| -> std::io::Result<Vec<u8>> {
            calls.set(calls.get() + 1);
            Err(std::io::Error::other("refused"))
        };
        assert!(store.train_claims_dict_with(refuse).is_err());
        assert_eq!(calls.get(), 1);
        // Within the wait: nothing is sampled or trained.
        assert_eq!(store.train_claims_dict_with(refuse).unwrap(), None);
        assert_eq!(calls.get(), 1, "no second training within the wait");
        // After it, training runs again (and succeeds here).
        store.claims_dicts.retry_at = Some(std::time::Instant::now());
        let t = store
            .train_claims_dict()
            .unwrap()
            .expect("trained after the wait");
        assert_eq!(store.active_claims_dict(), Some(t.id));
    }
}
