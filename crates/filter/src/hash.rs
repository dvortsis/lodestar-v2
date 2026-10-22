//! Hashed terms (§20.4, §20.5.1). Core term lists and the core's
//! fixtures exist only as hashes of normalized tokens, so no indicative
//! text is in the repository or the binary, and matching runs on hashes.
//!
//! The hash is FNV-1a over a domain prefix and the token, finished with
//! the MurmurHash3 64-bit mixer: stable forever (stored fixtures and
//! shipped lists depend on it), and fast enough to hash every token of a
//! torrent within the per-torrent budget. It hides text from casual
//! reading, not from a dictionary attack, which no unsalted hash can.

use std::collections::{HashMap, HashSet};
use std::hash::{BuildHasherDefault, Hasher};

const DOMAIN: &[u8] = b"lodestar/term/v1\0";

const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

const fn fnv(mut h: u64, bytes: &[u8]) -> u64 {
    let mut i = 0;
    while i < bytes.len() {
        h ^= bytes[i] as u64;
        h = h.wrapping_mul(FNV_PRIME);
        i += 1;
    }
    h
}

/// The state after the domain prefix, computed once.
const PREFIXED: u64 = fnv(FNV_OFFSET, DOMAIN);

/// The hash of one normalized token.
pub fn term_hash(token: &str) -> u64 {
    fmix64(fnv(PREFIXED, token.as_bytes()))
}

pub(crate) fn fmix64(mut h: u64) -> u64 {
    h ^= h >> 33;
    h = h.wrapping_mul(0xff51_afd7_ed55_8ccd);
    h ^= h >> 33;
    h = h.wrapping_mul(0xc4ce_b9fe_1a85_ec53);
    h ^ (h >> 33)
}

/// Passes an already-mixed term hash through as the table hash.
#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct IdHasher(u64);

impl Hasher for IdHasher {
    fn finish(&self) -> u64 {
        self.0
    }

    fn write(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.0 = self.0.rotate_left(8) ^ u64::from(b);
        }
    }

    fn write_u64(&mut self, n: u64) {
        self.0 = n;
    }
}

pub(crate) type IdMap<V> = HashMap<u64, V, BuildHasherDefault<IdHasher>>;
type IdSet = HashSet<u64, BuildHasherDefault<IdHasher>>;

/// A set of hashed terms.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TermSet(IdSet);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TermSetError {
    pub line: usize,
}

impl std::fmt::Display for TermSetError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "term list line {}: expected 16 hex digits", self.line)
    }
}

impl std::error::Error for TermSetError {}

impl TermSet {
    /// Reads a list of 16-hex-digit hashes, one per line. Blank lines
    /// and `#` comments are skipped.
    pub fn parse(text: &str) -> Result<TermSet, TermSetError> {
        let mut set = IdSet::default();
        for (i, line) in text.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let ok = line.len() == 16 && line.bytes().all(|b| b.is_ascii_hexdigit());
            let h = u64::from_str_radix(line, 16)
                .ok()
                .filter(|_| ok)
                .ok_or(TermSetError { line: i + 1 })?;
            set.insert(h);
        }
        Ok(TermSet(set))
    }

    /// A set from plain terms, hashed. For words that aren't indicative
    /// (age markers, rating systems, context words).
    pub fn of(terms: &[&str]) -> TermSet {
        TermSet(terms.iter().map(|t| term_hash(t)).collect())
    }

    pub fn contains(&self, hash: u64) -> bool {
        self.0.contains(&hash)
    }

    pub fn insert(&mut self, hash: u64) -> bool {
        self.0.insert(hash)
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = u64> + '_ {
        self.0.iter().copied()
    }

    /// The list format [`TermSet::parse`] reads, sorted.
    pub fn to_list(&self) -> String {
        let mut v: Vec<u64> = self.0.iter().copied().collect();
        v.sort_unstable();
        v.iter().map(|h| format!("{h:016x}\n")).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stable() {
        // Shipped lists and stored fixtures depend on these values.
        assert_eq!(term_hash("hello"), 0xa321_5c23_e27c_fd68);
        assert_ne!(term_hash("hello"), term_hash("hellp"));
        assert_ne!(term_hash(""), term_hash("\0"));
    }

    #[test]
    fn lists() {
        let set = TermSet::of(&["a", "b"]);
        assert_eq!(TermSet::parse(&set.to_list()).unwrap(), set);
        assert!(TermSet::parse("# c\n\n").unwrap().is_empty());
        for bad in ["xyz", "123", "0123456789abcdefg", "+123456789abcdef"] {
            assert_eq!(TermSet::parse(bad), Err(TermSetError { line: 1 }));
        }
    }
}
