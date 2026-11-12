//! The payload signature (§6 "Grouping/dedup"): a hash of the sorted
//! sizes of a torrent's non-padding files, the same under any name and in
//! any file order. Search indexes it for `sig:` and filter's CS-8a blocks
//! reposts by it, so both read this one function (plan v0.7 §3).
//!
//! The value is stored (the search index's `payload_sig` field and the
//! filter's CS-8 keys), so it must never change: the goldens below pin
//! it. A change would need every index backfilled and every stored key
//! rewritten.

use crate::FileInfo;

/// The signature of a torrent's files: `None` when it has no content
/// files (none at all, or only BEP 47 padding).
pub fn payload_sig(files: &[FileInfo]) -> Option<u64> {
    payload_sig_of_sizes(
        files
            .iter()
            .filter(|f| !f.padding)
            .map(|f| f.size)
            .collect(),
    )
}

/// [`payload_sig`] of the content files' sizes, in any order: the caller
/// has already left padding files out. `None` when there are none.
pub fn payload_sig_of_sizes(mut sizes: Vec<u64>) -> Option<u64> {
    if sizes.is_empty() {
        return None;
    }
    sizes.sort_unstable();
    let mut h: u64 = 0x6c6f_6465_7374_6172; // "lodestar"
    for s in &sizes {
        h = fmix64(h ^ s);
    }
    Some(fmix64(h ^ sizes.len() as u64))
}

/// MurmurHash3's 64-bit finalizer, as `lodestar_filter`'s term hashes use
/// it.
fn fmix64(mut h: u64) -> u64 {
    h ^= h >> 33;
    h = h.wrapping_mul(0xff51_afd7_ed55_8ccd);
    h ^= h >> 33;
    h = h.wrapping_mul(0xc4ce_b9fe_1a85_ec53);
    h ^ (h >> 33)
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn files(sizes: &[u64]) -> Vec<FileInfo> {
        sizes
            .iter()
            .enumerate()
            .map(|(i, &s)| FileInfo::new(vec![format!("f{i}")], s))
            .collect()
    }

    /// The function as `lodestar_filter::payload` had it before the move
    /// (CS-8a), kept verbatim as the reference.
    fn old_filter_sig(files: &[FileInfo]) -> Option<u64> {
        let mut sizes: Vec<u64> = files
            .iter()
            .filter(|f| !f.padding)
            .map(|f| f.size)
            .collect();
        if sizes.is_empty() {
            return None;
        }
        sizes.sort_unstable();
        let mut h: u64 = 0x6c6f_6465_7374_6172;
        for s in &sizes {
            h = fmix64(h ^ s);
        }
        Some(fmix64(h ^ sizes.len() as u64))
    }

    /// Values computed before the move: stored keys depend on them.
    #[test]
    fn goldens() {
        let cases: [(&[u64], u64); 6] = [
            (&[0], 0x45c7_cc7a_d09f_1bd7),
            (&[0, 0], 0xf0f3_2bcd_c2b8_4f38),
            (&[1, 2, 3], 0xc139_462d_0f89_1eb8),
            (&[100_001, 200_003], 0xf90e_dc4d_82fd_355c),
            (&[50 << 20], 0xa89f_7740_e0f3_d1ff),
            (&[u64::MAX, 1 << 63, 4_700_000_000], 0x132d_ea17_fc37_2d7d),
        ];
        for (sizes, want) in cases {
            assert_eq!(payload_sig(&files(sizes)), Some(want), "{sizes:?}");
        }
    }

    #[test]
    fn order_and_names_dont_matter_and_counts_do() {
        assert_eq!(
            payload_sig(&files(&[3, 1, 2])),
            payload_sig(&files(&[1, 2, 3]))
        );
        assert_ne!(
            payload_sig(&files(&[1, 2, 3])),
            payload_sig(&files(&[1, 2, 4]))
        );
        assert_ne!(payload_sig(&files(&[0])), payload_sig(&files(&[0, 0])));
        let mut renamed = files(&[1, 2, 3]);
        renamed[0].path = vec!["other".into(), "name.mkv".into()];
        assert_eq!(payload_sig(&renamed), payload_sig(&files(&[1, 2, 3])));
    }

    #[test]
    fn padding_is_ignored_and_nothing_left_is_none() {
        let mut f = files(&[100_000, 200_000]);
        let plain = payload_sig(&f);
        let mut pad = FileInfo::new(vec!["pad".into()], 16 << 10);
        pad.padding = true;
        f.insert(1, pad.clone());
        assert_eq!(payload_sig(&f), plain);
        assert_eq!(payload_sig(&[]), None);
        assert_eq!(payload_sig(&[pad]), None);
        assert_eq!(payload_sig_of_sizes(Vec::new()), None);
        assert_eq!(payload_sig_of_sizes(vec![200_000, 100_000]), plain);
    }

    fn file_list() -> impl Strategy<Value = Vec<FileInfo>> {
        let size = prop_oneof![0u64..4, 0u64..1 << 20, any::<u64>()];
        proptest::collection::vec((size, any::<bool>()), 0..24).prop_map(|v| {
            v.into_iter()
                .enumerate()
                .map(|(i, (size, padding))| FileInfo {
                    path: vec![format!("f{i}")],
                    size,
                    padding,
                })
                .collect()
        })
    }

    proptest! {
        #[test]
        fn equals_the_old_filter_computation(f in file_list()) {
            prop_assert_eq!(payload_sig(&f), old_filter_sig(&f));
        }

        #[test]
        fn is_order_independent(mut f in file_list(), seed in any::<u64>()) {
            let want = payload_sig(&f);
            // A deterministic shuffle.
            let n = f.len();
            for i in (1..n).rev() {
                let j = (seed.wrapping_mul(i as u64 + 1) >> 7) as usize % (i + 1);
                f.swap(i, j);
            }
            prop_assert_eq!(payload_sig(&f), want);
        }
    }
}
