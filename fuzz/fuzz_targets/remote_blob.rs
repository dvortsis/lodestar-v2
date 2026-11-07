//! A remote worker's file-tree blob, as the coordinator decodes it (WK-9,
//! `Preparer::decode_files`): bytes it didn't choose, under a cap. Arbitrary
//! bytes never panic and never decompress past the cap; whatever decodes is
//! a tree that re-encodes and decodes to itself.
//!
//! The first byte picks the codec (even: raw, odd: zstd); the next two give
//! the cap in KiB. Run it with a small allocation limit to check the cap
//! holds: `cargo +nightly fuzz run remote_blob -- -malloc_limit_mb=64`.

#![no_main]

use libfuzzer_sys::fuzz_target;
use lodestar_store::filetree::{decode, encode};
use lodestar_store::{Codec, Compressed, Preparer};

fuzz_target!(|data: &[u8]| {
    let [pick, hi, lo, body @ ..] = data else {
        return;
    };
    let codec = if pick % 2 == 0 { Codec::Raw } else { Codec::Zstd };
    let max = (usize::from(*hi) << 8 | usize::from(*lo)) << 10;
    let blob = Compressed {
        codec,
        dict_id: None,
        data: body.to_vec(),
    };
    let preparer = Preparer::from_dict(None, 3, false);
    if let Ok((tree, took)) = preparer.decode_files(&blob, max) {
        assert!(took <= max, "decompressed past the cap");
        let again = encode(&tree.files, tree.encoding).expect("a decoded tree encodes");
        assert_eq!(decode(&again).expect("re-encoded tree decodes"), tree);
    }
});
