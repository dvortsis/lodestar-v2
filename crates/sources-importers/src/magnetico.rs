//! The Magnetico SQLite adapter (IMP-2, `docs/decisions/2026-10-30-IMP-2.md`):
//! reads a stopped magneticod database read-only, in keyset batches, into
//! IMP-1 import records.
//!
//! Everything in the source is untrusted. Rows are read as raw values and
//! normalised by [`Normaliser`], which has no I/O so it can be fuzzed.
//! Nothing from a row reaches SQL text, the filesystem, an error message or
//! a log: errors and findings are fixed enums.

use std::ffi::OsString;
use std::fmt;
use std::fs;
use std::path::{Component, Path, PathBuf, Prefix};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use lodestar_metainfo::{
    File, FileAttrs, FileHashes, Info, InfoHash, MetaVersion, Text, TextEncoding,
};
use lodestar_store::Prepared;
use lodestar_store::import::{
    Completeness, Decision, ImportCandidate, ImportTorrent, Privacy, Reason, Verification, repair,
};
use rusqlite::limits::Limit;
use rusqlite::types::ValueRef;
use rusqlite::{Connection, ErrorCode, OpenFlags, config::DbConfig};
use sha2::{Digest, Sha256};

/// The adapter name in the source identity (`import_sources.adapter`).
pub const ADAPTER: &str = "magnetico";
/// Bumped when the mapping of a row to a record changes.
pub const ADAPTER_VERSION: i64 = 1;

/// Parents per batch (plan §9).
pub const MAX_PARENTS: usize = 500;
/// Normalised bytes per batch, and per parent (plan §9).
pub const MAX_BATCH_BYTES: usize = 8 << 20;
/// Earliest plausible `discovered_on`: 2008-01-01, before mainline DHT
/// crawling existed.
pub const MIN_DATE: i64 = 1_199_145_600;
/// How far after the batch time a date may lie (clock skew).
pub const FUTURE_SLACK: i64 = 86_400;

const LIMITS: lodestar_metainfo::Limits = lodestar_metainfo::Limits::DEFAULT;
const POSITION_VERSION: u8 = 1;

/// Why a source can't be read. Bounded labels only: no names, hashes,
/// paths or row content, so it's safe for logs and metrics.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// Not a regular file: a directory, device, pipe or socket.
    NotAFile,
    /// A symlink or reparse point leading outside its own directory.
    SymlinkEscape,
    /// A network (UNC) path that isn't under an approved root.
    UncNotApproved,
    /// magneticod may be running: a non-empty `-wal` or a `-journal`.
    NotQuiescent,
    /// The file couldn't be opened or read. The `&str` is a fixed label.
    Unavailable(&'static str),
    /// Required columns, tables or indexes are missing, named from this
    /// adapter's own list.
    UnsupportedSchema(Vec<&'static str>),
    /// SQLite reports the file damaged.
    Corrupt,
    /// A statement ran past its deadline.
    StatementTimeout,
    /// The caller cancelled.
    Cancelled,
    /// The source changed since it was opened or since the job began.
    SourceChanged,
    /// A resume position this adapter didn't write.
    BadPosition,
}

impl Refusal {
    /// The stable reason label.
    pub fn label(&self) -> &'static str {
        match self {
            Refusal::NotAFile => "not_a_file",
            Refusal::SymlinkEscape => "symlink_escape",
            Refusal::UncNotApproved => "unc_not_approved",
            Refusal::NotQuiescent => "source_not_quiescent",
            Refusal::Unavailable(_) => "source_unavailable",
            Refusal::UnsupportedSchema(_) => "unsupported_schema",
            Refusal::Corrupt => "source_corrupt",
            Refusal::StatementTimeout => "statement_timeout",
            Refusal::Cancelled => "cancelled",
            Refusal::SourceChanged => "source_changed",
            Refusal::BadPosition => "bad_position",
        }
    }

    /// The IMP-1 reason a running job blocks with, or `None` when it
    /// should fail (an unsupported source) or the caller stopped it.
    pub fn job_reason(&self) -> Option<Reason> {
        match self {
            Refusal::SourceChanged => Some(Reason::SourceChanged),
            Refusal::NotQuiescent
            | Refusal::Unavailable(_)
            | Refusal::Corrupt
            | Refusal::StatementTimeout => Some(Reason::SourceUnavailable),
            Refusal::NotAFile
            | Refusal::SymlinkEscape
            | Refusal::UncNotApproved
            | Refusal::UnsupportedSchema(_)
            | Refusal::BadPosition
            | Refusal::Cancelled => None,
        }
    }
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Refusal::NotAFile => f.write_str("the source isn't a regular file"),
            Refusal::SymlinkEscape => {
                f.write_str("the source is a link to a file outside its folder")
            }
            Refusal::UncNotApproved => {
                f.write_str("the network path isn't an approved import root")
            }
            Refusal::NotQuiescent => f.write_str(
                "the database is in use or wasn't closed cleanly (a -wal or -journal file is \
                 beside it): stop magneticod, or import a consistent copy",
            ),
            Refusal::Unavailable(why) => write!(
                f,
                "the database can't be opened ({why}): check the file, and that its folder is \
                 writable, or import a copy from a writable folder"
            ),
            Refusal::UnsupportedSchema(missing) => write!(
                f,
                "not a supported magneticod database, missing {}; if it's an old magneticod \
                 database, run a current magneticod on it once to upgrade it",
                missing.join(", ")
            ),
            Refusal::Corrupt => f.write_str("the database is damaged"),
            Refusal::StatementTimeout => f.write_str("a read took too long"),
            Refusal::Cancelled => f.write_str("cancelled"),
            Refusal::SourceChanged => f.write_str("the database changed since the import began"),
            Refusal::BadPosition => f.write_str("the resume position isn't this adapter's"),
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
    ReadmeNotImported,
}

impl Finding {
    pub const ALL: [Finding; 12] = [
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
        Finding::ReadmeNotImported,
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
            Finding::ReadmeNotImported => "readme_not_imported",
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

/// One SQLite value as stored, before any interpretation.
#[derive(Debug, Clone, PartialEq)]
pub enum Raw {
    Null,
    Integer(i64),
    Real(f64),
    Text(Vec<u8>),
    Blob(Vec<u8>),
}

impl Raw {
    fn from_ref(v: ValueRef<'_>) -> Raw {
        match v {
            ValueRef::Null => Raw::Null,
            ValueRef::Integer(i) => Raw::Integer(i),
            ValueRef::Real(r) => Raw::Real(r),
            ValueRef::Text(t) => Raw::Text(t.to_vec()),
            ValueRef::Blob(b) => Raw::Blob(b.to_vec()),
        }
    }

    /// Bytes this value took from the source.
    fn len(&self) -> usize {
        match self {
            Raw::Null => 0,
            Raw::Integer(_) | Raw::Real(_) => 8,
            Raw::Text(b) | Raw::Blob(b) => b.len(),
        }
    }

    /// Text or blob bytes; other storage classes aren't text.
    fn bytes(&self) -> Option<&[u8]> {
        match self {
            Raw::Text(b) | Raw::Blob(b) => Some(b),
            _ => None,
        }
    }
}

/// One `torrents` row as read.
#[derive(Debug, Clone, PartialEq)]
pub struct RawTorrent {
    pub id: i64,
    pub info_hash: Raw,
    pub name: Raw,
    pub total_size: Raw,
    pub discovered_on: Raw,
}

/// One `files` row as read.
#[derive(Debug, Clone, PartialEq)]
pub struct RawFile {
    pub path: Raw,
    pub size: Raw,
}

/// One file of a normalised torrent: path segments as raw bytes.
#[derive(Clone, PartialEq, Eq)]
pub struct FileEntry {
    pub path: Vec<Vec<u8>>,
    pub size: u64,
}

/// A torrent whose name and size fill a canonical row.
#[derive(Clone, PartialEq, Eq)]
pub struct TorrentRecord {
    pub hash: [u8; 20],
    /// The original bytes. `Store::prepare` keeps them in `name_raw`
    /// whenever they aren't valid UTF-8.
    pub name: Vec<u8>,
    pub size: u64,
    pub source_at: Option<i64>,
    pub files: Vec<FileEntry>,
    pub completeness: Completeness,
    /// [`repair`] bits.
    pub repair: i64,
    pub files_dropped: u32,
}

/// A valid hash too incomplete for a canonical row.
#[derive(Clone, PartialEq, Eq)]
pub struct CandidateRecord {
    pub hash: [u8; 20],
    pub name: Option<Vec<u8>>,
    pub size: Option<u64>,
    pub source_at: Option<i64>,
}

// Hand-written so a record can never print a name or path.
impl fmt::Debug for TorrentRecord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TorrentRecord")
            .field("name_len", &self.name.len())
            .field("size", &self.size)
            .field("files", &self.files.len())
            .field("completeness", &self.completeness)
            .field("repair", &self.repair)
            .field("files_dropped", &self.files_dropped)
            .finish_non_exhaustive()
    }
}

impl fmt::Debug for CandidateRecord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CandidateRecord")
            .field("name_len", &self.name.as_ref().map(Vec::len))
            .field("size", &self.size)
            .finish_non_exhaustive()
    }
}

impl fmt::Debug for FileEntry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FileEntry")
            .field("segments", &self.path.len())
            .field("size", &self.size)
            .finish()
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

impl TorrentRecord {
    /// The record as an unverified [`Info`] for `Store::prepare`. Magnetico
    /// keeps no info dict: piece fields and `info_size` are 0, `private`
    /// is false (privacy is carried separately, as unknown), and it's never
    /// single-file, since that can't be told from the rows.
    pub fn info(&self) -> Info {
        Info {
            v1: Some(InfoHash(self.hash)),
            v2: None,
            version: MetaVersion::V1,
            name: Text {
                raw: self.name.clone(),
                encoding: TextEncoding::UTF8,
            },
            piece_length: 0,
            piece_count: 0,
            total_size: self.size,
            private: false,
            source: None,
            single_file: false,
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

    /// The IMP-1 record, with privacy unknown and verification
    /// unverified: never verified because magneticod once fetched it.
    pub fn import<'a>(&self, prepared: &'a Prepared, decision: Decision<'a>) -> ImportTorrent<'a> {
        ImportTorrent {
            torrent: prepared,
            completeness: self.completeness,
            verification: Verification::Unverified,
            privacy: Privacy::Unknown,
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
            privacy: Privacy::Unknown,
            source_at: self.source_at,
            decision,
        }
    }
}

/// Turns raw rows into records. Pure: no I/O, so it's the fuzz target.
#[derive(Debug, Clone, Copy)]
pub struct Normaliser {
    /// The batch time, for the date range check.
    pub now: i64,
}

impl Normaliser {
    /// Normalises one parent and its file rows, in file order. `overflow`
    /// is how many further file rows the reader didn't keep because the
    /// parent was over its file-count or byte budget.
    pub fn torrent(
        &self,
        row: &RawTorrent,
        files: &[RawFile],
        overflow: u32,
        findings: &mut Findings,
    ) -> Record {
        let Some(hash) = hash(&row.info_hash) else {
            findings.add(Finding::BadHash, 1);
            return Record::Invalid(Finding::BadHash);
        };
        let source_at = match row.discovered_on {
            Raw::Integer(d) if (MIN_DATE..=self.now.saturating_add(FUTURE_SLACK)).contains(&d) => {
                Some(d)
            }
            _ => {
                findings.add(Finding::DateOutOfRange, 1);
                None
            }
        };
        let name = match row.name.bytes() {
            Some(n) if n.len() > LIMITS.max_name_bytes => {
                findings.add(Finding::NameOverLimit, 1);
                None
            }
            Some(n) if !n.is_empty() => {
                if std::str::from_utf8(n).is_err() {
                    findings.add(Finding::NameLossy, 1);
                }
                Some(n.to_vec())
            }
            _ => {
                findings.add(Finding::MissingName, 1);
                None
            }
        };
        let size = match row.total_size {
            Raw::Integer(s) if s > 0 => Some(s as u64),
            _ => {
                findings.add(Finding::MissingSize, 1);
                None
            }
        };
        let (Some(name), Some(size)) = (name.clone(), size) else {
            return Record::Candidate(CandidateRecord {
                hash,
                name,
                size,
                source_at,
            });
        };

        let mut kept = Vec::with_capacity(files.len().min(LIMITS.max_files));
        let mut dropped = overflow;
        let mut sum = Some(0u64);
        for f in files {
            if kept.len() >= LIMITS.max_files {
                dropped += 1;
                continue;
            }
            match file(f, findings) {
                Some(e) => {
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
        if repair == 0 && sum != Some(size) {
            findings.add(Finding::SizeMismatch, 1);
            repair |= repair::INCONSISTENT_FILES;
        }
        Record::Torrent(TorrentRecord {
            hash,
            name,
            size,
            source_at,
            files: kept,
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

/// A 20-byte BLOB that isn't all zeros.
fn hash(v: &Raw) -> Option<[u8; 20]> {
    match v {
        Raw::Blob(b) => <[u8; 20]>::try_from(b.as_slice())
            .ok()
            .filter(|h| h.iter().any(|x| *x != 0)),
        _ => None,
    }
}

/// A well-formed file row, or `None`.
fn file(f: &RawFile, findings: &mut Findings) -> Option<FileEntry> {
    let Raw::Integer(size) = f.size else {
        return None;
    };
    let size = u64::try_from(size).ok()?;
    let path = f.path.bytes()?;
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

/// What [`open`] accepts beyond the path itself. IMP-4's CLI fills it.
#[derive(Debug, Clone)]
pub struct OpenPolicy {
    /// Network (UNC) roots a source may live under. Empty refuses them all.
    pub approved_unc_roots: Vec<PathBuf>,
    /// The longest one SQLite step may run.
    pub statement_timeout: Duration,
}

impl Default for OpenPolicy {
    fn default() -> Self {
        OpenPolicy {
            approved_unc_roots: Vec::new(),
            statement_timeout: Duration::from_secs(30),
        }
    }
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
#[derive(Debug)]
pub struct Batch {
    pub records: Vec<Record>,
    /// Where to resume after this batch: IMP-1's opaque cursor.
    pub position: Vec<u8>,
    pub source_bytes: u64,
    pub findings: Findings,
}

/// A validated, opened Magnetico database.
pub struct Source {
    conn: Connection,
    path: PathBuf,
    file_id: FileId,
    identity: Identity,
    files_sql: String,
    deadline: Deadline,
    cancel: Arc<AtomicBool>,
}

impl fmt::Debug for Source {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("magnetico::Source { .. }")
    }
}

/// A per-step deadline the progress handler checks.
#[derive(Clone)]
struct Deadline {
    base: Instant,
    /// Milliseconds after `base`.
    at: Arc<AtomicU64>,
    step: Duration,
}

impl Deadline {
    fn arm(&self) {
        let at = self.base.elapsed() + self.step;
        self.at.store(
            at.as_millis().try_into().unwrap_or(u64::MAX),
            Ordering::Relaxed,
        );
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FileId(u64, u64);

impl FileId {
    #[cfg(unix)]
    fn of(m: &fs::Metadata) -> FileId {
        use std::os::unix::fs::MetadataExt;
        FileId(m.dev(), m.ino())
    }

    // The Windows file index is unstable in std; length and creation
    // time stand in for it.
    #[cfg(not(unix))]
    fn of(m: &fs::Metadata) -> FileId {
        let created = m
            .created()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or(0, |d| d.as_nanos() as u64);
        FileId(m.len(), created)
    }
}

/// The columns this adapter needs, by table.
const REQUIRED: [(&str, &[&str]); 2] = [
    (
        "torrents",
        &["id", "info_hash", "name", "total_size", "discovered_on"],
    ),
    ("files", &["id", "torrent_id", "size", "path"]),
];
/// Optional columns whose presence goes into the schema signature.
const OPTIONAL: [(&str, &[&str]); 2] = [
    (
        "torrents",
        &["updated_on", "n_seeders", "n_leechers", "modified_on"],
    ),
    ("files", &["is_readme", "content"]),
];

const TORRENTS_SQL: &str = "SELECT id, info_hash, name, total_size, discovered_on \
                            FROM torrents WHERE id > ?1 ORDER BY id LIMIT ?2";

/// Validates `path` and opens it read-only. See the decisions file,
/// "Opening the source".
pub fn open(path: &Path, policy: &OpenPolicy) -> Result<Source, Refusal> {
    let (canonical, file_id) = validate(path, policy)?;
    quiescent(&canonical)?;
    let flags = OpenFlags::SQLITE_OPEN_READ_ONLY
        | OpenFlags::SQLITE_OPEN_NO_MUTEX
        | OpenFlags::SQLITE_OPEN_NOFOLLOW;
    // No SQLITE_OPEN_URI: a file name can't smuggle in `immutable=1` or
    // another VFS.
    let conn = Connection::open_with_flags(&canonical, flags)
        .map_err(|_| Refusal::Unavailable("open_failed"))?;
    let deadline = Deadline {
        base: Instant::now(),
        at: Arc::new(AtomicU64::new(0)),
        step: policy.statement_timeout,
    };
    let cancel = Arc::new(AtomicBool::new(false));
    deadline.arm();
    harden(&conn, &deadline, &cancel).map_err(map_err)?;
    match fs::metadata(&canonical) {
        Ok(m) if FileId::of(&m) == file_id => {}
        _ => return Err(Refusal::SourceChanged),
    }
    deadline.arm();
    let files_sql = schema(&conn).map_err(|e| match e {
        SchemaError::Sql(e) => map_err(e),
        SchemaError::Unsupported(missing) => Refusal::UnsupportedSchema(missing),
    })?;
    deadline.arm();
    let identity = identity(&conn, &files_sql).map_err(map_err)?;
    Ok(Source {
        conn,
        path: canonical,
        file_id,
        identity,
        files_sql,
        deadline,
        cancel,
    })
}

fn validate(path: &Path, policy: &OpenPolicy) -> Result<(PathBuf, FileId), Refusal> {
    let link = fs::symlink_metadata(path).map_err(|_| Refusal::Unavailable("not_found"))?;
    let canonical = fs::canonicalize(path).map_err(|_| Refusal::Unavailable("not_found"))?;
    if link.file_type().is_symlink() {
        let dir = match path.parent() {
            Some(d) if !d.as_os_str().is_empty() => d,
            _ => Path::new("."),
        };
        let dir = fs::canonicalize(dir).map_err(|_| Refusal::Unavailable("not_found"))?;
        if !canonical.starts_with(&dir) {
            return Err(Refusal::SymlinkEscape);
        }
    }
    let meta = fs::metadata(&canonical).map_err(|_| Refusal::Unavailable("not_found"))?;
    if !meta.is_file() {
        return Err(Refusal::NotAFile);
    }
    if is_unc(&canonical)
        && !policy
            .approved_unc_roots
            .iter()
            .filter_map(|r| fs::canonicalize(r).ok())
            .any(|r| canonical.starts_with(r))
    {
        return Err(Refusal::UncNotApproved);
    }
    Ok((canonical, FileId::of(&meta)))
}

fn is_unc(path: &Path) -> bool {
    matches!(
        path.components().next(),
        Some(Component::Prefix(p)) if matches!(p.kind(), Prefix::UNC(..) | Prefix::VerbatimUNC(..))
    )
}

fn sibling(path: &Path, suffix: &str) -> PathBuf {
    let mut s = OsString::from(path.as_os_str());
    s.push(suffix);
    PathBuf::from(s)
}

/// Refuses a source magneticod may still be writing. An empty `-wal` is
/// fine: SQLite leaves one after any clean close, including ours.
fn quiescent(path: &Path) -> Result<(), Refusal> {
    if fs::symlink_metadata(sibling(path, "-wal")).is_ok_and(|m| m.len() > 0)
        || fs::symlink_metadata(sibling(path, "-journal")).is_ok()
    {
        return Err(Refusal::NotQuiescent);
    }
    Ok(())
}

fn harden(
    conn: &Connection,
    deadline: &Deadline,
    cancel: &Arc<AtomicBool>,
) -> rusqlite::Result<()> {
    conn.set_db_config(DbConfig::SQLITE_DBCONFIG_DEFENSIVE, true)?;
    conn.set_db_config(DbConfig::SQLITE_DBCONFIG_TRUSTED_SCHEMA, false)?;
    conn.set_db_config(DbConfig::SQLITE_DBCONFIG_ENABLE_TRIGGER, false)?;
    conn.set_db_config(DbConfig::SQLITE_DBCONFIG_ENABLE_VIEW, false)?;
    for (limit, value) in [
        (Limit::SQLITE_LIMIT_LENGTH, 16 << 20),
        (Limit::SQLITE_LIMIT_SQL_LENGTH, 64 << 10),
        (Limit::SQLITE_LIMIT_COLUMN, 64),
        (Limit::SQLITE_LIMIT_ATTACHED, 0),
        (Limit::SQLITE_LIMIT_EXPR_DEPTH, 100),
        (Limit::SQLITE_LIMIT_VARIABLE_NUMBER, 1024),
    ] {
        conn.set_limit(limit, value)?;
    }
    conn.pragma_update(None, "query_only", true)?;
    conn.pragma_update(None, "trusted_schema", false)?;
    conn.pragma_update(None, "cell_size_check", true)?;
    conn.pragma_update(None, "mmap_size", 0)?;
    let (base, at, cancel) = (deadline.base, deadline.at.clone(), cancel.clone());
    conn.progress_handler(
        10_000,
        Some(move || {
            cancel.load(Ordering::Relaxed)
                || base.elapsed().as_millis() > u128::from(at.load(Ordering::Relaxed))
        }),
    )
}

/// Checks the tables and columns and picks the files query whose plan
/// uses an index. Returns that query.
fn schema(conn: &Connection) -> Result<String, SchemaError> {
    let mut missing = Vec::new();
    for (table, columns) in REQUIRED {
        let sql: Option<String> = match conn.query_row(
            "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = ?1",
            [table],
            |r| r.get(0),
        ) {
            Ok(sql) => sql,
            Err(rusqlite::Error::QueryReturnedNoRows) => None,
            Err(e) => return Err(e.into()),
        };
        let Some(sql) = sql else {
            missing.push(table);
            continue;
        };
        let upper = sql.to_ascii_uppercase();
        let present = table_columns(conn, table)?;
        let id_ok = upper.starts_with("CREATE TABLE")
            && !upper.contains("WITHOUT ROWID")
            && present
                .iter()
                .any(|c| c.name == "id" && c.decl.eq_ignore_ascii_case("INTEGER") && c.pk == 1)
            && present.iter().filter(|c| c.pk > 0).count() == 1;
        for col in columns {
            if !present.iter().any(|c| c.name == *col) {
                missing.push(col);
            }
        }
        if !id_ok {
            missing.push(if table == "torrents" {
                "torrents.id as rowid"
            } else {
                "files.id as rowid"
            });
        }
    }
    if !missing.is_empty() {
        return Err(SchemaError::Unsupported(missing));
    }
    if !plan(conn, TORRENTS_SQL)?
        .iter()
        .any(|d| d.starts_with("SEARCH torrents USING INTEGER PRIMARY KEY"))
    {
        return Err(SchemaError::Unsupported(vec!["torrents.id keyset plan"]));
    }
    let readme = table_columns(conn, "files")?
        .iter()
        .any(|c| c.name == "is_readme");
    let orders: &[&str] = if readme {
        &["torrent_id, is_readme, id", "torrent_id, id"]
    } else {
        &["torrent_id, id"]
    };
    for order in orders {
        let sql = files_sql(readme, order);
        let details = plan(conn, &sql)?;
        let indexed = details.iter().any(|d| d.starts_with("SEARCH files USING "))
            && !details
                .iter()
                .any(|d| d.starts_with("SCAN") || d.contains("TEMP B-TREE"));
        if indexed {
            return Ok(sql);
        }
    }
    Err(SchemaError::Unsupported(vec![
        "missing_index: files.torrent_id",
    ]))
}

enum SchemaError {
    Sql(rusqlite::Error),
    Unsupported(Vec<&'static str>),
}

impl From<rusqlite::Error> for SchemaError {
    fn from(e: rusqlite::Error) -> Self {
        SchemaError::Sql(e)
    }
}

fn files_sql(readme: bool, order: &str) -> String {
    let readme = if readme { "is_readme IS NOT NULL" } else { "0" };
    format!(
        "SELECT torrent_id, path, size, {readme} FROM files \
         WHERE torrent_id BETWEEN ?1 AND ?2 ORDER BY {order}"
    )
}

struct Column {
    name: String,
    decl: String,
    notnull: i64,
    pk: i64,
}

fn table_columns(conn: &Connection, table: &str) -> rusqlite::Result<Vec<Column>> {
    let mut s = conn
        .prepare("SELECT name, type, \"notnull\", pk FROM pragma_table_info(?1) ORDER BY name")?;
    s.query_map([table], |r| {
        Ok(Column {
            name: r.get(0)?,
            decl: r.get(1)?,
            notnull: r.get(2)?,
            pk: r.get(3)?,
        })
    })?
    .collect()
}

/// `EXPLAIN QUERY PLAN` details for `sql`.
fn plan(conn: &Connection, sql: &str) -> rusqlite::Result<Vec<String>> {
    let mut s = conn.prepare(&format!("EXPLAIN QUERY PLAN {sql}"))?;
    let mut rows = s.query(rusqlite::params![0, 0])?;
    let mut out = Vec::new();
    while let Some(r) = rows.next()? {
        out.push(r.get::<_, String>(3)?);
    }
    Ok(out)
}

fn identity(conn: &Connection, files_sql: &str) -> rusqlite::Result<Identity> {
    let mut schema = Sha256::new();
    schema.update(b"lodestar-magnetico-schema-v1\0");
    for (table, _) in REQUIRED {
        let known: Vec<&str> = REQUIRED
            .iter()
            .chain(OPTIONAL.iter())
            .filter(|(t, _)| *t == table)
            .flat_map(|(_, c)| c.iter().copied())
            .collect();
        for c in table_columns(conn, table)? {
            if known.contains(&c.name.as_str()) {
                for part in [table, &c.name, &c.decl.to_ascii_uppercase()] {
                    schema.update(part.as_bytes());
                    schema.update(b"\0");
                }
                schema.update(c.notnull.to_be_bytes());
                schema.update(c.pk.to_be_bytes());
            }
        }
    }
    schema.update(files_sql.as_bytes());

    let mut data = Sha256::new();
    data.update(b"lodestar-magnetico-dataset-v1\0");
    let pragma = |name: &str| conn.pragma_query_value(None, name, |r| r.get::<_, i64>(0));
    for name in ["user_version", "page_size", "page_count"] {
        data.update(pragma(name)?.to_be_bytes());
    }
    let mut s = conn.prepare("SELECT type, name, sql FROM sqlite_master ORDER BY type, name")?;
    let mut rows = s.query([])?;
    while let Some(r) = rows.next()? {
        for i in 0..3 {
            let v = r.get_ref(i)?;
            let text = match v {
                ValueRef::Text(t) => t,
                _ => b"",
            };
            // Whitespace collapsed, so a reformatted schema dump is the
            // same schema.
            let mut space = false;
            for b in text {
                if b.is_ascii_whitespace() {
                    space = true;
                } else {
                    if space {
                        data.update(b" ");
                        space = false;
                    }
                    data.update([*b]);
                }
            }
            data.update(b"\0");
        }
    }
    let (first, last): (Option<i64>, Option<i64>) = conn.query_row(
        "SELECT (SELECT id FROM torrents ORDER BY id LIMIT 1), \
                (SELECT id FROM torrents ORDER BY id DESC LIMIT 1)",
        [],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    data.update(first.unwrap_or(0).to_be_bytes());
    data.update(last.unwrap_or(0).to_be_bytes());
    Ok(Identity {
        dataset: data.finalize().into(),
        schema_sig: schema.finalize().into(),
    })
}

fn map_err(e: rusqlite::Error) -> Refusal {
    match e.sqlite_error_code() {
        Some(ErrorCode::OperationInterrupted) => Refusal::StatementTimeout,
        Some(ErrorCode::DatabaseCorrupt | ErrorCode::NotADatabase) => Refusal::Corrupt,
        Some(ErrorCode::CannotOpen | ErrorCode::ReadOnly | ErrorCode::PermissionDenied) => {
            Refusal::Unavailable("folder_not_writable")
        }
        Some(ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked) => Refusal::NotQuiescent,
        _ => Refusal::Unavailable("read_failed"),
    }
}

/// The cursor after the parent with id `id`.
pub fn position(id: i64) -> Vec<u8> {
    let mut p = Vec::with_capacity(9);
    p.push(POSITION_VERSION);
    p.extend_from_slice(&id.to_be_bytes());
    p
}

/// The last parent id a [`position`] names; an empty position is the
/// start of the source.
pub fn parse_position(p: &[u8]) -> Result<i64, Refusal> {
    match p {
        [] => Ok(i64::MIN),
        [POSITION_VERSION, rest @ ..] => rest
            .try_into()
            .map(i64::from_be_bytes)
            .map_err(|_| Refusal::BadPosition),
        _ => Err(Refusal::BadPosition),
    }
}

/// A parent and the file rows kept for it.
struct Parent {
    row: RawTorrent,
    files: Vec<RawFile>,
    overflow: u32,
    bytes: usize,
}

impl Source {
    /// The identity to create or revalidate the job with.
    pub fn identity(&self) -> Identity {
        self.identity
    }

    /// A handle that cancels the running statement and every later one.
    pub fn canceller(&self) -> Arc<AtomicBool> {
        self.cancel.clone()
    }

    /// Reads the next batch after `position` (empty to start). `None` when
    /// the source is exhausted. `now` is the batch time.
    ///
    /// Before reading it checks the source is still quiescent and still
    /// the same dataset, so a magneticod started mid-import blocks the job
    /// with `source_changed` instead of feeding it a moving source.
    pub fn read_batch(&mut self, position: &[u8], now: i64) -> Result<Option<Batch>, Refusal> {
        let after = parse_position(position)?;
        self.check_unchanged()?;
        self.read(after, now).map_err(|e| self.refusal(e))
    }

    fn refusal(&self, e: rusqlite::Error) -> Refusal {
        match map_err(e) {
            Refusal::StatementTimeout if self.cancel.load(Ordering::Relaxed) => Refusal::Cancelled,
            r => r,
        }
    }

    fn check_unchanged(&mut self) -> Result<(), Refusal> {
        if self.cancel.load(Ordering::Relaxed) {
            return Err(Refusal::Cancelled);
        }
        quiescent(&self.path)?;
        match fs::metadata(&self.path) {
            Ok(m) if FileId::of(&m) == self.file_id => {}
            _ => return Err(Refusal::SourceChanged),
        }
        self.deadline.arm();
        let now = identity(&self.conn, &self.files_sql).map_err(|e| self.refusal(e))?;
        if now != self.identity {
            return Err(Refusal::SourceChanged);
        }
        Ok(())
    }

    fn read(&mut self, after: i64, now: i64) -> rusqlite::Result<Option<Batch>> {
        let mut parents: Vec<Parent> = Vec::new();
        {
            self.deadline.arm();
            let mut s = self.conn.prepare_cached(TORRENTS_SQL)?;
            let mut rows = s.query(rusqlite::params![after, MAX_PARENTS as i64])?;
            loop {
                self.deadline.arm();
                let Some(r) = rows.next()? else { break };
                let row = RawTorrent {
                    id: r.get(0)?,
                    info_hash: Raw::from_ref(r.get_ref(1)?),
                    name: Raw::from_ref(r.get_ref(2)?),
                    total_size: Raw::from_ref(r.get_ref(3)?),
                    discovered_on: Raw::from_ref(r.get_ref(4)?),
                };
                let bytes = row.info_hash.len() + row.name.len() + 16;
                parents.push(Parent {
                    row,
                    files: Vec::new(),
                    overflow: 0,
                    bytes,
                });
            }
        }
        let (Some(first), Some(last)) = (parents.first(), parents.last()) else {
            return Ok(None);
        };
        let (first, last) = (first.row.id, last.row.id);
        let mut findings = Findings::default();
        let mut keep = parents.len();
        {
            let mut s = self.conn.prepare_cached(&self.files_sql)?;
            let mut rows = s.query(rusqlite::params![first, last])?;
            let mut i = 0;
            let mut before = 0usize;
            loop {
                self.deadline.arm();
                let Some(r) = rows.next()? else { break };
                let tid: i64 = match r.get_ref(0)? {
                    ValueRef::Integer(t) => t,
                    _ => continue,
                };
                while i < parents.len() && parents[i].row.id < tid {
                    before += parents[i].bytes;
                    i += 1;
                }
                let Some(p) = parents.get_mut(i).filter(|p| p.row.id == tid) else {
                    // Ids in range are all this batch's parents; anything
                    // else is a non-integer or vanished id. Skip it.
                    continue;
                };
                if r.get_ref(3)? == ValueRef::Integer(1) {
                    findings.add(Finding::ReadmeNotImported, 1);
                }
                let f = RawFile {
                    path: Raw::from_ref(r.get_ref(1)?),
                    size: Raw::from_ref(r.get_ref(2)?),
                };
                let cost = f.path.len() + 16;
                if p.files.len() < LIMITS.max_files && p.bytes + cost <= MAX_BATCH_BYTES {
                    p.bytes += cost;
                    p.files.push(f);
                } else {
                    p.overflow = p.overflow.saturating_add(1);
                }
                if i > 0 && before + p.bytes > MAX_BATCH_BYTES {
                    // This parent doesn't fit beside the earlier ones: it
                    // starts the next batch instead.
                    keep = i;
                    break;
                }
            }
        }
        parents.truncate(keep);
        let normaliser = Normaliser { now };
        let mut source_bytes = 0u64;
        let records = parents
            .iter()
            .map(|p| {
                source_bytes += p.bytes as u64;
                normaliser.torrent(&p.row, &p.files, p.overflow, &mut findings)
            })
            .collect();
        let last = parents.last().map_or(after, |p| p.row.id);
        Ok(Some(Batch {
            records,
            position: position(last),
            source_bytes,
            findings,
        }))
    }

    #[cfg(test)]
    pub(crate) fn conn(&self) -> &Connection {
        &self.conn
    }

    /// The query plans the reader uses, for inspection and tests.
    pub fn plans(&self) -> Result<Vec<String>, Refusal> {
        let mut out = plan(&self.conn, TORRENTS_SQL).map_err(map_err)?;
        out.extend(plan(&self.conn, &self.files_sql).map_err(map_err)?);
        Ok(out)
    }
}

#[cfg(test)]
pub(crate) mod fixtures;
#[cfg(test)]
mod tests;
