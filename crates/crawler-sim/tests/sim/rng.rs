//! A tiny seeded RNG (SplitMix64) and an order-free hash, so the
//! simulator needs no new dependency and never reads OS randomness.

/// SplitMix64: fast, seedable, and good enough for scripting a network.
#[derive(Debug, Clone)]
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Rng {
        Rng(seed)
    }

    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        mix(self.0)
    }

    /// Uniform in `0..n` (`n > 0`); the modulo bias is irrelevant here.
    pub fn below(&mut self, n: u64) -> u64 {
        self.next_u64() % n.max(1)
    }

    /// True with probability `permille / 1000`.
    pub fn chance(&mut self, permille: u32) -> bool {
        self.below(1000) < u64::from(permille)
    }

    pub fn bytes20(&mut self) -> [u8; 20] {
        let mut out = [0u8; 20];
        for chunk in out.chunks_mut(8) {
            let v = self.next_u64().to_be_bytes();
            chunk.copy_from_slice(&v[..chunk.len()]);
        }
        out
    }
}

/// The SplitMix64 finaliser.
pub fn mix(mut z: u64) -> u64 {
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

/// Hashes words into one value. Outcomes keyed this way don't depend on
/// the order the real code asks for them in.
pub fn hash(words: &[u64]) -> u64 {
    words.iter().fold(0x2545_f491_4f6c_dd1d, |acc, w| {
        mix(acc ^ w.wrapping_mul(0x9e37_79b9))
    })
}

pub fn word(bytes: &[u8]) -> u64 {
    let mut b = [0u8; 8];
    let n = bytes.len().min(8);
    b[..n].copy_from_slice(&bytes[..n]);
    u64::from_be_bytes(b)
}

/// FNV-1a over the outcome log, for a compact golden fingerprint.
#[derive(Debug, Clone, Copy)]
pub struct Fnv(pub u64);

impl Default for Fnv {
    fn default() -> Fnv {
        Fnv(0xcbf2_9ce4_8422_2325)
    }
}

impl Fnv {
    pub fn write(&mut self, bytes: &[u8]) {
        for b in bytes {
            self.0 ^= u64::from(*b);
            self.0 = self.0.wrapping_mul(0x0100_0000_01b3);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seeded_streams_repeat_and_differ() {
        let a: Vec<u64> = (0..4)
            .scan(Rng::new(7), |r, _| Some(r.next_u64()))
            .collect();
        let b: Vec<u64> = (0..4)
            .scan(Rng::new(7), |r, _| Some(r.next_u64()))
            .collect();
        let c: Vec<u64> = (0..4)
            .scan(Rng::new(8), |r, _| Some(r.next_u64()))
            .collect();
        assert_eq!(a, b);
        assert_ne!(a, c);
    }

    #[test]
    fn chance_is_roughly_calibrated() {
        let mut r = Rng::new(1);
        let hits = (0..10_000).filter(|_| r.chance(250)).count();
        assert!((2_250..2_750).contains(&hits), "{hits}");
        assert!(!(0..1000).any(|_| r.chance(0)));
        assert!((0..1000).all(|_| r.chance(1000)));
    }

    #[test]
    fn hash_depends_on_every_word_and_order() {
        assert_ne!(hash(&[1, 2]), hash(&[2, 1]));
        assert_ne!(hash(&[1, 2]), hash(&[1, 3]));
        assert_eq!(hash(&[1, 2]), hash(&[1, 2]));
    }
}
