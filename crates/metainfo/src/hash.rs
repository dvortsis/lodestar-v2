use std::fmt;
use std::str::FromStr;

/// A 20-byte swarm identity: the v1 info hash (SHA-1 of the info dict) or a
/// v2 info hash truncated to 20 bytes, which is what the DHT uses for v2
/// swarms (BEP 52).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct InfoHash(pub [u8; 20]);

/// A full v2 info hash (SHA-256 of the info dict).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct InfoHashV2(pub [u8; 32]);

impl InfoHash {
    pub fn as_bytes(&self) -> &[u8; 20] {
        &self.0
    }
}

impl InfoHashV2 {
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// The 20-byte form announced on the DHT.
    pub fn truncated(&self) -> InfoHash {
        let mut out = [0u8; 20];
        out.copy_from_slice(&self.0[..20]);
        InfoHash(out)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParseHashError;

impl fmt::Display for ParseHashError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("expected a hex-encoded hash of the right length")
    }
}

impl std::error::Error for ParseHashError {}

fn write_hex(f: &mut fmt::Formatter<'_>, bytes: &[u8]) -> fmt::Result {
    for b in bytes {
        write!(f, "{b:02x}")?;
    }
    Ok(())
}

pub(crate) fn decode_hex<const N: usize>(s: &[u8]) -> Option<[u8; N]> {
    fn nibble(c: u8) -> Option<u8> {
        match c {
            b'0'..=b'9' => Some(c - b'0'),
            b'a'..=b'f' => Some(c - b'a' + 10),
            b'A'..=b'F' => Some(c - b'A' + 10),
            _ => None,
        }
    }
    if s.len() != N * 2 {
        return None;
    }
    let mut out = [0u8; N];
    for (i, pair) in s.chunks_exact(2).enumerate() {
        out[i] = nibble(pair[0])? << 4 | nibble(pair[1])?;
    }
    Some(out)
}

macro_rules! hash_impls {
    ($t:ident, $n:literal) => {
        impl fmt::Display for $t {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write_hex(f, &self.0)
            }
        }

        impl fmt::Debug for $t {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, concat!(stringify!($t), "({})"), self)
            }
        }

        impl FromStr for $t {
            type Err = ParseHashError;
            fn from_str(s: &str) -> Result<Self, Self::Err> {
                decode_hex::<$n>(s.as_bytes()).map($t).ok_or(ParseHashError)
            }
        }
    };
}

hash_impls!(InfoHash, 20);
hash_impls!(InfoHashV2, 32);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_roundtrip() {
        let s = "c9e15763f722f23e98a29decdfae341b98d53056";
        let h: InfoHash = s.parse().unwrap();
        assert_eq!(h.to_string(), s);
        assert_eq!(
            "C9E15763F722F23E98A29DECDFAE341B98D53056"
                .parse::<InfoHash>()
                .unwrap(),
            h
        );
        assert!("c9e1".parse::<InfoHash>().is_err());
        assert!("zz".repeat(20).parse::<InfoHash>().is_err());
    }

    #[test]
    fn truncation() {
        let mut full = [0u8; 32];
        for (i, b) in full.iter_mut().enumerate() {
            *b = i as u8;
        }
        let t = InfoHashV2(full).truncated();
        assert_eq!(&t.0[..], &full[..20]);
    }
}
