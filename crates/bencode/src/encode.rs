use crate::value::{OwnedValue, Value};

/// Writes exactly one bencode value into a buffer. Containers are written
/// through closures, so nesting is always balanced.
///
/// ```
/// use lodestar_bencode::Encoder;
/// let mut out = Vec::new();
/// Encoder::new(&mut out).dict(|d| {
///     d.entry(b"id").bytes(b"abcdefghij0123456789");
///     d.entry(b"port").int(6881);
/// });
/// assert_eq!(out, b"d2:id20:abcdefghij01234567894:porti6881ee");
/// ```
#[derive(Debug)]
pub struct Encoder<'o> {
    out: &'o mut Vec<u8>,
}

impl<'o> Encoder<'o> {
    pub fn new(out: &'o mut Vec<u8>) -> Self {
        Encoder { out }
    }

    pub fn int(self, v: i64) {
        self.out.push(b'i');
        write_decimal(self.out, v.unsigned_abs(), v < 0);
        self.out.push(b'e');
    }

    pub fn bytes(self, b: &[u8]) {
        write_byte_string(self.out, b);
    }

    pub fn str(self, s: &str) {
        write_byte_string(self.out, s.as_bytes());
    }

    pub fn list(self, f: impl FnOnce(&mut ListEncoder<'_>)) {
        self.out.push(b'l');
        f(&mut ListEncoder { out: self.out });
        self.out.push(b'e');
    }

    /// Keys must be added in strictly increasing byte order; the encoder
    /// panics otherwise rather than emit non-canonical output.
    pub fn dict(self, f: impl FnOnce(&mut DictEncoder<'_>)) {
        self.out.push(b'd');
        f(&mut DictEncoder {
            out: self.out,
            last_key: None,
        });
        self.out.push(b'e');
    }

    pub fn value<E: Encode + ?Sized>(self, v: &E) {
        v.encode(self);
    }

    /// Splices in bytes that are already one complete, canonical bencode
    /// value (e.g. a verified raw info dictionary). Not checked.
    pub fn raw(self, encoded: &[u8]) {
        self.out.extend_from_slice(encoded);
    }
}

#[derive(Debug)]
pub struct ListEncoder<'o> {
    out: &'o mut Vec<u8>,
}

impl ListEncoder<'_> {
    pub fn item(&mut self) -> Encoder<'_> {
        Encoder { out: self.out }
    }
}

#[derive(Debug)]
pub struct DictEncoder<'o> {
    out: &'o mut Vec<u8>,
    /// Span of the previous key inside `out`, for the ordering check.
    last_key: Option<(usize, usize)>,
}

impl DictEncoder<'_> {
    /// Writes `key` and returns the encoder for its value. The caller must
    /// write exactly one value with it.
    pub fn entry(&mut self, key: &[u8]) -> Encoder<'_> {
        if let Some((start, end)) = self.last_key {
            assert!(
                &self.out[start..end] < key,
                "bencode dictionary keys must be strictly increasing: {:?} after {:?}",
                key.escape_ascii().to_string(),
                self.out[start..end].escape_ascii().to_string(),
            );
        }
        write_decimal(self.out, key.len() as u64, false);
        self.out.push(b':');
        let start = self.out.len();
        self.out.extend_from_slice(key);
        self.last_key = Some((start, self.out.len()));
        Encoder { out: self.out }
    }
}

fn write_byte_string(out: &mut Vec<u8>, b: &[u8]) {
    write_decimal(out, b.len() as u64, false);
    out.push(b':');
    out.extend_from_slice(b);
}

fn write_decimal(out: &mut Vec<u8>, mut n: u64, negative: bool) {
    let mut buf = [0u8; 20];
    let mut i = buf.len();
    loop {
        i -= 1;
        buf[i] = b'0' + (n % 10) as u8;
        n /= 10;
        if n == 0 {
            break;
        }
    }
    if negative {
        out.push(b'-');
    }
    out.extend_from_slice(&buf[i..]);
}

/// Types that can be written as bencode.
pub trait Encode {
    fn encode(&self, enc: Encoder<'_>);
}

/// Encodes a value into a new buffer.
pub fn to_vec<E: Encode + ?Sized>(v: &E) -> Vec<u8> {
    let mut out = Vec::new();
    v.encode(Encoder::new(&mut out));
    out
}

impl Encode for i64 {
    fn encode(&self, enc: Encoder<'_>) {
        enc.int(*self);
    }
}

impl Encode for [u8] {
    fn encode(&self, enc: Encoder<'_>) {
        enc.bytes(self);
    }
}

impl Encode for str {
    fn encode(&self, enc: Encoder<'_>) {
        enc.str(self);
    }
}

impl Encode for Value<'_> {
    /// Canonical encoding: dictionaries decoded leniently are re-sorted.
    fn encode(&self, enc: Encoder<'_>) {
        match self {
            Value::Int(i) => enc.int(*i),
            Value::Bytes(b) => enc.bytes(b),
            Value::List(l) => enc.list(|le| {
                for item in l.iter() {
                    item.encode(le.item());
                }
            }),
            Value::Dict(d) => enc.dict(|de| {
                if d.is_sorted() {
                    for (k, v) in d.iter() {
                        v.encode(de.entry(k));
                    }
                } else {
                    let mut entries: Vec<_> = d.iter().collect();
                    entries.sort_unstable_by_key(|(k, _)| *k);
                    for (k, v) in entries {
                        v.encode(de.entry(k));
                    }
                }
            }),
        }
    }
}

impl Encode for OwnedValue {
    fn encode(&self, enc: Encoder<'_>) {
        match self {
            OwnedValue::Int(i) => enc.int(*i),
            OwnedValue::Bytes(b) => enc.bytes(b),
            OwnedValue::List(l) => enc.list(|le| {
                for item in l {
                    item.encode(le.item());
                }
            }),
            OwnedValue::Dict(d) => enc.dict(|de| {
                for (k, v) in d {
                    v.encode(de.entry(k));
                }
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decode;

    #[test]
    fn integers() {
        for (v, s) in [
            (0i64, &b"i0e"[..]),
            (-1, b"i-1e"),
            (1234567890, b"i1234567890e"),
            (i64::MAX, b"i9223372036854775807e"),
            (i64::MIN, b"i-9223372036854775808e"),
        ] {
            assert_eq!(to_vec(&v), s);
        }
    }

    #[test]
    fn lenient_dict_reencodes_sorted() {
        let v = decode(b"d1:bi1e1:ai2ee").unwrap();
        assert_eq!(to_vec(&v), b"d1:ai2e1:bi1ee");
    }

    #[test]
    #[should_panic(expected = "strictly increasing")]
    fn unsorted_keys_panic() {
        let mut out = Vec::new();
        Encoder::new(&mut out).dict(|d| {
            d.entry(b"b").int(1);
            d.entry(b"a").int(2);
        });
    }

    #[test]
    #[should_panic(expected = "strictly increasing")]
    fn duplicate_keys_panic() {
        let mut out = Vec::new();
        Encoder::new(&mut out).dict(|d| {
            d.entry(b"a").int(1);
            d.entry(b"a").int(2);
        });
    }

    #[test]
    fn nested_builder() {
        let mut out = Vec::new();
        Encoder::new(&mut out).list(|l| {
            l.item().str("x");
            l.item().list(|_| {});
            l.item().dict(|d| d.entry(b"").int(0));
        });
        assert_eq!(out, b"l1:xled0:i0eee");
    }
}
