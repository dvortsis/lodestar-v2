use std::collections::BTreeMap;

use lodestar_bencode::{OwnedValue, to_vec};
use lodestar_metainfo::{
    Error, FileAttrs, Info, Limits, MetaVersion, ParseOptions, TextEncoding, TorrentFile, Warning,
    info_without_pieces,
};

fn d(entries: Vec<(&str, OwnedValue)>) -> OwnedValue {
    OwnedValue::Dict(
        entries
            .into_iter()
            .map(|(k, v)| (k.as_bytes().to_vec(), v))
            .collect(),
    )
}

fn b(s: impl AsRef<[u8]>) -> OwnedValue {
    OwnedValue::Bytes(s.as_ref().to_vec())
}

fn i(v: i64) -> OwnedValue {
    OwnedValue::Int(v)
}

fn l(items: Vec<OwnedValue>) -> OwnedValue {
    OwnedValue::List(items)
}

fn path(segs: &[&str]) -> OwnedValue {
    l(segs.iter().map(b).collect())
}

fn pieces(n: usize) -> OwnedValue {
    b(vec![0xAB; n * 20])
}

fn v1_file(segs: &[&str], len: i64) -> OwnedValue {
    d(vec![("length", i(len)), ("path", path(segs))])
}

fn multi(name: &str, files: Vec<OwnedValue>, piece_len: i64, n_pieces: usize) -> OwnedValue {
    d(vec![
        ("files", l(files)),
        ("name", b(name)),
        ("piece length", i(piece_len)),
        ("pieces", pieces(n_pieces)),
    ])
}

fn with(mut v: OwnedValue, key: &str, value: OwnedValue) -> OwnedValue {
    if let OwnedValue::Dict(m) = &mut v {
        m.insert(key.as_bytes().to_vec(), value);
    }
    v
}

fn without(mut v: OwnedValue, key: &str) -> OwnedValue {
    if let OwnedValue::Dict(m) = &mut v {
        m.remove(key.as_bytes());
    }
    v
}

fn parse(v: &OwnedValue) -> Result<Info, Error> {
    Info::parse(&to_vec(v))
}

#[test]
fn single_file_v1_matches_independent_hash() {
    let raw = [
        &b"d6:lengthi1000e4:name8:test.bin12:piece lengthi16384e6:pieces20:"[..],
        &[b'a'; 20],
        b"e",
    ]
    .concat();
    let info = Info::parse(&raw).unwrap();
    // Reference digests computed with Python's hashlib over the same bytes.
    assert_eq!(
        info.v1.unwrap().to_string(),
        "02d09e90bcc57b18b6897aad012331e9c884a8e1"
    );
    assert!(info.v2.is_none());
    assert_eq!(info.primary_hash(), info.v1.unwrap());
    assert_eq!(info.version, MetaVersion::V1);
    assert!(info.single_file);
    assert_eq!(info.name.decode(), "test.bin");
    assert_eq!(info.total_size, 1000);
    assert_eq!(info.piece_count, 1);
    assert_eq!(info.info_size, raw.len());
    assert_eq!(info.files.len(), 1);
    assert_eq!(info.files[0].path, vec![b"test.bin".to_vec()]);
    assert_eq!(info.path_encoding, TextEncoding::UTF8);
    assert!(info.warnings.is_empty());
    assert!(info.extra.is_none());
}

#[test]
fn multi_file_v1_full_capture() {
    let files = vec![
        with(
            v1_file(&["Show", "S01E01.mkv"], 30_000),
            "md5sum",
            b("0123456789abcdef0123456789ABCDEF"),
        ),
        with(
            d(vec![
                ("length", i(2_768)),
                ("path", path(&[".pad", "2768"])),
            ]),
            "attr",
            b("p"),
        ),
        with(
            with(v1_file(&["run.sh"], 0), "attr", b("xl")),
            "symlink path",
            path(&["bin", "real.sh"]),
        ),
        with(v1_file(&["Show", "hidden.nfo"], 100), "sha1", b([7u8; 20])),
        v1_file(&["_____padding_file_0_"], 50),
        with(v1_file(&["bad.bin"], 1), "ed2k", b("nothex")),
    ];
    let v = with(
        with(
            with(multi("Show.Pack", files, 16_384, 2), "private", i(1)),
            "source",
            b("TRK"),
        ),
        "x_cross_seed",
        b("abc"),
    );
    let info = parse(&v).unwrap();
    assert!(!info.single_file);
    assert!(info.private);
    assert_eq!(info.source.as_deref(), Some(&b"TRK"[..]));
    assert_eq!(info.files.len(), 6);
    assert_eq!(info.total_size, 30_000 + 100 + 1);
    assert_eq!(info.path_string(&info.files[0]), "Show/S01E01.mkv");
    assert_eq!(
        info.files[0].hashes.md5.unwrap(),
        [
            0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd, 0xef, 0x01, 0x23, 0x45, 0x67, 0x89, 0xab,
            0xcd, 0xef
        ]
    );
    assert!(info.files[1].is_padding());
    assert!(info.files[2].attrs.contains(FileAttrs::SYMLINK));
    assert!(info.files[2].attrs.contains(FileAttrs::EXECUTABLE));
    assert_eq!(
        info.files[2].symlink_target,
        Some(vec![b"bin".to_vec(), b"real.sh".to_vec()])
    );
    assert_eq!(info.files[3].hashes.sha1, Some([7u8; 20]));
    assert!(info.files[4].is_padding(), "legacy BitComet padding name");
    assert!(info.files[5].hashes.ed2k.is_none());
    assert!(info.warnings.contains(&Warning::BadFileHash));
    assert_eq!(info.extra.as_deref(), Some(&b"d12:x_cross_seed3:abce"[..]));
}

#[test]
fn piece_count_mismatch_is_a_warning() {
    let info = parse(&multi("x", vec![v1_file(&["a"], 40_000)], 16_384, 2)).unwrap();
    assert_eq!(
        info.warnings,
        vec![Warning::PieceCountMismatch {
            expected: 3,
            actual: 2
        }]
    );
}

#[test]
fn suspicious_paths_are_flagged() {
    let info = parse(&multi("x", vec![v1_file(&["..", "etc"], 1)], 16_384, 1)).unwrap();
    assert!(info.warnings.contains(&Warning::SuspiciousPath));
}

fn v2_leaf(len: i64, root: u8) -> OwnedValue {
    let mut props = vec![("length", i(len))];
    if len > 0 {
        props.push(("pieces root", b([root; 32])));
    }
    d(vec![("", d(props))])
}

fn v2_tree() -> OwnedValue {
    d(vec![
        (
            "dir",
            d(vec![
                ("b.mkv", v2_leaf(70_000, 2)),
                ("a.txt", v2_leaf(10, 1)),
            ]),
        ),
        ("empty.dat", v2_leaf(0, 0)),
    ])
}

#[test]
fn pure_v2() {
    let v = d(vec![
        ("file tree", v2_tree()),
        ("meta version", i(2)),
        ("name", b("v2 torrent")),
        ("piece length", i(16_384)),
    ]);
    let raw = to_vec(&v);
    let info = Info::parse(&raw).unwrap();
    assert_eq!(info.version, MetaVersion::V2);
    assert!(info.v1.is_none());
    let v2 = info.v2.unwrap();
    assert_eq!(info.primary_hash(), v2.truncated());
    assert_eq!(
        info.swarm_hashes().collect::<Vec<_>>(),
        vec![v2.truncated()]
    );
    let paths: Vec<_> = info.files.iter().map(|f| info.path_string(f)).collect();
    assert_eq!(paths, ["dir/a.txt", "dir/b.mkv", "empty.dat"]);
    assert_eq!(info.files[0].pieces_root, Some([1; 32]));
    assert_eq!(info.files[2].pieces_root, None);
    // Per-file piece alignment: ceil(10/16k) + ceil(70000/16k) + 0.
    assert_eq!(info.piece_count, 1 + 5);
    assert!(info.warnings.is_empty());
}

fn hybrid(v1_sizes: (i64, i64)) -> OwnedValue {
    let files = vec![
        v1_file(&["dir", "a.txt"], v1_sizes.0),
        with(v1_file(&[".pad", "16374"], 16_374), "attr", b("p")),
        v1_file(&["dir", "b.mkv"], v1_sizes.1),
        with(v1_file(&[".pad", "11920"], 11_920), "attr", b("p")),
        v1_file(&["empty.dat"], 0),
    ];
    with(
        with(multi("hy", files, 16_384, 6), "file tree", v2_tree()),
        "meta version",
        i(2),
    )
}

#[test]
fn hybrid_attaches_pieces_roots() {
    let info = parse(&hybrid((10, 70_000))).unwrap();
    assert_eq!(info.version, MetaVersion::Hybrid);
    assert!(info.v1.is_some() && info.v2.is_some());
    assert_eq!(info.swarm_hashes().count(), 2);
    assert_eq!(info.files.len(), 5);
    assert_eq!(info.files[0].pieces_root, Some([1; 32]));
    assert_eq!(info.files[2].pieces_root, Some([2; 32]));
    assert_eq!(info.total_size, 70_010);
    assert!(info.warnings.is_empty(), "{:?}", info.warnings);
}

#[test]
fn hybrid_mismatch_is_flagged() {
    let info = parse(&hybrid((10, 69_999))).unwrap();
    assert!(info.warnings.contains(&Warning::HybridMismatch));
}

#[test]
fn legacy_encoding_is_detected_and_raw_bytes_kept() {
    let gbk = |s: &str| encoding_rs::GBK.encode(s).0.into_owned();
    let files = vec![
        d(vec![
            ("length", i(1)),
            ("path", l(vec![b(gbk("第一集 高清中文字幕.mkv"))])),
        ]),
        d(vec![
            ("length", i(1)),
            ("path", l(vec![b(gbk("第二集 高清中文字幕.mkv"))])),
        ]),
    ];
    let v = d(vec![
        ("files", l(files)),
        ("name", b(gbk("电视剧合集"))),
        ("piece length", i(16_384)),
        ("pieces", pieces(1)),
    ]);
    let info = parse(&v).unwrap();
    assert_ne!(info.path_encoding, TextEncoding::UTF8);
    assert_eq!(info.name.decode(), "电视剧合集");
    assert_eq!(info.path_string(&info.files[1]), "第二集 高清中文字幕.mkv");
    assert_eq!(info.files[0].path[0], gbk("第一集 高清中文字幕.mkv"));
}

#[test]
fn utf8_keys_take_precedence() {
    let files = vec![d(vec![
        ("length", i(1)),
        ("path", l(vec![b(b"\xb5\xda\xd2\xbb")])),
        ("path.utf-8", path(&["第一"])),
    ])];
    let v = with(multi("\u{fffd}", files, 16_384, 1), "name.utf-8", b("名字"));
    let info = parse(&v).unwrap();
    assert_eq!(info.path_encoding, TextEncoding::UTF8);
    assert_eq!(info.name.decode(), "名字");
    assert_eq!(info.path_string(&info.files[0]), "第一");
}

#[test]
fn info_without_pieces_strips_only_pieces() {
    let v = multi("x", vec![v1_file(&["a"], 5)], 16_384, 1);
    let stripped = info_without_pieces(&to_vec(&v)).unwrap();
    assert_eq!(stripped, to_vec(&without(v, "pieces")));
}

#[test]
fn rejections() {
    let base = multi("x", vec![v1_file(&["a"], 5)], 16_384, 1);
    let cases: Vec<(OwnedValue, &str)> = vec![
        (without(base.clone(), "name"), "missing field \"name\""),
        (without(base.clone(), "pieces"), "missing field \"pieces\""),
        (
            without(base.clone(), "files"),
            "missing field \"length or files\"",
        ),
        (with(base.clone(), "length", i(5)), "both length and files"),
        (with(base.clone(), "files", l(vec![])), "empty file list"),
        (
            with(base.clone(), "files", l(vec![v1_file(&["a"], -1)])),
            "negative",
        ),
        (
            with(base.clone(), "files", l(vec![v1_file(&[], 1)])),
            "empty path",
        ),
        (
            // Only padding may omit its path, and only symlinks their length.
            with(base.clone(), "files", l(vec![d(vec![("length", i(1))])])),
            "missing field \"path\"",
        ),
        (
            with(
                base.clone(),
                "files",
                l(vec![d(vec![("attr", b("hx")), ("path", path(&["a"]))])]),
            ),
            "missing field \"length\"",
        ),
        (
            d(vec![
                (
                    "file tree",
                    d(vec![("f", d(vec![("", d(vec![("attr", b("x"))]))]))]),
                ),
                ("meta version", i(2)),
                ("name", b("x")),
                ("piece length", i(16_384)),
            ]),
            "missing field \"length\"",
        ),
        (with(base.clone(), "pieces", b([0; 19])), "multiple of 20"),
        (with(base.clone(), "piece length", i(0)), "zero"),
        (with(base.clone(), "meta version", i(3)), "meta version 3"),
        (with(base.clone(), "name", i(3)), "should be a byte string"),
        (
            with(
                base.clone(),
                "files",
                l(vec![
                    v1_file(&["a"], i64::MAX),
                    v1_file(&["b"], i64::MAX),
                    v1_file(&["c"], 2),
                ]),
            ),
            "overflows",
        ),
        (
            with(
                with(base.clone(), "meta version", i(2)),
                "piece length",
                i(1000),
            ),
            "power of two",
        ),
        (
            d(vec![
                (
                    "file tree",
                    d(vec![(
                        "f",
                        d(vec![("", d(vec![("length", i(1))])), ("g", v2_leaf(1, 1))]),
                    )]),
                ),
                ("meta version", i(2)),
                ("name", b("x")),
                ("piece length", i(16_384)),
            ]),
            "both a file and a directory",
        ),
        (
            // Fits in u64 but not in a SQLite integer.
            with(
                base.clone(),
                "files",
                l(vec![v1_file(&["a"], i64::MAX), v1_file(&["b"], 1)]),
            ),
            "overflows",
        ),
        (b("not a dict"), "should be a dictionary"),
    ];
    for (v, expected) in cases {
        let err = parse(&v).unwrap_err().to_string();
        assert!(
            err.contains(expected),
            "{err:?} should mention {expected:?}"
        );
    }
}

#[test]
fn limits_are_enforced() {
    let opts = ParseOptions {
        limits: Limits {
            max_files: 2,
            max_path_depth: 3,
            max_path_bytes: 10,
            max_info_bytes: 400,
            ..Limits::DEFAULT
        },
        encoding_hint: None,
    };
    let check = |v: OwnedValue, expected: &str| {
        let err = Info::parse_with(&to_vec(&v), &opts)
            .unwrap_err()
            .to_string();
        assert!(
            err.contains(expected),
            "{err:?} should mention {expected:?}"
        );
    };
    let f = |segs: &[&str]| v1_file(segs, 1);
    check(
        multi("x", vec![f(&["a"]), f(&["b"]), f(&["c"])], 16_384, 1),
        "file count",
    );
    check(
        multi("x", vec![f(&["a", "b", "c", "d"])], 16_384, 1),
        "path depth",
    );
    check(
        multi("x", vec![f(&["abcdef", "ghijkl"])], 16_384, 1),
        "path length",
    );
    check(
        multi("x", vec![f(&["a"])], 16_384, 20),
        "over the 400-byte limit",
    );
}

#[test]
fn torrent_file_fields() {
    let info = multi("x", vec![v1_file(&["a"], 5)], 16_384, 1);
    let mut top = BTreeMap::new();
    top.insert(b"info".to_vec(), info.clone());
    top.insert(b"announce".to_vec(), b("udp://fallback"));
    top.insert(
        b"announce-list".to_vec(),
        l(vec![
            l(vec![b("udp://a"), b("udp://b")]),
            l(vec![]),
            l(vec![b("http://c")]),
        ]),
    );
    top.insert(b"creation date".to_vec(), i(1_700_000_000));
    top.insert(b"created by".to_vec(), b("mktorrent 1.1"));
    top.insert(b"comment".to_vec(), b(b"caf\xe9"));
    top.insert(b"encoding".to_vec(), b("windows-1252"));
    top.insert(b"url-list".to_vec(), b("http://seed"));
    let t = TorrentFile::parse(&to_vec(&OwnedValue::Dict(top.clone()))).unwrap();
    assert_eq!(t.info, parse(&info).unwrap());
    assert_eq!(
        t.announce,
        vec![vec!["udp://a", "udp://b"], vec!["http://c"]]
    );
    assert_eq!(t.creation_date, Some(1_700_000_000));
    assert_eq!(t.created_by.as_deref(), Some("mktorrent 1.1"));
    assert_eq!(t.comment.as_deref(), Some("café"));
    assert_eq!(t.url_list, vec!["http://seed"]);

    top.remove(&b"announce-list"[..]);
    top.insert(b"comment.utf-8".to_vec(), b("naïve"));
    top.insert(b"url-list".to_vec(), l(vec![b("http://1"), b("http://2")]));
    let t = TorrentFile::parse(&to_vec(&OwnedValue::Dict(top))).unwrap();
    assert_eq!(t.announce, vec![vec!["udp://fallback"]]);
    assert_eq!(t.comment.as_deref(), Some("naïve"));
    assert_eq!(t.url_list, vec!["http://1", "http://2"]);
}

#[test]
fn torrent_info_hash_uses_exact_raw_bytes() {
    // Non-canonical (unsorted) info dict: the hash must cover the bytes as
    // sent, not a canonical re-encoding.
    let info_raw = [
        &b"d4:name1:x12:piece lengthi16384e6:lengthi5e6:pieces20:"[..],
        &[0u8; 20],
        b"e",
    ]
    .concat();
    let torrent = [&b"d4:info"[..], &info_raw, b"e"].concat();
    let t = TorrentFile::parse(&torrent).unwrap();
    assert_eq!(t.info, Info::parse(&info_raw).unwrap());
    assert_eq!(t.info.info_size, info_raw.len());
}

#[test]
fn bep47_padding_and_symlink_shapes() {
    let link = |target: OwnedValue| {
        d(vec![
            ("attr", b("l")),
            ("path", path(&["link"])),
            ("symlink path", target),
        ])
    };
    let files = vec![
        v1_file(&["a"], 1),
        // An empty path list is as good as none for padding.
        d(vec![
            ("attr", b("p")),
            ("length", i(10)),
            ("path", l(vec![])),
        ]),
        link(path(&["a"])),
        // Targets go through the bounded path parser; a bad one is dropped.
        link(b("not a list")),
        link(l(vec![b("x"); 100])),
    ];
    let info = parse(&multi("x", files, 16_384, 1)).unwrap();
    assert_eq!(info.files[1].path, [b".pad".to_vec(), b"padding".to_vec()]);
    assert!(info.files[1].is_padding());
    assert_eq!(info.total_size, 1);
    assert_eq!(info.files[2].symlink_target, Some(vec![b"a".to_vec()]));
    for f in &info.files[2..] {
        assert_eq!(f.size, 0);
        assert!(f.attrs.contains(FileAttrs::SYMLINK));
    }
    assert_eq!(info.files[3].symlink_target, None);
    assert_eq!(info.files[4].symlink_target, None, "over max_path_depth");

    // An explicit length is still taken as given.
    let files = vec![with(link(path(&["a"])), "length", i(7))];
    assert_eq!(
        parse(&multi("x", files, 16_384, 1)).unwrap().files[0].size,
        7
    );
}

#[test]
fn v2_symlink_targets_are_bounded() {
    let opts = ParseOptions {
        limits: Limits {
            max_path_bytes: 8,
            ..Limits::DEFAULT
        },
        encoding_hint: None,
    };
    let link = |target: &[&str]| {
        d(vec![(
            "",
            d(vec![("attr", b("l")), ("symlink path", path(target))]),
        )])
    };
    let v = d(vec![
        (
            "file tree",
            d(vec![
                ("long", link(&["123456789"])),
                ("ok", link(&["dir", "f"])),
            ]),
        ),
        ("meta version", i(2)),
        ("name", b("x")),
        ("piece length", i(16_384)),
    ]);
    let info = Info::parse_with(&to_vec(&v), &opts).unwrap();
    assert_eq!(info.files[0].symlink_target, None);
    assert_eq!(
        info.files[1].symlink_target,
        Some(vec![b"dir".to_vec(), b"f".to_vec()])
    );
}

#[test]
fn utf8_overrides_correct_a_wrong_encoding_guess() {
    let gbk = |s: &str| encoding_rs::GBK.encode(s).0.into_owned();
    let files = vec![
        d(vec![
            ("length", i(1)),
            ("path", l(vec![b(gbk("第一集 高清"))])),
            ("path.utf-8", path(&["第一集 高清"])),
        ]),
        d(vec![
            ("length", i(1)),
            ("path", l(vec![b(gbk("第二集 高清"))])),
        ]),
    ];
    let v = multi("x", files, 16_384, 1);
    // A wrong hint stands in for a wrong statistical guess: the pair of
    // `path` and `path.utf-8` shows the creator wrote GBK.
    let opts = ParseOptions {
        encoding_hint: TextEncoding::for_label(b"euc-kr"),
        ..ParseOptions::default()
    };
    let info = Info::parse_with(&to_vec(&v), &opts).unwrap();
    assert_eq!(info.path_encoding.name(), "GBK");
    assert_eq!(info.files[1].path[0], gbk("第二集 高清"));
    assert_eq!(info.path_string(&info.files[0]), "第一集 高清");
    assert_eq!(info.path_string(&info.files[1]), "第二集 高清");

    // A pair that matches no encoding (a wrong override) changes nothing.
    let files = vec![
        d(vec![
            ("length", i(1)),
            ("path", l(vec![b(b"\xff\xfe")])),
            ("path.utf-8", path(&["第一集"])),
        ]),
        d(vec![
            ("length", i(1)),
            ("path", l(vec![b(gbk("第二集 高清"))])),
        ]),
    ];
    let info = Info::parse_with(&to_vec(&multi("x", files, 16_384, 1)), &opts).unwrap();
    assert_eq!(info.path_encoding.name(), "EUC-KR");
}
