//! Property tests for mixed `path.utf-8` overrides, BEP 47 symlinks and
//! padding: every shape parses, names come out right, and the parsed file
//! list survives the store's file-tree codec unchanged.

use lodestar_bencode::{OwnedValue, to_vec};
use lodestar_metainfo::{File, FileAttrs, Info, ParseOptions, TextEncoding};
use lodestar_store::filetree;
use proptest::prelude::*;

const PIECE: u64 = 16_384;

fn gbk() -> TextEncoding {
    TextEncoding::for_label(b"gbk").unwrap()
}

fn gbk_bytes(s: &str) -> Vec<u8> {
    encoding_rs::GBK.encode(s).0.into_owned()
}

/// Text a legacy (GBK) creator can write: ASCII or CJK ideographs.
fn legacy_segment() -> impl Strategy<Value = String> {
    prop_oneof![
        "[A-Za-z0-9 _-]{1,8}",
        "[\u{4e00}-\u{4e80}]{1,6}",
        "[a-z]{1,3}[\u{4e00}-\u{4e80}]{1,4}",
    ]
}

/// `path.utf-8` text: also Hangul, which GBK can't encode.
fn utf8_segment() -> impl Strategy<Value = String> {
    prop_oneof![
        3 => legacy_segment(),
        1 => "[\u{ac00}-\u{ac40}]{1,4}",
    ]
}

fn segments(s: impl Strategy<Value = String>) -> impl Strategy<Value = Vec<String>> {
    prop::collection::vec(s, 1..4)
}

#[derive(Debug, Clone)]
enum Entry {
    Plain {
        legacy: Vec<String>,
        utf8: Option<Vec<String>>,
        size: u64,
    },
    Symlink {
        legacy: Vec<String>,
        utf8: Option<Vec<String>>,
        target: Vec<String>,
        explicit_length: bool,
    },
    Padding {
        size: u64,
        with_path: bool,
    },
}

fn entry() -> impl Strategy<Value = Entry> {
    prop_oneof![
        4 => (segments(legacy_segment()), prop::option::of(segments(utf8_segment())), 0u64..5000)
            .prop_map(|(legacy, utf8, size)| Entry::Plain { legacy, utf8, size }),
        1 => (
            segments(legacy_segment()),
            prop::option::of(segments(utf8_segment())),
            segments(legacy_segment()),
            any::<bool>(),
        )
            .prop_map(|(legacy, utf8, target, explicit_length)| Entry::Symlink {
                legacy,
                utf8,
                target,
                explicit_length,
            }),
        1 => (1u64..PIECE, any::<bool>())
            .prop_map(|(size, with_path)| Entry::Padding { size, with_path }),
    ]
}

fn bytes(v: impl Into<Vec<u8>>) -> OwnedValue {
    OwnedValue::Bytes(v.into())
}

fn path_value(segs: &[String], enc: impl Fn(&str) -> Vec<u8>) -> OwnedValue {
    OwnedValue::List(segs.iter().map(|s| bytes(enc(s))).collect())
}

fn dict(entries: Vec<(&str, OwnedValue)>) -> OwnedValue {
    OwnedValue::Dict(
        entries
            .into_iter()
            .map(|(k, v)| (k.as_bytes().to_vec(), v))
            .collect(),
    )
}

/// The legacy `path` bytes of a file with an override are deliberately not
/// GBK, so a parser that still looked at them would get them wrong.
const SHADOWED: &[u8] = b"\xff\xfe?";

fn file_value(e: &Entry) -> OwnedValue {
    let named =
        |legacy: &[String], utf8: &Option<Vec<String>>, mut v: Vec<(&'static str, OwnedValue)>| {
            match utf8 {
                Some(u) => {
                    v.push(("path", OwnedValue::List(vec![bytes(SHADOWED)])));
                    v.push(("path.utf-8", path_value(u, |s| s.as_bytes().to_vec())));
                }
                None => v.push(("path", path_value(legacy, gbk_bytes))),
            }
            v
        };
    match e {
        Entry::Plain { legacy, utf8, size } => dict(named(
            legacy,
            utf8,
            vec![("length", OwnedValue::Int(*size as i64))],
        )),
        Entry::Symlink {
            legacy,
            utf8,
            target,
            explicit_length,
        } => {
            let mut v = vec![
                ("attr", bytes(&b"l"[..])),
                ("symlink path", path_value(target, gbk_bytes)),
            ];
            if *explicit_length {
                v.push(("length", OwnedValue::Int(0)));
            }
            dict(named(legacy, utf8, v))
        }
        Entry::Padding { size, with_path } => {
            let mut v = vec![
                ("attr", bytes(&b"p"[..])),
                ("length", OwnedValue::Int(*size as i64)),
            ];
            if *with_path {
                v.push((
                    "path",
                    path_value(&[".pad".into(), size.to_string()], |s| {
                        s.as_bytes().to_vec()
                    }),
                ));
            }
            dict(v)
        }
    }
}

fn entry_size(e: &Entry) -> u64 {
    match e {
        Entry::Plain { size, .. } | Entry::Padding { size, .. } => *size,
        Entry::Symlink { .. } => 0,
    }
}

fn decoded(info: &Info, segs: &[Vec<u8>]) -> Vec<String> {
    segs.iter()
        .map(|s| info.path_encoding.decode(s).into_owned())
        .collect()
}

fn assert_codec_roundtrip(info: &Info) -> Result<(), TestCaseError> {
    prop_assert!(info.files.iter().all(|f: &File| !f.path.is_empty()));
    let blob = filetree::encode(&info.files, info.path_encoding).unwrap();
    let tree = filetree::decode(&blob).unwrap();
    prop_assert_eq!(tree.encoding, info.path_encoding);
    prop_assert_eq!(&tree.files, &info.files);
    Ok(())
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    #[test]
    fn v1_mixed_shapes(entries in prop::collection::vec(entry(), 1..12)) {
        let total: u64 = entries.iter().map(entry_size).sum();
        let raw = to_vec(&dict(vec![
            ("files", OwnedValue::List(entries.iter().map(file_value).collect())),
            ("name", bytes(&b"t"[..])),
            ("piece length", OwnedValue::Int(PIECE as i64)),
            ("pieces", bytes(vec![0; total.div_ceil(PIECE) as usize * 20])),
        ]));
        let opts = ParseOptions { encoding_hint: Some(gbk()), ..ParseOptions::default() };
        let info = Info::parse_with(&raw, &opts).unwrap();
        prop_assert_eq!(info.files.len(), entries.len());

        // Legacy text: paths without an override, and every symlink target.
        let legacy: Vec<Vec<u8>> = entries
            .iter()
            .flat_map(|e| {
                let (path, target) = match e {
                    Entry::Plain { legacy, utf8: None, .. } => (Some(legacy), None),
                    Entry::Symlink { legacy, utf8, target, .. } => {
                        (utf8.is_none().then_some(legacy), Some(target))
                    }
                    _ => (None, None),
                };
                path.into_iter().chain(target).flatten().map(|s| gbk_bytes(s))
            })
            .collect();
        let legacy_is_utf8 = legacy.iter().all(|s| std::str::from_utf8(s).is_ok());
        let unencodable = entries.iter().any(|e| match e {
            Entry::Plain { utf8: Some(u), .. } | Entry::Symlink { utf8: Some(u), .. } => {
                u.iter().any(|s| s.chars().any(|c| ('\u{ac00}'..='\u{d7a3}').contains(&c)))
            }
            _ => false,
        });
        let want_encoding = if legacy_is_utf8 || unencodable { TextEncoding::UTF8 } else { gbk() };
        prop_assert_eq!(info.path_encoding, want_encoding);

        // Raw legacy bytes survive unless they had to be decoded to UTF-8.
        let raw_kept = legacy_is_utf8 || !unencodable;
        let check_legacy = |got: &[Vec<u8>], want: &[String]| -> Result<(), TestCaseError> {
            if raw_kept {
                let raw: Vec<_> = want.iter().map(|s| gbk_bytes(s)).collect();
                prop_assert_eq!(got, &raw[..]);
            }
            if !legacy_is_utf8 {
                prop_assert_eq!(decoded(&info, got), want);
            }
            Ok(())
        };

        let mut size = 0;
        for (f, e) in info.files.iter().zip(&entries) {
            match e {
                Entry::Plain { legacy, utf8, size: s } => {
                    prop_assert_eq!(f.size, *s);
                    prop_assert!(f.attrs.is_empty());
                    match utf8 {
                        Some(u) => prop_assert_eq!(&decoded(&info, &f.path), u),
                        None => check_legacy(&f.path, legacy)?,
                    }
                    size += s;
                }
                Entry::Symlink { legacy, utf8, target, .. } => {
                    prop_assert_eq!(f.size, 0);
                    prop_assert!(f.attrs.contains(FileAttrs::SYMLINK));
                    match utf8 {
                        Some(u) => prop_assert_eq!(&decoded(&info, &f.path), u),
                        None => check_legacy(&f.path, legacy)?,
                    }
                    check_legacy(f.symlink_target.as_deref().unwrap(), target)?;
                }
                Entry::Padding { size: s, with_path } => {
                    prop_assert!(f.is_padding());
                    prop_assert_eq!(f.size, *s);
                    if !with_path {
                        prop_assert_eq!(&f.path, &vec![b".pad".to_vec(), b"padding".to_vec()]);
                    }
                }
            }
        }
        prop_assert_eq!(info.total_size, size);
        assert_codec_roundtrip(&info)?;
    }

    #[test]
    fn v2_symlinks_keep_their_targets(
        links in prop::collection::btree_map(
            "[a-m]{1,4}",
            (segments("[a-z.]{1,6}".prop_map(String::from)), any::<bool>()),
            1..8,
        ),
    ) {
        let mut tree = std::collections::BTreeMap::new();
        tree.insert(
            b"zz-real".to_vec(),
            dict(vec![("", dict(vec![
                ("length", OwnedValue::Int(1)),
                ("pieces root", bytes(vec![1; 32])),
            ]))]),
        );
        for (name, (target, explicit_length)) in &links {
            let mut props = vec![
                ("attr", bytes(&b"l"[..])),
                ("symlink path", path_value(target, |s| s.as_bytes().to_vec())),
            ];
            if *explicit_length {
                props.push(("length", OwnedValue::Int(0)));
            }
            tree.insert(name.as_bytes().to_vec(), dict(vec![("", dict(props))]));
        }
        let raw = to_vec(&dict(vec![
            ("file tree", OwnedValue::Dict(tree)),
            ("meta version", OwnedValue::Int(2)),
            ("name", bytes(&b"t"[..])),
            ("piece length", OwnedValue::Int(PIECE as i64)),
        ]));
        let info = Info::parse(&raw).unwrap();
        prop_assert_eq!(info.files.len(), links.len() + 1);
        for (f, (name, (target, _))) in info.files.iter().zip(&links) {
            prop_assert_eq!(&f.path, &vec![name.as_bytes().to_vec()]);
            prop_assert_eq!(f.size, 0);
            prop_assert!(f.attrs.contains(FileAttrs::SYMLINK));
            prop_assert_eq!(&decoded(&info, f.symlink_target.as_deref().unwrap()), target);
        }
        prop_assert_eq!(info.total_size, 1);
        assert_codec_roundtrip(&info)?;
    }
}
