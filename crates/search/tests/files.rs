//! `file:(size:…)` through the whole index (X-search-files-3): a size
//! range reads the postings of the `size_bucket`s inside it and checks
//! the stored size only in its edge buckets. Every result here must be
//! exactly what a scan of every file's size gives, at range edges, open
//! ends, bucket boundaries, and zero and huge sizes.

use lodestar_bencode::{OwnedValue, to_vec};
use lodestar_metainfo::Info;
use lodestar_search::files::{bucket_floor, code_floor, code_floor_count, size_bucket};
use lodestar_search::query::{NumField, TextField};
use lodestar_search::{Node, SearchIndex};
use lodestar_store::{NewTorrent, SourceId, Store, StoreConfig};

const NOW: i64 = 1_700_000_100;

fn d(entries: Vec<(&str, OwnedValue)>) -> OwnedValue {
    OwnedValue::Dict(
        entries
            .into_iter()
            .map(|(k, v)| (k.as_bytes().to_vec(), v))
            .collect(),
    )
}

fn b(s: &str) -> OwnedValue {
    OwnedValue::Bytes(s.as_bytes().to_vec())
}

/// A torrent of files `(path, length)`.
fn sized(name: &str, files: &[(String, i64)]) -> Vec<u8> {
    let files = files
        .iter()
        .map(|(p, len)| {
            d(vec![
                ("length", OwnedValue::Int(*len)),
                ("path", OwnedValue::List(p.split('/').map(b).collect())),
            ])
        })
        .collect();
    to_vec(&d(vec![
        ("files", OwnedValue::List(files)),
        ("name", b(name)),
        ("piece length", OwnedValue::Int(1 << 16)),
        ("pieces", OwnedValue::Bytes(vec![0; 20])),
    ]))
}

fn store_with(torrents: &[Vec<u8>]) -> Store {
    let mut store = Store::open_in_memory(StoreConfig::default()).unwrap();
    let infos: Vec<Info> = torrents.iter().map(|t| Info::parse(t).unwrap()).collect();
    let batch: Vec<NewTorrent<'_>> = infos
        .iter()
        .zip(torrents)
        .map(|(info, raw)| NewTorrent {
            info,
            raw_info: Some(raw),
            discovered_at: 1_700_000_000,
            metadata_at: 1_700_000_001,
            extra_flags: 0,
            source: SourceId::new(0).unwrap(),
        })
        .collect();
    store.insert(&batch).unwrap();
    store
}

/// The sizes at every bucket and size-code boundary that a store can hold (lengths are
/// signed 64-bit in bencode), each with its neighbours, plus 0, 1 and
/// the largest length.
fn boundary_sizes() -> Vec<u64> {
    let max = i64::MAX as u64;
    let mut sizes: Vec<u64> = (0..128)
        .map(bucket_floor)
        .chain((0..code_floor_count()).filter_map(code_floor))
        .filter(|f| *f <= max)
        .flat_map(|f| [f.saturating_sub(1), f, f.saturating_add(1)])
        .chain([0, 1, 2, max - 1, max])
        .filter(|s| *s <= max)
        .collect();
    sizes.sort_unstable();
    sizes.dedup();
    sizes
}

/// Whether torrent `i` (from 0) of size `s` also has `x.txt`, 7 bytes:
/// every third, so torrents aren't one file each, unless the total
/// would pass the largest length.
fn has_txt(i: usize, s: u64) -> bool {
    i.is_multiple_of(3) && s <= i64::MAX as u64 - 7
}

/// One torrent a size (its file `f.bin`), some with an `x.txt`
/// ([`has_txt`]). Torrent ids are given out from 1, in order.
fn corpus(sizes: &[u64]) -> (SearchIndex, Store) {
    let torrents: Vec<Vec<u8>> = sizes
        .iter()
        .enumerate()
        .map(|(i, s)| {
            let mut files = vec![("f.bin".to_owned(), *s as i64)];
            if has_txt(i, *s) {
                files.push(("x.txt".to_owned(), 7));
            }
            sized(&format!("torrent {i}"), &files)
        })
        .collect();
    let mut store = store_with(&torrents);
    let index = SearchIndex::in_memory().unwrap();
    let mut w = index.writer(30 << 20).unwrap();
    w.catch_up(&mut store, 10_000).unwrap();
    w.commit().unwrap();
    (index, store)
}

fn ids(index: &SearchIndex, node: &Node) -> Vec<u64> {
    let mut ids: Vec<u64> = index
        .search(&index.query(node, NOW), 10_000)
        .unwrap()
        .hits
        .into_iter()
        .map(|h| h.id)
        .collect();
    ids.sort_unstable();
    ids
}

fn range(lo: Option<u64>, hi: Option<u64>) -> Node {
    Node::Range {
        field: NumField::Size,
        lo,
        hi,
    }
}

fn file(of: Node) -> Node {
    Node::File { of: Box::new(of) }
}

/// What a scan of every file's size gives: the torrents with a file in
/// `[lo, hi]` (`x.txt` included).
fn scanned(sizes: &[u64], lo: Option<u64>, hi: Option<u64>) -> Vec<u64> {
    let r = lo.unwrap_or(0)..=hi.unwrap_or(u64::MAX);
    sizes
        .iter()
        .enumerate()
        .filter(|(i, s)| r.contains(s) || (has_txt(*i, **s) && r.contains(&7)))
        .map(|(i, _)| i as u64 + 1)
        .collect()
}

#[test]
fn size_ranges_match_a_scan_at_every_bucket_edge() {
    let sizes = boundary_sizes();
    let (index, _store) = corpus(&sizes);
    // Bounds on each side of every boundary, and the extremes.
    let mut bounds: Vec<u64> = sizes.clone();
    bounds.extend([u64::MAX - 1, u64::MAX]);
    let mut checked = 0;
    for (k, &x) in bounds.iter().enumerate() {
        // Open ranges each way, an exact size, and closed ranges to a
        // few later bounds: inside one bucket, across one edge, and
        // across many buckets.
        let mut cases = vec![(Some(x), None), (None, Some(x)), (Some(x), Some(x))];
        for step in [1, 2, 3, 17, 60] {
            if let Some(&y) = bounds.get(k + step) {
                cases.push((Some(x), Some(y)));
            }
        }
        for (lo, hi) in cases {
            let want = scanned(&sizes, lo, hi);
            assert_eq!(ids(&index, &file(range(lo, hi))), want, "{lo:?}..={hi:?}");
            checked += 1;
        }
    }
    assert!(checked > 1_000, "{checked}");
}

#[test]
fn zero_huge_and_open_ranges() {
    let max = i64::MAX as u64;
    let sizes = [0u64, 1, 2, 3, 1 << 30, (1 << 30) + (1 << 29), max - 1, max];
    let (index, _store) = corpus(&sizes);
    let all: Vec<u64> = (1..=sizes.len() as u64).collect();
    // Nothing is outside an unbounded range, or `size:>=0`.
    assert_eq!(ids(&index, &file(range(None, None))), all);
    assert_eq!(ids(&index, &file(range(Some(0), None))), all);
    // Exactly zero: only the empty file (the 7-byte `x.txt`s don't).
    assert_eq!(ids(&index, &file(range(None, Some(0)))), [1]);
    assert_eq!(ids(&index, &file(range(Some(0), Some(1)))), [1, 2]);
    // The top of a signed length, and past it.
    assert_eq!(ids(&index, &file(range(Some(max), None))), [8]);
    assert_eq!(ids(&index, &file(range(Some(max - 1), Some(max)))), [7, 8]);
    assert!(ids(&index, &file(range(Some(max + 1), None))).is_empty());
    assert!(ids(&index, &file(range(Some(u64::MAX), None))).is_empty());
    // A bucket's floor and the one below it: 1 GiB and 1.5 GiB are
    // adjacent buckets' floors.
    let g = 1u64 << 30;
    assert_eq!(size_bucket(g) + 1, size_bucket(g + g / 2));
    assert_eq!(ids(&index, &file(range(Some(g), Some(g + g / 2 - 1)))), [5]);
    assert_eq!(ids(&index, &file(range(Some(g), Some(g + g / 2)))), [5, 6]);
    assert_eq!(ids(&index, &file(range(Some(g + 1), Some(g + g / 2)))), [6]);
    // Parsed, as a user types them.
    let q = |s: &str| ids(&index, &lodestar_search::parse(s).unwrap());
    assert_eq!(q("file:(size:>1GiB)"), [6, 7, 8]);
    assert_eq!(q("file:(size:>=1GiB)"), [5, 6, 7, 8]);
    // Torrents 1 to 4, and none for `x.txt`: torrent 7's would overflow.
    assert_eq!(q("file:(size:<1GiB)"), [1, 2, 3, 4]);
    assert_eq!(q("file:(size:0)"), [1]);
}

/// The same ranges under a boolean (with another clause, negated, and
/// in an `OR`), where the range is a scorer rather than a scan.
#[test]
fn nested_size_ranges_match_a_scan() {
    let sizes = boundary_sizes();
    let (index, _store) = corpus(&sizes);
    let n = sizes.len() as u64;
    for (lo, hi) in [
        (Some(4096), Some(6143)),
        (Some(4095), Some(6144)),
        (Some(1 << 20), None),
        (None, Some(1 << 40)),
        (Some(3), Some(3)),
        (Some(1 << 62), None),
    ] {
        let r = lo.unwrap_or(0)..=hi.unwrap_or(u64::MAX);
        let bin: Vec<u64> = (1..=n)
            .filter(|i| r.contains(&sizes[*i as usize - 1]))
            .collect();
        let with_ext = file(Node::And {
            of: vec![ext("bin"), range(lo, hi)],
        });
        assert_eq!(ids(&index, &with_ext), bin, "bin {lo:?}..={hi:?}");
        // A file outside the range: every `x.txt` (7 bytes) unless the
        // range holds 7, and each `f.bin` outside it.
        let outside: Vec<u64> = (1..=n)
            .filter(|i| {
                let at = *i as usize - 1;
                !r.contains(&sizes[at]) || (has_txt(at, sizes[at]) && !r.contains(&7))
            })
            .collect();
        let negated = file(Node::Not {
            of: Box::new(range(lo, hi)),
        });
        assert_eq!(ids(&index, &negated), outside, "not {lo:?}..={hi:?}");
        let either = file(Node::Or {
            of: vec![range(lo, hi), ext("txt")],
        });
        let mut want = scanned(&sizes, lo, hi);
        want.extend((1..=n).filter(|i| has_txt(*i as usize - 1, sizes[*i as usize - 1])));
        want.sort_unstable();
        want.dedup();
        assert_eq!(ids(&index, &either), want, "or {lo:?}..={hi:?}");
    }
}

/// A torrent of files `(path, length, attr)`.
fn with_attrs(name: &str, files: &[(String, i64, &str)]) -> Vec<u8> {
    let files = files
        .iter()
        .map(|(p, len, attr)| {
            let mut e = vec![
                ("length", OwnedValue::Int(*len)),
                ("path", OwnedValue::List(p.split('/').map(b).collect())),
            ];
            if !attr.is_empty() {
                e.push(("attr", b(attr)));
            }
            d(e)
        })
        .collect();
    to_vec(&d(vec![
        ("files", OwnedValue::List(files)),
        ("name", b(name)),
        ("piece length", OwnedValue::Int(1 << 16)),
        ("pieces", OwnedValue::Bytes(vec![0; 20])),
    ]))
}

fn ext(e: &str) -> Node {
    Node::Ext {
        any: vec![e.into()],
    }
}

fn word(field: TextField, w: &str) -> Node {
    Node::Word {
        field,
        word: w.into(),
    }
}

/// One-atom `file:(ext:…)` clauses are answered from the torrent index
/// (X-search-files-4, option (a), runner 2026-11-05): its extensions
/// cover every file, while smart coverage leaves small images, documents
/// and other files out of a large pack's file documents and hidden files
/// out of every torrent's. So they gain those torrents (and a negation
/// loses them). Clauses of two or more atoms, and words, still ask the
/// file index; words differ from `path:` both ways, since the paths stop
/// at `MAX_PATH_TOKENS` and the file index doesn't.
#[test]
fn one_atom_extension_clauses_cover_every_file_and_words_stay_on_the_file_index() {
    const MIB: i64 = 1 << 20;
    // 1: a photo pack past `SMART_ALL_FILES`, every image under
    // `SMART_MIN_SIZE`, and a small readme.
    let mut photos: Vec<(String, i64, &str)> = (0..1_200)
        .map(|i| (format!("pics/p{i:04}.jpg"), 3 * MIB, ""))
        .collect();
    photos.push(("readme.txt".into(), 2_000, ""));
    // 2: one cover image.
    let cover = vec![("cover.jpg".to_owned(), MIB, "")];
    // 3: its only video hidden (BEP 47).
    let hidden = vec![
        ("c.mkv".to_owned(), 700 * MIB, "h"),
        ("c.txt".to_owned(), 10, ""),
    ];
    // 4: a pack whose paths pass `MAX_PATH_TOKENS` (18 words a name)
    // before its last file, a video, which the file index keeps.
    let mut deep: Vec<(String, i64, &str)> = (0..1_200)
        .map(|i| {
            (
                format!("a b c d e f g h i j k l m n o p q {i}.flac"),
                30 * MIB,
                "",
            )
        })
        .collect();
    deep.push(("zebra.mkv".into(), 900 * MIB, ""));
    let mut store = store_with(&[
        with_attrs("photos", &photos),
        with_attrs("cover", &cover),
        with_attrs("hidden", &hidden),
        with_attrs("deep", &deep),
    ]);
    let index = SearchIndex::in_memory().unwrap();
    let mut w = index.writer(60 << 20).unwrap();
    w.catch_up(&mut store, 10_000).unwrap();
    w.commit().unwrap();
    let none: Vec<u64> = Vec::new();
    // One extension atom: what the torrent level finds.
    assert_eq!(ids(&index, &file(ext("jpg"))), vec![1, 2]);
    assert_eq!(ids(&index, &ext("jpg")), vec![1, 2]);
    assert_eq!(ids(&index, &file(ext("mkv"))), vec![3, 4]);
    assert_eq!(ids(&index, &file(ext("flac"))), vec![4]);
    let either = Node::Ext {
        any: vec!["jpg".into(), "flac".into()],
    };
    assert_eq!(ids(&index, &file(either)), vec![1, 2, 4]);
    assert_eq!(
        ids(
            &index,
            &Node::Not {
                of: Box::new(file(ext("mkv")))
            }
        ),
        vec![1, 2]
    );
    // Alternatives of extensions are one clause: `ext:jpg | ext:flac`
    // is `ext:jpg,flac`.
    let or = Node::Or {
        of: vec![ext("jpg"), ext("flac")],
    };
    assert_eq!(ids(&index, &file(or)), vec![1, 2, 4]);
    // Beside a word, a filter that adds nothing to the score.
    let with_word = Node::And {
        of: vec![word(TextField::Any, "photos"), file(ext("jpg"))],
    };
    assert_eq!(ids(&index, &with_word), vec![1]);
    let scores = |n: &Node| -> Vec<(u64, f32)> {
        let mut hits: Vec<(u64, f32)> = index
            .search(&index.query(n, NOW), 10)
            .unwrap()
            .hits
            .into_iter()
            .map(|h| (h.id, h.score))
            .collect();
        hits.sort_by_key(|h| h.0);
        hits
    };
    let plain = word(TextField::Any, "photos");
    assert_eq!(scores(&with_word), scores(&plain));
    // Two atoms ask about one file: the file index, with its coverage.
    let big_jpg = file(Node::And {
        of: vec![ext("jpg"), range(Some(1), None)],
    });
    assert_eq!(ids(&index, &big_jpg), vec![2]);
    // Matched files still come from the file index.
    assert_eq!(
        index.matched_files(&file(ext("jpg")), 2, 10).unwrap(),
        vec![0]
    );
    assert_eq!(
        index.matched_files(&file(ext("mkv")), 4, 10).unwrap(),
        vec![1_200]
    );
    // Words: either can find what the other misses.
    assert_eq!(ids(&index, &file(word(TextField::Any, "readme"))), none);
    assert_eq!(ids(&index, &word(TextField::Path, "readme")), vec![1]);
    assert_eq!(ids(&index, &file(word(TextField::Any, "zebra"))), vec![4]);
    assert_eq!(ids(&index, &word(TextField::Path, "zebra")), none);
    // Where coverage is whole (under `SMART_ALL_FILES` files, nothing
    // hidden) they agree.
    assert_eq!(ids(&index, &file(word(TextField::Any, "cover"))), vec![2]);
    assert_eq!(ids(&index, &word(TextField::Path, "cover")), vec![2]);
}

/// `node` with an always-matching atom (`Node::All`) added inside every
/// `file:(…)` clause but a lone extension's (which is the torrent's own
/// `ext`, by X-search-files-4's accepted rule): the same files, but no
/// bucket clause or negated-word clause, so each runs on the file index
/// (the path before X-search-files-5 and -6).
fn on_the_file_index(node: &Node) -> Node {
    let never = Node::All;
    match node {
        Node::And { of } => Node::And {
            of: of.iter().map(on_the_file_index).collect(),
        },
        Node::Or { of } => Node::Or {
            of: of.iter().map(on_the_file_index).collect(),
        },
        Node::Not { of } => Node::Not {
            of: Box::new(on_the_file_index(of)),
        },
        Node::File { of } if lone_extension(of) => node.clone(),
        Node::File { of } => {
            let mut inner = match &**of {
                Node::And { of } => of.clone(),
                n => vec![n.clone()],
            };
            inner.push(never.clone());
            file(Node::And { of: inner })
        }
        n => n.clone(),
    }
}

/// `ext:a`, or alternatives of extensions only.
fn lone_extension(n: &Node) -> bool {
    match n {
        Node::Ext { .. } => true,
        Node::Or { of } => !of.is_empty() && of.iter().all(lone_extension),
        _ => false,
    }
}

/// Hits by id, for comparing sets whose ties may order differently.
fn by_id(hits: &[(u64, f32)]) -> Vec<(u64, f32)> {
    let mut v = hits.to_vec();
    v.sort_by_key(|h| h.0);
    v
}

/// Every hit's id and score, in result order.
fn ranked(index: &SearchIndex, node: &Node) -> Vec<(u64, f32)> {
    index
        .search(&index.query(node, NOW), 10_000)
        .unwrap()
        .hits
        .into_iter()
        .map(|h| (h.id, h.score))
        .collect()
}

/// The torrent index's bucket terms (X-search-files-5) give exactly what
/// the file index does: the same torrents, in the same order, with the
/// same scores, for every positive shape of size clause — bare and with
/// extensions, at and beside every bucket boundary, open at either end,
/// alone, beside a word, a top-level range or another file clause, in an
/// alternative and negated — over files smart coverage leaves out (a
/// large pack's small images, a hidden file) as well as covered ones.
#[test]
fn bucket_terms_answer_as_the_file_index_does() {
    const MIB: u64 = 1 << 20;
    let exts = ["mkv", "srt", "bin", "jpg"];
    // One torrent a boundary size, its file's extension cycling, every
    // third with a 7-byte `x.txt` too; sizes kept under 2^40 so totals
    // stay small.
    let sizes: Vec<u64> = boundary_sizes()
        .into_iter()
        .filter(|s| *s < 1 << 40)
        .collect();
    let mut torrents: Vec<Vec<u8>> = sizes
        .iter()
        .enumerate()
        .map(|(i, s)| {
            let mut files = vec![(format!("f.{}", exts[i % exts.len()]), *s as i64)];
            if has_txt(i, *s) {
                files.push(("x.txt".to_owned(), 7));
            }
            sized(&format!("torrent {i}"), &files)
        })
        .collect();
    // A pack whose small images smart coverage leaves out, beside one
    // covered video; and a torrent whose only video is hidden.
    let mut pack: Vec<(String, i64, &str)> = (0..1_200)
        .map(|i| (format!("pics/p{i:04}.jpg"), (3 * MIB) as i64, ""))
        .collect();
    pack.push(("big.mkv".into(), (2u64 << 30) as i64, ""));
    torrents.push(with_attrs("pack", &pack));
    torrents.push(with_attrs(
        "hidden",
        &[
            ("c.mkv".to_owned(), (700 * MIB) as i64, "h"),
            ("c.txt".to_owned(), 10, ""),
        ],
    ));
    let mut store = store_with(&torrents);
    let index = SearchIndex::in_memory().unwrap();
    let mut w = index.writer(60 << 20).unwrap();
    w.catch_up(&mut store, 100_000).unwrap();
    w.commit().unwrap();

    // Ranges: open both ways from every boundary, one bucket exactly,
    // whole buckets, a part of one, and wider spans.
    let mut ranges: Vec<(Option<u64>, Option<u64>)> = Vec::new();
    for b in [0u64, 1, 2, 5, 20, 41, 45, 53, 59, 60, 61, 70] {
        let floor = bucket_floor(b);
        let next = bucket_floor(b + 1);
        ranges.extend([
            (Some(floor), None),
            (Some(floor + 1), None),
            (None, Some(floor)),
            (None, Some(floor.saturating_sub(1))),
            (Some(floor), Some(next.saturating_sub(1))),
            (Some(floor + 1), Some(next.saturating_sub(2))),
            (Some(floor), Some(bucket_floor(b + 4).saturating_sub(1))),
            (Some(floor.saturating_sub(1)), Some(next + 1)),
        ]);
    }
    // The same about the decimal floors of the torrent's size codes
    // (X-search-files-6), and one between two of them.
    for floor in [50 << 10, 100 * MIB, 500 * MIB, 5 << 30, 200 << 30] {
        let next = code_floor(
            (0..code_floor_count())
                .find(|&i| code_floor(i) > Some(floor))
                .unwrap(),
        )
        .unwrap();
        ranges.extend([
            (Some(floor), None),
            (Some(floor + 1), None),
            (None, Some(floor)),
            (None, Some(floor - 1)),
            (Some(floor), Some(next - 1)),
            (Some(floor + 1), Some(next - 2)),
            (Some(floor - 1), Some(next + 1)),
        ]);
    }
    ranges.extend([
        (Some(700 * MIB + 1), None),
        (Some(100 * MIB + 1), None),
        (Some((1 << 30) + 1), None),
        (Some(0), Some(0)),
        (None, None),
    ]);
    let word = |w: &str| Node::Word {
        field: TextField::Any,
        word: w.into(),
    };
    let exts_node = |any: &[&str]| Node::Ext {
        any: any.iter().map(|e| (*e).to_owned()).collect(),
    };
    let mut checked = 0;
    for &(lo, hi) in &ranges {
        let r = range(lo, hi);
        let bare = file(r.clone());
        let mkv = file(Node::And {
            of: vec![exts_node(&["mkv"]), r.clone()],
        });
        let media = file(Node::And {
            of: vec![r.clone(), exts_node(&["mkv", "jpg"])],
        });
        let either = file(Node::And {
            of: vec![
                Node::Or {
                    of: vec![exts_node(&["srt"]), exts_node(&["bin"])],
                },
                r.clone(),
            ],
        });
        let shapes = vec![
            bare.clone(),
            mkv.clone(),
            media.clone(),
            either,
            Node::And {
                of: vec![word("torrent"), bare.clone()],
            },
            Node::And {
                of: vec![bare.clone(), range(Some(MIB), None)],
            },
            Node::And {
                of: vec![range(Some(MIB), None), mkv.clone()],
            },
            Node::And {
                of: vec![bare.clone(), file(exts_node(&["jpg"]))],
            },
            Node::And {
                of: vec![mkv.clone(), media.clone()],
            },
            Node::Or {
                of: vec![bare.clone(), file(exts_node(&["srt"]))],
            },
            Node::Or {
                of: vec![word("pack"), mkv.clone()],
            },
            Node::And {
                of: vec![
                    word("torrent"),
                    Node::Not {
                        of: Box::new(bare.clone()),
                    },
                ],
            },
            Node::And {
                of: vec![
                    file(exts_node(&["mkv", "jpg", "bin"])),
                    Node::Not {
                        of: Box::new(media.clone()),
                    },
                ],
            },
        ];
        for q in shapes {
            let reference = on_the_file_index(&q);
            assert_eq!(ranked(&index, &q), ranked(&index, &reference), "{q:?}");
            checked += 1;
        }
    }
    assert!(checked > 1_000, "{checked}");
}

/// A `file:(ext:… -word)` clause (X-search-files-6) gives exactly what the
/// file index does — the same torrents, order and scores — whether the
/// bad torrents come through the word's postings (a rare word) or the
/// clause runs whole on the file index (a common one): a torrent whose
/// only matching file has the word, one with both kinds, one with the
/// word in the name rather than a directory, a pack whose small images
/// coverage leaves out, a hidden video, and several words.
#[test]
fn negated_words_answer_as_the_file_index_does() {
    const MIB: u64 = 1 << 20;
    let file3 = |p: &str, len: u64| (p.to_owned(), len as i64, "");
    let mut torrents = vec![
        // 1: its only video is a sample.
        with_attrs(
            "only sample",
            &[file3("Sample/s.mkv", 40 * MIB), file3("a.txt", 9)],
        ),
        // 2: a sample and the film.
        with_attrs(
            "both",
            &[file3("sample/s.mkv", 40 * MIB), file3("film.mkv", 2 << 30)],
        ),
        // 3: the film only.
        with_attrs("film only", &[file3("film.mp4", 700 * MIB)]),
        // 4: `sample` in the name, not a directory.
        with_attrs("named", &[file3("film.sample.avi", 50 * MIB)]),
        // 5: no video.
        with_attrs("subs", &[file3("sample/x.srt", 1000)]),
        // 6: a hidden video and a sample.
        with_attrs(
            "hidden",
            &[
                ("c.mkv".to_owned(), (700 * MIB) as i64, "h"),
                file3("sample/c.mkv", 30 * MIB),
            ],
        ),
        // 7: `web-dl` in a directory, a compound word.
        with_attrs(
            "webdl",
            &[file3("web-dl/x.mkv", 900 * MIB), file3("y.srt", 5)],
        ),
    ];
    // A pack of small images in `sample/`, which coverage leaves out,
    // and one covered video.
    let mut pack: Vec<(String, i64, &str)> = (0..1_200)
        .map(|i| (format!("sample/p{i:04}.jpg"), (3 * MIB) as i64, ""))
        .collect();
    pack.push(file3("big.mkv", 2 << 30));
    torrents.push(with_attrs("pack", &pack));
    // Many videos in `common/`, so `common` is too common for its
    // postings (the clause falls back to the file index whole).
    for i in 0..40 {
        torrents.push(with_attrs(
            &format!("filler {i}"),
            &[
                file3(&format!("common/f{i}.mkv"), (i + 1) * MIB),
                file3(&format!("v{i}.mp4"), 3 * MIB),
            ],
        ));
    }
    torrents.push(with_attrs("common only", &[file3("common/z.mkv", 5 * MIB)]));
    let mut store = store_with(&torrents);
    let index = SearchIndex::in_memory().unwrap();
    let mut w = index.writer(60 << 20).unwrap();
    w.catch_up(&mut store, 100_000).unwrap();
    w.commit().unwrap();

    let exts = |any: &[&str]| Node::Ext {
        any: any.iter().map(|e| (*e).to_owned()).collect(),
    };
    let not = |n: Node| Node::Not { of: Box::new(n) };
    let clause = |of: Vec<Node>| file(Node::And { of });
    let video = || exts(&["mkv", "mp4", "avi"]);
    let mut shapes = Vec::new();
    for w in [
        word(TextField::Path, "sample"),
        word(TextField::Any, "sample"),
        word(TextField::Name, "sample"),
        word(TextField::Path, "web-dl"),
        word(TextField::Path, "common"),
        word(TextField::Any, "zzqxabsent"),
    ] {
        let c = clause(vec![video(), not(w.clone())]);
        shapes.extend([
            c.clone(),
            clause(vec![not(w.clone()), exts(&["mkv"])]),
            clause(vec![
                Node::Or {
                    of: vec![exts(&["mkv"]), exts(&["jpg"])],
                },
                not(w.clone()),
            ]),
            clause(vec![
                video(),
                not(w.clone()),
                not(word(TextField::Path, "sample")),
            ]),
            Node::And {
                of: vec![word(TextField::Any, "film"), c.clone()],
            },
            Node::And {
                of: vec![c.clone(), range(Some(MIB), None)],
            },
            Node::And {
                of: vec![c.clone(), file(exts(&["srt"]))],
            },
            Node::Or {
                of: vec![c.clone(), word(TextField::Any, "subs")],
            },
            Node::And {
                of: vec![word(TextField::Any, "film"), not(c.clone())],
            },
        ]);
    }
    for q in &shapes {
        let reference = on_the_file_index(q);
        assert_eq!(ranked(&index, q), ranked(&index, &reference), "{q:?}");
    }
    // The answers themselves, for the cases named above.
    let path_sample = clause(vec![video(), not(word(TextField::Path, "sample"))]);
    let got: Vec<u64> = by_id(&ranked(&index, &path_sample))
        .into_iter()
        .map(|h| h.0)
        .collect();
    assert_eq!(&got[..6], [2, 3, 4, 7, 8, 9], "{got:?}");
    assert!(!got.contains(&1) && !got.contains(&6));
    // The rare word goes through its postings (the candidates are the
    // torrent's `file_ext_bucket` terms); the common one doesn't.
    let plan = |q: &Node| format!("{:?}", index.query(q, NOW));
    assert!(
        plan(&path_sample).contains("RegexQuery"),
        "{}",
        plan(&path_sample)
    );
    let common = clause(vec![video(), not(word(TextField::Path, "common"))]);
    assert!(!plan(&common).contains("RegexQuery"), "{}", plan(&common));
}

/// While the file index lags the torrent index (a crash between their
/// commits, until the catch-up), an extension less a word doesn't read
/// the torrent's terms: the bad set would miss the lagging torrents'
/// files and let in one whose only video is a sample. It runs on the
/// file index, which has nothing yet, as on `main`; after the catch-up
/// it answers from the terms (X-search-files-6).
#[test]
fn negated_words_wait_for_a_lagging_file_index() {
    let dir = tempfile::tempdir().unwrap();
    let path = lodestar_search::index_dir(dir.path());
    // Films enough that `sample` is rare (`NEGATED_RATIO`).
    let mut torrents = vec![with_attrs(
        "only sample",
        &[("Sample/s.mkv".into(), 1 << 25, "")],
    )];
    torrents.extend(
        (0..10).map(|i| with_attrs(&format!("film {i}"), &[(format!("f{i}.mkv"), 1 << 30, "")])),
    );
    let mut store = store_with(&torrents);
    {
        let index = SearchIndex::open(&path).unwrap();
        let mut w = index.writer(30 << 20).unwrap();
        w.catch_up(&mut store, 100).unwrap();
        w.commit().unwrap();
    }
    std::fs::remove_dir_all(path.join("files")).unwrap();
    let index = SearchIndex::open(&path).unwrap();
    let q = file(Node::And {
        of: vec![
            ext("mkv"),
            Node::Not {
                of: Box::new(word(TextField::Path, "sample")),
            },
        ],
    });
    assert!(ids(&index, &q).is_empty(), "nothing while behind");
    let mut w = index.writer(30 << 20).unwrap();
    w.catch_up(&mut store, 100).unwrap();
    w.commit().unwrap();
    assert_eq!(ids(&index, &q), (2..=11).collect::<Vec<u64>>());
    assert!(format!("{:?}", index.query(&q, NOW)).contains("RegexQuery"));
}

/// While the bucket terms are backfilled (a build shipping a new
/// generation of `file_bucket`), every bucket clause runs on the file
/// index, so a document the walk hasn't reached can't be missed; once
/// the walk completes they answer again, the same. A lease taken
/// mid-walk and paged after it keeps the answer it was compiled for.
#[test]
fn bucket_clauses_fall_back_while_their_terms_are_backfilled() {
    use lodestar_search::{Availability, CancelToken, Generations, Lease, Sort, Start};
    let sizes: Vec<u64> = boundary_sizes()
        .into_iter()
        .filter(|s| *s < 1 << 40)
        .collect();
    let torrents: Vec<Vec<u8>> = sizes
        .iter()
        .enumerate()
        .map(|(i, s)| sized(&format!("torrent {i}"), &[(format!("f{i}.bin"), *s as i64)]))
        .collect();
    let mut store = store_with(&torrents);
    let index = SearchIndex::in_memory().unwrap();
    let mut w = index.writer(60 << 20).unwrap();
    w.catch_up(&mut store, 100_000).unwrap();
    w.commit().unwrap();
    drop(w);
    let gens = Generations::current();
    let next = gens.get("file_bucket").unwrap() + 1;
    let index = std::sync::Arc::new(
        index
            .with_generations(gens.with("file_bucket", next))
            .unwrap(),
    );
    let queries: Vec<Node> = [
        (Some((1u64 << 30) + 1), None),
        (None, Some(1 << 20)),
        (Some(bucket_floor(30)), Some(bucket_floor(40) - 1)),
        (Some(100 << 20), None),
    ]
    .iter()
    .flat_map(|&(lo, hi)| {
        [
            file(range(lo, hi)),
            file(Node::And {
                of: vec![ext("bin"), range(lo, hi)],
            }),
        ]
    })
    // An extension less a rare word reads `file_ext_bucket` too
    // (X-search-files-6).
    .chain([file(Node::And {
        of: vec![
            ext("bin"),
            Node::Not {
                of: Box::new(word(TextField::Any, "f5")),
            },
        ],
    })])
    .collect();
    let want: Vec<Vec<(u64, f32)>> = queries
        .iter()
        .map(|q| ranked(&index, &on_the_file_index(q)))
        .collect();
    assert!(want.iter().all(|w| !w.is_empty()));
    // A third of the walk done.
    let mut w = index.writer(60 << 20).unwrap();
    w.catch_up(&mut store, sizes.len() / 3).unwrap();
    w.commit().unwrap();
    assert!(matches!(
        index.availability("file_bucket"),
        Availability::Backfilling { .. }
    ));
    // The same as the file index at this moment (ties order by document,
    // and the walk rewrites documents), and the same torrents as before.
    for (q, want) in queries.iter().zip(&want) {
        let got = ranked(&index, q);
        assert_eq!(got, ranked(&index, &on_the_file_index(q)), "mid-walk {q:?}");
        assert_eq!(by_id(&got), by_id(want), "mid-walk {q:?}");
    }
    let lease = Lease::new(
        index.clone(),
        &queries[0],
        NOW,
        Sort::Relevance,
        &CancelToken::new(),
    )
    .unwrap();
    // The walk done: the terms answer, the same.
    while w.catch_up(&mut store, 100_000).unwrap() > 0 {}
    w.commit().unwrap();
    assert_eq!(index.availability("file_bucket"), Availability::Ready);
    // The same as the file index at this moment (ties order by document,
    // and the walk rewrites documents), and the same torrents as before.
    for (q, want) in queries.iter().zip(&want) {
        let got = ranked(&index, q);
        assert_eq!(
            got,
            ranked(&index, &on_the_file_index(q)),
            "after the walk {q:?}"
        );
        assert_eq!(by_id(&got), by_id(want), "after the walk {q:?}");
    }
    let page = lease
        .page(Start::Offset(0), 10_000, &CancelToken::new())
        .unwrap();
    let mut got: Vec<u64> = page.hits.iter().map(|h| h.id).collect();
    let mut expect: Vec<u64> = want[0].iter().map(|h| h.0).collect();
    got.sort_unstable();
    expect.sort_unstable();
    assert_eq!(got, expect, "the mid-walk lease");
}
