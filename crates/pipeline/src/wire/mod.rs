//! What a worker sends the coordinator (architecture §19.2, §19.4): the
//! byte encodings of the `Results` and `Counters` messages, whose frames
//! carry them as opaque payloads (`lodestar_cluster::proto`).
//!
//! The coordinator decodes bytes from workers, so every decoder here is
//! **bounded and all-or-nothing**:
//! - the whole payload is capped ([`MAX_PAYLOAD`]) before anything is read;
//! - every count and length is checked against its cap and against the
//!   bytes left before anything is allocated for it;
//! - an unknown format version, a non-canonical integer, an unknown flag
//!   bit or trailing bytes refuse the whole payload;
//! - a decode returns the whole batch or an error, never part of one.
//!
//! The encoders check the same limits, so a worker never sends what the
//! coordinator would refuse: an over-cap batch is an error to split, not a
//! frame to drop. Both formats are hand-written varint layouts (no serde),
//! so the bytes are exactly what this module says and nothing else.
//!
//! **Versions:** each payload starts with its format version, and a decoder
//! reads only its own. `Hello` refuses only a different protocol major
//! (`lodestar_cluster::proto`), so a new payload version needs a protocol
//! major bump, or a protocol minor (`Hello.minor`) that the coordinator
//! checks. A new field in `Hello`'s capabilities wouldn't do: adding one to
//! a postcard struct is itself a breaking change. Without either, a worker on another payload version
//! would join and then have every batch refused.
//!
//! The worker's id is not in either payload: the coordinator takes it from
//! the authenticated session, never from bytes the worker chose.

pub mod counters;
pub mod filter_snapshot;
pub mod results;

pub use counters::CountersBatch;
pub use results::{ResultRecord, ResultsBatch};

/// The largest payload either decoder reads: the cluster protocol's cap
/// on an opaque message body, so whatever a frame can carry fits.
pub const MAX_PAYLOAD: usize = lodestar_cluster::proto::MAX_OPAQUE;

/// Why a payload was refused (or an encode couldn't be sent).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// The payload ended inside a field.
    Truncated,
    /// Bytes after the last record.
    Trailing,
    /// A format version this build doesn't read.
    Version(u8),
    /// A count or length over its cap.
    TooLarge(&'static str),
    /// A varint longer than its type, or not in its shortest form.
    Varint,
    /// A field whose value isn't allowed.
    Invalid(&'static str),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Truncated => write!(f, "truncated"),
            Error::Trailing => write!(f, "trailing bytes"),
            Error::Version(v) => write!(f, "unknown format version {v}"),
            Error::TooLarge(what) => write!(f, "{what} over its cap"),
            Error::Varint => write!(f, "malformed varint"),
            Error::Invalid(what) => write!(f, "invalid {what}"),
        }
    }
}

impl std::error::Error for Error {}

pub type Result<T, E = Error> = std::result::Result<T, E>;

/// Appends fields in the wire layout.
#[derive(Debug, Default)]
pub(crate) struct Writer {
    pub out: Vec<u8>,
}

impl Writer {
    pub fn u8(&mut self, v: u8) {
        self.out.push(v);
    }

    /// LEB128, shortest form.
    pub fn uvar(&mut self, mut v: u64) {
        while v >= 0x80 {
            self.out.push((v as u8) | 0x80);
            v >>= 7;
        }
        self.out.push(v as u8);
    }

    /// Zigzag, then [`uvar`](Self::uvar).
    pub fn ivar(&mut self, v: i64) {
        self.uvar(((v << 1) ^ (v >> 63)) as u64);
    }

    pub fn fixed(&mut self, b: &[u8]) {
        self.out.extend_from_slice(b);
    }

    /// A length, then the bytes. The caller has checked the cap.
    pub fn bytes(&mut self, b: &[u8]) {
        self.uvar(b.len() as u64);
        self.out.extend_from_slice(b);
    }
}

/// Reads fields in the wire layout, never past the end.
#[derive(Debug)]
pub(crate) struct Reader<'a> {
    rest: &'a [u8],
}

impl<'a> Reader<'a> {
    /// Refuses a payload over [`MAX_PAYLOAD`] before reading any of it.
    pub fn new(payload: &'a [u8]) -> Result<Reader<'a>> {
        if payload.len() > MAX_PAYLOAD {
            return Err(Error::TooLarge("payload"));
        }
        Ok(Reader { rest: payload })
    }

    pub fn u8(&mut self) -> Result<u8> {
        let (&b, rest) = self.rest.split_first().ok_or(Error::Truncated)?;
        self.rest = rest;
        Ok(b)
    }

    pub fn fixed<const N: usize>(&mut self) -> Result<[u8; N]> {
        if self.rest.len() < N {
            return Err(Error::Truncated);
        }
        let (head, rest) = self.rest.split_at(N);
        self.rest = rest;
        Ok(head.try_into().expect("split at N"))
    }

    /// LEB128 in its shortest form; at most ten bytes, the tenth at most 1.
    pub fn uvar(&mut self) -> Result<u64> {
        let mut v = 0u64;
        for i in 0..10 {
            let b = self.u8()?;
            let low = u64::from(b & 0x7f);
            if i == 9 && b > 1 {
                return Err(Error::Varint);
            }
            v |= low << (7 * i);
            if b & 0x80 == 0 {
                // A zero last byte (after the first) isn't the shortest form.
                if i > 0 && b == 0 {
                    return Err(Error::Varint);
                }
                return Ok(v);
            }
        }
        Err(Error::Varint)
    }

    pub fn ivar(&mut self) -> Result<i64> {
        let z = self.uvar()?;
        Ok(((z >> 1) as i64) ^ -((z & 1) as i64))
    }

    /// A count, refused over `cap` or over what the bytes left could hold
    /// at `min_each` bytes per item, so nothing is allocated for items that
    /// can't be there.
    pub fn count(&mut self, cap: usize, min_each: usize, what: &'static str) -> Result<usize> {
        let n = self.uvar()?;
        if n > cap as u64 {
            return Err(Error::TooLarge(what));
        }
        let n = n as usize;
        if n.saturating_mul(min_each.max(1)) > self.rest.len() {
            return Err(Error::Truncated);
        }
        Ok(n)
    }

    /// A length-prefixed byte string of at most `cap` bytes, borrowed.
    pub fn bytes(&mut self, cap: usize, what: &'static str) -> Result<&'a [u8]> {
        let n = self.uvar()?;
        if n > cap as u64 {
            return Err(Error::TooLarge(what));
        }
        let n = n as usize;
        if n > self.rest.len() {
            return Err(Error::Truncated);
        }
        let (head, rest) = self.rest.split_at(n);
        self.rest = rest;
        Ok(head)
    }

    /// Nothing may follow the last field.
    pub fn finish(self) -> Result<()> {
        if self.rest.is_empty() {
            Ok(())
        } else {
            Err(Error::Trailing)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round(v: u64) -> Vec<u8> {
        let mut w = Writer::default();
        w.uvar(v);
        w.out
    }

    #[test]
    fn varints_round_trip_at_the_edges() {
        for v in [
            0,
            1,
            127,
            128,
            16_383,
            16_384,
            u64::from(u32::MAX),
            u64::MAX,
        ] {
            let b = round(v);
            let mut r = Reader::new(&b).unwrap();
            assert_eq!(r.uvar().unwrap(), v);
            r.finish().unwrap();
        }
        assert_eq!(round(u64::MAX).len(), 10);
        for v in [0, -1, 1, i64::MIN, i64::MAX] {
            let mut w = Writer::default();
            w.ivar(v);
            let mut r = Reader::new(&w.out).unwrap();
            assert_eq!(r.ivar().unwrap(), v);
        }
    }

    #[test]
    fn non_canonical_and_overlong_varints_are_refused() {
        // 0 written in two bytes.
        assert_eq!(
            Reader::new(&[0x80, 0x00]).unwrap().uvar(),
            Err(Error::Varint)
        );
        // 1 with a padding byte.
        assert_eq!(
            Reader::new(&[0x81, 0x00]).unwrap().uvar(),
            Err(Error::Varint)
        );
        // Eleven bytes.
        let long = [0xff; 11];
        assert_eq!(Reader::new(&long).unwrap().uvar(), Err(Error::Varint));
        // The tenth byte may only hold bit 63.
        let mut over = [0xff; 10];
        over[9] = 0x02;
        assert_eq!(Reader::new(&over).unwrap().uvar(), Err(Error::Varint));
        assert_eq!(Reader::new(&[0x80]).unwrap().uvar(), Err(Error::Truncated));
    }

    #[test]
    fn counts_and_lengths_are_capped_before_allocation() {
        // A count of a million in a three-byte payload.
        let mut w = Writer::default();
        w.uvar(1_000_000);
        assert_eq!(
            Reader::new(&w.out).unwrap().count(10, 1, "x"),
            Err(Error::TooLarge("x"))
        );
        assert_eq!(
            Reader::new(&w.out).unwrap().count(usize::MAX, 1, "x"),
            Err(Error::Truncated)
        );
        let mut w = Writer::default();
        w.uvar(u64::MAX);
        assert_eq!(
            Reader::new(&w.out).unwrap().bytes(usize::MAX, "b"),
            Err(Error::Truncated)
        );
        assert_eq!(
            Reader::new(&w.out).unwrap().bytes(4, "b"),
            Err(Error::TooLarge("b"))
        );
        let big = vec![0u8; MAX_PAYLOAD + 1];
        assert_eq!(Reader::new(&big).err(), Some(Error::TooLarge("payload")));
    }
}
