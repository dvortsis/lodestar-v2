//! A remote worker's spool (architecture §19.6, workers plan WK-11): what
//! it has fetched and processed while the coordinator can't be reached,
//! kept on disk until the coordinator acknowledges it.
//!
//! **The format.** A spool is a folder of append-only segments, each
//! `<seq>.lsp` (16 hex digits, oldest first):
//!
//! ```text
//! header  "LSP1" | version u8 | 0 0 0 | worker u32 | seq u64 | created i64 | sum u64
//! frame*  len u32 | sum u64 | body (len bytes)
//! ```
//!
//! Integers are little-endian. `sum` is SipHash-1-3 under a fixed key, over
//! the header's first 28 bytes or over a frame's `len` and body: it finds a
//! torn or damaged frame without opening it (it's no defence against an
//! attacker; the seal is). Each body is one [`Record`], sealed with the
//! worker's own secrets key ([`Sealer`], owner wkq1) for the context
//! `worker-spool/v1:<worker>:<seq>`, so a record can't be moved into another
//! segment or another worker's spool and still open. A record is one
//! `Results` or `Counters` payload exactly as it was encoded for the wire,
//! with its batch id, when it was spooled, and the fencing tokens of the
//! leases it carries. Nothing else is written: **no peer address** (§18.7,
//! §19.6), and the payload's own sealed names stay sealed under the wire
//! key, which the worker never writes down (WK-8).
//!
//! **Reading is bounded.** Every length is checked against the bytes left
//! and against [`MAX_FRAME`] (and a record's parts against [`MAX_LEASES`]
//! and [`MAX_PAYLOAD`](crate::wire::MAX_PAYLOAD)) before anything is
//! allocated. A final frame that is cut short or fails its sum is a torn
//! write: dropped, never half-used ([`Tail::Torn`]). A bad frame with more
//! after it ends that segment there ([`Tail::Corrupt`]): what came before
//! it is used, nothing after.
//!
//! **Writing is atomic and durable.** A segment is created as `<seq>.tmp`
//! with its header, synced, renamed into place, and the folder synced; a
//! crash leaves either no segment or an empty one. Appends are synced at
//! most [`SYNC_EVERY`] apart and whenever the segment is closed; a crash
//! loses at most that much, as a torn tail. Nothing is ever renamed over a
//! file that is open, and every handle is closed before a file is
//! renamed or removed, so this holds on Windows as on Linux.
//!
//! **Caps** ([`Caps`]): at most 1 GB on disk and records at most 24 h old
//! (owner wkq1). A full spool refuses the record ([`Full`]); the worker
//! pauses fetching. A record older than the age cap is dropped and counted
//! when it is read, never replayed.
//!
//! **Removal** ([`Spool::remove`]) overwrites a segment with zeros, syncs
//! it, then unlinks it, as owner §2.17 asks of the store. The decisions
//! file says what that does and doesn't give on SSDs and on Windows.

use std::collections::{BTreeMap, HashSet};
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::hash::Hasher;
use std::io::{self, BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use lodestar_integrations::secret::Zeroizing;
use siphasher::sip::SipHasher13;

use crate::wire::MAX_PAYLOAD;

/// The segment header's magic.
pub const MAGIC: [u8; 4] = *b"LSP1";
/// The one segment version this build reads and writes.
pub const VERSION: u8 = 1;
/// The header's length: magic, version, three zero bytes, worker, seq,
/// created, sum.
pub const HEADER_LEN: usize = 4 + 1 + 3 + 4 + 8 + 8 + 8;
/// A frame's own bytes before its body: `len` and `sum`.
pub const FRAME_HEAD: usize = 4 + 8;
/// The largest sealed body a frame may hold: a record's largest plaintext
/// ([`MAX_PLAIN`], under `SecretStore::MAX_BYTES`) with the seal's
/// 45 bytes fits inside it.
pub const MAX_FRAME: usize = 1 << 20;
/// Leases one record may carry: a `Results` batch's record cap.
pub const MAX_LEASES: usize = crate::wire::results::MAX_RECORDS;
/// A record's largest plaintext.
pub const MAX_PLAIN: usize = RECORD_HEAD + MAX_LEASES * LEASE_LEN + 4 + MAX_PAYLOAD;
const RECORD_HEAD: usize = 1 + 1 + 8 + 8 + 4;
const _: () = assert!(MAX_PLAIN <= lodestar_integrations::SecretStore::MAX_BYTES);
const _: () = assert!(MAX_PLAIN + 64 <= MAX_FRAME);
const LEASE_LEN: usize = 20 + 8;
/// The record encoding's version.
const RECORD_VERSION: u8 = 1;
/// The seal context's prefix, followed by `:<worker>:<seq>`.
pub const CONTEXT: &str = "worker-spool/v1";

/// A segment is closed once it holds this many bytes: replay reads one
/// segment at a time, so this bounds what a replay holds in memory.
pub const SEGMENT_BYTES: u64 = 8 << 20;
/// A segment is closed once it is this old, so an old one can be dropped
/// whole by its age.
pub const SEGMENT_SPAN_SECS: i64 = 3600;
/// Appends are synced at most this far apart.
pub const SYNC_EVERY: Duration = Duration::from_secs(1);

/// The caps' defaults, which are also their ceilings (owner wkq1).
pub const MAX_BYTES: u64 = 1_000_000_000;
pub const MAX_AGE_SECS: i64 = 24 * 3600;
/// The caps' floors: a spool smaller than a few segments, or younger than
/// a segment's span, would drop what it was meant to keep.
pub const MIN_BYTES: u64 = 4 * SEGMENT_BYTES;
pub const MIN_AGE_SECS: i64 = SEGMENT_SPAN_SECS;

const SEGMENT_EXT: &str = "lsp";
const TEMP_EXT: &str = "tmp";
/// The checksum's key: fixed, since it only finds damage.
const SUM_KEY: (u64, u64) = (0x6c6f_6465_7374_6172, 0x7370_6f6f_6c2d_7631);

/// What a record holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u8)]
pub enum Kind {
    Results = 1,
    Counters = 2,
}

impl Kind {
    fn from_u8(v: u8) -> Option<Kind> {
        match v {
            1 => Some(Kind::Results),
            2 => Some(Kind::Counters),
            _ => None,
        }
    }
}

/// One spooled payload.
#[derive(Clone, PartialEq, Eq)]
pub struct Record {
    pub kind: Kind,
    /// The batch id it was encoded under (never 0).
    pub batch: u64,
    /// When it was spooled (unix seconds, the worker's clock).
    pub at: i64,
    /// The leases it carries: key and fencing token.
    pub leases: Vec<([u8; 20], u64)>,
    /// WK-6's encoding, as sent on the wire.
    pub payload: Vec<u8>,
}

/// `Debug` shows only sizes: no key or payload byte reaches a log.
impl fmt::Debug for Record {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Record")
            .field("kind", &self.kind)
            .field("batch", &self.batch)
            .field("at", &self.at)
            .field("leases", &self.leases.len())
            .field("payload_bytes", &self.payload.len())
            .finish()
    }
}

/// Why a record's plaintext, or a segment, didn't decode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FormatError {
    /// Shorter than its own lengths say.
    Truncated,
    /// A length over its cap, an unknown version or kind, a zero batch,
    /// or bytes left over.
    Invalid,
    /// A sum that doesn't match.
    Checksum,
}

impl fmt::Display for FormatError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            FormatError::Truncated => "truncated",
            FormatError::Invalid => "invalid",
            FormatError::Checksum => "checksum mismatch",
        })
    }
}

impl std::error::Error for FormatError {}

impl Record {
    /// The plaintext encoding (sealed before it is written).
    pub fn encode(&self) -> Result<Vec<u8>, FormatError> {
        if self.batch == 0 || self.leases.len() > MAX_LEASES || self.payload.len() > MAX_PAYLOAD {
            return Err(FormatError::Invalid);
        }
        let mut out = Vec::with_capacity(
            RECORD_HEAD + self.leases.len() * LEASE_LEN + 4 + self.payload.len(),
        );
        out.push(RECORD_VERSION);
        out.push(self.kind as u8);
        out.extend_from_slice(&self.batch.to_le_bytes());
        out.extend_from_slice(&self.at.to_le_bytes());
        out.extend_from_slice(&(self.leases.len() as u32).to_le_bytes());
        for (key, token) in &self.leases {
            out.extend_from_slice(key);
            out.extend_from_slice(&token.to_le_bytes());
        }
        out.extend_from_slice(&(self.payload.len() as u32).to_le_bytes());
        out.extend_from_slice(&self.payload);
        Ok(out)
    }

    /// Decodes a plaintext, all or nothing: every count and length is
    /// checked against its cap and the bytes left before allocating.
    pub fn decode(plain: &[u8]) -> Result<Record, FormatError> {
        if plain.len() > MAX_PLAIN {
            return Err(FormatError::Invalid);
        }
        let mut r = Cursor(plain);
        if r.u8()? != RECORD_VERSION {
            return Err(FormatError::Invalid);
        }
        let kind = Kind::from_u8(r.u8()?).ok_or(FormatError::Invalid)?;
        let batch = r.u64()?;
        if batch == 0 {
            return Err(FormatError::Invalid);
        }
        let at = r.u64()? as i64;
        let n = r.u32()? as usize;
        if n > MAX_LEASES {
            return Err(FormatError::Invalid);
        }
        if r.0.len() < n * LEASE_LEN {
            return Err(FormatError::Truncated);
        }
        let mut leases = Vec::with_capacity(n);
        for _ in 0..n {
            let key: [u8; 20] = r.take(20)?.try_into().map_err(|_| FormatError::Truncated)?;
            leases.push((key, r.u64()?));
        }
        let len = r.u32()? as usize;
        if len > MAX_PAYLOAD {
            return Err(FormatError::Invalid);
        }
        let payload = r.take(len)?.to_vec();
        if !r.0.is_empty() {
            return Err(FormatError::Invalid);
        }
        Ok(Record {
            kind,
            batch,
            at,
            leases,
            payload,
        })
    }
}

struct Cursor<'a>(&'a [u8]);

impl<'a> Cursor<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], FormatError> {
        if self.0.len() < n {
            return Err(FormatError::Truncated);
        }
        let (head, rest) = self.0.split_at(n);
        self.0 = rest;
        Ok(head)
    }
    fn u8(&mut self) -> Result<u8, FormatError> {
        Ok(self.take(1)?[0])
    }
    fn u32(&mut self) -> Result<u32, FormatError> {
        Ok(u32::from_le_bytes(
            self.take(4)?.try_into().expect("4 bytes"),
        ))
    }
    fn u64(&mut self) -> Result<u64, FormatError> {
        Ok(u64::from_le_bytes(
            self.take(8)?.try_into().expect("8 bytes"),
        ))
    }
}

fn sum(parts: &[&[u8]]) -> u64 {
    let mut h = SipHasher13::new_with_keys(SUM_KEY.0, SUM_KEY.1);
    for p in parts {
        h.write(p);
    }
    h.finish()
}

/// A segment's header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Header {
    pub worker: u32,
    pub seq: u64,
    /// When the segment was created (unix seconds).
    pub created: i64,
}

impl Header {
    pub fn encode(&self) -> [u8; HEADER_LEN] {
        let mut out = [0u8; HEADER_LEN];
        out[..4].copy_from_slice(&MAGIC);
        out[4] = VERSION;
        out[8..12].copy_from_slice(&self.worker.to_le_bytes());
        out[12..20].copy_from_slice(&self.seq.to_le_bytes());
        out[20..28].copy_from_slice(&self.created.to_le_bytes());
        let s = sum(&[&out[..28]]);
        out[28..].copy_from_slice(&s.to_le_bytes());
        out
    }

    pub fn decode(bytes: &[u8]) -> Result<Header, FormatError> {
        let Some(h) = bytes.get(..HEADER_LEN) else {
            return Err(FormatError::Truncated);
        };
        if h[..4] != MAGIC || h[4] != VERSION || h[5..8] != [0, 0, 0] {
            return Err(FormatError::Invalid);
        }
        let mut r = Cursor(&h[8..]);
        let worker = r.u32()?;
        let seq = r.u64()?;
        let created = r.u64()? as i64;
        if r.u64()? != sum(&[&h[..28]]) {
            return Err(FormatError::Checksum);
        }
        Ok(Header {
            worker,
            seq,
            created,
        })
    }
}

/// One frame around a sealed body.
pub fn encode_frame(body: &[u8]) -> Result<Vec<u8>, FormatError> {
    if body.is_empty() || body.len() > MAX_FRAME {
        return Err(FormatError::Invalid);
    }
    let len = (body.len() as u32).to_le_bytes();
    let mut out = Vec::with_capacity(FRAME_HEAD + body.len());
    out.extend_from_slice(&len);
    out.extend_from_slice(&sum(&[&len, body]).to_le_bytes());
    out.extend_from_slice(body);
    Ok(out)
}

/// How a segment's frames ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tail {
    /// Every byte was a whole, good frame.
    Clean,
    /// The final frame was cut short or failed its sum: a torn write,
    /// dropped.
    Torn,
    /// A bad frame with more bytes after it: the segment stops there.
    Corrupt,
}

/// A segment, framed but not opened.
#[derive(Debug)]
pub struct Decoded<'a> {
    pub header: Header,
    /// The sealed bodies, in order.
    pub frames: Vec<&'a [u8]>,
    pub tail: Tail,
}

/// Reads a segment's header and frames. Only the header can fail it; a
/// bad frame ends the frames ([`Tail`]).
pub fn decode_segment(bytes: &[u8]) -> Result<Decoded<'_>, FormatError> {
    let header = Header::decode(bytes)?;
    let mut rest = &bytes[HEADER_LEN..];
    let mut frames = Vec::new();
    let tail = loop {
        if rest.is_empty() {
            break Tail::Clean;
        }
        // A crash can leave a tail of zeros (a file grown before its
        // data was written): that's a torn write, not damage.
        if rest.iter().all(|&b| b == 0) {
            break Tail::Torn;
        }
        if rest.len() < FRAME_HEAD {
            break Tail::Torn;
        }
        let len_bytes: [u8; 4] = rest[..4].try_into().expect("4 bytes");
        let len = u32::from_le_bytes(len_bytes) as usize;
        if len == 0 || len > MAX_FRAME {
            break Tail::Corrupt;
        }
        if rest.len() - FRAME_HEAD < len {
            // Runs past the end: the last write didn't finish.
            break Tail::Torn;
        }
        let want = u64::from_le_bytes(rest[4..12].try_into().expect("8 bytes"));
        let body = &rest[FRAME_HEAD..FRAME_HEAD + len];
        let after = &rest[FRAME_HEAD + len..];
        if sum(&[&len_bytes, body]) != want {
            break if after.is_empty() || after.iter().all(|&b| b == 0) {
                Tail::Torn
            } else {
                Tail::Corrupt
            };
        }
        frames.push(body);
        rest = after;
    };
    Ok(Decoded {
        header,
        frames,
        tail,
    })
}

/// Seals and opens spool records with the worker's own key (owner wkq1).
/// Plaintext comes out in a buffer zeroed when dropped.
pub trait Sealer: Send + Sync {
    fn seal(&self, context: &str, plain: &[u8]) -> Result<Vec<u8>, SealError>;
    fn open(&self, context: &str, sealed: &[u8]) -> Result<Zeroizing<Vec<u8>>, SealError>;
    /// Whether `sealed` was sealed with a key other than the one
    /// [`seal`](Self::seal) uses now (a rotation is under way).
    fn is_stale(&self, sealed: &[u8]) -> bool;
}

/// A seal that failed, or one that doesn't open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SealError;

impl fmt::Display for SealError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a spool record couldn't be sealed or opened")
    }
}

impl std::error::Error for SealError {}

/// The worker's [`SecretStore`](lodestar_integrations::SecretStore) as a
/// spool sealer: its XChaCha20-Poly1305 byte seal (`seal_bytes`) under
/// its primary key, so `--rotate-key` covers the spool as it does the
/// credentials. The byte seal's associated data can't match a credential's,
/// so a spool record never opens as a credential, nor one as a record.
impl Sealer for lodestar_integrations::SecretStore {
    fn seal(&self, context: &str, plain: &[u8]) -> Result<Vec<u8>, SealError> {
        self.seal_bytes(context, plain)
            .map(|s| s.as_bytes().to_vec())
            .map_err(|_| SealError)
    }

    fn open(&self, context: &str, sealed: &[u8]) -> Result<Zeroizing<Vec<u8>>, SealError> {
        let sealed = lodestar_integrations::SealedSecret::from_bytes(sealed.to_vec())
            .map_err(|_| SealError)?;
        self.open_bytes(context, &sealed).map_err(|_| SealError)
    }

    fn is_stale(&self, sealed: &[u8]) -> bool {
        lodestar_integrations::SealedSecret::from_bytes(sealed.to_vec())
            .map_or(true, |s| self.needs_reseal(&s))
    }
}

/// The spool's caps, clamped to the owner's ceilings (1 GB, 24 h) and to
/// floors that keep it useful.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Caps {
    max_bytes: u64,
    max_age_secs: i64,
}

impl Default for Caps {
    fn default() -> Caps {
        Caps {
            max_bytes: MAX_BYTES,
            max_age_secs: MAX_AGE_SECS,
        }
    }
}

impl Caps {
    /// Caps within the bounds: a larger value is lowered to the ceiling, a
    /// smaller one raised to the floor.
    pub fn new(max_bytes: u64, max_age_secs: i64) -> Caps {
        Caps {
            max_bytes: max_bytes.clamp(MIN_BYTES, MAX_BYTES),
            max_age_secs: max_age_secs.clamp(MIN_AGE_SECS, MAX_AGE_SECS),
        }
    }
    pub fn max_bytes(&self) -> u64 {
        self.max_bytes
    }
    pub fn max_age_secs(&self) -> i64 {
        self.max_age_secs
    }
    /// For tests in other crates: caps below the floors.
    #[doc(hidden)]
    pub fn unclamped(max_bytes: u64, max_age_secs: i64) -> Caps {
        Caps {
            max_bytes,
            max_age_secs,
        }
    }
}

/// What the spool has done (the metrics, `docs/metrics.md`).
#[derive(Debug, Default)]
pub struct SpoolStats {
    /// Bytes on disk now, and records spooled and not yet removed.
    pub bytes: AtomicU64,
    pub records: AtomicU64,
    /// Records appended in all.
    pub appended: AtomicU64,
    /// Records refused because the spool was full.
    pub full: AtomicU64,
    /// Records dropped when read: older than the age cap, a torn final
    /// write, after damage in a segment, or that didn't open.
    pub dropped_old: AtomicU64,
    pub dropped_torn: AtomicU64,
    pub dropped_corrupt: AtomicU64,
    /// Segments removed once acknowledged or dropped.
    pub removed: AtomicU64,
}

fn get(n: &AtomicU64) -> u64 {
    n.load(Ordering::Relaxed)
}

/// The spool is full: the record wasn't written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Full;

/// What a spool operation can fail with.
#[derive(Debug)]
pub enum SpoolError {
    Io(&'static str, io::ErrorKind),
    Full,
    Format(FormatError),
    Seal,
}

impl fmt::Display for SpoolError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SpoolError::Io(op, kind) => write!(f, "couldn't {op} the spool: {kind}"),
            SpoolError::Full => f.write_str("the spool is full"),
            SpoolError::Format(e) => write!(f, "a spool record is {e}"),
            SpoolError::Seal => f.write_str("a spool record couldn't be sealed or opened"),
        }
    }
}

impl std::error::Error for SpoolError {}

fn io_err(op: &'static str) -> impl Fn(io::Error) -> SpoolError {
    move |e| SpoolError::Io(op, e.kind())
}

/// A segment, as [`Spool::read`] gives it.
#[derive(Debug)]
pub struct Segment {
    pub seq: u64,
    /// Its records that opened and are young enough, in order.
    pub records: Vec<Record>,
    /// Each record's frame position in the segment (0-based), so a
    /// caller can tell records apart across reads whatever was dropped.
    pub index: Vec<usize>,
    /// The segment's whole frames, good or not.
    pub frames: usize,
    /// What was damaged, by the segment's file name and the part only
    /// (never a byte of it), for the caller to report.
    pub damage: Vec<Damage>,
}

/// A damaged part of a segment: its file name and what failed (`header`,
/// `frame`, `seal` or `record`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Damage {
    pub segment: String,
    pub part: &'static str,
}

/// A closed segment's size, and its frames (the records gauge).
#[derive(Debug, Clone, Copy)]
struct Closed {
    bytes: u64,
    frames: u64,
}

struct Active {
    seq: u64,
    file: File,
    bytes: u64,
    frames: u64,
    created: i64,
    synced: Instant,
    dirty: bool,
}

/// A worker's spool folder (see the module docs). One process at a time
/// (the worker's lock), one owner within it.
pub struct Spool {
    dir: PathBuf,
    worker: u32,
    caps: Caps,
    sealer: Arc<dyn Sealer>,
    /// Closed segments, by seq.
    closed: BTreeMap<u64, Closed>,
    active: Option<Active>,
    next_seq: u64,
    stats: Arc<SpoolStats>,
    /// Segments whose drops were counted already: a segment read again
    /// (after a lost session) counts nothing twice.
    counted: HashSet<u64>,
    /// Records counted as too old, by segment and frame.
    old: HashSet<(u64, usize)>,
}

impl fmt::Debug for Spool {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Spool")
            .field("dir", &self.dir)
            .field("worker", &self.worker)
            .field("segments", &self.closed.len())
            .field("bytes", &get(&self.stats.bytes))
            .finish_non_exhaustive()
    }
}

fn segment_name(seq: u64, ext: &str) -> String {
    format!("{seq:016x}.{ext}")
}

fn parse_name(name: &str) -> Option<(u64, &str)> {
    let (stem, ext) = name.split_once('.')?;
    if stem.len() != 16 {
        return None;
    }
    Some((u64::from_str_radix(stem, 16).ok()?, ext))
}

/// Makes a rename or a removal in `dir` durable (Unix; Windows' rename and
/// delete are durable once they return on NTFS's journal).
fn sync_dir(dir: &Path) -> Result<(), SpoolError> {
    #[cfg(unix)]
    File::open(dir)
        .and_then(|d| d.sync_all())
        .map_err(io_err("sync the folder of"))?;
    #[cfg(not(unix))]
    let _ = dir;
    Ok(())
}

/// Overwrites `path` with zeros, syncs it, closes it, then removes it
/// (owner §2.17). A file that is already gone is fine.
pub fn secure_remove(path: &Path) -> Result<(), SpoolError> {
    let mut file = match OpenOptions::new().write(true).open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(SpoolError::Io("overwrite a segment of", e.kind())),
    };
    let len = file
        .metadata()
        .map_err(io_err("overwrite a segment of"))?
        .len();
    let zeros = vec![0u8; 64 << 10];
    let mut left = len;
    while left > 0 {
        let n = left.min(zeros.len() as u64) as usize;
        file.write_all(&zeros[..n])
            .map_err(io_err("overwrite a segment of"))?;
        left -= n as u64;
    }
    file.sync_all().map_err(io_err("overwrite a segment of"))?;
    drop(file);
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(SpoolError::Io("remove a segment of", e.kind())),
    }
}

/// Writes `bytes` as `<dir>/<seq>.lsp` atomically: a temp file, synced,
/// renamed into place, the folder synced. Never renames over an open file.
fn write_atomic(dir: &Path, seq: u64, bytes: &[u8]) -> Result<(), SpoolError> {
    let tmp = dir.join(segment_name(seq, TEMP_EXT));
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(&tmp)
        .map_err(io_err("create a segment in"))?;
    file.write_all(bytes)
        .and_then(|()| file.sync_all())
        .map_err(io_err("write a segment in"))?;
    drop(file);
    fs::rename(&tmp, dir.join(segment_name(seq, SEGMENT_EXT)))
        .map_err(io_err("rename a segment in"))?;
    sync_dir(dir)
}

impl Spool {
    /// Opens (creating) the spool in `dir` for `worker`. Leftover temp
    /// files are removed; a segment of another worker (a folder that joined
    /// again under a new id) or with a header that doesn't read is dropped:
    /// its tokens are no other worker's to replay, and its seal wouldn't
    /// open for this one.
    pub fn open(
        dir: &Path,
        worker: u32,
        caps: Caps,
        sealer: Arc<dyn Sealer>,
    ) -> Result<Spool, SpoolError> {
        fs::create_dir_all(dir).map_err(io_err("create the folder of"))?;
        let stats = Arc::new(SpoolStats::default());
        let mut closed = BTreeMap::new();
        let mut next_seq = 1;
        for entry in fs::read_dir(dir).map_err(io_err("list"))? {
            let entry = entry.map_err(io_err("list"))?;
            let name = entry.file_name();
            let Some((seq, ext)) = name.to_str().and_then(parse_name) else {
                continue;
            };
            let path = entry.path();
            next_seq = next_seq.max(seq.saturating_add(1));
            match ext {
                TEMP_EXT => secure_remove(&path)?,
                SEGMENT_EXT => {
                    let mut head = [0u8; HEADER_LEN];
                    let read = File::open(&path).and_then(|mut f| f.read_exact(&mut head));
                    let ours = read.is_ok()
                        && Header::decode(&head).is_ok_and(|h| h.worker == worker && h.seq == seq);
                    if ours {
                        let len = entry.metadata().map_err(io_err("list"))?.len();
                        let frames = count_frames(&path).unwrap_or(0);
                        closed.insert(seq, Closed { bytes: len, frames });
                    } else {
                        secure_remove(&path)?;
                        bump(&stats.dropped_corrupt);
                        bump(&stats.removed);
                    }
                }
                _ => {}
            }
        }
        sync_dir(dir)?;
        stats
            .bytes
            .store(closed.values().map(|c| c.bytes).sum(), Ordering::Relaxed);
        stats
            .records
            .store(closed.values().map(|c| c.frames).sum(), Ordering::Relaxed);
        Ok(Spool {
            dir: dir.to_path_buf(),
            worker,
            caps,
            sealer,
            closed,
            active: None,
            next_seq,
            stats,
            counted: HashSet::new(),
            old: HashSet::new(),
        })
    }

    pub fn stats(&self) -> Arc<SpoolStats> {
        self.stats.clone()
    }

    pub fn caps(&self) -> Caps {
        self.caps
    }

    /// Bytes on disk now.
    pub fn bytes(&self) -> u64 {
        get(&self.stats.bytes)
    }

    /// Whether nothing is spooled.
    pub fn is_empty(&self) -> bool {
        self.closed.is_empty() && self.active.is_none()
    }

    /// Whether a record of `len` payload bytes would fit now.
    pub fn has_room(&self, len: usize) -> bool {
        self.bytes() + Self::frame_estimate(len) <= self.caps.max_bytes
    }

    fn frame_estimate(len: usize) -> u64 {
        // The seal's overhead, the frame and a header.
        (FRAME_HEAD + 64 + RECORD_HEAD + len + HEADER_LEN) as u64
    }

    fn context(&self, seq: u64) -> String {
        format!("{CONTEXT}:{}:{seq}", self.worker)
    }

    /// Appends `record` at `now`: sealed, framed, written; synced if the
    /// last sync is [`SYNC_EVERY`] old. A record that would pass the byte
    /// cap is refused ([`SpoolError::Full`], counted).
    pub fn append(&mut self, record: &Record, now: i64) -> Result<(), SpoolError> {
        let plain = Zeroizing::new(record.encode().map_err(SpoolError::Format)?);
        let roll = self.active.as_ref().is_some_and(|a| {
            a.bytes >= SEGMENT_BYTES || now.saturating_sub(a.created) >= SEGMENT_SPAN_SECS
        });
        if roll {
            self.close_active()?;
        }
        let seq = match &self.active {
            Some(a) => a.seq,
            None => self.next_seq,
        };
        let sealed = self
            .sealer
            .seal(&self.context(seq), &plain)
            .map_err(|_| SpoolError::Seal)?;
        let frame = encode_frame(&sealed).map_err(SpoolError::Format)?;
        let new_segment = if self.active.is_none() {
            HEADER_LEN as u64
        } else {
            0
        };
        if self.bytes() + new_segment + frame.len() as u64 > self.caps.max_bytes {
            bump(&self.stats.full);
            return Err(SpoolError::Full);
        }
        if self.active.is_none() {
            self.start_segment(now)?;
        }
        let active = self.active.as_mut().expect("a segment was started");
        if let Err(e) = active.file.write_all(&frame) {
            // A partial write leaves a torn tail, which a read drops; the
            // segment is closed so nothing is appended after it.
            let _ = active.file.sync_all();
            let bytes = active.file.metadata().map_or(active.bytes, |m| m.len());
            self.stats
                .bytes
                .fetch_add(bytes.saturating_sub(active.bytes), Ordering::Relaxed);
            active.bytes = bytes;
            self.close_active()?;
            return Err(SpoolError::Io("append to", e.kind()));
        }
        active.bytes += frame.len() as u64;
        active.frames += 1;
        active.dirty = true;
        self.stats
            .bytes
            .fetch_add(frame.len() as u64, Ordering::Relaxed);
        bump(&self.stats.records);
        bump(&self.stats.appended);
        if active.synced.elapsed() >= SYNC_EVERY {
            self.sync()?;
        }
        Ok(())
    }

    fn start_segment(&mut self, now: i64) -> Result<(), SpoolError> {
        let seq = self.next_seq;
        let header = Header {
            worker: self.worker,
            seq,
            created: now,
        };
        write_atomic(&self.dir, seq, &header.encode())?;
        self.next_seq += 1;
        let file = OpenOptions::new()
            .append(true)
            .open(self.dir.join(segment_name(seq, SEGMENT_EXT)))
            .map_err(io_err("open a segment of"))?;
        self.stats
            .bytes
            .fetch_add(HEADER_LEN as u64, Ordering::Relaxed);
        self.active = Some(Active {
            seq,
            file,
            bytes: HEADER_LEN as u64,
            frames: 0,
            created: now,
            synced: Instant::now(),
            dirty: false,
        });
        Ok(())
    }

    /// Syncs what has been appended.
    pub fn sync(&mut self) -> Result<(), SpoolError> {
        if let Some(a) = &mut self.active
            && a.dirty
        {
            a.file.sync_data().map_err(io_err("sync"))?;
            a.dirty = false;
            a.synced = Instant::now();
        }
        Ok(())
    }

    /// Syncs and closes the segment being appended to, so it can be read
    /// and removed.
    pub fn close_active(&mut self) -> Result<(), SpoolError> {
        self.sync()?;
        if let Some(a) = self.active.take() {
            self.closed.insert(
                a.seq,
                Closed {
                    bytes: a.bytes,
                    frames: a.frames,
                },
            );
        }
        Ok(())
    }

    /// The closed segments, oldest first.
    pub fn segments(&self) -> Vec<u64> {
        self.closed.keys().copied().collect()
    }

    /// Reads a closed segment at `now`: its records that open and are
    /// younger than the age cap, in order, with their frame positions. The
    /// rest are counted, the first time this segment is read only: a torn
    /// final write, damage (the segment stops there; reported by name
    /// only), a record that doesn't open, or one too old.
    pub fn read(&mut self, seq: u64, now: i64) -> Result<Segment, SpoolError> {
        let path = self.dir.join(segment_name(seq, SEGMENT_EXT));
        let bytes = read_capped(&path, SEGMENT_BYTES + MAX_FRAME as u64 + FRAME_HEAD as u64)?;
        let count = self.counted.insert(seq);
        let mut seg = Segment {
            seq,
            records: Vec::new(),
            index: Vec::new(),
            frames: 0,
            damage: Vec::new(),
        };
        let decoded = match decode_segment(&bytes) {
            Ok(d) if d.header.worker == self.worker && d.header.seq == seq => d,
            _ => {
                self.damaged(seq, "header", count, &mut seg.damage);
                return Ok(seg);
            }
        };
        seg.frames = decoded.frames.len();
        match decoded.tail {
            Tail::Clean => {}
            Tail::Torn if count => bump(&self.stats.dropped_torn),
            Tail::Torn => {}
            Tail::Corrupt => self.damaged(seq, "frame", count, &mut seg.damage),
        }
        let oldest = now.saturating_sub(self.caps.max_age_secs);
        let context = self.context(seq);
        for (i, frame) in decoded.frames.into_iter().enumerate() {
            let Ok(plain) = self.sealer.open(&context, frame) else {
                self.damaged(seq, "seal", count, &mut seg.damage);
                continue;
            };
            match Record::decode(&plain[..]) {
                Ok(r) if r.at < oldest => {
                    // Once per record, even one that turned old between
                    // two reads.
                    if self.old.insert((seq, i)) {
                        bump(&self.stats.dropped_old);
                    }
                }
                Ok(r) => {
                    seg.records.push(r);
                    seg.index.push(i);
                }
                Err(_) => self.damaged(seq, "record", count, &mut seg.damage),
            }
        }
        Ok(seg)
    }

    fn damaged(&self, seq: u64, part: &'static str, count: bool, out: &mut Vec<Damage>) {
        if count {
            bump(&self.stats.dropped_corrupt);
        }
        out.push(Damage {
            segment: segment_name(seq, SEGMENT_EXT),
            part,
        });
    }

    /// Whether every record in closed segment `seq` is older than the age
    /// cap at `now`, by its header alone: created before the cap less a
    /// segment's span (a segment takes appends for at most that long).
    pub fn is_expired(&self, seq: u64, now: i64) -> bool {
        let path = self.dir.join(segment_name(seq, SEGMENT_EXT));
        let mut head = [0u8; HEADER_LEN];
        let Ok(()) = File::open(path).and_then(|mut f| f.read_exact(&mut head)) else {
            return false;
        };
        Header::decode(&head).is_ok_and(|h| {
            h.created.saturating_add(SEGMENT_SPAN_SECS) < now.saturating_sub(self.caps.max_age_secs)
        })
    }

    /// Removes closed segment `seq` securely (overwrite, sync, unlink).
    /// It leaves the counts only once it's gone: a removal that fails
    /// (a file another program holds open on Windows) keeps it, to be
    /// tried again.
    pub fn remove(&mut self, seq: u64) -> Result<(), SpoolError> {
        if !self.closed.contains_key(&seq) {
            return Ok(());
        }
        secure_remove(&self.dir.join(segment_name(seq, SEGMENT_EXT)))?;
        sync_dir(&self.dir)?;
        self.forget(seq);
        bump(&self.stats.removed);
        Ok(())
    }

    /// Removes closed segment `seq` as wholly past the age cap
    /// ([`is_expired`](Self::is_expired)): its records not counted as old
    /// yet are, without opening any.
    pub fn drop_expired(&mut self, seq: u64) -> Result<(), SpoolError> {
        let Some(c) = self.closed.get(&seq).copied() else {
            return Ok(());
        };
        let counted = self.old.iter().filter(|&&(s, _)| s == seq).count() as u64;
        self.remove(seq)?;
        self.stats
            .dropped_old
            .fetch_add(c.frames.saturating_sub(counted), Ordering::Relaxed);
        Ok(())
    }

    /// Stops tracking closed segment `seq` without touching the disk: one
    /// that can't be read or removed now is left for the next start, so it
    /// never holds up the segments after it.
    pub fn forget(&mut self, seq: u64) {
        if let Some(c) = self.closed.remove(&seq) {
            self.counted.remove(&seq);
            self.old.retain(|&(s, _)| s != seq);
            let sub = |n: &AtomicU64, by: u64| {
                let _ = n.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |v| {
                    Some(v.saturating_sub(by))
                });
            };
            sub(&self.stats.bytes, c.bytes);
            sub(&self.stats.records, c.frames);
        }
    }

    /// Re-seals every segment with the sealer's current key (a secrets-key
    /// rotation, before the old key is retired): each segment whose records
    /// aren't all under the current key is rewritten whole under a temp
    /// name, synced and renamed over the old one, which is closed. A record
    /// that doesn't open is left out (counted). Returns the segments
    /// rewritten. The worker isn't running (its lock is held).
    pub fn reseal(&mut self) -> Result<usize, SpoolError> {
        self.close_active()?;
        let mut rewritten = 0;
        for seq in self.segments() {
            let path = self.dir.join(segment_name(seq, SEGMENT_EXT));
            let bytes = read_capped(&path, SEGMENT_BYTES + MAX_FRAME as u64 + FRAME_HEAD as u64)?;
            let Ok(decoded) = decode_segment(&bytes) else {
                continue;
            };
            if decoded.frames.iter().all(|f| !self.sealer.is_stale(f)) {
                continue;
            }
            let context = self.context(seq);
            let mut out = decoded.header.encode().to_vec();
            let mut frames = 0;
            for frame in &decoded.frames {
                let Ok(plain) = self.sealer.open(&context, frame) else {
                    bump(&self.stats.dropped_corrupt);
                    continue;
                };
                let sealed = self
                    .sealer
                    .seal(&context, &plain[..])
                    .map_err(|_| SpoolError::Seal)?;
                out.extend_from_slice(&encode_frame(&sealed).map_err(SpoolError::Format)?);
                frames += 1;
            }
            // Renamed over the old file, whose blocks aren't zeroed: what
            // they hold is sealed under the key about to be retired.
            write_atomic(&self.dir, seq, &out)?;
            self.forget(seq);
            self.closed.insert(
                seq,
                Closed {
                    bytes: out.len() as u64,
                    frames,
                },
            );
            self.stats
                .bytes
                .fetch_add(out.len() as u64, Ordering::Relaxed);
            self.stats.records.fetch_add(frames, Ordering::Relaxed);
            rewritten += 1;
        }
        Ok(rewritten)
    }
}

/// Reads a whole file of at most `cap` bytes (a segment can't be larger:
/// it closes past [`SEGMENT_BYTES`] after one frame). A larger one is read
/// up to the cap, which ends it as a torn or damaged tail.
fn read_capped(path: &Path, cap: u64) -> Result<Vec<u8>, SpoolError> {
    let file = File::open(path).map_err(io_err("read a segment of"))?;
    let len = file.metadata().map_err(io_err("read a segment of"))?.len();
    let mut out = Vec::with_capacity(len.min(cap) as usize);
    file.take(cap)
        .read_to_end(&mut out)
        .map_err(io_err("read a segment of"))?;
    Ok(out)
}

/// Counts a segment's frames by their lengths alone (no sum checked, no
/// body read), for the records gauge at open.
fn count_frames(path: &Path) -> io::Result<u64> {
    let file = File::open(path)?;
    let len = file.metadata()?.len();
    let mut r = BufReader::new(file);
    r.seek(SeekFrom::Start(HEADER_LEN as u64))?;
    let mut at = HEADER_LEN as u64;
    let mut n = 0;
    let mut head = [0u8; FRAME_HEAD];
    while at + FRAME_HEAD as u64 <= len {
        r.read_exact(&mut head)?;
        let body = u64::from(u32::from_le_bytes(head[..4].try_into().expect("4 bytes")));
        if body == 0 || body > MAX_FRAME as u64 || at + FRAME_HEAD as u64 + body > len {
            break;
        }
        r.seek_relative(body as i64)?;
        at += FRAME_HEAD as u64 + body;
        n += 1;
    }
    Ok(n)
}

fn bump(n: &AtomicU64) {
    n.fetch_add(1, Ordering::Relaxed);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_header_round_trips_and_its_sum_is_checked() {
        let h = Header {
            worker: 7,
            seq: 3,
            created: 1_700_000_000,
        };
        let mut bytes = h.encode();
        assert_eq!(Header::decode(&bytes), Ok(h));
        bytes[13] ^= 1;
        assert_eq!(Header::decode(&bytes), Err(FormatError::Checksum));
        bytes[13] ^= 1;
        bytes[4] = 2;
        assert_eq!(Header::decode(&bytes), Err(FormatError::Invalid));
        assert_eq!(Header::decode(&bytes[..10]), Err(FormatError::Truncated));
    }

    #[test]
    fn caps_stay_within_the_owners_bounds() {
        let c = Caps::new(u64::MAX, i64::MAX);
        assert_eq!((c.max_bytes(), c.max_age_secs()), (MAX_BYTES, MAX_AGE_SECS));
        let c = Caps::new(0, 0);
        assert_eq!((c.max_bytes(), c.max_age_secs()), (MIN_BYTES, MIN_AGE_SECS));
        assert_eq!(Caps::default(), Caps::new(MAX_BYTES, MAX_AGE_SECS));
    }

    #[test]
    fn a_record_refuses_lengths_over_its_caps_before_allocating() {
        let mut plain = vec![RECORD_VERSION, 1];
        plain.extend_from_slice(&1u64.to_le_bytes());
        plain.extend_from_slice(&0i64.to_le_bytes());
        plain.extend_from_slice(&u32::MAX.to_le_bytes());
        assert_eq!(Record::decode(&plain[..]), Err(FormatError::Invalid));
        let mut plain = vec![RECORD_VERSION, 1];
        plain.extend_from_slice(&1u64.to_le_bytes());
        plain.extend_from_slice(&0i64.to_le_bytes());
        plain.extend_from_slice(&0u32.to_le_bytes());
        plain.extend_from_slice(&u32::MAX.to_le_bytes());
        assert_eq!(Record::decode(&plain[..]), Err(FormatError::Invalid));
        plain.truncate(plain.len() - 4);
        plain.extend_from_slice(&10u32.to_le_bytes());
        assert_eq!(Record::decode(&plain[..]), Err(FormatError::Truncated));
    }
}
