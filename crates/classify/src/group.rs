//! Near-duplicate payloads (§6 "Grouping/dedup"): a MinHash signature
//! over a torrent's **major** file sizes, banded into index terms, so
//! "similar payloads" finds torrents that share most of their big files
//! (the same season with one episode swapped, a pack with an extra
//! disc) without comparing every pair. Search indexes the band terms as
//! `group_minhash`; Siblings looks candidates up by them and confirms
//! each on the stored sizes with [`similar`]. Both read this one module
//! (plan v0.7 Q9).
//!
//! **Parameters (Q9):**
//! - a file is *major* when it's at least [`MAJOR_MIN_BYTES`] and at
//!   least [`MAJOR_MIN_PERCENT`] % of the payload (the non-padding
//!   sizes), so a torrent has at most 100 major files and the cost is
//!   bounded whatever its file count;
//! - only a torrent with at least [`MIN_MAJOR_FILES`] major files is
//!   signed: with fewer, an overlap of one or two sizes says little,
//!   and those torrents rely on the exact payload signature
//!   ([`crate::payload`]);
//! - [`HASHES`] hashes in [`BANDS`] bands of [`ROWS`]; two torrents are
//!   candidates when any band agrees;
//! - two torrents are *similar* when the Jaccard similarity of their
//!   major sizes, as multisets, is at least 4/5 ([`SIMILAR`]).
//!
//! The probability that two payloads with Jaccard `J` share a band is
//! `1 − (1 − J^ROWS)^BANDS`: about 0.88 at `J = 0.8`, 0.99 at 0.9,
//! 0.23 at 0.5 and under 0.04 at 0.3. Candidates are always confirmed
//! on the exact sizes, so a band shared by chance never shows.
//!
//! The band terms are stored in the index, so what this hashes, and
//! how, must never change without bumping
//! `population::GENERATIONS["group_minhash"]` in the search crate: the
//! goldens below pin it.

/// The smallest major file.
pub const MAJOR_MIN_BYTES: u64 = 1 << 20;
/// A major file's smallest share of the payload, in percent.
pub const MAJOR_MIN_PERCENT: u64 = 1;
/// Major files a torrent needs for a signature.
pub const MIN_MAJOR_FILES: usize = 3;
/// MinHash values per signature.
pub const HASHES: usize = 16;
/// Bands of the signature, each one index term.
pub const BANDS: usize = 4;
/// Values per band.
pub const ROWS: usize = HASHES / BANDS;
/// The similarity threshold as a fraction: Jaccard ≥ 4/5.
pub const SIMILAR: (u64, u64) = (4, 5);

const _: () = assert!(HASHES == BANDS * ROWS);

/// Why a torrent has no signature.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unsigned {
    /// No content files (none, or only padding).
    NoContent,
    /// Fewer than [`MIN_MAJOR_FILES`] major files.
    FewMajorFiles,
}

/// A torrent's MinHash signature over its major file sizes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MinHash(pub [u64; HASHES]);

/// The major sizes among `content_sizes` (the non-padding file sizes,
/// any order), sorted ascending.
pub fn major_sizes(content_sizes: &[u64]) -> Vec<u64> {
    let total: u128 = content_sizes.iter().map(|&s| u128::from(s)).sum();
    let mut major: Vec<u64> = content_sizes
        .iter()
        .copied()
        .filter(|&s| {
            s >= MAJOR_MIN_BYTES && u128::from(s) * 100 >= total * u128::from(MAJOR_MIN_PERCENT)
        })
        .collect();
    major.sort_unstable();
    major
}

/// The signature of a torrent's content sizes (non-padding, any
/// order), or why it has none.
pub fn signature(content_sizes: &[u64]) -> Result<MinHash, Unsigned> {
    if content_sizes.is_empty() {
        return Err(Unsigned::NoContent);
    }
    MinHash::of_major(&major_sizes(content_sizes)).ok_or(Unsigned::FewMajorFiles)
}

/// The band terms search indexes as `group_minhash`: [`BANDS`] values,
/// or none for a torrent without a signature.
pub fn band_terms(content_sizes: &[u64]) -> Vec<u64> {
    signature(content_sizes)
        .map(|m| m.bands().to_vec())
        .unwrap_or_default()
}

impl MinHash {
    /// The signature of `major` (sorted major sizes, [`major_sizes`]);
    /// `None` with fewer than [`MIN_MAJOR_FILES`].
    ///
    /// A multiset is hashed as the set of `(size, k)`, `k` counting the
    /// earlier copies of the same size, so three equal episodes count
    /// three times and the result doesn't depend on the order.
    pub fn of_major(major: &[u64]) -> Option<MinHash> {
        debug_assert!(major.is_sorted(), "major sizes are sorted");
        if major.len() < MIN_MAJOR_FILES {
            return None;
        }
        let mut min = [u64::MAX; HASHES];
        let mut k = 0u64;
        for (i, &size) in major.iter().enumerate() {
            k = if i > 0 && major[i - 1] == size {
                k + 1
            } else {
                0
            };
            let element = fmix64(fmix64(size ^ ELEMENT_SEED) ^ k.wrapping_mul(COPY_MUL));
            for (m, seed) in min.iter_mut().zip(SEEDS) {
                *m = (*m).min(fmix64(element ^ seed));
            }
        }
        Some(MinHash(min))
    }

    /// The band terms: each band's [`ROWS`] values hashed with its
    /// position, so equal values in different bands are different terms.
    pub fn bands(&self) -> [u64; BANDS] {
        let mut out = [0; BANDS];
        for (b, (term, rows)) in out.iter_mut().zip(self.0.chunks_exact(ROWS)).enumerate() {
            let mut h = BAND_SEED ^ b as u64;
            for &v in rows {
                h = fmix64(h ^ v);
            }
            *term = fmix64(h ^ ROWS as u64);
        }
        out
    }

    /// How many of the [`HASHES`] values agree: `/ HASHES` estimates the
    /// Jaccard similarity.
    pub fn agreeing(&self, other: &MinHash) -> usize {
        self.0.iter().zip(&other.0).filter(|(a, b)| a == b).count()
    }
}

/// The Jaccard similarity of two sorted multisets of sizes, as
/// `(intersection, union)` counts.
pub fn jaccard(a: &[u64], b: &[u64]) -> (u64, u64) {
    debug_assert!(a.is_sorted() && b.is_sorted(), "sizes are sorted");
    let (mut i, mut j, mut both) = (0, 0, 0u64);
    while i < a.len() && j < b.len() {
        match a[i].cmp(&b[j]) {
            std::cmp::Ordering::Less => i += 1,
            std::cmp::Ordering::Greater => j += 1,
            std::cmp::Ordering::Equal => {
                both += 1;
                i += 1;
                j += 1;
            }
        }
    }
    let union = (a.len() + b.len()) as u64 - both;
    (both, union)
}

/// Whether two torrents' payloads are similar: both signed (at least
/// [`MIN_MAJOR_FILES`] major files each) and the Jaccard similarity of
/// their major sizes is at least [`SIMILAR`]. `a` and `b` are sorted
/// major sizes ([`major_sizes`]). Exact: this is what confirms a
/// candidate the band terms found.
pub fn similar(a: &[u64], b: &[u64]) -> bool {
    if a.len() < MIN_MAJOR_FILES || b.len() < MIN_MAJOR_FILES {
        return false;
    }
    let (both, union) = jaccard(a, b);
    both * SIMILAR.1 >= union * SIMILAR.0
}

const ELEMENT_SEED: u64 = 0x6772_6f75_706b_6579; // "groupkey"
const COPY_MUL: u64 = 0x9e37_79b9_7f4a_7c15;
const BAND_SEED: u64 = 0x6d69_6e68_6173_6862; // "minhashb"

/// One seed per hash: [`fmix64`] of `i + 1` times the golden ratio.
const SEEDS: [u64; HASHES] = {
    let mut s = [0; HASHES];
    let mut i = 0;
    while i < HASHES {
        s[i] = fmix64((i as u64 + 1).wrapping_mul(COPY_MUL));
        i += 1;
    }
    s
};

/// MurmurHash3's 64-bit finalizer, as [`crate::payload`] uses it.
const fn fmix64(mut h: u64) -> u64 {
    h ^= h >> 33;
    h = h.wrapping_mul(0xff51_afd7_ed55_8ccd);
    h ^= h >> 33;
    h = h.wrapping_mul(0xc4ce_b9fe_1a85_ec53);
    h ^ (h >> 33)
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIB: u64 = 1 << 20;

    #[test]
    fn major_files_are_big_and_a_real_share() {
        // 1 GiB, 1 GiB, 1 MiB (under 1 % of ~2 GiB), 512 KiB (under 1 MiB).
        let sizes = [1 << 30, 1 << 30, MIB, 512 << 10];
        assert_eq!(major_sizes(&sizes), [1 << 30, 1 << 30]);
        // Three 1 MiB files: each a third of the payload, exactly 1 MiB.
        assert_eq!(major_sizes(&[MIB; 3]), [MIB; 3]);
        assert_eq!(major_sizes(&[MIB - 1; 3]), Vec::<u64>::new());
        // Exactly 1 %: 1 file of 100 MiB in a 100 MiB × 100 payload.
        let mut hundred = vec![100 * MIB; 100];
        assert_eq!(major_sizes(&hundred).len(), 100);
        hundred.push(1);
        assert_eq!(major_sizes(&hundred).len(), 0, "now just under 1 % each");
        // No overflow near u64::MAX.
        assert_eq!(major_sizes(&[u64::MAX; 4]).len(), 4);
    }

    #[test]
    fn fewer_than_three_major_files_have_no_signature() {
        assert_eq!(signature(&[]), Err(Unsigned::NoContent));
        assert_eq!(
            signature(&[1 << 30, 1 << 30, 10]),
            Err(Unsigned::FewMajorFiles)
        );
        assert!(band_terms(&[1 << 30, 1 << 30]).is_empty());
        assert!(signature(&[1 << 30, 1 << 30, 1 << 30]).is_ok());
        assert_eq!(band_terms(&[3 << 30, 2 << 30, 1 << 30]).len(), BANDS);
    }

    /// Stored in every index: a build can't change them without bumping
    /// the field's generation.
    #[test]
    fn goldens() {
        let cases: [(&[u64], [u64; BANDS]); 3] = [
            (
                &[MIB, MIB, MIB],
                [
                    0x120c_ffe1_d175_6fe3,
                    0xc12c_17f1_4be3_fe19,
                    0x17ce_b23f_72f1_19d7,
                    0xe262_af62_e20a_5f80,
                ],
            ),
            (
                &[1_400_000_000, 1_400_000_123, 1_399_999_999, 7 << 20],
                [
                    0x701b_bdb7_6e07_b392,
                    0xc0d4_3699_1fca_c0b6,
                    0xf6b6_af48_0e18_bb36,
                    0x3b38_2c8c_4636_4da3,
                ],
            ),
            (
                &[u64::MAX, u64::MAX - 1, u64::MAX - 2],
                [
                    0x6f7b_aaae_63e0_9af0,
                    0xa4c5_9d57_9e3d_5bbf,
                    0x2f8c_a4da_a43f_2c17,
                    0x0c64_b4d5_f1eb_eed5,
                ],
            ),
        ];
        for (sizes, want) in cases {
            assert_eq!(band_terms(sizes), want, "{sizes:?}");
        }
    }

    #[test]
    fn jaccard_counts_multisets() {
        assert_eq!(jaccard(&[1, 2, 3], &[1, 2, 3]), (3, 3));
        assert_eq!(jaccard(&[1, 1, 2], &[1, 2, 2]), (2, 4));
        assert_eq!(jaccard(&[1, 2, 3, 4, 5], &[1, 2, 3, 4, 6]), (4, 6));
        assert_eq!(jaccard(&[], &[1]), (0, 1));
        // 4 of 5 shared, nothing else: exactly the threshold.
        assert!(similar(&[1, 2, 3, 4, 5], &[1, 2, 3, 4]));
        // 4 shared of 6: two thirds.
        assert!(!similar(&[1, 2, 3, 4, 5], &[1, 2, 3, 4, 6]));
        // A torrent with too few major files is similar to nothing, not
        // even another one like it.
        assert!(!similar(&[1, 2], &[1, 2]));
        assert!(!similar(&[1, 2, 3], &[1, 2]));
    }
}
