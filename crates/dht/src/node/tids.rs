//! Transaction ids for outgoing queries (BEP 5 `t`): a keyed permutation
//! of a counter. Every id is distinct within the process, because the
//! counter never repeats and the permutation is a bijection, and an id
//! reveals nothing useful about the ids before or after it without the
//! key. The key comes from the actor's RNG, which production seeds from
//! the OS (the deterministic simulator seeds it from a fixed value).
//!
//! The permutation is an 8-round balanced Feistel network over 64 bits
//! with keyed SipHash-2-4 as the round function, restricted to the lower
//! half of the range by cycle walking: an output with the top bit set is
//! encrypted again until one below it comes out. The walk ends because
//! the input's own cycle returns to a value below the bit, and takes two
//! steps on average. The top bit stays clear for the sampler's proof
//! tokens (CR-0d).

use std::hash::Hasher;

use rand::RngExt;
use siphasher::sip::SipHasher24;

/// Wire ids stay below this; the sampler's proof tokens use the bit.
pub(crate) const LIMIT: u64 = 1 << 63;
const ROUNDS: u8 = 8;

pub(crate) struct TxIds {
    /// How many ids have been issued; the next counter value.
    next: u64,
    prf: SipHasher24,
}

impl TxIds {
    pub fn new(rng: &mut impl RngExt) -> TxIds {
        TxIds {
            next: 0,
            prf: SipHasher24::new_with_keys(rng.random(), rng.random()),
        }
    }

    /// The next id, or `None` once all 2^63 are spent (which never
    /// happens in practice).
    pub fn next_id(&mut self) -> Option<u64> {
        let n = self.next;
        if n >= LIMIT {
            return None;
        }
        self.next = n + 1;
        Some(self.permute(n))
    }

    /// A bijection on `0..LIMIT`.
    fn permute(&self, mut x: u64) -> u64 {
        loop {
            x = self.encrypt(x);
            if x < LIMIT {
                return x;
            }
        }
    }

    fn round(&self, round: u8, half: u32) -> u32 {
        let mut h = self.prf.clone();
        h.write(&[round]);
        h.write(&half.to_le_bytes());
        // Truncation to the half's width.
        h.finish() as u32
    }

    /// A bijection on all of `u64`.
    fn encrypt(&self, x: u64) -> u64 {
        let (mut l, mut r) = ((x >> 32) as u32, x as u32);
        for i in 0..ROUNDS {
            (l, r) = (r, l ^ self.round(i, r));
        }
        (u64::from(l) << 32) | u64::from(r)
    }

    #[cfg(test)]
    fn decrypt(&self, x: u64) -> u64 {
        let (mut l, mut r) = ((x >> 32) as u32, x as u32);
        for i in (0..ROUNDS).rev() {
            (l, r) = (r ^ self.round(i, l), l);
        }
        (u64::from(l) << 32) | u64::from(r)
    }

    #[cfg(test)]
    fn unpermute(&self, mut x: u64) -> u64 {
        loop {
            x = self.decrypt(x);
            if x < LIMIT {
                return x;
            }
        }
    }

    /// Spends every id, for the exhaustion path's test.
    #[cfg(test)]
    pub fn exhaust(&mut self) {
        self.next = LIMIT;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use rand::SeedableRng;
    use rand::rngs::StdRng;
    use std::collections::HashSet;

    fn ids(seed: u64) -> TxIds {
        TxIds::new(&mut StdRng::seed_from_u64(seed))
    }

    #[test]
    fn ids_are_distinct_and_below_the_proof_bit_over_a_long_run() {
        let mut t = ids(1);
        let mut seen = HashSet::new();
        for _ in 0..200_000 {
            let id = t.next_id().unwrap();
            assert!(id < LIMIT);
            assert!(seen.insert(id), "id {id} issued twice");
        }
    }

    #[test]
    fn exhaustion_is_reported_not_wrapped() {
        let mut t = ids(2);
        t.exhaust();
        assert_eq!(t.next_id(), None);
        assert_eq!(t.next_id(), None);
    }

    /// Consecutive ids look independent: their differences don't repeat,
    /// and every bit is set in about half of them.
    #[test]
    fn consecutive_ids_show_no_pattern() {
        const N: usize = 8192;
        let mut t = ids(3);
        let v: Vec<u64> = (0..N).map(|_| t.next_id().unwrap()).collect();
        let deltas: HashSet<u64> = v.windows(2).map(|w| w[1].wrapping_sub(w[0])).collect();
        assert!(deltas.len() > N - 16, "{} distinct deltas", deltas.len());
        let xors: HashSet<u64> = v.windows(2).map(|w| w[1] ^ w[0]).collect();
        assert!(xors.len() > N - 16, "{} distinct xors", xors.len());
        for bit in 0..63 {
            let ones = v.iter().filter(|x| *x >> bit & 1 == 1).count();
            // ±6 standard deviations around N/2 (sd = sqrt(N)/2 ≈ 45).
            assert!(ones.abs_diff(N / 2) < 270, "bit {bit}: {ones} of {N}");
        }
        // The id's position in the sequence doesn't show in its size.
        let low = v[..N / 2].iter().filter(|x| **x < LIMIT / 2).count();
        let high = v[N / 2..].iter().filter(|x| **x < LIMIT / 2).count();
        assert!(low.abs_diff(N / 4) < 200 && high.abs_diff(N / 4) < 200);
    }

    #[test]
    fn different_keys_give_different_sequences() {
        let (mut a, mut b) = (ids(4), ids(5));
        let same = (0..1000)
            .filter(|_| a.next_id().unwrap() == b.next_id().unwrap())
            .count();
        assert_eq!(same, 0);
    }

    proptest! {
        /// The Feistel network is a bijection on u64, so the walk is one on
        /// the lower half: distinct counters always give distinct ids.
        #[test]
        fn permutation_inverts(seed: u64, x: u64) {
            let t = ids(seed);
            prop_assert_eq!(t.decrypt(t.encrypt(x)), x);
            let low = x % LIMIT;
            let id = t.permute(low);
            prop_assert!(id < LIMIT);
            prop_assert_eq!(t.unpermute(id), low);
        }
    }
}
