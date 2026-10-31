//! The Bitmagnet PostgreSQL adapter (IMP-3, `docs/decisions/2026-10-30-IMP-3.md`):
//! reads a stopped Bitmagnet database read-only, in keyset batches, into
//! IMP-1 import records, the way IMP-2's Magnetico adapter does.
//!
//! Everything in the source is untrusted. Names and paths are read as raw
//! bytes (`convert_to(…, 'SQL_ASCII')`, which converts nothing; IMP-0) and
//! normalised by [`Normaliser`], which has no I/O. Nothing from a row
//! reaches SQL text, an error message or a log: errors and findings are
//! fixed enums. [`import`] is the legacy `import-bitmagnet` path, now on
//! top of the adapter, until IMP-4 replaces the CLI.

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use lodestar_metainfo::{
    File, FileAttrs, FileHashes, Info, InfoHash, MetaVersion, Text, TextEncoding,
};
use lodestar_store::import::{
    Completeness, Decision, ImportCandidate, ImportTorrent, Privacy, Reason, Verification, repair,
};
use lodestar_store::{NewTorrent, Prepared, SourceId, Store, flags};
use sha2::{Digest, Sha256};
use tokio_postgres::{Client, IsolationLevel, Statement};

mod conn;
pub use conn::{PASSWORD_ENV, Password, Settings, Tls, password_file_warning};

/// The adapter name in the source identity, and the legacy plugin id.
pub const ADAPTER: &str = "bitmagnet";
pub const PLUGIN_ID: &str = ADAPTER;
/// Bumped when the mapping of a row to a record changes.
pub const ADAPTER_VERSION: i64 = 1;

/// Parents per batch (plan §9).
pub const MAX_PARENTS: usize = 500;
/// Memory per batch, and per parent (plan §9).
pub const MAX_BATCH_BYTES: usize = 8 << 20;
/// Earliest plausible `created_at`: 2008-01-01.
pub const MIN_DATE: i64 = 1_199_145_600;
/// How far after the batch time a date may lie (clock skew).
pub const FUTURE_SLACK: i64 = 86_400;
/// The longest `info_hash` the keyset can carry in a position. Bitmagnet's
/// are 20 bytes; a longer one is only read as a key, never imported.
pub const MAX_KEY: usize = 500;

const LIMITS: lodestar_metainfo::Limits = lodestar_metainfo::Limits::DEFAULT;
const POSITION_VERSION: u8 = 1;
/// File rows per round trip.
const FILE_CHUNK: i32 = 1_000;
/// Memory a kept value costs beyond its bytes: per row and per path
/// segment (IMP-2).
const ROW_OVERHEAD: usize = 64;
const SEGMENT_OVERHEAD: usize = 24;
/// A batch may take this many statement timeouts in all.
const BATCH_STEPS: u32 = 10;

const _: () = assert!(LIMITS.max_name_bytes == 4096 && LIMITS.max_path_bytes == 4096);

/// Why a source can't be read. Bounded labels only: no names, hashes,
/// paths, row content or server text, so it's safe for logs and metrics.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// A setting is malformed: an empty or over-long name, port 0, a host
    /// that isn't a name, address or socket directory.
    BadSettings,
    /// Plaintext to a host that isn't loopback.
    TlsRequired,
    /// The database couldn't be reached or read. The `&str` is a fixed
    /// label.
    Unavailable(&'static str),
    /// Something may be writing the source: a write lock on a table it
    /// reads, write counters that moved, a standby, or counters that are
    /// off.
    NotQuiescent,
    /// Required tables, columns or indexes are missing, named from this
    /// adapter's own list.
    UnsupportedSchema(Vec<&'static str>),
    /// A statement ran past its deadline.
    StatementTimeout,
    /// The caller cancelled.
    Cancelled,
    /// The source changed since it was opened.
    SourceChanged,
    /// A resume position this adapter didn't write.
    BadPosition,
    /// An `info_hash` over [`MAX_KEY`] bytes: the keyset can't step past it.
    BadKey,
}

impl Refusal {
    /// The stable reason label.
    pub fn label(&self) -> &'static str {
        match self {
            Refusal::BadSettings => "bad_settings",
            Refusal::TlsRequired => "tls_required",
            Refusal::Unavailable(_) => "source_unavailable",
            Refusal::NotQuiescent => "source_not_quiescent",
            Refusal::UnsupportedSchema(_) => "unsupported_schema",
            Refusal::StatementTimeout => "statement_timeout",
            Refusal::Cancelled => "cancelled",
            Refusal::SourceChanged => "source_changed",
            Refusal::BadPosition => "bad_position",
            Refusal::BadKey => "bad_key",
        }
    }

    /// The IMP-1 reason a running job blocks with, or `None` when it
    /// should fail (bad settings, an unsupported source) or the caller
    /// stopped it.
    pub fn job_reason(&self) -> Option<Reason> {
        match self {
            Refusal::SourceChanged => Some(Reason::SourceChanged),
            Refusal::Unavailable("auth_failed") => Some(Reason::NeedsCredentials),
            Refusal::Unavailable(_) | Refusal::NotQuiescent | Refusal::StatementTimeout => {
                Some(Reason::SourceUnavailable)
            }
            Refusal::BadSettings
            | Refusal::TlsRequired
            | Refusal::UnsupportedSchema(_)
            | Refusal::BadPosition
            | Refusal::BadKey
            | Refusal::Cancelled => None,
        }
    }
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Refusal::BadSettings => f.write_str("the connection settings aren't valid"),
            Refusal::TlsRequired => f.write_str(
                "plaintext is allowed only to a Unix socket or a loopback address: use TLS for \
                 any other host",
            ),
            Refusal::Unavailable(why) => {
                write!(f, "the database can't be reached or read ({why})")
            }
            Refusal::NotQuiescent => f.write_str(
                "the database may be changing (something holds a write lock, it was written \
                 since the import began, or it's a standby): stop Bitmagnet, or import a \
                 restored copy",
            ),
            Refusal::UnsupportedSchema(missing) => write!(
                f,
                "not a supported Bitmagnet database ({})",
                missing.join(", ")
            ),
            Refusal::StatementTimeout => f.write_str("a read took too long"),
            Refusal::Cancelled => f.write_str("cancelled"),
            Refusal::SourceChanged => f.write_str("the database changed since the import began"),
            Refusal::BadPosition => f.write_str("the resume position isn't this adapter's"),
            Refusal::BadKey => f.write_str("an info_hash is too long to page past"),
        }
    }
}

impl std::error::Error for Refusal {}

/// A per-record finding, counted per batch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Finding {
    BadHash,
    NameLossy,
    PathLossy,
    NameOverLimit,
    FilesOverLimit,
    FileMalformed,
    NoFiles,
    SizeMismatch,
    DateOutOfRange,
    MissingName,
    MissingSize,
    /// Indexes with gaps, or fewer rows than `files_count`.
    FilesIncomplete,
    /// `files_status` is `no_info`, unknown, or NULL.
    NoInfo,
    /// `files_status` is `over_threshold`: Bitmagnet kept no full list.
    OverThreshold,
    /// `single` with file rows.
    StatusMismatch,
    /// Piece fields that don't fit the size; not imported.
    PiecesMalformed,
}

impl Finding {
    pub const ALL: [Finding; 16] = [
        Finding::BadHash,
        Finding::NameLossy,
        Finding::PathLossy,
        Finding::NameOverLimit,
        Finding::FilesOverLimit,
        Finding::FileMalformed,
        Finding::NoFiles,
        Finding::SizeMismatch,
        Finding::DateOutOfRange,
        Finding::MissingName,
        Finding::MissingSize,
        Finding::FilesIncomplete,
        Finding::NoInfo,
        Finding::OverThreshold,
        Finding::StatusMismatch,
        Finding::PiecesMalformed,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Finding::BadHash => "bad_hash",
            Finding::NameLossy => "name_lossy",
            Finding::PathLossy => "path_lossy",
            Finding::NameOverLimit => "name_over_limit",
            Finding::FilesOverLimit => "files_over_limit",
            Finding::FileMalformed => "file_malformed",
            Finding::NoFiles => "no_files",
            Finding::SizeMismatch => "size_mismatch",
            Finding::DateOutOfRange => "date_out_of_range",
            Finding::MissingName => "missing_name",
            Finding::MissingSize => "missing_size",
            Finding::FilesIncomplete => "files_incomplete",
            Finding::NoInfo => "no_info",
            Finding::OverThreshold => "over_threshold",
            Finding::StatusMismatch => "status_mismatch",
            Finding::PiecesMalformed => "pieces_malformed",
        }
    }
}

/// Counts per [`Finding`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Findings([u64; Finding::ALL.len()]);

impl Findings {
    pub fn add(&mut self, f: Finding, n: u64) {
        self.0[f as usize] += n;
    }

    pub fn get(&self, f: Finding) -> u64 {
        self.0[f as usize]
    }

    pub fn merge(&mut self, other: &Findings) {
        for (a, b) in self.0.iter_mut().zip(other.0) {
            *a += b;
        }
    }

    /// Non-zero counts, by label.
    pub fn iter(&self) -> impl Iterator<Item = (&'static str, u64)> + '_ {
        Finding::ALL
            .iter()
            .map(|f| (f.label(), self.get(*f)))
            .filter(|(_, n)| *n > 0)
    }
}

/// A text or bytea value as read: its bytes, or only its length when the
/// reader's SQL withheld it for being over the limit.
#[derive(Clone, PartialEq, Eq)]
pub enum Bytes {
    Null,
    Value(Vec<u8>),
    TooLong(u64),
}

// Hand-written so a value never prints its content.
impl fmt::Debug for Bytes {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Bytes::Null => f.write_str("Null"),
            Bytes::Value(b) => write!(f, "Value({} bytes)", b.len()),
            Bytes::TooLong(n) => write!(f, "TooLong({n} bytes)"),
        }
    }
}

impl Bytes {
    /// From a guarded column and its `octet_length`.
    fn guarded(v: Option<Vec<u8>>, len: Option<i32>) -> Bytes {
        match (v, len) {
            (Some(v), _) => Bytes::Value(v),
            (None, Some(n)) => Bytes::TooLong(n.max(0) as u64),
            (None, None) => Bytes::Null,
        }
    }

    fn len(&self) -> usize {
        match self {
            Bytes::Value(b) => b.len(),
            _ => 0,
        }
    }
}

/// Bitmagnet's `files_status`, or the legacy `single_file` read as one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Single,
    Multi,
    OverThreshold,
    /// `no_info`, NULL, or a label this adapter doesn't know.
    NoInfo,
}

impl Status {
    /// From the reader's `CASE`: 1 single, 2 multi, 3 over_threshold.
    fn parse(v: Option<i32>) -> Status {
        match v {
            Some(1) => Status::Single,
            Some(2) => Status::Multi,
            Some(3) => Status::OverThreshold,
            _ => Status::NoInfo,
        }
    }
}

/// One `torrents` row as read.
#[derive(Clone, PartialEq, Eq)]
pub struct RawTorrent {
    /// `info_hash`: the keyset key, and the hash when it's 20 bytes.
    pub key: Vec<u8>,
    pub name: Bytes,
    pub size: Option<i64>,
    pub private: Option<bool>,
    pub status: Status,
    pub files_count: Option<i32>,
    pub piece_length: Option<i64>,
    /// `octet_length(pieces)`: the pieces themselves are never read.
    pub pieces_len: Option<i32>,
    /// `created_at`, Unix seconds; NULL for ±infinity.
    pub created_at: Option<i64>,
}

/// One `torrent_files` row as read.
#[derive(Clone, PartialEq, Eq)]
pub struct RawFile {
    pub index: Option<i32>,
    pub path: Bytes,
    pub size: Option<i64>,
}

// No key, size or date: none of the row's values are printed.
impl fmt::Debug for RawTorrent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RawTorrent")
            .field("key_len", &self.key.len())
            .field("name", &self.name)
            .field("status", &self.status)
            .finish_non_exhaustive()
    }
}

impl fmt::Debug for RawFile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RawFile")
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}

/// One file of a normalised torrent: path segments as raw bytes.
#[derive(Clone, PartialEq, Eq)]
pub struct FileEntry {
    pub path: Vec<Vec<u8>>,
    pub size: u64,
}

impl fmt::Debug for FileEntry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FileEntry")
            .field("segments", &self.path.len())
            .finish_non_exhaustive()
    }
}

/// A torrent whose name and size fill a canonical row.
#[derive(Clone, PartialEq, Eq)]
pub struct TorrentRecord {
    pub hash: [u8; 20],
    /// The original bytes. `Store::prepare` keeps them in `name_raw`
    /// whenever they aren't valid UTF-8.
    pub name: Vec<u8>,
    pub size: u64,
    pub private: bool,
    pub source_at: Option<i64>,
    /// In `index` order, which is the info dict's order.
    pub files: Vec<FileEntry>,
    /// Bitmagnet said `single` and kept no file rows.
    pub single_file: bool,
    pub piece_len: u64,
    pub piece_count: u64,
    pub completeness: Completeness,
    /// [`repair`] bits.
    pub repair: i64,
    pub files_dropped: u32,
}

impl fmt::Debug for TorrentRecord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TorrentRecord")
            .field("name_len", &self.name.len())
            .field("files", &self.files.len())
            .field("single_file", &self.single_file)
            .field("completeness", &self.completeness)
            .field("repair", &self.repair)
            .field("files_dropped", &self.files_dropped)
            .finish_non_exhaustive()
    }
}

/// A valid hash too incomplete for a canonical row.
#[derive(Clone, PartialEq, Eq)]
pub struct CandidateRecord {
    pub hash: [u8; 20],
    pub name: Option<Vec<u8>>,
    pub size: Option<u64>,
    pub private: bool,
    pub source_at: Option<i64>,
}

impl fmt::Debug for CandidateRecord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CandidateRecord")
            .field("name_len", &self.name.as_ref().map(Vec::len))
            .field("has_size", &self.size.is_some())
            .finish_non_exhaustive()
    }
}

/// One normalised parent row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Record {
    Torrent(TorrentRecord),
    Candidate(CandidateRecord),
    /// Rejected, with the finding that says why.
    Invalid(Finding),
}

fn privacy(private: bool) -> Privacy {
    // `private = false` isn't a verified claim: it stays unknown, so
    // IMP-1's sticky-private merge decides.
    if private {
        Privacy::Private
    } else {
        Privacy::Unknown
    }
}

impl TorrentRecord {
    /// The record as an unverified [`Info`] for `Store::prepare`.
    pub fn info(&self) -> Info {
        Info {
            v1: Some(InfoHash(self.hash)),
            v2: None,
            version: MetaVersion::V1,
            name: Text {
                raw: self.name.clone(),
                encoding: TextEncoding::UTF8,
            },
            piece_length: self.piece_len,
            piece_count: self.piece_count,
            total_size: self.size,
            private: self.private,
            source: None,
            single_file: self.single_file,
            files: self
                .files
                .iter()
                .map(|f| File {
                    path: f.path.clone(),
                    size: f.size,
                    attrs: FileAttrs::empty(),
                    symlink_target: None,
                    pieces_root: None,
                    hashes: FileHashes::default(),
                })
                .collect(),
            path_encoding: TextEncoding::UTF8,
            extra: None,
            info_size: 0,
            warnings: Vec::new(),
        }
    }

    /// The IMP-1 record: never verified because Bitmagnet once fetched it.
    pub fn import<'a>(&self, prepared: &'a Prepared, decision: Decision<'a>) -> ImportTorrent<'a> {
        ImportTorrent {
            torrent: prepared,
            completeness: self.completeness,
            verification: Verification::Unverified,
            privacy: privacy(self.private),
            repair: self.repair,
            source_at: self.source_at,
            files_dropped: self.files_dropped,
            decision,
        }
    }
}

impl CandidateRecord {
    pub fn import<'a>(&'a self, decision: Decision<'a>) -> ImportCandidate<'a> {
        ImportCandidate {
            hash: InfoHash(self.hash),
            hash_v2: None,
            name: self.name.as_deref(),
            size: self.size,
            privacy: privacy(self.private),
            source_at: self.source_at,
            decision,
        }
    }
}

/// Turns raw rows into records. Pure: no I/O, so it's property-tested.
#[derive(Debug, Clone, Copy)]
pub struct Normaliser {
    /// The batch time, for the date range check.
    pub now: i64,
}

impl Normaliser {
    /// Normalises one parent and its file rows, in `index` order.
    /// `overflow` is how many further file rows the reader didn't keep
    /// because the parent was over its budget.
    pub fn torrent(
        &self,
        row: &RawTorrent,
        files: &[RawFile],
        overflow: u32,
        findings: &mut Findings,
    ) -> Record {
        let Some(hash) = <[u8; 20]>::try_from(row.key.as_slice())
            .ok()
            .filter(|h| h.iter().any(|b| *b != 0))
        else {
            findings.add(Finding::BadHash, 1);
            return Record::Invalid(Finding::BadHash);
        };
        let source_at = match row.created_at {
            Some(d) if (MIN_DATE..=self.now.saturating_add(FUTURE_SLACK)).contains(&d) => Some(d),
            _ => {
                findings.add(Finding::DateOutOfRange, 1);
                None
            }
        };
        let name = match &row.name {
            Bytes::TooLong(_) => {
                findings.add(Finding::NameOverLimit, 1);
                None
            }
            Bytes::Value(n) if n.len() > LIMITS.max_name_bytes => {
                findings.add(Finding::NameOverLimit, 1);
                None
            }
            Bytes::Value(n) if !n.is_empty() => {
                if std::str::from_utf8(n).is_err() {
                    findings.add(Finding::NameLossy, 1);
                }
                Some(n.clone())
            }
            _ => {
                findings.add(Finding::MissingName, 1);
                None
            }
        };
        let size = match row.size {
            Some(s) if s > 0 => Some(s as u64),
            _ => {
                findings.add(Finding::MissingSize, 1);
                None
            }
        };
        let private = row.private == Some(true);
        let (Some(name), Some(size)) = (name.clone(), size) else {
            return Record::Candidate(CandidateRecord {
                hash,
                name,
                size,
                private,
                source_at,
            });
        };
        let (piece_len, piece_count) = pieces(row, size, findings);

        if row.status == Status::Single && files.is_empty() && overflow == 0 {
            return Record::Torrent(TorrentRecord {
                hash,
                files: vec![FileEntry {
                    path: vec![name.clone()],
                    size,
                }],
                name,
                size,
                private,
                source_at,
                single_file: true,
                piece_len,
                piece_count,
                completeness: Completeness::ReportedComplete,
                repair: 0,
                files_dropped: 0,
            });
        }

        let mut kept = Vec::with_capacity(files.len().min(LIMITS.max_files));
        let mut dropped = overflow;
        let mut sum = Some(0u64);
        // Indexes 0, 1, 2, … with no gap: the whole list, in order.
        let mut contiguous = true;
        for f in files {
            if kept.len() >= LIMITS.max_files {
                dropped += 1;
                continue;
            }
            match file(f, findings) {
                Some(e) => {
                    contiguous &= f.index == i32::try_from(kept.len()).ok();
                    sum = sum.and_then(|s| s.checked_add(e.size));
                    kept.push(e);
                }
                None => {
                    findings.add(Finding::FileMalformed, 1);
                    dropped += 1;
                }
            }
        }
        if overflow > 0 || files.len() > LIMITS.max_files {
            findings.add(Finding::FilesOverLimit, 1);
        }
        let mut repair = 0;
        if files.is_empty() && overflow == 0 {
            findings.add(Finding::NoFiles, 1);
            repair |= repair::MISSING_FILES;
        } else if kept.is_empty() {
            repair |= repair::MISSING_FILES | repair::INCONSISTENT_FILES;
        } else if dropped > 0 {
            repair |= repair::INCONSISTENT_FILES;
        }
        match row.status {
            Status::Multi => {}
            Status::Single => {
                findings.add(Finding::StatusMismatch, 1);
                repair |= repair::INCONSISTENT_FILES;
            }
            // Bitmagnet kept no full list: whatever rows there are, the
            // record can't be complete. With none, MISSING_FILES is set.
            Status::OverThreshold | Status::NoInfo => {
                findings.add(
                    if row.status == Status::OverThreshold {
                        Finding::OverThreshold
                    } else {
                        Finding::NoInfo
                    },
                    1,
                );
                if !kept.is_empty() {
                    repair |= repair::INCONSISTENT_FILES;
                }
            }
        }
        if repair == 0
            && (!contiguous
                || row
                    .files_count
                    .is_some_and(|n| i64::from(n) != kept.len() as i64))
        {
            findings.add(Finding::FilesIncomplete, 1);
            repair |= repair::INCONSISTENT_FILES;
        }
        if repair == 0 && sum != Some(size) {
            findings.add(Finding::SizeMismatch, 1);
            repair |= repair::INCONSISTENT_FILES;
        }
        Record::Torrent(TorrentRecord {
            hash,
            name,
            size,
            private,
            source_at,
            files: kept,
            single_file: false,
            piece_len,
            piece_count,
            completeness: if repair == 0 {
                Completeness::ReportedComplete
            } else {
                Completeness::Partial
            },
            repair,
            files_dropped: dropped,
        })
    }
}

/// Piece length and count when they fit the size; else `(0, 0)`.
fn pieces(row: &RawTorrent, size: u64, findings: &mut Findings) -> (u64, u64) {
    let (len, bytes) = match (row.piece_length, row.pieces_len) {
        (None, None) => return (0, 0),
        (Some(l), Some(b)) => (l, b),
        _ => (0, 0),
    };
    let ok = len > 0 && bytes > 0 && bytes % 20 == 0 && {
        let count = (bytes / 20) as u64;
        size.div_ceil(len as u64) == count
    };
    if ok {
        (len as u64, (bytes / 20) as u64)
    } else {
        findings.add(Finding::PiecesMalformed, 1);
        (0, 0)
    }
}

/// A well-formed file row, or `None`.
fn file(f: &RawFile, findings: &mut Findings) -> Option<FileEntry> {
    f.index.filter(|i| *i >= 0)?;
    let size = u64::try_from(f.size?).ok()?;
    let Bytes::Value(path) = &f.path else {
        return None;
    };
    if path.is_empty() || path.len() > LIMITS.max_path_bytes || path.contains(&0) {
        return None;
    }
    let segments: Vec<Vec<u8>> = path.split(|b| *b == b'/').map(<[u8]>::to_vec).collect();
    if segments.len() > LIMITS.max_path_depth || segments.iter().any(Vec::is_empty) {
        return None;
    }
    if std::str::from_utf8(path).is_err() {
        findings.add(Finding::PathLossy, 1);
    }
    Some(FileEntry {
        path: segments,
        size,
    })
}

/// The source identity IMP-1 binds a job and its cursor to.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Identity {
    pub dataset: [u8; 32],
    pub schema_sig: [u8; 32],
}

impl fmt::Debug for Identity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Identity { .. }")
    }
}

/// One source batch, ready to commit as one IMP-1 batch.
pub struct Batch {
    pub records: Vec<Record>,
    /// Where to resume after this batch: IMP-1's opaque cursor.
    pub position: Vec<u8>,
    pub source_bytes: u64,
    pub findings: Findings,
}

// The position holds a hash, so only its length is printed.
impl fmt::Debug for Batch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Batch")
            .field("records", &self.records.len())
            .field("position_len", &self.position.len())
            .field("source_bytes", &self.source_bytes)
            .field("findings", &self.findings)
            .finish()
    }
}

/// The cursor after the parent whose `info_hash` is `key`.
pub fn position(key: &[u8]) -> Vec<u8> {
    let mut p = Vec::with_capacity(key.len() + 1);
    p.push(POSITION_VERSION);
    p.extend_from_slice(key);
    p
}

/// The last parent key a [`position`] names, or `None` for the empty
/// position, the start of the source.
pub fn parse_position(p: &[u8]) -> Result<Option<&[u8]>, Refusal> {
    match p {
        [] => Ok(None),
        [POSITION_VERSION, rest @ ..] if rest.len() <= MAX_KEY => Ok(Some(rest)),
        _ => Err(Refusal::BadPosition),
    }
}

/// What the schema offers, by columns (plan §5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Caps {
    /// `files_status` (0.2+), else the legacy `single_file`.
    pub files_status: bool,
    /// `torrents.files_count` (0.10).
    pub files_count: bool,
    pub pieces: Pieces,
}

/// Where piece data lives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pieces {
    /// `torrent_pieces` (0.7+), joined by a unique `info_hash`.
    Table,
    /// `torrents.piece_length` and `torrents.pieces` (before 0.7).
    Columns,
    None,
}

/// The tables read, in the order their OIDs are kept.
const TABLES: [&str; 3] = ["torrents", "torrent_files", "torrent_pieces"];

/// Required columns and their `typname`.
const REQUIRED: [(&str, &str, &str); 9] = [
    ("torrents", "info_hash", "bytea"),
    ("torrents", "name", "text"),
    ("torrents", "size", "int8"),
    ("torrents", "private", "bool"),
    ("torrents", "created_at", "timestamptz"),
    ("torrent_files", "info_hash", "bytea"),
    ("torrent_files", "index", "int4"),
    ("torrent_files", "path", "text"),
    ("torrent_files", "size", "int8"),
];

/// Optional columns, each a capability.
const OPTIONAL: [(&str, &str, &str); 7] = [
    ("torrents", "files_status", "FilesStatus"),
    ("torrents", "single_file", "bool"),
    ("torrents", "files_count", "int4"),
    ("torrents", "piece_length", "int8"),
    ("torrents", "pieces", "bytea"),
    ("torrent_pieces", "info_hash", "bytea"),
    ("torrent_pieces", "piece_length", "int8"),
];

const COLUMNS_SQL: &str = "\
SELECT convert_to(c.relname::text, 'SQL_ASCII'), convert_to(a.attname::text, 'SQL_ASCII'), \
       convert_to(t.typname::text, 'SQL_ASCII'), a.attnum, a.attnotnull, c.relkind::text = 'r', \
       c.oid, pg_catalog.pg_relation_filenode(c.oid), \
       t.typnamespace = 'pg_catalog'::pg_catalog.regnamespace, t.typtype::text = 'e', \
       a.atttypid \
FROM pg_catalog.pg_class c \
JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
JOIN pg_catalog.pg_attribute a ON a.attrelid = c.oid AND a.attnum > 0 AND NOT a.attisdropped \
JOIN pg_catalog.pg_type t ON t.oid = a.atttypid \
WHERE n.nspname = 'public' AND c.relname IN ('torrents', 'torrent_files', 'torrent_pieces') \
ORDER BY c.relname, a.attnum LIMIT 2000";

/// Valid, non-partial btree indexes on plain columns of the read tables,
/// with built-in operator classes only: a source's own class could order
/// `bytea` differently and make the keyset skip rows.
const INDEXES_SQL: &str = "\
SELECT i.indrelid, i.indkey::int2[], i.indisunique \
FROM pg_catalog.pg_index i \
JOIN pg_catalog.pg_class ic ON ic.oid = i.indexrelid \
JOIN pg_catalog.pg_am am ON am.oid = ic.relam \
WHERE i.indrelid = ANY($1) AND am.amname = 'btree' AND i.indisvalid AND i.indisready \
  AND i.indpred IS NULL AND i.indexprs IS NULL \
  AND NOT EXISTS (SELECT 1 FROM pg_catalog.unnest(i.indclass::pg_catalog.oid[]) u(oc) \
                  JOIN pg_catalog.pg_opclass o ON o.oid = u.oc \
                  WHERE o.opcnamespace <> 'pg_catalog'::pg_catalog.regnamespace) \
ORDER BY i.indexrelid LIMIT 1000";

/// Casts to or from the source's own enum.
const CASTS_SQL: &str = "\
SELECT count(*) FROM pg_catalog.pg_cast c \
WHERE c.castsource = ANY($1) OR c.casttarget = ANY($1)";

/// The database's own identity: its OID and name.
const DATABASE_SQL: &str = "\
SELECT d.oid, convert_to(d.datname::text, 'SQL_ASCII') \
FROM pg_catalog.pg_database d WHERE d.datname = pg_catalog.current_database()";

/// Quiescence: other sessions' write locks on the read tables (a prepared
/// transaction's `pid` is NULL, hence `IS DISTINCT FROM`), the tables'
/// cumulative write counters, whether this is a standby, whether the
/// counters are kept at all, the cluster's whole snapshot (`xmin:xmax:xip`),
/// and how many casts the source's enum has. Our reads assign no
/// transaction id, so the snapshot moves when any writer starts or ends,
/// including an older one committing late, where the counters can lag by
/// seconds; `xmax` alone misses a commit that finishes out of order.
const QUIET_SQL: &str = "\
SELECT (SELECT count(*) FROM pg_catalog.pg_locks l \
        WHERE l.database = $1 AND l.relation = ANY($2) \
          AND l.pid IS DISTINCT FROM pg_catalog.pg_backend_pid() \
          AND l.mode IN ('RowExclusiveLock', 'ShareRowExclusiveLock', 'ExclusiveLock', \
                         'AccessExclusiveLock')), \
       (SELECT coalesce(sum(pg_catalog.pg_stat_get_tuples_inserted(o) \
                          + pg_catalog.pg_stat_get_tuples_updated(o) \
                          + pg_catalog.pg_stat_get_tuples_deleted(o)), 0)::int8 \
        FROM pg_catalog.unnest($2::oid[]) o), \
       pg_catalog.pg_is_in_recovery(), \
       pg_catalog.current_setting('track_counts') = 'on', \
       pg_catalog.pg_current_snapshot()::text, \
       (SELECT count(*) FROM pg_catalog.pg_cast c \
        WHERE c.castsource = ANY($3) OR c.casttarget = ANY($3))";

/// The keyset's ends, hashed so a long key is never transferred.
const ENDS_SQL: &str = "\
SELECT pg_catalog.sha256((SELECT info_hash FROM public.torrents ORDER BY info_hash LIMIT 1)), \
       pg_catalog.sha256((SELECT info_hash FROM public.torrents ORDER BY info_hash DESC LIMIT 1))";

fn torrents_sql(c: &Caps, first: bool) -> String {
    let status = if c.files_status {
        // A schema-qualified function call, not `=`: it has one candidate,
        // the enum binds to `anyenum` and the labels go through enum_in,
        // so no cast is ever looked up, even one added after the open.
        // Casts on the enum are refused as well, at open and every batch.
        "CASE WHEN pg_catalog.enum_eq(t.files_status, 'single') THEN 1 \
              WHEN pg_catalog.enum_eq(t.files_status, 'multi') THEN 2 \
              WHEN pg_catalog.enum_eq(t.files_status, 'over_threshold') THEN 3 END"
    } else {
        "CASE t.single_file WHEN true THEN 1 WHEN false THEN 2 END"
    };
    let count = if c.files_count {
        "t.files_count"
    } else {
        "NULL::int4"
    };
    let (pieces, join) = match c.pieces {
        Pieces::Table => (
            "p.piece_length, octet_length(p.pieces)",
            " LEFT JOIN public.torrent_pieces p ON p.info_hash = t.info_hash",
        ),
        Pieces::Columns => ("t.piece_length, octet_length(t.pieces)", ""),
        Pieces::None => ("NULL::int8, NULL::int4", ""),
    };
    format!(
        "SELECT CASE WHEN octet_length(t.info_hash) <= {MAX_KEY} THEN t.info_hash END, \
                CASE WHEN octet_length(t.name) <= {name} THEN convert_to(t.name, 'SQL_ASCII') END, \
                octet_length(t.name), t.size, t.private, {status}, {count}, {pieces}, \
                CASE WHEN isfinite(t.created_at) \
                     THEN floor(extract(epoch FROM t.created_at))::int8 END \
         FROM public.torrents t{join} \
         WHERE t.info_hash {op} $1 ORDER BY t.info_hash LIMIT $2",
        name = LIMITS.max_name_bytes,
        op = if first { ">=" } else { ">" },
    )
}

fn files_sql(first: bool) -> String {
    format!(
        "SELECT CASE WHEN octet_length(f.info_hash) <= {MAX_KEY} THEN f.info_hash END, f.\"index\", \
                CASE WHEN octet_length(f.path) <= {path} THEN convert_to(f.path, 'SQL_ASCII') END, \
                octet_length(f.path), f.size \
         FROM public.torrent_files f \
         WHERE f.info_hash {op} $1 AND f.info_hash <= $2 ORDER BY f.info_hash, f.\"index\"",
        path = LIMITS.max_path_bytes,
        op = if first { ">=" } else { ">" },
    )
}

/// One read table's catalog identity: its OID and file node.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Rel {
    oid: u32,
    filenode: Option<u32>,
}

/// What [`schema`] found.
struct Schema {
    caps: Caps,
    /// The capability rows, for the signature.
    sig: [u8; 32],
    /// `torrents`, `torrent_files`, and `torrent_pieces` when it's read.
    rels: Vec<Rel>,
    /// The `FilesStatus` enum's OID, when the source has one.
    enums: Vec<u32>,
}

/// Reads the catalog and decides support by columns and indexes.
async fn schema(client: &impl tokio_postgres::GenericClient) -> Result<Schema, Refusal> {
    struct Col {
        table: Vec<u8>,
        name: Vec<u8>,
        typ: Vec<u8>,
        /// A built-in `pg_catalog` type, or an enum: a `public` domain or
        /// type named `bytea` or `FilesStatus` doesn't pass.
        trusted: bool,
        num: i16,
        notnull: bool,
        plain: bool,
        rel: Rel,
        type_oid: u32,
    }
    let rows = client.query(COLUMNS_SQL, &[]).await.map_err(map_err)?;
    let mut cols = Vec::with_capacity(rows.len());
    for r in &rows {
        cols.push(Col {
            table: r.try_get(0).map_err(map_err)?,
            name: r.try_get(1).map_err(map_err)?,
            typ: r.try_get(2).map_err(map_err)?,
            trusted: {
                let builtin: bool = r.try_get(8).map_err(map_err)?;
                let is_enum: bool = r.try_get(9).map_err(map_err)?;
                let typ: &[u8] = r.try_get(2).map_err(map_err)?;
                if typ == b"FilesStatus" {
                    is_enum
                } else {
                    builtin
                }
            },
            num: r.try_get(3).map_err(map_err)?,
            notnull: r.try_get(4).map_err(map_err)?,
            plain: r.try_get(5).map_err(map_err)?,
            rel: Rel {
                oid: r.try_get(6).map_err(map_err)?,
                filenode: r.try_get(7).map_err(map_err)?,
            },
            type_oid: r.try_get(10).map_err(map_err)?,
        });
    }
    let find = |t: &str, c: &str, ty: &str| {
        cols.iter().find(|x| {
            x.table == t.as_bytes() && x.name == c.as_bytes() && x.typ == ty.as_bytes() && x.trusted
        })
    };
    let mut missing: Vec<&'static str> = Vec::new();
    for t in &TABLES[..2] {
        if cols.iter().any(|x| x.table == t.as_bytes() && !x.plain) {
            missing.push(if *t == "torrents" {
                "table: torrents"
            } else {
                "table: torrent_files"
            });
        }
    }
    const NAMES: [&str; 9] = [
        "torrents.info_hash",
        "torrents.name",
        "torrents.size",
        "torrents.private",
        "torrents.created_at",
        "torrent_files.info_hash",
        "torrent_files.index",
        "torrent_files.path",
        "torrent_files.size",
    ];
    for ((t, c, ty), label) in REQUIRED.iter().zip(NAMES) {
        if find(t, c, ty).is_none() {
            missing.push(label);
        }
    }
    let has = |i: usize| {
        let (t, c, ty) = OPTIONAL[i];
        find(t, c, ty).is_some()
    };
    let files_status = has(0);
    if !files_status && !has(1) {
        missing.push("torrents.files_status");
    }
    if !missing.is_empty() {
        return Err(Refusal::UnsupportedSchema(missing));
    }
    // FilesStatus is the one type the source owns, so the only one it can
    // define a cast on. Casts are global: search_path doesn't stop one,
    // and an implicit cast's function would run as the importer's role,
    // even in a read-only transaction. So a source with any cast to or
    // from it is refused before a row is read, and again in every batch
    // (`quiet`).
    let enums: Vec<u32> = cols
        .iter()
        .filter(|x| files_status && x.table == b"torrents" && x.name == b"files_status")
        .map(|x| x.type_oid)
        .collect();
    if files_status {
        let casts: i64 = client
            .query_one(CASTS_SQL, &[&enums])
            .await
            .map_err(map_err)?
            .try_get(0)
            .map_err(map_err)?;
        if casts > 0 {
            return Err(cast_refusal());
        }
    }
    let attnum = |t: &str, c: &str| {
        cols.iter()
            .find(|x| x.table == t.as_bytes() && x.name == c.as_bytes())
            .map(|x| x.num)
    };
    let rel = |t: &str| cols.iter().find(|x| x.table == t.as_bytes()).map(|x| x.rel);
    let (Some(torrents), Some(files)) = (rel("torrents"), rel("torrent_files")) else {
        return Err(Refusal::UnsupportedSchema(vec!["table: torrents"]));
    };
    let pieces_rel = rel("torrent_pieces").filter(|_| {
        cols.iter().any(|x| {
            x.table == b"torrent_pieces" && x.name == b"pieces" && x.typ == b"bytea" && x.trusted
        }) && has(5)
            && has(6)
    });
    let oids: Vec<u32> = [Some(torrents), Some(files), pieces_rel]
        .iter()
        .flatten()
        .map(|r| r.oid)
        .collect();
    let idx = client.query(INDEXES_SQL, &[&oids]).await.map_err(map_err)?;
    let mut indexes: Vec<(u32, Vec<i16>, bool)> = Vec::with_capacity(idx.len());
    for r in &idx {
        indexes.push((
            r.try_get(0).map_err(map_err)?,
            r.try_get(1).map_err(map_err)?,
            r.try_get(2).map_err(map_err)?,
        ));
    }
    // The keyset needs a unique index on exactly `info_hash` (with NOT
    // NULL), so `> last` never skips a duplicate; the files read needs one
    // leading with (info_hash, index), so it streams in order with no sort.
    let unique_on = |rel: Rel, col: Option<i16>| {
        indexes
            .iter()
            .any(|(r, k, u)| *r == rel.oid && *u && k.len() == 1 && Some(k[0]) == col)
    };
    let hash_col = attnum("torrents", "info_hash");
    if !unique_on(torrents, hash_col)
        || !cols
            .iter()
            .any(|x| x.table == b"torrents" && x.name == b"info_hash" && x.notnull)
    {
        missing.push("unique_index: torrents.info_hash");
    }
    let (fh, fi) = (
        attnum("torrent_files", "info_hash"),
        attnum("torrent_files", "index"),
    );
    if !indexes
        .iter()
        .any(|(r, k, _)| *r == files.oid && k.len() >= 2 && Some(k[0]) == fh && Some(k[1]) == fi)
    {
        missing.push("index: torrent_files(info_hash, index)");
    }
    if !missing.is_empty() {
        return Err(Refusal::UnsupportedSchema(missing));
    }
    // torrent_pieces without a unique info_hash would duplicate rows in
    // the join, so it's read only with one; otherwise pieces are skipped.
    let pieces_rel = pieces_rel.filter(|p| unique_on(*p, attnum("torrent_pieces", "info_hash")));
    let pieces = if pieces_rel.is_some() {
        Pieces::Table
    } else if has(3)
        && cols.iter().any(|x| {
            x.table == b"torrents" && x.name == b"pieces" && x.typ == b"bytea" && x.trusted
        })
    {
        Pieces::Columns
    } else {
        Pieces::None
    };
    let caps = Caps {
        files_status,
        files_count: has(2),
        pieces,
    };
    let mut h = Sha256::new();
    h.update(b"lodestar.import.bitmagnet.schema.v1\0");
    for c in cols.iter().filter(|c| {
        REQUIRED
            .iter()
            .chain(OPTIONAL.iter())
            .any(|(t, n, _)| c.table == t.as_bytes() && c.name == n.as_bytes())
    }) {
        for part in [&c.table[..], &c.name, &c.typ] {
            h.update((part.len() as u32).to_be_bytes());
            h.update(part);
        }
        h.update([u8::from(c.notnull)]);
    }
    h.update(torrents_sql(&caps, false));
    h.update(files_sql(false));
    let mut rels = vec![torrents, files];
    rels.extend(pieces_rel.filter(|_| pieces == Pieces::Table));
    Ok(Schema {
        caps,
        sig: h.finalize().into(),
        rels,
        enums,
    })
}

fn cast_refusal() -> Refusal {
    Refusal::UnsupportedSchema(vec!["a cast on torrents.files_status (refused)"])
}

/// Maps a PostgreSQL error to a fixed refusal. The server's text is never
/// kept: it can quote the source's own bytes.
fn map_err(e: tokio_postgres::Error) -> Refusal {
    match e.as_db_error().map(|d| d.code().code()) {
        Some("57014") => Refusal::StatementTimeout,
        Some("55P03") => Refusal::NotQuiescent,
        Some("42501") => Refusal::Unavailable("permission_denied"),
        Some("42P01" | "42703" | "42883") => Refusal::SourceChanged,
        Some(_) => Refusal::Unavailable("read_failed"),
        None if e.is_closed() => Refusal::Unavailable("connection_lost"),
        None => Refusal::Unavailable("read_failed"),
    }
}

/// Column `i` of a row, or a fixed refusal.
fn col<'a, T: tokio_postgres::types::FromSql<'a>>(
    r: &'a tokio_postgres::Row,
    i: usize,
) -> Result<T, Refusal> {
    r.try_get(i).map_err(map_err)
}

/// A connected, checked Bitmagnet database.
pub struct Source {
    client: Client,
    caps: Caps,
    identity: Identity,
    db_oid: u32,
    /// The database name and the read tables' OIDs and file nodes, hashed;
    /// rechecked in every batch.
    fingerprint: [u8; 32],
    rels: Vec<Rel>,
    /// The read tables' write counters at open, rechecked in every batch.
    writes: i64,
    /// The cluster's snapshot at open, rechecked in every batch.
    snapshot: String,
    /// The `FilesStatus` enum's OID, whose casts every batch rechecks.
    enums: Vec<u32>,
    torrents_first: Statement,
    torrents_next: Statement,
    files_first: Statement,
    files_next: Statement,
    statement_timeout: Duration,
    cancel: Arc<AtomicBool>,
    max_parents: usize,
}

impl fmt::Debug for Source {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("bitmagnet::Source")
            .field("caps", &self.caps)
            .finish_non_exhaustive()
    }
}

/// A parent and the file rows kept for it.
struct Parent {
    row: RawTorrent,
    files: Vec<RawFile>,
    overflow: u32,
    /// Memory charged to the batch budget.
    bytes: usize,
    /// Bytes read from the source.
    source: usize,
}

impl Source {
    /// Connects, sets the session up, checks the schema and that the source
    /// is quiescent, and computes its identity.
    pub async fn open(settings: &Settings) -> Result<Source, Refusal> {
        let client = conn::connect(settings).await?;
        let limit = settings.statement_timeout * BATCH_STEPS;
        tokio::time::timeout(limit, Self::open_on(client, settings.statement_timeout))
            .await
            .map_err(|_| Refusal::StatementTimeout)?
    }

    async fn open_on(mut client: Client, statement_timeout: Duration) -> Result<Source, Refusal> {
        let tx = client
            .build_transaction()
            .isolation_level(IsolationLevel::RepeatableRead)
            .read_only(true)
            .start()
            .await
            .map_err(map_err)?;
        let schema = schema(&tx).await?;
        let db = tx.query_one(DATABASE_SQL, &[]).await.map_err(map_err)?;
        let db_oid: u32 = db.try_get(0).map_err(map_err)?;
        let db_name: Vec<u8> = db.try_get(1).map_err(map_err)?;
        let fingerprint = fingerprint(db_oid, &db_name, &schema.rels);
        let oids: Vec<u32> = schema.rels.iter().map(|r| r.oid).collect();
        let (writes, snapshot) = quiet(&tx, db_oid, &oids, &schema.enums, None).await?;
        let ends = tx.query_one(ENDS_SQL, &[]).await.map_err(map_err)?;
        let (lo, hi): (Option<Vec<u8>>, Option<Vec<u8>>) = (
            ends.try_get(0).map_err(map_err)?,
            ends.try_get(1).map_err(map_err)?,
        );
        let mut h = Sha256::new();
        h.update(b"lodestar.import.bitmagnet.dataset.v1\0");
        h.update(fingerprint);
        h.update(writes.to_be_bytes());
        for end in [lo, hi] {
            h.update(end.unwrap_or_default());
            h.update([0]);
        }
        let identity = Identity {
            dataset: h.finalize().into(),
            schema_sig: schema.sig,
        };
        tx.commit().await.map_err(map_err)?;
        let caps = schema.caps;
        let prep = |sql: String| {
            let client = &client;
            async move { client.prepare(&sql).await.map_err(map_err) }
        };
        Ok(Source {
            torrents_first: prep(torrents_sql(&caps, true)).await?,
            torrents_next: prep(torrents_sql(&caps, false)).await?,
            files_first: prep(files_sql(true)).await?,
            files_next: prep(files_sql(false)).await?,
            caps,
            identity,
            db_oid,
            fingerprint,
            rels: schema.rels,
            writes,
            snapshot,
            enums: schema.enums,
            statement_timeout,
            cancel: Arc::new(AtomicBool::new(false)),
            max_parents: MAX_PARENTS,
            client,
        })
    }

    /// The identity to create or revalidate the job with.
    pub fn identity(&self) -> Identity {
        self.identity
    }

    /// What the schema offers.
    pub fn caps(&self) -> Caps {
        self.caps
    }

    /// A handle that stops the import before its next round trip.
    pub fn canceller(&self) -> Arc<AtomicBool> {
        self.cancel.clone()
    }

    /// PostgreSQL's own estimate of the `torrents` rows (`reltuples`, as of
    /// the last `ANALYZE`): what `inspect` estimates from, with no
    /// `count(*)` scan. `None` when the table was never analysed.
    pub async fn estimate_rows(&self) -> Result<Option<u64>, Refusal> {
        let row = self
            .client
            .query_one(
                "SELECT c.reltuples::float8 FROM pg_catalog.pg_class c \
                 WHERE c.oid = 'public.torrents'::pg_catalog.regclass",
                &[],
            )
            .await
            .map_err(map_err)?;
        let n: f64 = row.get(0);
        Ok((n >= 0.0).then_some(n as u64))
    }

    /// Reads the next batch after `position` (empty to start). `None` when
    /// the source is exhausted. `now` is the batch time.
    ///
    /// Each batch is one read-only, repeatable-read transaction: the
    /// identity and quiescence checks and both reads see one snapshot, and
    /// no transaction stays open between batches.
    pub async fn read_batch(
        &mut self,
        position: &[u8],
        now: i64,
    ) -> Result<Option<Batch>, Refusal> {
        let start = parse_position(position)?.map(<[u8]>::to_vec);
        if self.cancel.load(Ordering::Relaxed) {
            return Err(Refusal::Cancelled);
        }
        let limit = self.statement_timeout * BATCH_STEPS;
        match tokio::time::timeout(limit, self.read(start, now)).await {
            Ok(r) => r,
            Err(_) => Err(Refusal::StatementTimeout),
        }
    }

    async fn read(&mut self, start: Option<Vec<u8>>, now: i64) -> Result<Option<Batch>, Refusal> {
        let tx = self
            .client
            .build_transaction()
            .isolation_level(IsolationLevel::RepeatableRead)
            .read_only(true)
            .start()
            .await
            .map_err(map_err)?;
        // The same database and tables, then nothing writing them.
        let db = tx.query_one(DATABASE_SQL, &[]).await.map_err(map_err)?;
        let rels = current_rels(&tx, &self.rels).await?;
        if fingerprint(
            db.try_get(0).map_err(map_err)?,
            &db.try_get::<_, Vec<u8>>(1).map_err(map_err)?,
            &rels,
        ) != self.fingerprint
        {
            return Err(Refusal::SourceChanged);
        }
        let oids: Vec<u32> = self.rels.iter().map(|r| r.oid).collect();
        quiet(
            &tx,
            self.db_oid,
            &oids,
            &self.enums,
            Some((self.writes, self.snapshot.as_str())),
        )
        .await?;

        let (stmt, key) = match &start {
            None => (&self.torrents_first, Vec::new()),
            Some(k) => (&self.torrents_next, k.clone()),
        };
        let rows = tx
            .query(stmt, &[&key, &(self.max_parents as i64)])
            .await
            .map_err(map_err)?;
        let mut parents: Vec<Parent> = Vec::with_capacity(rows.len());
        let mut total = 0usize;
        for r in &rows {
            let key: Option<Vec<u8>> = col(r, 0)?;
            let Some(key) = key else {
                return Err(Refusal::BadKey);
            };
            let row = RawTorrent {
                key,
                name: Bytes::guarded(col(r, 1)?, col(r, 2)?),
                size: col(r, 3)?,
                private: col(r, 4)?,
                status: Status::parse(col(r, 5)?),
                files_count: col(r, 6)?,
                piece_length: col(r, 7)?,
                pieces_len: col(r, 8)?,
                created_at: col(r, 9)?,
            };
            let source = row.key.len() + row.name.len() + 40;
            let bytes = source + ROW_OVERHEAD;
            if !parents.is_empty() && total + bytes > MAX_BATCH_BYTES {
                break;
            }
            total += bytes;
            parents.push(Parent {
                row,
                files: Vec::new(),
                overflow: 0,
                bytes,
                source,
            });
        }
        drop(rows);
        let (Some(first), Some(last)) = (parents.first(), parents.last()) else {
            return Ok(None);
        };
        let (mut lower, upper) = (first.row.key.clone(), last.row.key.clone());
        let mut inclusive = true;
        let mut keep = parents.len();
        let mut i = 0;
        let mut before = 0usize;
        let count = parents.len();
        // One ranged read, in chunks; restarted after a parent over its
        // budget so the rest of its rows are never transferred.
        'query: loop {
            let stmt = if inclusive {
                &self.files_first
            } else {
                &self.files_next
            };
            let portal = tx.bind(stmt, &[&lower, &upper]).await.map_err(map_err)?;
            loop {
                if self.cancel.load(Ordering::Relaxed) {
                    return Err(Refusal::Cancelled);
                }
                let chunk = tx
                    .query_portal(&portal, FILE_CHUNK)
                    .await
                    .map_err(map_err)?;
                if chunk.is_empty() {
                    break 'query;
                }
                for r in &chunk {
                    let tid: Option<Vec<u8>> = col(r, 0)?;
                    let Some(tid) = tid else { continue };
                    while i < parents.len() && parents[i].row.key < tid {
                        before += parents[i].bytes;
                        i += 1;
                    }
                    let Some(p) = parents.get_mut(i).filter(|p| p.row.key == tid) else {
                        // In range, so it's an orphan of a constraint-free
                        // source: skip it.
                        continue;
                    };
                    let f = RawFile {
                        index: col(r, 1)?,
                        path: Bytes::guarded(col(r, 2)?, col(r, 3)?),
                        size: col(r, 4)?,
                    };
                    let read = f.path.len() + 12;
                    let segments = match &f.path {
                        Bytes::Value(b) => b.iter().filter(|c| **c == b'/').count() + 1,
                        _ => 0,
                    };
                    let cost = read + ROW_OVERHEAD + segments * SEGMENT_OVERHEAD;
                    p.source += read;
                    if p.files.len() < LIMITS.max_files && p.bytes + cost <= MAX_BATCH_BYTES {
                        p.bytes += cost;
                        p.files.push(f);
                    } else {
                        // Over its budget: kept as a prefix, the rest
                        // skipped by restarting after this parent.
                        p.overflow = 1;
                        if i + 1 == count {
                            break 'query;
                        }
                        lower = p.row.key.clone();
                        inclusive = false;
                        continue 'query;
                    }
                    if i > 0 && before + p.bytes > MAX_BATCH_BYTES {
                        // This parent doesn't fit beside the earlier ones:
                        // it starts the next batch instead.
                        keep = i;
                        break 'query;
                    }
                }
            }
        }
        // Parents without file rows never reach the check above.
        let mut total = 0usize;
        for (i, p) in parents.iter().enumerate().take(keep) {
            total += p.bytes;
            if i > 0 && total > MAX_BATCH_BYTES {
                keep = i;
                break;
            }
        }
        parents.truncate(keep);
        // A read-only transaction: ending it can't lose anything.
        tx.commit().await.map_err(map_err)?;
        let normaliser = Normaliser { now };
        let mut findings = Findings::default();
        let mut source_bytes = 0u64;
        let records = parents
            .iter()
            .map(|p| {
                source_bytes += p.source as u64;
                normaliser.torrent(&p.row, &p.files, p.overflow, &mut findings)
            })
            .collect();
        let Some(last) = parents.last() else {
            return Ok(None);
        };
        Ok(Some(Batch {
            records,
            position: position(&last.row.key),
            source_bytes,
            findings,
        }))
    }

    #[cfg(test)]
    pub(crate) fn client(&self) -> &Client {
        &self.client
    }

    #[cfg(test)]
    pub(crate) fn set_max_parents(&mut self, n: usize) {
        self.max_parents = n;
    }
}

fn fingerprint(db_oid: u32, db_name: &[u8], rels: &[Rel]) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(b"lodestar.import.bitmagnet.fingerprint.v1\0");
    h.update(db_oid.to_be_bytes());
    h.update((db_name.len() as u32).to_be_bytes());
    h.update(db_name);
    for r in rels {
        h.update(r.oid.to_be_bytes());
        h.update(r.filenode.unwrap_or(0).to_be_bytes());
    }
    h.finalize().into()
}

/// The read tables' current OIDs and file nodes, in `rels`' order. A
/// table that's gone keeps its OID with no file node.
async fn current_rels(
    client: &impl tokio_postgres::GenericClient,
    rels: &[Rel],
) -> Result<Vec<Rel>, Refusal> {
    let oids: Vec<u32> = rels.iter().map(|r| r.oid).collect();
    let rows = client
        .query(
            "SELECT o, (SELECT pg_catalog.pg_relation_filenode(c.oid) FROM pg_catalog.pg_class c \
                        WHERE c.oid = u.o) \
             FROM pg_catalog.unnest($1::oid[]) WITH ORDINALITY u(o, n) ORDER BY n",
            &[&oids],
        )
        .await
        .map_err(map_err)?;
    rows.iter()
        .map(|r| {
            Ok(Rel {
                oid: r.try_get(0).map_err(map_err)?,
                filenode: r.try_get(1).map_err(map_err)?,
            })
        })
        .collect()
}

/// Refuses a source that may be changing, or whose enum gained a cast;
/// returns the write counters and the snapshot. With `expect`, neither may
/// have moved since the open.
async fn quiet(
    client: &impl tokio_postgres::GenericClient,
    db: u32,
    oids: &[u32],
    enums: &[u32],
    expect: Option<(i64, &str)>,
) -> Result<(i64, String), Refusal> {
    let r = client
        .query_one(QUIET_SQL, &[&db, &oids, &enums])
        .await
        .map_err(map_err)?;
    let locks: i64 = r.try_get(0).map_err(map_err)?;
    let writes: i64 = r.try_get(1).map_err(map_err)?;
    let standby: bool = r.try_get(2).map_err(map_err)?;
    let counted: bool = r.try_get(3).map_err(map_err)?;
    let snapshot: String = r.try_get(4).map_err(map_err)?;
    let casts: i64 = r.try_get(5).map_err(map_err)?;
    if casts > 0 {
        return Err(cast_refusal());
    }
    if locks > 0
        || standby
        || !counted
        || expect.is_some_and(|(w, snap)| w != writes || snap != snapshot)
    {
        return Err(Refusal::NotQuiescent);
    }
    Ok((writes, snapshot))
}

// ---- The legacy `import-bitmagnet` path, until IMP-4 ----

#[derive(Debug)]
pub enum ImportError {
    Source(Refusal),
    Store(lodestar_store::Error),
    Io(std::io::Error),
}

impl fmt::Display for ImportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ImportError::Source(r) => write!(f, "{r} [{}]", r.label()),
            ImportError::Store(e) => e.fmt(f),
            ImportError::Io(e) => write!(f, "cursor file: {e}"),
        }
    }
}

impl std::error::Error for ImportError {}

impl From<Refusal> for ImportError {
    fn from(e: Refusal) -> Self {
        ImportError::Source(e)
    }
}

impl From<lodestar_store::Error> for ImportError {
    fn from(e: lodestar_store::Error) -> Self {
        ImportError::Store(e)
    }
}

#[derive(Debug, Clone)]
pub struct Options {
    /// Torrents per batch, at most [`MAX_PARENTS`].
    pub batch: usize,
    /// Where to remember progress, so a rerun continues where it stopped.
    pub cursor: Option<PathBuf>,
    /// Stop after this many torrents (for trying it out).
    pub limit: Option<u64>,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            batch: MAX_PARENTS,
            cursor: None,
            limit: None,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Report {
    /// Torrents read from bitmagnet.
    pub read: u64,
    /// Newly stored.
    pub new: u64,
    /// Already stored (from an earlier run or our own crawler).
    pub existing: u64,
    /// Stored with an incomplete file list, to be re-fetched.
    pub partial: u64,
    /// Rows that couldn't be imported: a bad hash, or no name or size.
    pub skipped: u64,
}

const CURSOR_TAG: &str = "bitmagnet-cursor-v2";

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex(t: &str) -> Option<Vec<u8>> {
    if !t.len().is_multiple_of(2) {
        return None;
    }
    (0..t.len())
        .step_by(2)
        .map(|i| t.get(i..i + 2).and_then(|h| u8::from_str_radix(h, 16).ok()))
        .collect()
}

/// The saved position, bound to the dataset it was read from: a cursor
/// from another database (or one changed since) is `source_changed`, never
/// a silent skip. A cursor without the dataset, from before IMP-3, isn't
/// trusted (IMP-1): it's refused with the safe step.
fn read_cursor(path: &Path, dataset: &[u8; 32]) -> Result<Vec<u8>, ImportError> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(ImportError::Io(e)),
    };
    let mut parts = text.split_whitespace();
    let (Some(CURSOR_TAG), Some(saved), Some(pos), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err(ImportError::Io(std::io::Error::other(
            "the cursor file isn't this version's: delete it to start over (torrents already \
             imported are kept and recognised)",
        )));
    };
    if unhex(saved).as_deref() != Some(&dataset[..]) {
        return Err(ImportError::Source(Refusal::SourceChanged));
    }
    let pos = unhex(pos).ok_or_else(|| ImportError::Io(std::io::Error::other("bad cursor")))?;
    parse_position(&pos)?;
    Ok(pos)
}

/// The position saved in the legacy `import-bitmagnet` cursor file at
/// `path`, if it was read from this dataset; empty when there's no file.
/// IMP-4 binds it to a job once, explicitly (IMP-1: never automatically).
pub fn legacy_cursor(path: &Path, dataset: &[u8; 32]) -> Result<Vec<u8>, ImportError> {
    read_cursor(path, dataset)
}

fn write_cursor(path: &Path, dataset: &[u8; 32], position: &[u8]) -> Result<(), ImportError> {
    let tmp = path.with_extension("tmp");
    let text = format!("{CURSOR_TAG} {} {}\n", hex(dataset), hex(position));
    std::fs::write(&tmp, text).map_err(ImportError::Io)?;
    std::fs::rename(&tmp, path).map_err(ImportError::Io)
}

/// Imports everything after the saved cursor into `store` as `source`,
/// through the adapter. `progress` is called after each batch with the
/// totals so far.
pub async fn import(
    settings: &Settings,
    store: &mut Store,
    source: SourceId,
    opts: &Options,
    mut progress: impl FnMut(&Report),
) -> Result<Report, ImportError> {
    let mut src = Source::open(settings).await?;
    let mut cursor = match &opts.cursor {
        Some(p) => read_cursor(p, &src.identity().dataset)?,
        None => Vec::new(),
    };
    let mut report = Report::default();
    loop {
        let mut batch = opts.batch.clamp(1, MAX_PARENTS);
        if let Some(limit) = opts.limit {
            batch = batch.min(limit.saturating_sub(report.read) as usize);
            if batch == 0 {
                break;
            }
        }
        src.max_parents = batch;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs() as i64);
        let Some(b) = src.read_batch(&cursor, now).await? else {
            break;
        };
        report.read += b.records.len() as u64;
        let torrents: Vec<(Info, bool, i64)> = b
            .records
            .iter()
            .filter_map(|r| match r {
                Record::Torrent(t) => Some((
                    t.info(),
                    t.completeness == Completeness::Partial,
                    t.source_at.unwrap_or(now),
                )),
                _ => None,
            })
            .collect();
        report.skipped += (b.records.len() - torrents.len()) as u64;
        let new: Vec<NewTorrent<'_>> = torrents
            .iter()
            .map(|(info, partial, at)| NewTorrent {
                info,
                raw_info: None,
                discovered_at: *at,
                metadata_at: now,
                extra_flags: flags::IMPORTED | if *partial { flags::FILES_PARTIAL } else { 0 },
                source,
            })
            .collect();
        let inserted = store.insert(&new)?;
        for ((_, partial, _), ins) in torrents.iter().zip(&inserted) {
            match ins {
                lodestar_store::Inserted::New(_) => {
                    report.new += 1;
                    report.partial += u64::from(*partial);
                }
                lodestar_store::Inserted::Existing(_) => report.existing += 1,
            }
        }
        cursor = b.position;
        if let Some(p) = &opts.cursor {
            write_cursor(p, &src.identity().dataset, &cursor)?;
        }
        progress(&report);
    }
    Ok(report)
}

#[cfg(test)]
mod pg_tests;
#[cfg(test)]
mod tests;
