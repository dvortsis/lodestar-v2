//! A remote worker's file-tree blob, as the coordinator decodes it (WK-9,
//! `Preparer::decode_files`): bytes it didn't choose, under caps. Arbitrary
//! bytes never panic and never decompress past the cap; whatever decodes is
//! a tree that re-encodes and decodes to itself, and builds no more path
//! bytes than its budget. The shape pre-pass
//! (`filetree::shape`) agrees with the decoder: a blob it refuses doesn't
//! decode, and one that decodes has the file count and depth it measured.
//!
//! The first byte picks the codec (even: raw, odd: zstd); the next two give
//! the cap in KiB. Run it with a small allocation limit to check the caps
//! hold: `cargo +nightly fuzz run remote_blob -- -malloc_limit_mb=64`.

#![no_main]

use libfuzzer_sys::fuzz_target;
use lodestar_store::filetree::{decode, encode, shape};
use lodestar_store::{Codec, Compressed, Preparer};

/// Components the target lets a tree build: small, so a deep or wide blob
/// is refused by its shape well inside the allocation limit.
const COMPONENTS: u64 = 1 << 16;
/// Bytes of paths the target lets a tree build.
const PATH_BYTES: u64 = 4 << 20;

fuzz_target!(|data: &[u8]| {
    let [pick, hi, lo, body @ ..] = data else {
        return;
    };
    // The pre-pass and the decoder, on the bytes as given: a raw blob.
    // `decode` runs only on what the shape bounds, as `decode_files` does.
    match shape(body) {
        Err(_) => assert!(decode(body).is_err(), "shape refused what decode reads"),
        Ok(s) if s.components <= COMPONENTS && s.path_bytes <= PATH_BYTES => match decode(body) {
            Ok(tree) => {
                assert_eq!(s.files, tree.files.len());
                let deepest = tree.files.iter().map(|f| f.path.len()).max().unwrap_or(0);
                // A file's depth is its own; a deeper empty directory can
                // only make the shape's larger.
                assert!(deepest <= s.depth || s.depth >= 255);
            }
            Err(_) => {}
        },
        Ok(_) => {}
    }
    let codec = if pick % 2 == 0 { Codec::Raw } else { Codec::Zstd };
    let max = (usize::from(*hi) << 8 | usize::from(*lo)) << 10;
    let blob = Compressed {
        codec,
        dict_id: None,
        data: body.to_vec(),
    };
    let preparer = Preparer::from_dict(None, 3, false);
    if let Ok((tree, took, shape)) = preparer.decode_files(&blob, max, COMPONENTS, PATH_BYTES) {
        assert!(took <= max, "decompressed past the cap");
        assert!(shape.components <= COMPONENTS, "built past the component budget");
        // What decoding copied is what the shape measured, within budget.
        let dirs_and_files: u64 = tree
            .files
            .iter()
            .map(|f| f.path.iter().map(|p| p.len() as u64).sum::<u64>())
            .sum();
        assert!(dirs_and_files <= shape.path_bytes, "paths past the shape's count");
        assert!(shape.path_bytes <= PATH_BYTES, "paths past the budget");
        let again = encode(&tree.files, tree.encoding).expect("a decoded tree encodes");
        assert_eq!(decode(&again).expect("re-encoded tree decodes"), tree);
    }
});
