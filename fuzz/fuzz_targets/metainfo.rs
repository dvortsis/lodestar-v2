//! Info-dict and .torrent parsing never panic; parsed files satisfy the
//! model's invariants and survive the store's blob codec.
#![no_main]

use lodestar_metainfo::{Info, TorrentFile};
use lodestar_store::filetree;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(info) = Info::parse(data) {
        assert!(info.v1.is_some() || info.v2.is_some());
        assert!(info.files.iter().all(|f| !f.path.is_empty()));
        let blob = filetree::encode(&info.files, info.path_encoding).expect("metainfo files encode");
        let tree = filetree::decode(&blob).expect("own blob decodes");
        assert_eq!(tree.files, info.files);
    }
    let _ = TorrentFile::parse(data);
});
