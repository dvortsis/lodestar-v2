//! Crash-safe restart proof for BEP 51 sampling (CR-0d).
//!
//! The design, its state machine and every crash outcome are in
//! `docs/decisions/2026-10-30-CR-0d.md`; section numbers below refer to it.
//! This module is the on-disk half: the three file formats (§1.2, §1.3),
//! the two write paths (§1.6), the lock (§2.2), the clock helpers (§2.1,
//! I4) and validation with seeding (§2.1). The actor drives the sequence.
//!
//! Every file here is untrusted input. Decoding never panics, rejects
//! anything that isn't exactly what we write, and every failure means the
//! caller suppresses: nothing here can make a restriction shorter.

use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read as _, Seek, SeekFrom, Write};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
// The actor's clock: the same as std's unless a test pauses it, so
// restored deadlines run on the clock the sampler keeps. Re-exported so
// callers (the fuzz target) don't need tokio themselves.
pub use tokio::time::Instant;

use lodestar_bencode::{self as bencode, DecodeOptions, Encoder, Value};
use sha2::{Digest, Sha256};

pub const VERSION: u32 = 1;
pub const LOCK_FILE: &str = "dht-restart.lock";
pub const MARKER_FILE: &str = "dht-restart.mark";
pub const SNAPSHOT_FILE: &str = "dht-restart.bin";
/// The sampler's table bound (`sampler::MAX_KNOWN` is defined from this):
/// a snapshot never holds more. Seeding must insert every restored entry
/// or fail to Suppress; it may never drop one to make room.
pub const MAX_ENTRIES: usize = 200_000;
/// A decode bound only; validation requires the current `H_s` exactly.
pub const MAX_HORIZON_S: u32 = 86_400;
/// Generations stay far below `i64::MAX`, so `+ 1` never overflows.
const MAX_GEN: u64 = 1 << 62;
/// Orphan cleanup looks at no more than this many witness files.
const MAX_WITNESS_SCAN: usize = 256;

const MAGIC_SNAPSHOT: &[u8; 8] = b"LDSRSNAP";
const MAGIC_MARKER: &[u8; 8] = b"LDSRMARK";
const MAGIC_WITNESS: &[u8; 8] = b"LDSRWITN";
const HEADER: usize = 16;
const TRAILER: usize = 32;
const MAX_SNAPSHOT_BODY: usize = 8 << 20;
const MAX_SMALL_BODY: usize = 4 << 10;
const R4: usize = 4 + 2 + 4;
const R6: usize = 16 + 2 + 4;

pub type Digest32 = [u8; 32];
pub type Id16 = [u8; 16];

/// Why a file was rejected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fault {
    Corrupt(&'static str),
    Version(u32),
}

impl fmt::Display for Fault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Fault::Corrupt(what) => write!(f, "corrupt ({what})"),
            Fault::Version(v) => write!(f, "unsupported version {v}"),
        }
    }
}

// ---- Container (§1.2) -------------------------------------------------------

/// magic ‖ version ‖ len ‖ body ‖ SHA-256 of everything before it.
fn seal(magic: &[u8; 8], body: &[u8]) -> Vec<u8> {
    let len = u32::try_from(body.len()).expect("bodies are bounded far below 4 GiB");
    let mut out = Vec::with_capacity(HEADER + body.len() + TRAILER);
    out.extend_from_slice(magic);
    out.extend_from_slice(&VERSION.to_le_bytes());
    out.extend_from_slice(&len.to_le_bytes());
    out.extend_from_slice(body);
    let sum = Sha256::digest(&out);
    out.extend_from_slice(&sum);
    out
}

/// The body of a sealed file, and its checksum.
fn open<'a>(
    magic: &[u8; 8],
    max_body: usize,
    bytes: &'a [u8],
) -> Result<(&'a [u8], Digest32), Fault> {
    if bytes.len() < HEADER + TRAILER {
        return Err(Fault::Corrupt("short"));
    }
    if &bytes[..8] != magic {
        return Err(Fault::Corrupt("magic"));
    }
    let version = u32::from_le_bytes(bytes[8..12].try_into().expect("4 bytes"));
    if version != VERSION {
        return Err(Fault::Version(version));
    }
    let len = u32::from_le_bytes(bytes[12..16].try_into().expect("4 bytes")) as usize;
    if len > max_body || bytes.len() - HEADER - TRAILER != len {
        return Err(Fault::Corrupt("length"));
    }
    let (sealed, sum) = bytes.split_at(HEADER + len);
    if Sha256::digest(sealed).as_slice() != sum {
        return Err(Fault::Corrupt("checksum"));
    }
    Ok((&sealed[HEADER..], sum.try_into().expect("32 bytes")))
}

/// A strictly decoded body: one flat dictionary with exactly `keys`.
struct Body<'a> {
    dict: bencode::Dict<'a>,
}

impl<'a> Body<'a> {
    fn parse(body: &'a [u8], keys: &[&[u8]]) -> Result<Body<'a>, Fault> {
        let limits = bencode::Limits {
            max_depth: 1,
            max_items: 16,
        };
        let value = bencode::decode_with(body, &DecodeOptions::strict().with_limits(limits))
            .map_err(|_| Fault::Corrupt("not canonical bencode"))?;
        let Value::Dict(dict) = value else {
            return Err(Fault::Corrupt("not a dictionary"));
        };
        if dict.len() != keys.len() || dict.iter().any(|(k, _)| !keys.contains(&k)) {
            return Err(Fault::Corrupt("keys"));
        }
        Ok(Body { dict })
    }

    fn int(&self, key: &'static str, max: u64) -> Result<u64, Fault> {
        match self.dict.get(key.as_bytes()).and_then(Value::as_int) {
            Some(v) if v >= 0 && v as u64 <= max => Ok(v as u64),
            _ => Err(Fault::Corrupt(key)),
        }
    }

    fn bytes(&self, key: &'static str) -> Result<&'a [u8], Fault> {
        self.dict
            .get(key.as_bytes())
            .and_then(Value::as_bytes)
            .ok_or(Fault::Corrupt(key))
    }

    fn fixed<const N: usize>(&self, key: &'static str) -> Result<[u8; N], Fault> {
        self.bytes(key)?.try_into().map_err(|_| Fault::Corrupt(key))
    }
}

/// Saturates: decoders accept up to `i64::MAX`, so even an absurd clock
/// reading encodes, and validation then rejects it.
fn int(v: u64) -> i64 {
    i64::try_from(v).unwrap_or(i64::MAX)
}

// ---- Bodies (§1.3) ----------------------------------------------------------

/// Fields every marker and snapshot of one run share.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RunHeader {
    pub generation: u64,
    pub dir_id: Id16,
    /// SHA-256 of the virtual ids ([`ids_digest`]).
    pub ids: Digest32,
    pub run_id: Id16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MarkerState {
    /// `snap` is the snapshot's container checksum; `state_bin` is the
    /// SHA-256 of `dht-state.bin` as saved in the same shutdown.
    Clean {
        snap: Digest32,
        state_bin: Digest32,
    },
    Dirty,
    /// Written by a run with the switch off (§5.2).
    Off,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Marker {
    pub state: MarkerState,
    pub run: RunHeader,
    /// SHA-256 of the canonical data-directory path ([`path_digest`]).
    pub dir_path: Digest32,
    pub at_ms: u64,
}

const MARKER_KEYS: &[&[u8]] = &[
    b"at",
    b"dir_id",
    b"dir_path",
    b"gen",
    b"ids",
    b"run_id",
    b"state",
];
const CLEAN_KEYS: &[&[u8]] = &[
    b"at",
    b"dir_id",
    b"dir_path",
    b"gen",
    b"ids",
    b"run_id",
    b"snap",
    b"state",
    b"state_bin",
];

impl Marker {
    pub fn encode(&self) -> Vec<u8> {
        let mut body = Vec::new();
        Encoder::new(&mut body).dict(|d| {
            d.entry(b"at").int(int(self.at_ms));
            d.entry(b"dir_id").bytes(&self.run.dir_id);
            d.entry(b"dir_path").bytes(&self.dir_path);
            d.entry(b"gen").int(int(self.run.generation));
            d.entry(b"ids").bytes(&self.run.ids);
            d.entry(b"run_id").bytes(&self.run.run_id);
            if let MarkerState::Clean { snap, .. } = &self.state {
                d.entry(b"snap").bytes(snap);
            }
            d.entry(b"state").bytes(match self.state {
                MarkerState::Clean { .. } => b"clean",
                MarkerState::Dirty => b"dirty",
                MarkerState::Off => b"off",
            });
            if let MarkerState::Clean { state_bin, .. } = &self.state {
                d.entry(b"state_bin").bytes(state_bin);
            }
        });
        seal(MAGIC_MARKER, &body)
    }

    pub fn decode(bytes: &[u8]) -> Result<Marker, Fault> {
        let (body, _) = open(MAGIC_MARKER, MAX_SMALL_BODY, bytes)?;
        // The state decides the key set, so peek at it first.
        let clean = Body::parse(body, CLEAN_KEYS);
        let b = match clean {
            Ok(b) => b,
            Err(_) => Body::parse(body, MARKER_KEYS)?,
        };
        let state = match (b.bytes("state")?, b.dict.len() == CLEAN_KEYS.len()) {
            (b"clean", true) => MarkerState::Clean {
                snap: b.fixed("snap")?,
                state_bin: b.fixed("state_bin")?,
            },
            (b"dirty", false) => MarkerState::Dirty,
            (b"off", false) => MarkerState::Off,
            _ => return Err(Fault::Corrupt("state")),
        };
        Ok(Marker {
            state,
            run: RunHeader {
                generation: b.int("gen", MAX_GEN)?,
                dir_id: b.fixed("dir_id")?,
                ids: b.fixed("ids")?,
                run_id: b.fixed("run_id")?,
            },
            dir_path: b.fixed("dir_path")?,
            at_ms: b.int("at", i64::MAX as u64)?,
        })
    }
}

/// One restriction: no sample to `addr` before `snapshot.at + remaining_s`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Entry {
    pub addr: SocketAddr,
    pub remaining_s: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    pub run: RunHeader,
    pub at_ms: u64,
    pub horizon_s: u32,
    /// No sample to anyone before `at + gate_s` (N1).
    pub gate_s: u32,
    /// Sorted by family, then address bytes; no duplicates.
    pub entries: Vec<Entry>,
}

const SNAPSHOT_KEYS: &[&[u8]] = &[
    b"at", b"dir_id", b"gate", b"gen", b"horizon", b"ids", b"r4", b"r6", b"run_id",
];

/// The sort key: the address bytes as written, so byte order is numeric.
fn entry_key(addr: &SocketAddr) -> (bool, Vec<u8>) {
    let mut key = match addr.ip() {
        IpAddr::V4(ip) => ip.octets().to_vec(),
        IpAddr::V6(ip) => ip.octets().to_vec(),
    };
    key.extend_from_slice(&addr.port().to_be_bytes());
    (addr.is_ipv6(), key)
}

impl Snapshot {
    pub fn encode(&self) -> Vec<u8> {
        let mut r4 = Vec::new();
        let mut r6 = Vec::new();
        for e in &self.entries {
            let (v6, key) = entry_key(&e.addr);
            let out = if v6 { &mut r6 } else { &mut r4 };
            out.extend_from_slice(&key);
            out.extend_from_slice(&e.remaining_s.to_be_bytes());
        }
        let mut body = Vec::with_capacity(r4.len() + r6.len() + 256);
        Encoder::new(&mut body).dict(|d| {
            d.entry(b"at").int(int(self.at_ms));
            d.entry(b"dir_id").bytes(&self.run.dir_id);
            d.entry(b"gate").int(i64::from(self.gate_s));
            d.entry(b"gen").int(int(self.run.generation));
            d.entry(b"horizon").int(i64::from(self.horizon_s));
            d.entry(b"ids").bytes(&self.run.ids);
            d.entry(b"r4").bytes(&r4);
            d.entry(b"r6").bytes(&r6);
            d.entry(b"run_id").bytes(&self.run.run_id);
        });
        seal(MAGIC_SNAPSHOT, &body)
    }

    /// The snapshot and its container checksum (what a clean marker's
    /// `snap` names).
    pub fn decode(bytes: &[u8]) -> Result<(Snapshot, Digest32), Fault> {
        let (body, sum) = open(MAGIC_SNAPSHOT, MAX_SNAPSHOT_BODY, bytes)?;
        let b = Body::parse(body, SNAPSHOT_KEYS)?;
        let horizon_s = b.int("horizon", u64::from(MAX_HORIZON_S))? as u32;
        if horizon_s == 0 {
            return Err(Fault::Corrupt("horizon"));
        }
        let gate_s = b.int("gate", u64::from(horizon_s))? as u32;
        let (r4, r6) = (b.bytes("r4")?, b.bytes("r6")?);
        if r4.len() % R4 != 0 || r6.len() % R6 != 0 {
            return Err(Fault::Corrupt("entry length"));
        }
        if r4.len() / R4 + r6.len() / R6 > MAX_ENTRIES {
            return Err(Fault::Corrupt("too many entries"));
        }
        let mut entries = Vec::with_capacity(r4.len() / R4 + r6.len() / R6);
        decode_entries(r4, 4, horizon_s, &mut entries)?;
        decode_entries(r6, 16, horizon_s, &mut entries)?;
        let snapshot = Snapshot {
            run: RunHeader {
                generation: b.int("gen", MAX_GEN)?,
                dir_id: b.fixed("dir_id")?,
                ids: b.fixed("ids")?,
                run_id: b.fixed("run_id")?,
            },
            at_ms: b.int("at", i64::MAX as u64)?,
            horizon_s,
            gate_s,
            entries,
        };
        Ok((snapshot, sum))
    }
}

fn decode_entries(
    raw: &[u8],
    ip_len: usize,
    horizon_s: u32,
    out: &mut Vec<Entry>,
) -> Result<(), Fault> {
    let mut prev: Option<&[u8]> = None;
    for chunk in raw.chunks_exact(ip_len + 6) {
        let (key, rem) = chunk.split_at(ip_len + 2);
        // Strictly increasing: sorted, and no duplicates.
        if prev.is_some_and(|p| p >= key) {
            return Err(Fault::Corrupt("entry order"));
        }
        prev = Some(key);
        let port = u16::from_be_bytes([key[ip_len], key[ip_len + 1]]);
        let remaining_s = u32::from_be_bytes(rem.try_into().expect("4 bytes"));
        if port == 0 {
            return Err(Fault::Corrupt("entry port"));
        }
        if remaining_s == 0 || remaining_s > horizon_s {
            return Err(Fault::Corrupt("entry remaining"));
        }
        let ip = if ip_len == 4 {
            IpAddr::V4(Ipv4Addr::from(
                <[u8; 4]>::try_from(&key[..4]).expect("4 bytes"),
            ))
        } else {
            let ip = Ipv6Addr::from(<[u8; 16]>::try_from(&key[..16]).expect("16 bytes"));
            // We normalise before writing, so this was written by someone else.
            if ip.to_ipv4_mapped().is_some() {
                return Err(Fault::Corrupt("entry mapped"));
            }
            IpAddr::V6(ip)
        };
        out.push(Entry {
            addr: SocketAddr::new(ip, port),
            remaining_s,
        });
    }
    Ok(())
}

/// The last generation that ran on one data directory, kept outside it (§1.5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Witness {
    pub generation: u64,
    pub dir_id: Id16,
    pub at_ms: u64,
    /// The canonical data-directory path in full (N7).
    pub path: String,
}

const WITNESS_KEYS: &[&[u8]] = &[b"at", b"dir_id", b"gen", b"path"];

impl Witness {
    pub fn encode(&self) -> Vec<u8> {
        let mut body = Vec::new();
        Encoder::new(&mut body).dict(|d| {
            d.entry(b"at").int(int(self.at_ms));
            d.entry(b"dir_id").bytes(&self.dir_id);
            d.entry(b"gen").int(int(self.generation));
            d.entry(b"path").bytes(self.path.as_bytes());
        });
        seal(MAGIC_WITNESS, &body)
    }

    pub fn decode(bytes: &[u8]) -> Result<Witness, Fault> {
        let (body, _) = open(MAGIC_WITNESS, MAX_SMALL_BODY, bytes)?;
        let b = Body::parse(body, WITNESS_KEYS)?;
        let path = std::str::from_utf8(b.bytes("path")?).map_err(|_| Fault::Corrupt("path"))?;
        Ok(Witness {
            generation: b.int("gen", MAX_GEN)?,
            dir_id: b.fixed("dir_id")?,
            at_ms: b.int("at", i64::MAX as u64)?,
            path: path.to_owned(),
        })
    }

    /// Whether a witness for `path` fits the format at all; a longer
    /// path means the run has no witness (§1.5).
    pub fn fits(path: &str) -> bool {
        path.len() <= MAX_SMALL_BODY - 128
    }
}

/// SHA-256 of the virtual ids, sorted and concatenated (§1.3).
pub fn ids_digest(ids: &[crate::NodeId]) -> Digest32 {
    let mut sorted: Vec<[u8; 20]> = ids.iter().map(|id| id.0).collect();
    sorted.sort_unstable();
    let mut h = Sha256::new();
    for id in &sorted {
        h.update(id);
    }
    h.finalize().into()
}

pub fn path_digest(canonical: &str) -> Digest32 {
    Sha256::digest(canonical.as_bytes()).into()
}

pub fn digest(bytes: &[u8]) -> Digest32 {
    Sha256::digest(bytes).into()
}

// ---- Clocks (§3.3, I4) -----------------------------------------------------

/// Where the protocol takes time from, so tests never sleep (I7).
pub trait Clock: fmt::Debug + Send + Sync {
    /// Wall-clock time since the Unix epoch.
    fn wall(&self) -> Duration;
    fn mono(&self) -> Instant;
}

#[derive(Debug, Default, Clone, Copy)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn wall(&self) -> Duration {
        // A clock before 1970 reads as the epoch, which validation then
        // sees as a backward step.
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
    }

    fn mono(&self) -> Instant {
        Instant::now()
    }
}

pub fn floor_ms(d: Duration) -> u64 {
    u64::try_from(d.as_millis()).unwrap_or(u64::MAX)
}

pub fn ceil_ms(d: Duration) -> u64 {
    let up = u64::from(!d.subsec_nanos().is_multiple_of(1_000_000));
    floor_ms(d).saturating_add(up)
}

pub fn ceil_s(d: Duration) -> u64 {
    d.as_secs().saturating_add(u64::from(d.subsec_nanos() != 0))
}

/// `H_s`: the horizon in whole seconds, rounded up (N6).
pub fn horizon_s(horizon: Duration) -> u32 {
    u32::try_from(ceil_s(horizon)).unwrap_or(u32::MAX)
}

// ---- Capture (S10) ----------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureError {
    /// A restriction or the gate reaches past the horizon. That can't
    /// happen with the sampler's clamps; refusing the commit (the marker
    /// stays dirty) is safe, where clamping would shorten it.
    BeyondHorizon,
    /// `horizon_s` is outside `1..=MAX_HORIZON_S`: the file would never
    /// decode.
    BadHorizon,
    PortZero,
    /// An IPv6 key with a scope id or flow label. The sampler keys on the
    /// whole `SocketAddr`, so writing it without them would restore the
    /// restriction under a different key (and lose it). Refusing the
    /// commit is safe; only `allow_private` fixtures can reach this.
    Scoped,
    TooMany,
}

/// S10 with the clock reads in the required order: monotonic, then wall.
pub fn capture_now(
    clock: &dyn Clock,
    run: RunHeader,
    table: impl IntoIterator<Item = (SocketAddr, Instant)>,
    gate_until: Option<Instant>,
    horizon_s: u32,
) -> Result<Snapshot, CaptureError> {
    let mono = clock.mono();
    let wall = clock.wall();
    capture(run, table, gate_until, mono, wall, horizon_s)
}

/// S11's marker time: a wall reading taken after S10's, rounded up (N4).
pub fn stamp_after_capture(clock: &dyn Clock) -> u64 {
    ceil_ms(clock.wall())
}

/// Builds the snapshot from the live table. `mono` must be read before
/// `wall` (I4), so every deadline written is at or after the true one.
pub fn capture(
    run: RunHeader,
    table: impl IntoIterator<Item = (SocketAddr, Instant)>,
    gate_until: Option<Instant>,
    mono: Instant,
    wall: Duration,
    horizon_s: u32,
) -> Result<Snapshot, CaptureError> {
    if horizon_s == 0 || horizon_s > MAX_HORIZON_S {
        return Err(CaptureError::BadHorizon);
    }
    let left = |until: Instant| -> Result<u32, CaptureError> {
        let s = ceil_s(until.saturating_duration_since(mono));
        if s > u64::from(horizon_s) {
            return Err(CaptureError::BeyondHorizon);
        }
        Ok(s as u32)
    };
    let mut entries = Vec::new();
    for (addr, until) in table {
        let remaining_s = left(until)?;
        if remaining_s == 0 {
            continue;
        }
        let addr = lodestar_network_policy::normalize(addr);
        if addr.port() == 0 {
            return Err(CaptureError::PortZero);
        }
        if matches!(addr, SocketAddr::V6(v6) if v6.scope_id() != 0 || v6.flowinfo() != 0) {
            return Err(CaptureError::Scoped);
        }
        entries.push(Entry { addr, remaining_s });
    }
    entries.sort_by(|a, b| {
        entry_key(&a.addr)
            .cmp(&entry_key(&b.addr))
            .then(b.remaining_s.cmp(&a.remaining_s))
    });
    // Keeps the longest of any duplicates (sorted first above).
    entries.dedup_by(|later, first| later.addr == first.addr);
    if entries.len() > MAX_ENTRIES {
        return Err(CaptureError::TooMany);
    }
    let gate_s = match gate_until {
        Some(until) => left(until)?,
        None => 0,
    };
    Ok(Snapshot {
        run,
        at_ms: ceil_ms(wall),
        horizon_s,
        gate_s,
        entries,
    })
}

// ---- Validation and seeding (§2.1) -----------------------------------------

/// What a read found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Found<T> {
    Missing,
    Bad(Fault),
    Ok(T),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WitnessFound {
    /// The witness dir is inside the data dir, or the path doesn't fit.
    Unusable,
    Missing,
    Bad(Fault),
    Ok(Witness),
}

/// Everything S1 read.
#[derive(Debug, Clone)]
pub struct Loaded {
    pub marker: Found<Marker>,
    pub snapshot: Found<(Snapshot, Digest32)>,
    /// The witness named by the marker's `dir_id`.
    pub witness: WitnessFound,
    /// SHA-256 of `dht-state.bin`, if it exists.
    pub state_bin: Option<Digest32>,
}

/// This run's side of the comparison. Build it with [`Here::read`], which
/// takes the clock readings in the required order.
#[derive(Debug, Clone)]
pub struct Here {
    pub path: String,
    pub dir_path: Digest32,
    pub ids: Digest32,
    pub horizon_s: u32,
    /// Read before `mono0` (I4).
    pub wall0: Duration,
    pub mono0: Instant,
}

impl Here {
    /// S0: reads wall, then monotonic (I4), and derives `dir_path` from
    /// `path`, so the two can't disagree.
    pub fn read(clock: &dyn Clock, path: String, ids: Digest32, horizon_s: u32) -> Here {
        let wall0 = clock.wall();
        let mono0 = clock.mono();
        Here {
            dir_path: path_digest(&path),
            path,
            ids,
            horizon_s,
            wall0,
            mono0,
        }
    }
}

/// Why sampling isn't resumed, or `Resumed` (the metric's label set, §5.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Reason {
    #[default]
    Off,
    Locked,
    FirstStart,
    Corrupt,
    Version,
    Dirty,
    OffRun,
    DataDir,
    NodeIdentity,
    ForeignRun,
    SnapshotMismatch,
    NoWitness,
    Generation,
    HorizonChanged,
    ClockBackward,
    Stale,
    InvalidateFailed,
    Resumed,
}

impl Reason {
    pub const ALL: [Reason; 18] = [
        Reason::Off,
        Reason::Locked,
        Reason::FirstStart,
        Reason::Corrupt,
        Reason::Version,
        Reason::Dirty,
        Reason::OffRun,
        Reason::DataDir,
        Reason::NodeIdentity,
        Reason::ForeignRun,
        Reason::SnapshotMismatch,
        Reason::NoWitness,
        Reason::Generation,
        Reason::HorizonChanged,
        Reason::ClockBackward,
        Reason::Stale,
        Reason::InvalidateFailed,
        Reason::Resumed,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Reason::Off => "off",
            Reason::Locked => "locked",
            Reason::FirstStart => "first_start",
            Reason::Corrupt => "corrupt",
            Reason::Version => "version",
            Reason::Dirty => "dirty",
            Reason::OffRun => "off_run",
            Reason::DataDir => "data_dir",
            Reason::NodeIdentity => "node_identity",
            Reason::ForeignRun => "foreign_run",
            Reason::SnapshotMismatch => "snapshot_mismatch",
            Reason::NoWitness => "no_witness",
            Reason::Generation => "generation",
            Reason::HorizonChanged => "horizon_changed",
            Reason::ClockBackward => "clock_backward",
            Reason::Stale => "stale",
            Reason::InvalidateFailed => "invalidate_failed",
            Reason::Resumed => "resumed",
        }
    }
}

impl From<Fault> for Reason {
    fn from(f: Fault) -> Reason {
        match f {
            Fault::Corrupt(_) => Reason::Corrupt,
            Fault::Version(_) => Reason::Version,
        }
    }
}

/// What resuming restores: restrictions and the gate, on this run's
/// monotonic clock.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Seeded {
    pub entries: Vec<(SocketAddr, Instant)>,
    /// No sample to anyone before this (N1).
    pub gate_until: Option<Instant>,
    pub dropped_expired: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    Resume(Seeded),
    Suppress(Reason),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DirId {
    Keep(Id16),
    /// Generate a fresh random id (§1.4).
    New,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    /// This run's generation: `max(marker.generation, witness.generation) + 1` (P1).
    pub generation: u64,
    pub dir_id: DirId,
    pub decision: Decision,
}

/// S2: validates what S1 read (first failure wins) and, only if every
/// rule passes, converts the snapshot to this run's clock.
pub fn plan(loaded: &Loaded, here: &Here) -> Plan {
    let marker = match &loaded.marker {
        Found::Ok(m) => Some(m),
        _ => None,
    };
    let witness = match &loaded.witness {
        WitnessFound::Ok(w) => Some(w),
        _ => None,
    };
    let prev_gen = marker
        .map(|m| m.run.generation)
        .max(witness.map(|w| w.generation))
        .unwrap_or(0);
    // A witness for this id that names another path: this directory is a
    // copy or a move, and needs its own id (N7).
    let copied = witness.is_some_and(|w| w.path != here.path);
    let dir_id = match marker {
        Some(m) if !copied => DirId::Keep(m.run.dir_id),
        _ => DirId::New,
    };
    Plan {
        generation: prev_gen + 1,
        dir_id,
        decision: match validate(loaded, here) {
            Ok(seeded) => Decision::Resume(seeded),
            Err(reason) => Decision::Suppress(reason),
        },
    }
}

fn validate(loaded: &Loaded, here: &Here) -> Result<Seeded, Reason> {
    // 1–3: the marker.
    let m = match &loaded.marker {
        Found::Missing => return Err(Reason::FirstStart),
        Found::Bad(f) => return Err((*f).into()),
        Found::Ok(m) => m,
    };
    let (snap_sum, state_bin) = match m.state {
        MarkerState::Clean { snap, state_bin } => (snap, state_bin),
        MarkerState::Dirty => return Err(Reason::Dirty),
        MarkerState::Off => return Err(Reason::OffRun),
    };
    let (s, sum) = match &loaded.snapshot {
        Found::Missing => return Err(Reason::Corrupt),
        Found::Bad(f) => return Err((*f).into()),
        Found::Ok((s, sum)) => (s, sum),
    };
    // 4: this data directory.
    let witness = match &loaded.witness {
        WitnessFound::Ok(w) => Some(w),
        _ => None,
    };
    if m.dir_path != here.dir_path || witness.is_some_and(|w| w.path != here.path) {
        return Err(Reason::DataDir);
    }
    // 5–6: this node, and no other writer since the clean stop.
    if m.run.ids != here.ids {
        return Err(Reason::NodeIdentity);
    }
    if loaded.state_bin != Some(state_bin) {
        return Err(Reason::ForeignRun);
    }
    // 7: the snapshot is the one the marker names.
    if *sum != snap_sum || s.run != m.run {
        return Err(Reason::SnapshotMismatch);
    }
    // 8: the witness chains the generations.
    let w = witness.ok_or(Reason::NoWitness)?;
    if w.dir_id != m.run.dir_id {
        return Err(Reason::NoWitness);
    }
    if w.generation != m.run.generation {
        return Err(Reason::Generation);
    }
    // 9–11: the horizon and the clock.
    if s.horizon_s != here.horizon_s {
        return Err(Reason::HorizonChanged);
    }
    let now = floor_ms(here.wall0);
    if !(s.at_ms <= m.at_ms && m.at_ms <= now) {
        return Err(Reason::ClockBackward);
    }
    // Against `H_s`, which rounds `H` up: at most 1 s looser than `H`,
    // and harmless, because seeding still converts each deadline exactly
    // (N6).
    if now - m.at_ms > u64::from(here.horizon_s) * 1000 {
        return Err(Reason::Stale);
    }
    seed(s, now, here)
}

/// The restored deadlines on this run's clock. `now` is `floor_ms(wall0)`
/// and `mono0` was read after `wall0`, so every `until` is at or after
/// the true deadline (I4). Overflow anywhere is `corrupt` (I3).
fn seed(s: &Snapshot, now: u64, here: &Here) -> Result<Seeded, Reason> {
    let h_ms = u64::from(here.horizon_s) * 1000;
    let until = |remaining_s: u32| -> Result<Option<Instant>, Reason> {
        let deadline = s
            .at_ms
            .checked_add(u64::from(remaining_s) * 1000)
            .ok_or(Reason::Corrupt)?;
        if deadline <= now {
            return Ok(None);
        }
        // `left ≤ remaining ≤ horizon` already holds (rule 10); the clamp
        // is a guard that never shortens a valid entry.
        let left = (deadline - now).min(h_ms);
        here.mono0
            .checked_add(Duration::from_millis(left))
            .map(Some)
            .ok_or(Reason::Corrupt)
    };
    let mut entries = Vec::with_capacity(s.entries.len());
    let mut dropped_expired = 0;
    for e in &s.entries {
        match until(e.remaining_s)? {
            Some(t) => entries.push((lodestar_network_policy::normalize(e.addr), t)),
            None => dropped_expired += 1,
        }
    }
    let gate_until = if s.gate_s == 0 {
        None
    } else {
        until(s.gate_s)?
    };
    Ok(Seeded {
        entries,
        gate_until,
        dropped_expired,
    })
}

// ---- Files (§1.6) -----------------------------------------------------------

/// Test-only fault injection: the named step fails, leaving the files as
/// they are at that point, or aborts the process (§6 item 1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Point {
    AfterWrite,
    AfterSetLen,
    AfterSync,
    AfterRename,
    /// Before the directory fsync: the crash state of `AfterRename`, but
    /// it also injects that fsync's error.
    DirSync,
    AfterDirSync,
}

#[cfg(test)]
pub(crate) mod fail {
    use super::Point;
    use std::cell::Cell;

    thread_local! {
        static AT: Cell<Option<Point>> = const { Cell::new(None) };
        static STICKY: Cell<bool> = const { Cell::new(false) };
    }

    /// Fails the next write that reaches `p` on this thread.
    pub fn arm(p: Option<Point>) {
        AT.with(|a| a.set(p));
        STICKY.with(|s| s.set(false));
    }

    /// Fails every write that reaches `p` on this thread until disarmed.
    pub fn arm_sticky(p: Option<Point>) {
        AT.with(|a| a.set(p));
        STICKY.with(|s| s.set(p.is_some()));
    }

    pub fn hit(p: Point) -> std::io::Result<()> {
        if std::env::var("LODESTAR_TEST_ABORT_AT").ok().as_deref() == Some(&format!("{p:?}")) {
            std::process::abort();
        }
        if AT.with(|a| a.get()) == Some(p) {
            if !STICKY.with(|s| s.get()) {
                AT.with(|a| a.set(None));
            }
            return Err(std::io::Error::other(format!("injected at {p:?}")));
        }
        Ok(())
    }
}

#[cfg(not(test))]
mod fail {
    use super::Point;

    #[inline(always)]
    pub fn hit(_: Point) -> std::io::Result<()> {
        Ok(())
    }
}

/// Makes a new or renamed entry in `path`'s directory durable. Errors
/// propagate (I6); Windows can't open a directory for this, and the
/// protocol never relies on it there (§1.6).
fn sync_parent(path: &Path) -> io::Result<()> {
    fail::hit(Point::DirSync)?;
    #[cfg(unix)]
    {
        let dir = match path.parent() {
            Some(d) if !d.as_os_str().is_empty() => d,
            _ => Path::new("."),
        };
        File::open(dir)?.sync_all()?;
    }
    #[cfg(not(unix))]
    let _ = path;
    fail::hit(Point::AfterDirSync)
}

fn tmp_path(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(".tmp");
    PathBuf::from(name)
}

/// Commit writes: the snapshot and the clean marker. Temp, fsync, rename,
/// directory fsync. Losing the rename leaves the older file, which at
/// commit time is the dirty marker (§1.6).
pub fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let tmp = tmp_path(path);
    {
        let mut f = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&tmp)?;
        f.write_all(bytes)?;
        fail::hit(Point::AfterWrite)?;
        f.sync_all()?;
        fail::hit(Point::AfterSync)?;
    }
    fs::rename(&tmp, path)?;
    fail::hit(Point::AfterRename)?;
    sync_parent(path)
}

/// Invalidation writes: the dirty or off marker and the witness bump.
/// Overwrites the existing file's data and flushes it, so durability never
/// rests on a directory entry; a torn result fails the checksum (§1.6).
pub fn overwrite_in_place(path: &Path, bytes: &[u8]) -> io::Result<()> {
    match OpenOptions::new().write(true).open(path) {
        Ok(mut f) => {
            f.seek(SeekFrom::Start(0))?;
            f.write_all(bytes)?;
            fail::hit(Point::AfterWrite)?;
            f.set_len(bytes.len() as u64)?;
            fail::hit(Point::AfterSetLen)?;
            f.sync_all()?;
            fail::hit(Point::AfterSync)
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            {
                let mut f = OpenOptions::new().write(true).create_new(true).open(path)?;
                f.write_all(bytes)?;
                fail::hit(Point::AfterWrite)?;
                f.sync_all()?;
                fail::hit(Point::AfterSync)?;
            }
            sync_parent(path)
        }
        Err(e) => Err(e),
    }
}

/// Reads at most `max` bytes: a larger file is rejected without reading it
/// all.
fn read_bounded(path: &Path, max: usize) -> io::Result<Option<Vec<u8>>> {
    let f = match File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    let mut out = Vec::new();
    f.take(max as u64 + 1).read_to_end(&mut out)?;
    Ok(Some(out))
}

fn read_found<T>(
    path: &Path,
    max_body: usize,
    decode: impl FnOnce(&[u8]) -> Result<T, Fault>,
) -> Found<T> {
    match read_bounded(path, HEADER + max_body + TRAILER) {
        Ok(None) => Found::Missing,
        Ok(Some(bytes)) => match decode(&bytes) {
            Ok(v) => Found::Ok(v),
            Err(f) => Found::Bad(f),
        },
        // An unreadable file is as good as corrupt: suppress.
        Err(_) => Found::Bad(Fault::Corrupt("unreadable")),
    }
}

pub fn read_marker(data_dir: &Path) -> Found<Marker> {
    read_found(&data_dir.join(MARKER_FILE), MAX_SMALL_BODY, Marker::decode)
}

pub fn read_snapshot(data_dir: &Path) -> Found<(Snapshot, Digest32)> {
    read_found(
        &data_dir.join(SNAPSHOT_FILE),
        MAX_SNAPSHOT_BODY,
        Snapshot::decode,
    )
}

/// The canonical form of a directory, as UTF-8; `None` when it doesn't
/// exist or isn't UTF-8 (then the run has no witness).
pub fn canonical(dir: &Path) -> Option<String> {
    fs::canonicalize(dir)
        .ok()?
        .into_os_string()
        .into_string()
        .ok()
}

/// The witness dir is usable only outside the data dir, after following
/// symlinks (§1.5).
pub fn witness_usable(witness_dir: &Path, data_dir_canonical: &str) -> bool {
    let Ok(w) = fs::canonicalize(witness_dir) else {
        // Not there yet: S3 creates it, then checks again.
        return false;
    };
    !w.starts_with(data_dir_canonical) && Witness::fits(data_dir_canonical)
}

pub fn witness_path(witness_dir: &Path, dir_id: &Id16) -> PathBuf {
    let hex: String = dir_id.iter().map(|b| format!("{b:02x}")).collect();
    witness_dir.join(format!("{hex}.gen"))
}

pub fn read_witness(witness_dir: &Path, data_dir_canonical: &str, dir_id: &Id16) -> WitnessFound {
    if !witness_usable(witness_dir, data_dir_canonical) {
        return match fs::metadata(witness_dir) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => WitnessFound::Missing,
            _ => WitnessFound::Unusable,
        };
    }
    let file = witness_path(witness_dir, dir_id);
    // The file itself may not be a link (into the data dir, say). This is
    // check-then-use: a link swapped in between makes the read below see
    // a witness that isn't ours, which fails validation (another path, id
    // or generation) or decoding, so every outcome suppresses.
    if fs::symlink_metadata(&file).is_ok_and(|m| m.file_type().is_symlink()) {
        return WitnessFound::Unusable;
    }
    match read_found(&file, MAX_SMALL_BODY, Witness::decode) {
        Found::Missing => WitnessFound::Missing,
        Found::Bad(f) => WitnessFound::Bad(f),
        Found::Ok(w) => WitnessFound::Ok(w),
    }
}

/// S1: commit temp files are never read; remove ours, and only ours.
pub fn remove_stale_tmp(data_dir: &Path) -> io::Result<()> {
    for name in [MARKER_FILE, SNAPSHOT_FILE] {
        match fs::remove_file(tmp_path(&data_dir.join(name))) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
            _ => {}
        }
    }
    Ok(())
}

/// Another directory's witness is only judged once it's this old, so a
/// run between its S3 (new witness) and S4 (marker with that id) keeps it.
pub const ORPHAN_AGE_MS: u64 = 3_600_000;

/// Orphan cleanup (§1.4). It reads paths named in witness files, which can
/// hang (a dead network mount), so the caller runs it after S4, off the
/// S1–S4 critical path. Removes:
/// - witnesses for this path under another id (this directory's own; no
///   other run can be writing them while we hold its lock);
/// - another directory's witness older than [`ORPHAN_AGE_MS`] whose path
///   has no marker, or a readable marker with a different id.
///
/// An unreadable or torn marker, or an undecodable witness, is left alone:
/// it may be mid-write or on a disk that is spinning up.
pub fn prune_witnesses(
    witness_dir: &Path,
    keep: &Id16,
    path: &str,
    now_ms: u64,
) -> io::Result<usize> {
    let keep_name = witness_path(witness_dir, keep);
    let mut removed = 0;
    for entry in fs::read_dir(witness_dir)?.take(MAX_WITNESS_SCAN) {
        let p = entry?.path();
        if p == keep_name || p.extension().is_none_or(|e| e != "gen") {
            continue;
        }
        let Found::Ok(w) = read_found(&p, MAX_SMALL_BODY, Witness::decode) else {
            continue;
        };
        let orphan = if w.path == path {
            true
        } else if now_ms.saturating_sub(w.at_ms) < ORPHAN_AGE_MS {
            false
        } else {
            match read_marker(Path::new(&w.path)) {
                Found::Missing => true,
                Found::Ok(m) => m.run.dir_id != w.dir_id,
                Found::Bad(_) => false,
            }
        };
        if orphan {
            fs::remove_file(&p)?;
            removed += 1;
        }
    }
    Ok(removed)
}

// ---- The lock (§2.2) --------------------------------------------------------

/// Held from S0 until the commit; the OS releases it if the process dies.
#[derive(Debug)]
pub struct RestartLock {
    _file: File,
}

#[derive(Debug)]
pub enum LockAttempt {
    Held(RestartLock),
    Busy,
    /// The filesystem has no locks: never resume here.
    Unsupported,
}

/// `Err` (the lock file can't be created, or the OS refused for another
/// reason such as `ENOLCK`) means the same as `Busy` to the caller: no
/// protocol files touched, no samples, retried each tick, and counted as
/// `persist_errors{step="lock"}`.
pub fn try_lock(data_dir: &Path) -> io::Result<LockAttempt> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(data_dir.join(LOCK_FILE))?;
    match file.try_lock() {
        Ok(()) => Ok(LockAttempt::Held(RestartLock { _file: file })),
        Err(fs::TryLockError::WouldBlock) => Ok(LockAttempt::Busy),
        Err(fs::TryLockError::Error(e)) if e.kind() == io::ErrorKind::Unsupported => {
            Ok(LockAttempt::Unsupported)
        }
        Err(fs::TryLockError::Error(e)) => Err(e),
    }
}

#[cfg(test)]
mod tests;
