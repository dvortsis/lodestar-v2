//! The replicated have-filter's wire format (architecture §19.3, §7; plan
//! WK-3).
//!
//! The coordinator sends a worker its filter as:
//! - a [`HaveHeader`] in `ToWorker::HaveSnapshot`: the format version, the
//!   filter's generation, this transfer's serial, the first delta that
//!   follows it, the 128-bit SipHash key, the size in bits, the number of
//!   hashes, the capacity, the count and the number of chunks;
//! - the filter's bits in `ToWorker::Snapshot` chunks of
//!   [`SnapshotKind::HaveFilter`], at most [`MAX_CHUNK`] bytes each, chunk
//!   `seq` of `total` with the serial as their `version`. An [`Assembly`]
//!   takes them strictly in order;
//! - then [`HaveDelta`]s in `ToWorker::HaveDelta`: batches of inserted keys
//!   tagged with the generation and a delta sequence number.
//!
//! Every decoder here checks lengths and counts before it allocates, and
//! refuses unknown versions; the bytes come from the network.
//!
//! The key keeps peers from choosing hashes that collide in the filter. It
//! travels only in the header, over the cluster's mutual TLS. [`HaveKey`]'s
//! `Debug` hides it and it has no `Display`, so it never reaches a log or
//! an error text.

use std::fmt;
use std::ops::Range;

use ring::rand::{SecureRandom, SystemRandom};

use crate::ClusterError;
use crate::proto::{MAX_CHUNK, MAX_CHUNKS, MAX_OPAQUE, SnapshotAck, SnapshotChunk, SnapshotKind};

/// The header and delta format version this build writes and reads.
pub const HAVE_V1: u8 = 1;
/// A header's exact length.
pub const HEADER_V1_LEN: usize = 70;
/// The largest filter a worker accepts: 128 MiB (2^30 bits), half the
/// worker's 256 MB budget and twice the worker filter at 50M stored
/// (62.5 MB, plan Q6). A header asking for more is refused, never
/// allocated.
pub const MAX_FILTER_BITS: u64 = 1 << 30;
/// The smallest: one word.
pub const MIN_FILTER_BITS: u64 = 64;
/// Hash functions per key.
pub const MAX_HASHES: u8 = 16;
/// A delta's fixed part: version, generation, sequence, count.
const DELTA_HEAD: usize = 1 + 8 + 8 + 4;
/// Keys in one delta, so it fits `MAX_OPAQUE`.
pub const MAX_DELTA_KEYS: usize = (MAX_OPAQUE - DELTA_HEAD) / 20;

/// A filter's SipHash-1-3 key: 128 random bits, new for every filter.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct HaveKey([u8; 16]);

impl HaveKey {
    /// A key from the system's random source.
    pub fn random() -> Result<HaveKey, ClusterError> {
        let mut k = [0u8; 16];
        SystemRandom::new()
            .fill(&mut k)
            .map_err(|_| ClusterError::Random)?;
        Ok(HaveKey(k))
    }

    pub const fn from_bytes(k: [u8; 16]) -> HaveKey {
        HaveKey(k)
    }

    /// The key itself, for the hash and the header only.
    pub fn secret_bytes(&self) -> &[u8; 16] {
        &self.0
    }
}

impl fmt::Debug for HaveKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("HaveKey(..)")
    }
}

/// What a snapshot's header says about the filter that follows it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct HaveHeader {
    /// Bumped by every rebuild; deltas name the generation they belong to.
    pub generation: u64,
    /// This transfer, never 0: its chunks carry it as their `version`,
    /// and acknowledgements name it.
    pub serial: u64,
    /// The first delta sequence number that follows this snapshot.
    pub next_delta: u64,
    pub key: HaveKey,
    /// The filter's size in bits, a multiple of 64.
    pub bits: u64,
    pub hashes: u8,
    /// Keys the filter was sized for; past it the coordinator rebuilds.
    pub capacity: u64,
    /// Keys in it when the snapshot started (approximate, as the filter
    /// counts).
    pub count: u64,
    /// Chunks that follow, `ceil(bits / 8 / MAX_CHUNK)`.
    pub chunks: u32,
}

/// Why bytes from the coordinator were refused. The text never carries
/// the key or the bytes themselves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HaveError {
    /// A format version this build doesn't read.
    Version,
    /// The wrong length for what the fixed part says.
    Length,
    /// A field out of its range.
    Field(&'static str),
    /// A chunk of another kind or transfer.
    OtherTransfer,
    /// A chunk out of order: one is missing or repeated.
    OutOfOrder,
    /// More chunks than the header said.
    Complete,
}

impl fmt::Display for HaveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            HaveError::Version => f.write_str("have-filter data of an unknown version"),
            HaveError::Length => f.write_str("have-filter data of the wrong length"),
            HaveError::Field(what) => write!(f, "have-filter {what} out of range"),
            HaveError::OtherTransfer => f.write_str("a chunk of another have-filter transfer"),
            HaveError::OutOfOrder => f.write_str("a have-filter chunk out of order"),
            HaveError::Complete => f.write_str("a chunk past the end of the have-filter"),
        }
    }
}

impl std::error::Error for HaveError {}

/// Bytes in a filter of `bits` bits.
pub fn filter_bytes(bits: u64) -> u64 {
    bits / 8
}

/// Chunks a filter of `bits` bits travels in.
pub fn chunks_for(bits: u64) -> u32 {
    let n = filter_bytes(bits).div_ceil(MAX_CHUNK as u64);
    u32::try_from(n).unwrap_or(u32::MAX)
}

/// The filter's bytes chunk `seq` carries.
pub fn chunk_range(bits: u64, seq: u32) -> Range<usize> {
    let len = filter_bytes(bits) as usize;
    let start = (seq as usize).saturating_mul(MAX_CHUNK).min(len);
    start..start.saturating_add(MAX_CHUNK).min(len)
}

impl HaveHeader {
    /// Whether every field is in range: a header that passes describes a
    /// filter a worker can hold.
    pub fn check(&self) -> Result<(), HaveError> {
        if self.serial == 0 {
            return Err(HaveError::Field("serial"));
        }
        if !self.bits.is_multiple_of(64)
            || !(MIN_FILTER_BITS..=MAX_FILTER_BITS).contains(&self.bits)
        {
            return Err(HaveError::Field("size"));
        }
        if self.hashes == 0 || self.hashes > MAX_HASHES {
            return Err(HaveError::Field("hashes"));
        }
        // At least one bit per key it was sized for.
        if self.capacity == 0 || self.capacity > self.bits {
            return Err(HaveError::Field("capacity"));
        }
        if self.chunks != chunks_for(self.bits) || self.chunks > MAX_CHUNKS {
            return Err(HaveError::Field("chunks"));
        }
        Ok(())
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut b = Vec::with_capacity(HEADER_V1_LEN);
        b.push(HAVE_V1);
        b.extend_from_slice(&self.generation.to_le_bytes());
        b.extend_from_slice(&self.serial.to_le_bytes());
        b.extend_from_slice(&self.next_delta.to_le_bytes());
        b.extend_from_slice(&self.key.0);
        b.extend_from_slice(&self.bits.to_le_bytes());
        b.push(self.hashes);
        b.extend_from_slice(&self.capacity.to_le_bytes());
        b.extend_from_slice(&self.count.to_le_bytes());
        b.extend_from_slice(&self.chunks.to_le_bytes());
        debug_assert_eq!(b.len(), HEADER_V1_LEN);
        b
    }

    /// Reads a header: the version first, then the exact length, then
    /// every field ([`check`](Self::check)). Allocates nothing.
    pub fn decode(b: &[u8]) -> Result<HaveHeader, HaveError> {
        match b.first() {
            Some(&HAVE_V1) => {}
            _ => return Err(HaveError::Version),
        }
        if b.len() != HEADER_V1_LEN {
            return Err(HaveError::Length);
        }
        let mut r = Reader(&b[1..]);
        let h = HaveHeader {
            generation: r.u64(),
            serial: r.u64(),
            next_delta: r.u64(),
            key: HaveKey(r.array()),
            bits: r.u64(),
            hashes: r.array::<1>()[0],
            capacity: r.u64(),
            count: r.u64(),
            chunks: u32::from_le_bytes(r.array()),
        };
        h.check()?;
        Ok(h)
    }
}

/// A batch of keys inserted into generation `generation`'s filter.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct HaveDelta {
    pub generation: u64,
    /// Consecutive within a generation, from the header's `next_delta`.
    pub seq: u64,
    pub keys: Vec<[u8; 20]>,
}

impl HaveDelta {
    /// `None` for no keys or more than [`MAX_DELTA_KEYS`].
    pub fn encode(&self) -> Option<Vec<u8>> {
        if self.keys.is_empty() || self.keys.len() > MAX_DELTA_KEYS {
            return None;
        }
        let mut b = Vec::with_capacity(DELTA_HEAD + 20 * self.keys.len());
        b.push(HAVE_V1);
        b.extend_from_slice(&self.generation.to_le_bytes());
        b.extend_from_slice(&self.seq.to_le_bytes());
        b.extend_from_slice(&(self.keys.len() as u32).to_le_bytes());
        for k in &self.keys {
            b.extend_from_slice(k);
        }
        Some(b)
    }

    /// Reads a delta: the version, then the count, which must be 1 to
    /// [`MAX_DELTA_KEYS`] and match the length exactly, before the keys
    /// are allocated.
    pub fn decode(b: &[u8]) -> Result<HaveDelta, HaveError> {
        match b.first() {
            Some(&HAVE_V1) => {}
            _ => return Err(HaveError::Version),
        }
        if b.len() < DELTA_HEAD {
            return Err(HaveError::Length);
        }
        let mut r = Reader(&b[1..]);
        let generation = r.u64();
        let seq = r.u64();
        let count = u32::from_le_bytes(r.array()) as usize;
        if count == 0 || count > MAX_DELTA_KEYS {
            return Err(HaveError::Field("delta count"));
        }
        if r.0.len() != count * 20 {
            return Err(HaveError::Length);
        }
        let keys =
            r.0.chunks_exact(20)
                .map(|c| c.try_into().expect("20 bytes"))
                .collect();
        Ok(HaveDelta {
            generation,
            seq,
            keys,
        })
    }
}

/// A resynchronisation request: an acknowledgement of transfer 0, which no
/// transfer has. The worker sends it when it knows it missed something
/// (a chunk out of order, a gap in the deltas, a new session); the
/// coordinator answers with a fresh snapshot.
pub const RESYNC: SnapshotAck = SnapshotAck {
    kind: SnapshotKind::HaveFilter,
    version: 0,
    seq: 0,
};

/// Where an accepted chunk's bytes go. Its `Debug` prints the length of
/// the bytes, never the bytes.
#[derive(PartialEq, Eq)]
pub struct Placed<'a> {
    /// Byte offset into the filter, a multiple of 8.
    pub offset: usize,
    pub bytes: &'a [u8],
    /// The acknowledgement to send back.
    pub ack: SnapshotAck,
    /// The last chunk: the filter is complete.
    pub last: bool,
}

impl fmt::Debug for Placed<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Placed")
            .field("offset", &self.offset)
            .field("bytes", &self.bytes.len())
            .field("ack", &self.ack)
            .field("last", &self.last)
            .finish()
    }
}

/// One snapshot's chunks, taken strictly in order. A chunk out of order
/// means one is missing or repeated: the caller drops the assembly, so a
/// half-built filter is never used.
#[derive(Debug, Clone)]
pub struct Assembly {
    header: HaveHeader,
    next: u32,
}

impl Assembly {
    pub fn new(header: HaveHeader) -> Assembly {
        Assembly { header, next: 0 }
    }

    pub fn header(&self) -> &HaveHeader {
        &self.header
    }

    pub fn is_complete(&self) -> bool {
        self.next == self.header.chunks
    }

    /// Whether a chunk belongs to this transfer at all (its kind and
    /// serial). Chunks of an earlier transfer still in flight after a
    /// restart are not an error.
    pub fn owns(&self, c: &SnapshotChunk) -> bool {
        c.kind == SnapshotKind::HaveFilter && c.version == self.header.serial
    }

    /// Takes the next chunk, checking its kind, serial, total, sequence and
    /// exact length.
    pub fn accept<'a>(&mut self, c: &'a SnapshotChunk) -> Result<Placed<'a>, HaveError> {
        if !self.owns(c) {
            return Err(HaveError::OtherTransfer);
        }
        if self.is_complete() {
            return Err(HaveError::Complete);
        }
        if c.total != self.header.chunks {
            return Err(HaveError::Field("total"));
        }
        if c.seq != self.next {
            return Err(HaveError::OutOfOrder);
        }
        let range = chunk_range(self.header.bits, c.seq);
        if c.bytes.len() != range.len() {
            return Err(HaveError::Length);
        }
        self.next += 1;
        Ok(Placed {
            offset: range.start,
            bytes: &c.bytes,
            ack: SnapshotAck {
                kind: SnapshotKind::HaveFilter,
                version: self.header.serial,
                seq: c.seq,
            },
            last: self.is_complete(),
        })
    }
}

/// Reads fixed fields from a slice whose length was already checked.
struct Reader<'a>(&'a [u8]);

impl Reader<'_> {
    fn array<const N: usize>(&mut self) -> [u8; N] {
        let (a, rest) = self.0.split_at(N);
        self.0 = rest;
        a.try_into().expect("length checked")
    }

    fn u64(&mut self) -> u64 {
        u64::from_le_bytes(self.array())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header(bits: u64) -> HaveHeader {
        HaveHeader {
            generation: 3,
            serial: 9,
            next_delta: 4,
            key: HaveKey::from_bytes([0xA5; 16]),
            bits,
            hashes: 7,
            capacity: bits / 10,
            count: 12,
            chunks: chunks_for(bits),
        }
    }

    #[test]
    fn header_round_trips_and_checks_every_field() {
        let h = header(10 * 8 * MAX_CHUNK as u64 + 64);
        assert_eq!(h.chunks, 11);
        let b = h.encode();
        assert_eq!(b.len(), HEADER_V1_LEN);
        assert_eq!(HaveHeader::decode(&b), Ok(h));

        assert_eq!(HaveHeader::decode(&[]), Err(HaveError::Version));
        let mut v2 = b.clone();
        v2[0] = 2;
        assert_eq!(HaveHeader::decode(&v2), Err(HaveError::Version));
        assert_eq!(HaveHeader::decode(&b[..69]), Err(HaveError::Length));
        let bad = |f: fn(&mut HaveHeader)| {
            let mut h = h;
            f(&mut h);
            HaveHeader::decode(&h.encode())
        };
        assert_eq!(bad(|h| h.serial = 0), Err(HaveError::Field("serial")));
        assert_eq!(bad(|h| h.bits += 8), Err(HaveError::Field("size")));
        assert_eq!(bad(|h| h.bits = 0), Err(HaveError::Field("size")));
        assert_eq!(
            bad(|h| h.bits = MAX_FILTER_BITS + 64),
            Err(HaveError::Field("size"))
        );
        assert_eq!(bad(|h| h.hashes = 0), Err(HaveError::Field("hashes")));
        assert_eq!(bad(|h| h.hashes = 17), Err(HaveError::Field("hashes")));
        assert_eq!(bad(|h| h.capacity = 0), Err(HaveError::Field("capacity")));
        assert_eq!(
            bad(|h| h.capacity = h.bits + 1),
            Err(HaveError::Field("capacity"))
        );
        assert_eq!(bad(|h| h.chunks += 1), Err(HaveError::Field("chunks")));
    }

    #[test]
    fn the_key_stays_out_of_debug_and_errors() {
        let h = header(1 << 20);
        let shown = format!("{h:?} {:?}", h.key);
        assert!(shown.contains("HaveKey(..)"));
        for needle in ["a5", "A5", "165", "[165"] {
            assert!(!shown.contains(needle), "{needle} in {shown}");
        }
    }

    #[test]
    fn delta_round_trips_and_is_bounded() {
        let d = HaveDelta {
            generation: 2,
            seq: 7,
            keys: vec![[1; 20], [2; 20]],
        };
        let b = d.encode().unwrap();
        assert_eq!(HaveDelta::decode(&b), Ok(d.clone()));
        assert!(
            HaveDelta {
                keys: vec![],
                ..d.clone()
            }
            .encode()
            .is_none()
        );
        let most = HaveDelta {
            keys: vec![[3; 20]; MAX_DELTA_KEYS],
            ..d.clone()
        };
        assert!(most.encode().unwrap().len() <= MAX_OPAQUE);
        assert!(
            HaveDelta {
                keys: vec![[3; 20]; MAX_DELTA_KEYS + 1],
                ..d
            }
            .encode()
            .is_none()
        );
        assert_eq!(HaveDelta::decode(&b[..b.len() - 1]), Err(HaveError::Length));
        assert_eq!(HaveDelta::decode(&b[..20]), Err(HaveError::Length));
        let mut huge = b.clone();
        huge[17..21].copy_from_slice(&u32::MAX.to_le_bytes());
        assert_eq!(
            HaveDelta::decode(&huge),
            Err(HaveError::Field("delta count"))
        );
        let mut v0 = b;
        v0[0] = 0;
        assert_eq!(HaveDelta::decode(&v0), Err(HaveError::Version));
    }

    #[test]
    fn chunk_ranges_cover_the_filter_exactly() {
        let bits = 3 * 8 * MAX_CHUNK as u64 + 128;
        let n = chunks_for(bits);
        assert_eq!(n, 4);
        let mut end = 0;
        for s in 0..n {
            let r = chunk_range(bits, s);
            assert_eq!(r.start, end);
            assert_eq!(r.start % 8, 0);
            end = r.end;
        }
        assert_eq!(end as u64, bits / 8);
        assert!(chunk_range(bits, n).is_empty());
    }
}
