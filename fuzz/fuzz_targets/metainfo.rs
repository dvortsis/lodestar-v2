//! Info-dict and .torrent parsing never panic; parsed files satisfy the
//! model's invariants, stay within the combined path budget and survive
//! the store's blob codec.
#![no_main]

use libfuzzer_sys::fuzz_target;
use lodestar_metainfo::{FileAttrs, Info, Limits, TorrentFile};
use lodestar_store::filetree;

fuzz_target!(|data: &[u8]| {
    if let Ok(info) = Info::parse(data) {
        assert!(info.v1.is_some() || info.v2.is_some());
        assert!(info.files.iter().all(|f| !f.path.is_empty()));
        // The combined path budget holds over everything the parse built.
        let paths = || {
            info.files
                .iter()
                .flat_map(|f| std::iter::once(&f.path).chain(&f.symlink_target))
        };
        let segments: usize = paths().map(Vec::len).sum();
        let bytes: usize = paths().flatten().map(Vec::len).sum();
        assert!(segments <= Limits::DEFAULT.max_total_path_segments);
        assert!(bytes <= Limits::DEFAULT.max_total_path_bytes);
        // Targets exist only on symlinks and went through the path limits.
        assert!(info.files.iter().all(|f| match &f.symlink_target {
            None => true,
            Some(t) => {
                f.attrs.contains(FileAttrs::SYMLINK)
                    && t.len() <= Limits::DEFAULT.max_path_depth
                    && t.iter().map(Vec::len).sum::<usize>() <= Limits::DEFAULT.max_path_bytes
            }
        }));
        let content: u64 = info
            .files
            .iter()
            .filter(|f| !f.is_padding())
            .map(|f| f.size)
            .sum();
        assert_eq!(info.total_size, content, "padding never counts");
        let blob =
            filetree::encode(&info.files, info.path_encoding).expect("metainfo files encode");
        let tree = filetree::decode(&blob).expect("own blob decodes");
        assert_eq!(tree.files, info.files);
    }
    let _ = TorrentFile::parse(data);
});
