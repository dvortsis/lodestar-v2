//! Property tests for the file-tree blob: lossless round-trips through the
//! codec and zstd, and no panics on corrupt blobs.

use lodestar_metainfo::{File, FileAttrs, FileHashes, TextEncoding};
use lodestar_store::filetree::{decode, encode};
use lodestar_store::{BlobCodec, Codec};
use proptest::prelude::*;

fn segment() -> impl Strategy<Value = Vec<u8>> {
    prop_oneof![
        // Shared vocabulary so directories actually get interned.
        Just(b"Season 1".to_vec()),
        Just(b"Subs".to_vec()),
        Just(Vec::new()),
        prop::collection::vec(any::<u8>(), 0..10),
    ]
}

fn file() -> impl Strategy<Value = File> {
    (
        prop::collection::vec(segment(), 1..5),
        any::<u64>(),
        0u8..16,
        prop::option::of(prop::collection::vec(segment(), 0..3)),
        prop::option::of(any::<[u8; 32]>()),
        prop::option::of(any::<[u8; 16]>()),
        prop::option::of(any::<[u8; 20]>()),
        prop::option::of(any::<[u8; 16]>()),
    )
        .prop_map(|(path, size, attrs, target, root, md5, sha1, ed2k)| {
            let attrs = FileAttrs::from_bits_truncate(attrs);
            File {
                path,
                size,
                attrs,
                // Targets only exist on symlinks, as in metainfo.
                symlink_target: target.filter(|_| attrs.contains(FileAttrs::SYMLINK)),
                pieces_root: root,
                hashes: FileHashes { md5, sha1, ed2k },
            }
        })
}

fn encoding() -> impl Strategy<Value = TextEncoding> {
    (0u8..40).prop_map(|id| TextEncoding::from_id(id).unwrap())
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(1024))]

    #[test]
    fn roundtrip(files in prop::collection::vec(file(), 0..40), enc in encoding()) {
        let blob = encode(&files, enc).unwrap();
        let tree = decode(&blob).unwrap();
        prop_assert_eq!(tree.encoding, enc);
        prop_assert_eq!(tree.files, files);
    }

    #[test]
    fn roundtrip_through_zstd(files in prop::collection::vec(file(), 1..20)) {
        let mut codec = BlobCodec::new(3);
        let blob = encode(&files, TextEncoding::UTF8).unwrap();
        let z = codec.compress(&blob).unwrap();
        let back = codec.decompress(z.codec, z.dict_id, &z.data).unwrap();
        prop_assert_eq!(decode(&back).unwrap().files, files);
    }

    #[test]
    fn corrupt_blobs_never_panic(
        files in prop::collection::vec(file(), 1..10),
        edits in prop::collection::vec((any::<prop::sample::Index>(), any::<u8>()), 1..6),
        cut in any::<prop::sample::Index>(),
    ) {
        let mut blob = encode(&files, TextEncoding::UTF8).unwrap();
        for (idx, b) in edits {
            let i = idx.index(blob.len());
            blob[i] = b;
        }
        let _ = decode(&blob);
        let _ = decode(&blob[..cut.index(blob.len() + 1)]);
        let mut codec = BlobCodec::new(3);
        let _ = codec.decompress(Codec::Zstd, None, &blob);
    }

    #[test]
    fn arbitrary_bytes_never_panic(blob in prop::collection::vec(any::<u8>(), 0..128)) {
        let _ = decode(&blob);
        // Bias toward a valid header so the body parsers get exercised.
        let mut framed = vec![1, blob.first().copied().unwrap_or(0) & 0x3f, 0];
        framed.extend_from_slice(&blob);
        let _ = decode(&framed);
    }
}
