//! The payload signature (§6 "Grouping/dedup"): a hash of the sorted
//! sizes of a torrent's non-padding files, the same under any name. CS-8a
//! (§20.5.1) blocks reposts of a CS-1 torrent by it, but only when the
//! payload is specific enough that an unrelated torrent can't share it.
//!
//! The hash is `lodestar_classify::payload`'s, the one search indexes for
//! `sig:` (GS-0), so "block this payload" and `sig:` always agree. This
//! module adds CS-8a's specificity rule on top.

use lodestar_classify::FileInfo;

/// A payload at least this large is specific.
pub const SPECIFIC_BYTES: u64 = 50 << 20;
/// Or it has at least this many files of distinctive size.
pub const SPECIFIC_FILES: usize = 2;
/// A size is distinctive if it's at least this and not a multiple of it
/// (so not a padded, block-aligned or empty placeholder).
const DISTINCTIVE: u64 = 64 << 10;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PayloadSig {
    pub sig: u64,
    /// Specific enough for CS-8a.
    pub specific: bool,
}

/// The signature of a torrent's files; `None` with no content files.
pub fn payload_sig(files: &[FileInfo]) -> Option<PayloadSig> {
    let sig = lodestar_classify::payload::payload_sig(files)?;
    let content = || files.iter().filter(|f| !f.padding).map(|f| f.size);
    let total: u64 = content().fold(0u64, |a, s| a.saturating_add(s));
    let distinctive = content()
        .filter(|&s| s >= DISTINCTIVE && s % DISTINCTIVE != 0)
        .count();
    Some(PayloadSig {
        sig,
        specific: total >= SPECIFIC_BYTES || distinctive >= SPECIFIC_FILES,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn files(sizes: &[u64]) -> Vec<FileInfo> {
        sizes
            .iter()
            .enumerate()
            .map(|(i, &s)| FileInfo::new(vec![format!("f{i}")], s))
            .collect()
    }

    /// CS-8a as it was before the hash moved to `classify` (GS-0): the
    /// signature and the specificity of each, computed then.
    #[test]
    fn cs8a_golden() {
        let cases: [(&[u64], u64, bool); 6] = [
            (&[0], 0x45c7_cc7a_d09f_1bd7, false),
            (&[0, 0], 0xf0f3_2bcd_c2b8_4f38, false),
            (&[1, 2, 3], 0xc139_462d_0f89_1eb8, false),
            (&[100_001, 200_003], 0xf90e_dc4d_82fd_355c, true),
            (&[50 << 20], 0xa89f_7740_e0f3_d1ff, true),
            (
                &[u64::MAX, 1 << 63, 4_700_000_000],
                0x132d_ea17_fc37_2d7d,
                true,
            ),
        ];
        for (sizes, sig, specific) in cases {
            assert_eq!(
                payload_sig(&files(sizes)),
                Some(PayloadSig { sig, specific }),
                "{sizes:?}"
            );
            assert_eq!(
                lodestar_classify::payload::payload_sig(&files(sizes)),
                Some(sig)
            );
        }
    }

    #[test]
    fn order_and_names_dont_matter() {
        let a = payload_sig(&files(&[3, 1, 2])).unwrap();
        let b = payload_sig(&files(&[1, 2, 3])).unwrap();
        assert_eq!(a, b);
        assert_ne!(a.sig, payload_sig(&files(&[1, 2, 4])).unwrap().sig);
        assert_ne!(
            payload_sig(&files(&[0])).unwrap().sig,
            payload_sig(&files(&[0, 0])).unwrap().sig
        );
    }

    #[test]
    fn padding_is_ignored() {
        let mut f = files(&[100_000, 200_000]);
        let plain = payload_sig(&f).unwrap();
        let mut pad = FileInfo::new(vec!["pad".into()], 16 << 10);
        pad.padding = true;
        f.push(pad.clone());
        assert_eq!(payload_sig(&f).unwrap(), plain);
        assert_eq!(payload_sig(&[]), None);
        assert_eq!(payload_sig(&[pad]), None);
        // A padding file's size never counts toward specificity.
        let mut big_pad = FileInfo::new(vec!["pad".into()], SPECIFIC_BYTES);
        big_pad.padding = true;
        let mut small = files(&[10]);
        small.push(big_pad);
        assert!(!payload_sig(&small).unwrap().specific);
    }

    #[test]
    fn specificity() {
        assert!(payload_sig(&files(&[SPECIFIC_BYTES])).unwrap().specific);
        assert!(payload_sig(&files(&[100_001, 200_003])).unwrap().specific);
        // Small, or only round sizes: too easy to share by chance.
        assert!(!payload_sig(&files(&[100_001])).unwrap().specific);
        assert!(!payload_sig(&files(&[1 << 20, 2 << 20])).unwrap().specific);
        assert!(!payload_sig(&files(&[10, 20, 30])).unwrap().specific);
    }
}
