//! The `Results` payload (architecture §19.4): a batch of torrents a
//! worker prepared, for the coordinator to commit.
//!
//! Each record carries what the plan lists (`docs/plan/2026-11-06-v06-workers.md`
//! §4.3, WK-6): the leased key and the swarm hashes (v1 and v2), the
//! prepared row, the compressed file-tree blob, `torrent_extra`, the
//! parsed and classified fields, provenance (the sources the worker saw,
//! the fetch time), the lease's fencing token, and the worker's verdict
//! with the rule-set version it judged under. The raw info dict goes only
//! when `archive_raw` is on (plan Q8), and **`pieces` is never encoded**
//! (§3): [`Prepared`] has no field for it, and both the encoder and the
//! decoder refuse a raw dict or extra blob that holds one.
//!
//! What it doesn't carry, and why: the worker id (the coordinator knows
//! it from the authenticated session), the decoded file list and the
//! in-memory classification (both rebuilt from the blob and the name at
//! commit, which re-judges every remote result, §19.4), and the first
//! source (the coordinator's lease holds it).
//!
//! # Layout, version 1
//! All integers are LEB128 varints in their shortest form; signed ones are
//! zigzagged first. Byte strings are a varint length, then the bytes.
//!
//! ```text
//! batch   = version:u8(=1) batch_id:uvar count:uvar record*count
//! record  = flags:uvar info_hash:[20] [v2:[32]] token:uvar sources:uvar
//!           metadata_at:ivar rule_version:uvar [action:u8]
//!           name:bytes [name_raw:bytes encoding:u8]
//!           size:uvar piece_len:uvar piece_count:uvar file_count:uvar
//!           info_size:uvar torrent_flags:ivar
//!           codec:u8 [dict_id:ivar] blob:bytes
//!           [extra:bytes] [raw:bytes]
//!           [enrich_version:ivar content_type:u8 junk_score:u8 comp:bytes data:bytes]
//! ```
//! A `[field]` is present exactly when its bit in `flags` is set ([`flag`]).

use std::collections::HashSet;

use lodestar_classify::{Composition, ContentType, ENRICHMENT_VERSION, Record};
use lodestar_cluster::Token;
use lodestar_filter::Action;
use lodestar_metainfo::Limits;
use lodestar_metainfo::{InfoHash, InfoHashV2, TextEncoding, info_without_pieces};
use lodestar_store::{Codec, Compressed, Enriched, Prepared, flags};

use super::{Error, MAX_PAYLOAD, Reader, Result, Writer};

/// The format version this build writes and the only one it reads.
pub const VERSION: u8 = 1;

/// Records in one batch, at most.
pub const MAX_RECORDS: usize = 1024;
/// A torrent's name, decoded (metainfo caps the raw name at 4 KiB; a
/// legacy encoding can grow by up to three bytes per byte when decoded).
pub const MAX_NAME: usize = 16 << 10;
/// The raw name bytes (metainfo's `max_name_bytes`).
pub const MAX_NAME_RAW: usize = 4 << 10;
/// `torrent_extra` (metainfo's `max_extra_bytes`).
pub const MAX_EXTRA: usize = 64 << 10;
/// The packed composition (`torrents.comp`).
pub const MAX_COMP: usize = 1 << 10;
/// The enrichment record (`torrent_parsed`).
pub const MAX_ENRICHMENT: usize = 16 << 10;
/// The file-tree blob and the raw dict are capped only by the payload.
pub const MAX_BLOB: usize = MAX_PAYLOAD;

// The caps follow metainfo's, so a dict this build parses always fits.
const _: () = assert!(MAX_NAME_RAW == Limits::DEFAULT.max_name_bytes);
const _: () = assert!(MAX_NAME >= 4 * Limits::DEFAULT.max_name_bytes);
const _: () = assert!(MAX_EXTRA == Limits::DEFAULT.max_extra_bytes);

/// The torrent flags `Preparer` derives from the info dict alone. The
/// others (imported, backfill, partial file list, private source, and the
/// filter's hidden, demoted and flagged) are the coordinator's to set.
pub const DERIVED_FLAGS: i64 = flags::PRIVATE
    | flags::V2
    | flags::HYBRID
    | flags::PADDING
    | flags::SYMLINKS
    | flags::SINGLE_FILE
    | flags::WARNINGS;

/// The bytes of the smallest record: a flags byte, the hash, one byte for
/// each of the twelve always-present varints and for the codec, and an
/// empty name and blob.
pub const MIN_RECORD: usize = 1 + 20 + 12 + 1;

/// `flags` bits.
pub mod flag {
    /// The leased key is the truncated v2 hash, not `info_hash`.
    pub const KEY_IS_V2: u64 = 1 << 0;
    pub const V2: u64 = 1 << 1;
    pub const NAME_RAW: u64 = 1 << 2;
    pub const DICT_ID: u64 = 1 << 3;
    pub const EXTRA: u64 = 1 << 4;
    pub const RAW: u64 = 1 << 5;
    pub const ENRICHMENT: u64 = 1 << 6;
    pub const ACTION: u64 = 1 << 7;
    /// A classification rule applied ([`ResultRecord::fix`]); its value is
    /// [`FIX_ADULT`].
    pub const FIX: u64 = 1 << 8;
    pub const FIX_ADULT: u64 = 1 << 9;
    pub const PRIVATE_SOURCE: u64 = 1 << 10;
    pub const SOURCE_ADULT: u64 = 1 << 11;
    /// Every bit version 1 defines; any other refuses the record.
    pub const ALL: u64 = (1 << 12) - 1;
}

/// One `Results` message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResultsBatch {
    /// Idempotency: a replayed batch (a spool, a resend) has the same id.
    pub batch: u64,
    pub records: Vec<ResultRecord>,
}

/// One prepared torrent from a worker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResultRecord {
    /// The leased key: `torrent.info_hash`, or the truncated v2 hash of a
    /// hybrid fetched under it. Always one of the torrent's swarm hashes.
    pub key: InfoHash,
    /// The lease's fencing token.
    pub token: Token,
    /// The row, blob, extras, enrichment and (with `archive_raw`) the raw
    /// dict minus `pieces`.
    pub torrent: Prepared,
    /// The sources the worker saw for the key (a bitset).
    pub sources: u64,
    /// When the worker fetched the info dict (Unix seconds, its clock).
    pub metadata_at: i64,
    /// The worker's verdict: the action that applies, if any.
    pub action: Option<Action>,
    /// The rule-set version the worker judged under.
    pub rule_version: u64,
    /// The classification rule applied: Is adult (`true`) or Not adult.
    pub fix: Option<bool>,
    /// Only private sources reported the key.
    pub private_source: bool,
    /// An always-adult source found it.
    pub source_adult: bool,
}

impl ResultsBatch {
    /// The payload. Refuses a batch the coordinator would refuse: over a
    /// cap (split it), with a record that breaks a rule, or with a key
    /// twice.
    pub fn encode(&self) -> Result<Vec<u8>> {
        if self.records.len() > MAX_RECORDS {
            return Err(Error::TooLarge("records"));
        }
        let mut seen = HashSet::with_capacity(self.records.len());
        let mut w = Writer::default();
        w.u8(VERSION);
        w.uvar(self.batch);
        w.uvar(self.records.len() as u64);
        for r in &self.records {
            if !seen.insert(r.key) {
                return Err(Error::Invalid("duplicate key"));
            }
            write_record(&mut w, r)?;
            if w.out.len() > MAX_PAYLOAD {
                return Err(Error::TooLarge("payload"));
            }
        }
        Ok(w.out)
    }

    /// The whole batch, or an error and nothing.
    pub fn decode(payload: &[u8]) -> Result<ResultsBatch> {
        let mut r = Reader::new(payload)?;
        let version = r.u8()?;
        if version != VERSION {
            return Err(Error::Version(version));
        }
        let batch = r.uvar()?;
        let n = r.count(MAX_RECORDS, MIN_RECORD, "records")?;
        let mut records = Vec::with_capacity(n);
        let mut seen = HashSet::with_capacity(n);
        for _ in 0..n {
            let rec = read_record(&mut r)?;
            if !seen.insert(rec.key) {
                return Err(Error::Invalid("duplicate key"));
            }
            records.push(rec);
        }
        r.finish()?;
        Ok(ResultsBatch { batch, records })
    }
}

impl ResultRecord {
    /// The record's bytes in a batch, for packing batches under
    /// [`MAX_PAYLOAD`] (a batch adds at most 13 bytes of header).
    pub fn encoded_len(&self) -> Result<usize> {
        let mut w = Writer::default();
        write_record(&mut w, self)?;
        Ok(w.out.len())
    }
}

fn write_record(w: &mut Writer, r: &ResultRecord) -> Result<()> {
    let t = &r.torrent;
    let key_is_v2 = key_is_v2(r)?;
    check_strings(t)?;
    let mut flags = 0;
    let mut set = |bit, on: bool| {
        if on {
            flags |= bit;
        }
    };
    set(flag::KEY_IS_V2, key_is_v2);
    set(flag::V2, t.info_hash_v2.is_some());
    set(flag::NAME_RAW, t.name_raw.is_some());
    set(flag::DICT_ID, t.files.dict_id.is_some());
    set(flag::EXTRA, t.extra.is_some());
    set(flag::RAW, t.raw.is_some());
    set(flag::ENRICHMENT, t.enrichment.is_some());
    set(flag::ACTION, r.action.is_some());
    set(flag::FIX, r.fix.is_some());
    set(flag::FIX_ADULT, r.fix == Some(true));
    set(flag::PRIVATE_SOURCE, r.private_source);
    set(flag::SOURCE_ADULT, r.source_adult);
    w.uvar(flags);
    w.fixed(&t.info_hash.0);
    if let Some(v2) = t.info_hash_v2 {
        w.fixed(&v2.0);
    }
    w.uvar(r.token.0);
    w.uvar(r.sources);
    w.ivar(r.metadata_at);
    w.uvar(r.rule_version);
    if let Some(a) = r.action {
        w.u8(a.code());
    }
    w.bytes(t.name.as_bytes());
    if let Some((raw, enc)) = &t.name_raw {
        w.bytes(raw);
        w.u8(*enc);
    }
    for v in [
        t.size,
        t.piece_len,
        t.piece_count,
        t.file_count,
        t.info_size,
    ] {
        w.uvar(v);
    }
    w.ivar(t.flags);
    w.u8(t.files.codec as u8);
    if let Some(id) = t.files.dict_id {
        w.ivar(id);
    }
    w.bytes(&t.files.data);
    if let Some(extra) = &t.extra {
        w.bytes(extra);
    }
    if let Some(raw) = &t.raw {
        w.bytes(raw);
    }
    if let Some(e) = &t.enrichment {
        w.ivar(e.version);
        w.u8(e.content_type);
        w.u8(e.junk_score);
        w.bytes(&e.comp);
        w.bytes(&e.data);
    }
    Ok(())
}

fn read_record(r: &mut Reader<'_>) -> Result<ResultRecord> {
    let flags = r.uvar()?;
    if flags & !flag::ALL != 0 {
        return Err(Error::Invalid("flags"));
    }
    let has = |bit| flags & bit != 0;
    if has(flag::FIX_ADULT) && !has(flag::FIX) {
        return Err(Error::Invalid("flags"));
    }
    let info_hash = InfoHash(r.fixed()?);
    let info_hash_v2 = has(flag::V2)
        .then(|| r.fixed().map(InfoHashV2))
        .transpose()?;
    let token = Token(r.uvar()?);
    let sources = r.uvar()?;
    let metadata_at = r.ivar()?;
    let rule_version = r.uvar()?;
    let action = if has(flag::ACTION) {
        Some(Action::from_code(r.u8()?).ok_or(Error::Invalid("action"))?)
    } else {
        None
    };
    let name = std::str::from_utf8(r.bytes(MAX_NAME, "name")?)
        .map_err(|_| Error::Invalid("name"))?
        .to_owned();
    let name_raw = if has(flag::NAME_RAW) {
        let raw = r.bytes(MAX_NAME_RAW, "raw name")?.to_vec();
        Some((raw, r.u8()?))
    } else {
        None
    };
    let [size, piece_len, piece_count, file_count, info_size] =
        [r.uvar()?, r.uvar()?, r.uvar()?, r.uvar()?, r.uvar()?];
    let torrent_flags = r.ivar()?;
    let codec = Codec::from_id(i64::from(r.u8()?)).ok_or(Error::Invalid("codec"))?;
    let dict_id = has(flag::DICT_ID).then(|| r.ivar()).transpose()?;
    let data = r.bytes(MAX_BLOB, "file tree")?.to_vec();
    let extra = has(flag::EXTRA)
        .then(|| r.bytes(MAX_EXTRA, "extra").map(<[u8]>::to_vec))
        .transpose()?;
    let raw = has(flag::RAW)
        .then(|| r.bytes(MAX_BLOB, "raw info").map(<[u8]>::to_vec))
        .transpose()?;
    let enrichment = if has(flag::ENRICHMENT) {
        Some(Enriched {
            version: r.ivar()?,
            content_type: r.u8()?,
            junk_score: r.u8()?,
            comp: r.bytes(MAX_COMP, "composition")?.to_vec(),
            data: r.bytes(MAX_ENRICHMENT, "enrichment")?.to_vec(),
        })
    } else {
        None
    };
    let torrent = Prepared {
        info_hash,
        info_hash_v2,
        name,
        name_raw,
        size,
        piece_len,
        piece_count,
        file_count,
        info_size,
        flags: torrent_flags,
        files: Compressed {
            codec,
            dict_id,
            data,
        },
        extra,
        raw,
        enrichment,
    };
    let key = if has(flag::KEY_IS_V2) {
        info_hash_v2
            .map(|h| h.truncated())
            .filter(|t| *t != info_hash)
            .ok_or(Error::Invalid("key"))?
    } else {
        info_hash
    };
    let rec = ResultRecord {
        key,
        token,
        torrent,
        sources,
        metadata_at,
        action,
        rule_version,
        fix: has(flag::FIX).then_some(has(flag::FIX_ADULT)),
        private_source: has(flag::PRIVATE_SOURCE),
        source_adult: has(flag::SOURCE_ADULT),
    };
    check_strings(&rec.torrent)?;
    Ok(rec)
}

/// Whether the record's key is the truncated v2 hash; an error when it's
/// neither swarm hash.
fn key_is_v2(r: &ResultRecord) -> Result<bool> {
    let t = &r.torrent;
    if r.key == t.info_hash {
        return Ok(false);
    }
    match t.info_hash_v2 {
        Some(v2) if v2.truncated() == r.key => Ok(true),
        _ => Err(Error::Invalid("key")),
    }
}

/// The rules for a record's fields, the same on both sides: what a local
/// `Preparer` could have produced, and what the store can hold.
fn check_strings(t: &Prepared) -> Result<()> {
    // SQLite integers: a value over `i64::MAX` would fail the
    // coordinator's whole write batch, not just this record.
    for v in [
        t.size,
        t.piece_len,
        t.piece_count,
        t.file_count,
        t.info_size,
    ] {
        if i64::try_from(v).is_err() {
            return Err(Error::Invalid("integer"));
        }
    }
    if t.flags & !DERIVED_FLAGS != 0 {
        return Err(Error::Invalid("torrent flags"));
    }
    let v2 = t.info_hash_v2.is_some();
    let hybrid = t.info_hash_v2.is_some_and(|h| h.truncated() != t.info_hash);
    if (t.flags & flags::V2 != 0) != v2 || (t.flags & flags::HYBRID != 0) != hybrid {
        return Err(Error::Invalid("torrent flags"));
    }
    if t.files.dict_id.is_some_and(|id| id < 0) {
        return Err(Error::Invalid("dictionary"));
    }
    let cap = |len: usize, max: usize, what| {
        if len > max {
            Err(Error::TooLarge(what))
        } else {
            Ok(())
        }
    };
    cap(t.name.len(), MAX_NAME, "name")?;
    if let Some((raw, enc)) = &t.name_raw {
        cap(raw.len(), MAX_NAME_RAW, "raw name")?;
        TextEncoding::from_id(*enc).ok_or(Error::Invalid("name encoding"))?;
    }
    cap(t.files.data.len(), MAX_BLOB, "file tree")?;
    // `Preparer` stores a blob that wouldn't shrink as is, with no
    // dictionary; a dictionary id belongs only to zstd.
    if t.files.codec == Codec::Raw && t.files.dict_id.is_some() {
        return Err(Error::Invalid("dictionary"));
    }
    if let Some(extra) = &t.extra {
        cap(extra.len(), MAX_EXTRA, "extra")?;
        // `Preparer` leaves out an empty set of unknown keys.
        if extra.as_slice() == b"de" {
            return Err(Error::Invalid("extra"));
        }
        canonical_without_pieces(extra, "extra")?;
    }
    if let Some(raw) = &t.raw {
        cap(raw.len(), MAX_BLOB, "raw info")?;
        canonical_without_pieces(raw, "raw info")?;
    }
    if let Some(e) = &t.enrichment {
        cap(e.comp.len(), MAX_COMP, "composition")?;
        cap(e.data.len(), MAX_ENRICHMENT, "enrichment")?;
        check_enrichment(e)?;
    }
    Ok(())
}

/// A bencoded dictionary in the canonical form `Preparer` writes, with no
/// `pieces` key: re-encoding it without `pieces` changes nothing.
fn canonical_without_pieces(b: &[u8], what: &'static str) -> Result<()> {
    match info_without_pieces(b) {
        Ok(stripped) if stripped == b => Ok(()),
        _ => Err(Error::Invalid(what)),
    }
}

/// The parsed and classified fields. The hot columns are always a known
/// content type and a junk score of at most 100. This build's enrichment
/// must decode and agree with its hot columns; another version (a worker
/// on an older or newer parser) is kept as sent, since the store marks
/// any `parser_ver` but its own for enriching again in the background.
fn check_enrichment(e: &Enriched) -> Result<()> {
    if e.version <= 0 {
        return Err(Error::Invalid("enrichment version"));
    }
    ContentType::from_code(e.content_type).ok_or(Error::Invalid("content type"))?;
    if e.junk_score > 100 {
        return Err(Error::Invalid("junk score"));
    }
    if e.version != ENRICHMENT_VERSION {
        return Ok(());
    }
    Composition::decode(&e.comp).map_err(|_| Error::Invalid("composition"))?;
    let record = Record::decode(&e.data).map_err(|_| Error::Invalid("enrichment"))?;
    if record.content_type.code() != e.content_type || record.junk.score != e.junk_score {
        return Err(Error::Invalid("enrichment"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bare(key: u8) -> ResultRecord {
        ResultRecord {
            key: InfoHash([key; 20]),
            token: Token(0),
            torrent: Prepared {
                info_hash: InfoHash([key; 20]),
                info_hash_v2: None,
                name: String::new(),
                name_raw: None,
                size: 0,
                piece_len: 0,
                piece_count: 0,
                file_count: 0,
                info_size: 0,
                flags: 0,
                files: Compressed {
                    codec: Codec::Raw,
                    dict_id: None,
                    data: Vec::new(),
                },
                extra: None,
                raw: None,
                enrichment: None,
            },
            sources: 0,
            metadata_at: 0,
            action: None,
            rule_version: 0,
            fix: None,
            private_source: false,
            source_adult: false,
        }
    }

    #[test]
    fn the_smallest_record_is_min_record_bytes() {
        assert_eq!(bare(1).encoded_len().unwrap(), MIN_RECORD);
    }

    #[test]
    fn a_hybrid_leased_under_its_v2_hash_keeps_that_key() {
        let mut r = bare(1);
        let v2 = InfoHashV2([9; 32]);
        r.torrent.info_hash_v2 = Some(v2);
        r.torrent.flags = flags::V2 | flags::HYBRID;
        r.key = v2.truncated();
        let b = ResultsBatch {
            batch: 7,
            records: vec![r.clone()],
        };
        let back = ResultsBatch::decode(&b.encode().unwrap()).unwrap();
        assert_eq!(back.records[0], r);
        // A key that's neither swarm hash is refused on both sides.
        r.key = InfoHash([3; 20]);
        let b = ResultsBatch {
            batch: 7,
            records: vec![r],
        };
        assert_eq!(b.encode(), Err(Error::Invalid("key")));
    }

    #[test]
    fn a_key_twice_is_refused() {
        let b = ResultsBatch {
            batch: 1,
            records: vec![bare(1), bare(1)],
        };
        assert_eq!(b.encode(), Err(Error::Invalid("duplicate key")));
    }
}
