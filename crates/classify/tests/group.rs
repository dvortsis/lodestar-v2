//! `group_minhash` (plan v0.7 Q9): signatures are a function of the
//! multiset of major sizes, and the band terms find similar payloads
//! often and unrelated ones (almost) never, on a synthetic corpus.
//!
//! **The bounds this file holds the parameters to** (6 hashes, 2 bands
//! of 3; theory `P(candidate) = 1 − (1 − J³)²`):
//! - unrelated payloads (random sizes): no pair of 3,000 torrents shares
//!   a band (≈ 4.5M pairs; a chance band match is ~2⁻⁶⁴ per pair);
//! - unrelated split archives sharing a volume size: never similar (a
//!   torrent needs 3 distinct major sizes to be signed, and two similar
//!   ones must share 3);
//! - unrelated scene releases (volumes, a last part, a sample): the
//!   volume size is never signed, so at most 1 % of pairs share a band
//!   and no band term is carried by more than 1 % of them;
//! - Jaccard 0.5: at most 35 % become candidates (theory 23 %), and
//!   none of them is confirmed ([`similar`] is exact);
//! - Jaccard 0.8: at least 65 % are found (theory 76 %);
//! - Jaccard ≥ 0.9: at least 85 % are found (theory 93 %).

use std::collections::HashMap;

use lodestar_classify::group::{
    BANDS, HASHES, MIN_MAJOR_FILES, MinHash, band_terms, major_sizes, signature, signed_sizes,
    similar,
};
use proptest::prelude::*;

const MIB: u64 = 1 << 20;

/// A deterministic generator (splitmix64), so the rates are the same on
/// every run.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    /// A video-sized file: 2 to 4 GiB, any byte count.
    fn size(&mut self) -> u64 {
        2048 * MIB + self.next() % (2048 * MIB)
    }

    fn sizes(&mut self, n: usize) -> Vec<u64> {
        (0..n).map(|_| self.size()).collect()
    }
}

/// Whether two payloads share a band term.
fn candidates(a: &[u64], b: &[u64]) -> bool {
    let (a, b) = (band_terms(a), band_terms(b));
    a.iter().any(|t| b.contains(t))
}

/// `trials` pairs with `shared` sizes in common and `only` of their own
/// each: Jaccard = shared / (shared + 2 × only). The share of them that
/// share a band.
fn candidate_rate(seed: u64, trials: usize, shared: usize, only: usize) -> f64 {
    let mut rng = Rng(seed);
    let mut hits = 0;
    for _ in 0..trials {
        let common = rng.sizes(shared);
        let mut a = common.clone();
        a.extend(rng.sizes(only));
        let mut b = common;
        b.extend(rng.sizes(only));
        // Every size here is major (≥ 2 GiB, at least 1 % of a payload
        // of at most 25 files of at most 4 GiB).
        assert_eq!(major_sizes(&a).len(), a.len());
        if candidates(&a, &b) {
            hits += 1;
        }
    }
    f64::from(hits) / trials as f64
}

#[test]
fn unrelated_payloads_never_share_a_band() {
    let mut rng = Rng(1);
    let mut seen: HashMap<u64, usize> = HashMap::new();
    let n = 3000;
    for i in 0..n {
        let files = MIN_MAJOR_FILES + (rng.next() % 23) as usize;
        let terms = band_terms(&rng.sizes(files));
        assert_eq!(terms.len(), BANDS);
        for t in terms {
            if let Some(j) = seen.insert(t, i) {
                assert_eq!(j, i, "torrents {j} and {i} share a band");
            }
        }
    }
}

/// Split archives share their volume size (scene `.rNN` parts of
/// 50,000,000 bytes, `.7z.001` volumes): unrelated ones must never be
/// similar, whatever their part counts.
#[test]
fn unrelated_split_archives_are_never_similar() {
    let mut rng = Rng(8);
    for volume in [50_000_000u64, 100_000_000, (4 << 30) - 1] {
        for _ in 0..200 {
            let archive = |rng: &mut Rng, extra: usize| {
                let parts = 10 + (rng.next() % 40) as usize;
                let mut s = vec![volume; parts];
                // The last part, and up to two other big files (a sample,
                // a second archive's last part).
                s.extend((0..=extra).map(|_| volume / 2 + rng.next() % (volume / 3)));
                s
            };
            let (a, b) = (archive(&mut rng, 0), archive(&mut rng, 2));
            // Two sizes: no signature at all.
            assert!(band_terms(&a).is_empty());
            let (ma, mb) = (major_sizes(&a), major_sizes(&b));
            assert!(!similar(&ma, &mb));
            let c = archive(&mut rng, 2);
            assert!(!similar(&mb, &major_sizes(&c)), "{mb:?}");
        }
    }
}

/// Scene releases: 50,000,000-byte volumes, a last part and a sample.
/// The volume size is left out of the signature (more than
/// `MAX_COPIES` files share it), so a plain release is unsigned. One
/// with extra large files (a second disc's last part, a subtitle pack,
/// extras) is signed on those alone. Either way unrelated releases
/// share no volume-size term, so a request's candidate window doesn't
/// fill with releases that confirmation rejects (the runner's review,
/// B1). Bound: at most 1 % of unrelated pairs are candidates, and no
/// band term is carried by more than 1 % of the releases.
#[test]
fn unrelated_scene_releases_rarely_share_a_band() {
    let mut rng = Rng(9);
    let volume = 50_000_000u64;
    let n = 400;
    let mut by_term: HashMap<u64, usize> = HashMap::new();
    let mut releases = Vec::new();
    for i in 0..n {
        // 15–40 parts (up to 2 GB), so the last part (25–50 MB), the
        // sample (30–70 MB) and any extras are each over 1 % of it.
        let parts = 15 + (rng.next() % 26) as usize;
        let mut s = vec![volume; parts];
        s.push(25_000_000 + rng.next() % (volume - 25_000_000)); // the last part
        s.push(30_000_000 + rng.next() % 40_000_000); // the sample
        s.push(4_000 + rng.next() % 4_000); // .nfo, .sfv: never major
        let plain = band_terms(&s);
        assert!(plain.is_empty(), "two signed sizes: unsigned");
        // Every other release has three extra large files.
        if i % 2 == 0 {
            s.extend((0..3).map(|_| 25_000_000 + rng.next() % 50_000_000));
        }
        let terms = band_terms(&s);
        assert_eq!(terms.len(), if i % 2 == 0 { BANDS } else { 0 });
        for t in &terms {
            *by_term.entry(*t).or_default() += 1;
        }
        releases.push(terms);
    }
    let mut pairs = 0usize;
    let mut shared = 0usize;
    for i in 0..n {
        for j in i + 1..n {
            pairs += 1;
            if releases[i].iter().any(|t| releases[j].contains(t)) {
                shared += 1;
            }
        }
    }
    assert!(
        shared * 100 <= pairs,
        "{shared} of {pairs} unrelated pairs share a band"
    );
    let most = by_term.values().copied().max().unwrap_or(0);
    assert!(most <= n / 100, "one band term is carried by {most} of {n}");
}

#[test]
fn far_payloads_are_rarely_candidates_and_never_similar() {
    // 10 shared, 5 own each: J = 10 / 20 = 0.5.
    let rate = candidate_rate(2, 1000, 10, 5);
    assert!(rate <= 0.35, "J = 0.5 candidate rate {rate}");
    // 6 shared, 7 own each: J = 6 / 20 = 0.3.
    let rate = candidate_rate(3, 1000, 6, 7);
    assert!(rate <= 0.10, "J = 0.3 candidate rate {rate}");
    let mut rng = Rng(4);
    for _ in 0..200 {
        let common = rng.sizes(10);
        let (mut a, mut b) = (common.clone(), common);
        a.extend(rng.sizes(5));
        b.extend(rng.sizes(5));
        a.sort_unstable();
        b.sort_unstable();
        assert!(!similar(&major_sizes(&a), &major_sizes(&b)));
    }
}

#[test]
fn near_payloads_are_found() {
    // 8 shared, 1 own each: J = 8 / 10 = 0.8.
    let rate = candidate_rate(5, 1000, 8, 1);
    assert!(rate >= 0.65, "J = 0.8 found {rate}");
    // 18 shared, 1 own each: J = 18 / 20 = 0.9.
    let rate = candidate_rate(6, 1000, 18, 1);
    assert!(rate >= 0.85, "J = 0.9 found {rate}");
}

#[test]
fn a_season_with_one_extra_episode_is_similar_and_a_candidate() {
    let mut rng = Rng(7);
    let season = rng.sizes(10);
    let mut plus = season.clone();
    plus.push(rng.size());
    let (a, b) = (major_sizes(&season), major_sizes(&plus));
    assert!(similar(&a, &b), "10 of 11");
    let (sa, sb) = (signature(&season).unwrap(), signature(&plus).unwrap());
    assert!(sa.agreeing(&sb) >= HASHES / 2, "{}", sa.agreeing(&sb));
}

/// A non-empty multiset of sizes with repeats, some major and some not.
fn payload() -> impl Strategy<Value = Vec<u64>> {
    let size = prop_oneof![
        Just(MIB),
        Just(700 * MIB),
        0u64..2 * MIB,
        MIB..8 << 30,
        any::<u64>().prop_map(|s| s >> 8),
    ];
    proptest::collection::vec(size, 1..40)
}

fn shuffle(v: &mut [u64], seed: u64) {
    let mut rng = Rng(seed);
    for i in (1..v.len()).rev() {
        let j = (rng.next() % (i as u64 + 1)) as usize;
        v.swap(i, j);
    }
}

proptest! {
    #[test]
    fn the_order_of_files_doesnt_matter(p in payload(), seed in any::<u64>()) {
        let mut q = p.clone();
        shuffle(&mut q, seed);
        prop_assert_eq!(signature(&p), signature(&q));
        prop_assert_eq!(band_terms(&p), band_terms(&q));
        prop_assert_eq!(major_sizes(&p), major_sizes(&q));
    }

    /// Equal multisets of major sizes give equal signatures, whatever
    /// the minor files around them; so do equal signed sizes, whatever
    /// the copy counts (the signature hashes `signed_sizes`; `similar`
    /// weighs the copies).
    #[test]
    fn equal_major_multisets_give_equal_signatures(p in payload(), seed in any::<u64>()) {
        let major = major_sizes(&p);
        let from_major = MinHash::of_major(&major);
        prop_assert_eq!(signature(&p).ok(), from_major);
        let mut again = major.clone();
        shuffle(&mut again, seed);
        again.sort_unstable();
        prop_assert_eq!(MinHash::of_major(&again), from_major);
        // Two copies of every size: the same signed sizes, unless that
        // makes a size a volume size (three or more copies).
        let mut doubled = major.clone();
        doubled.extend(&major);
        doubled.sort_unstable();
        if signed_sizes(&doubled) == signed_sizes(&major) {
            prop_assert_eq!(MinHash::of_major(&doubled), from_major);
        }
    }

    /// Counts matter: three copies of a size aren't two.
    #[test]
    fn copies_count(a in 1u64 << 30..2 << 30, b in 1u64 << 30..2 << 30, c in 1u64 << 30..2 << 30) {
        let major = major_sizes(&[a, b, c]);
        prop_assert_eq!(major.len(), MIN_MAJOR_FILES);
        let mut more = major.clone();
        more.push(major[0]);
        more.sort_unstable();
        // 3 of 4: under the threshold.
        prop_assert!(!similar(&major, &more));
    }

    /// Below the threshold there's no signature, no terms, and nothing is
    /// similar to it: signature-less torrents never match one another.
    #[test]
    fn below_the_threshold_nothing_matches(p in payload()) {
        let major = major_sizes(&p);
        if signed_sizes(&major).len() < MIN_MAJOR_FILES {
            prop_assert!(signature(&p).is_err());
            prop_assert!(band_terms(&p).is_empty());
            prop_assert!(!similar(&major, &major));
        } else {
            prop_assert_eq!(band_terms(&p).len(), BANDS);
            prop_assert!(similar(&major, &major));
        }
    }

    #[test]
    fn similar_is_symmetric(a in payload(), b in payload()) {
        let (a, b) = (major_sizes(&a), major_sizes(&b));
        prop_assert_eq!(similar(&a, &b), similar(&b, &a));
    }
}
