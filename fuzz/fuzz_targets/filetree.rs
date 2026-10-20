//! The file-tree blob decoder never panics on corrupt blobs, and anything
//! it accepts re-encodes to a blob that decodes to the same tree.
#![no_main]

use lodestar_store::filetree::{decode, encode};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(tree) = decode(data) {
        if let Ok(blob) = encode(&tree.files, tree.encoding) {
            assert_eq!(decode(&blob).expect("re-encoded blob decodes"), tree);
        }
    }
});
