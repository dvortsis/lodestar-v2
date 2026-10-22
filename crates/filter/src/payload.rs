//! The payload signature (§6 "Grouping/dedup"): a hash of the sorted
//! sizes of a torrent's non-padding files, the same under any name. CS-8a
//! (§20.5.1) blocks reposts of a CS-1 torrent by it, but only when the
//! payload is specific enough that an unrelated torrent can't share it.

use lodestar_classify::FileInfo;

use crate::hash::fmix64;

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
    let mut sizes: Vec<u64> = files
        .iter()
        .filter(|f| !f.padding)
        .map(|f| f.size)
        .collect();
    if sizes.is_empty() {
        return None;
    }
    sizes.sort_unstable();
    let mut h: u64 = 0x6c6f_6465_7374_6172; // "lodestar"
    for s in &sizes {
        h = fmix64(h ^ s);
    }
    let total: u64 = sizes.iter().fold(0u64, |a, &s| a.saturating_add(s));
    let distinctive = sizes
        .iter()
        .filter(|&&s| s >= DISTINCTIVE && s % DISTINCTIVE != 0)
        .count();
    Some(PayloadSig {
        sig: fmix64(h ^ sizes.len() as u64),
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
        f.push(pad);
        assert_eq!(payload_sig(&f).unwrap(), plain);
        assert_eq!(payload_sig(&[]), None);
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
