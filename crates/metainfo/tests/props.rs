//! Property tests: generated torrents parse back losslessly, and corrupted
//! input never panics.

use std::collections::BTreeMap;

use lodestar_bencode::{OwnedValue, to_vec};
use lodestar_metainfo::{Info, TorrentFile};
use proptest::prelude::*;

type Path = Vec<Vec<u8>>;

fn segment() -> impl Strategy<Value = Vec<u8>> {
    prop_oneof![
        3 => "[A-Za-z0-9 ._-]{1,12}".prop_map(String::into_bytes),
        1 => prop::collection::vec(any::<u8>(), 1..12),
    ]
}

fn path() -> impl Strategy<Value = Path> {
    prop::collection::vec(segment(), 1..4)
        // Keep clear of legacy padding names, which change attributes.
        .prop_filter("padding-like", |p| p[0] != b".pad")
}

fn bytes(v: &[u8]) -> OwnedValue {
    OwnedValue::Bytes(v.to_vec())
}

fn dict(entries: Vec<(&str, OwnedValue)>) -> OwnedValue {
    OwnedValue::Dict(
        entries
            .into_iter()
            .map(|(k, v)| (k.as_bytes().to_vec(), v))
            .collect(),
    )
}

fn v1_info(name: &[u8], files: &[(Path, u64)], piece_len: u64) -> OwnedValue {
    let total: u64 = files.iter().map(|(_, s)| s).sum();
    let n_pieces = total.div_ceil(piece_len) as usize;
    let files = files
        .iter()
        .map(|(p, size)| {
            dict(vec![
                ("length", OwnedValue::Int(*size as i64)),
                (
                    "path",
                    OwnedValue::List(p.iter().map(|s| bytes(s)).collect()),
                ),
            ])
        })
        .collect();
    dict(vec![
        ("files", OwnedValue::List(files)),
        ("name", bytes(name)),
        ("piece length", OwnedValue::Int(piece_len as i64)),
        ("pieces", OwnedValue::Bytes(vec![0; n_pieces * 20])),
    ])
}

/// Builds a v2 file tree; paths must be prefix-free.
fn v2_tree(files: &BTreeMap<Path, u64>) -> OwnedValue {
    let mut root = BTreeMap::new();
    for (p, size) in files {
        let mut node = &mut root;
        for seg in &p[..p.len() - 1] {
            let child = node
                .entry(seg.clone())
                .or_insert_with(|| OwnedValue::Dict(BTreeMap::new()));
            let OwnedValue::Dict(child) = child else {
                unreachable!()
            };
            node = child;
        }
        let mut props = vec![("length", OwnedValue::Int(*size as i64))];
        if *size > 0 {
            props.push((
                "pieces root",
                OwnedValue::Bytes(vec![size.to_le_bytes()[0]; 32]),
            ));
        }
        node.insert(p.last().unwrap().clone(), dict(vec![("", dict(props))]));
    }
    OwnedValue::Dict(root)
}

fn prefix_free(files: &BTreeMap<Path, u64>) -> bool {
    let paths: Vec<_> = files.keys().collect();
    paths.iter().all(|a| {
        paths
            .iter()
            .all(|b| a == b || !(b.len() > a.len() && b.starts_with(a)))
    })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    #[test]
    fn v1_roundtrip(
        name in segment(),
        files in prop::collection::vec((path(), 0u64..1 << 31), 1..20),
        piece_exp in 18u32..24,
    ) {
        let raw = to_vec(&v1_info(&name, &files, 1 << piece_exp));
        let info = Info::parse(&raw).unwrap();
        prop_assert_eq!(&info.name.raw, &name);
        prop_assert_eq!(info.files.len(), files.len());
        for (f, (p, size)) in info.files.iter().zip(&files) {
            prop_assert_eq!(&f.path, p);
            prop_assert_eq!(f.size, *size);
        }
        prop_assert_eq!(info.total_size, files.iter().map(|(_, s)| s).sum::<u64>());
        prop_assert!(info.warnings.iter().all(|w| matches!(w, lodestar_metainfo::Warning::SuspiciousPath)));
    }

    #[test]
    fn v2_roundtrip(
        files in prop::collection::btree_map(
            prop::collection::vec("[a-z]{1,3}".prop_map(String::into_bytes), 1..4),
            0u64..1 << 30,
            1..16,
        ).prop_filter("prefix-free", prefix_free),
    ) {
        let info_dict = dict(vec![
            ("file tree", v2_tree(&files)),
            ("meta version", OwnedValue::Int(2)),
            ("name", bytes(b"t")),
            ("piece length", OwnedValue::Int(1 << 16)),
        ]);
        let info = Info::parse(&to_vec(&info_dict)).unwrap();
        let got: Vec<(Path, u64)> = info.files.iter().map(|f| (f.path.clone(), f.size)).collect();
        let want: Vec<(Path, u64)> = files.into_iter().collect();
        prop_assert_eq!(got, want);
        prop_assert!(info.warnings.is_empty());
    }

    #[test]
    fn corrupted_info_never_panics(
        files in prop::collection::vec((path(), 0u64..1 << 30), 1..6),
        edits in prop::collection::vec((any::<prop::sample::Index>(), any::<u8>()), 1..6),
        cut in any::<prop::sample::Index>(),
    ) {
        let mut raw = to_vec(&v1_info(b"name", &files, 1 << 14));
        for (idx, b) in edits {
            let i = idx.index(raw.len());
            raw[i] = b;
        }
        let _ = Info::parse(&raw);
        let _ = Info::parse(&raw[..cut.index(raw.len() + 1)]);
        let torrent = [&b"d4:info"[..], &raw, b"e"].concat();
        let _ = TorrentFile::parse(&torrent);
    }

    #[test]
    fn arbitrary_bytes_never_panic(raw in prop::collection::vec(any::<u8>(), 0..256)) {
        let _ = Info::parse(&raw);
        let _ = TorrentFile::parse(&raw);
    }
}
