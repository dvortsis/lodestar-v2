//! `file:(size:…)` through the whole index (X-search-files-3): a size
//! range reads the postings of the `size_bucket`s inside it and checks
//! the stored size only in its edge buckets. Every result here must be
//! exactly what a scan of every file's size gives, at range edges, open
//! ends, bucket boundaries, and zero and huge sizes.

use lodestar_bencode::{OwnedValue, to_vec};
use lodestar_metainfo::Info;
use lodestar_search::files::{bucket_floor, size_bucket};
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

/// The sizes at every bucket boundary that a store can hold (lengths are
/// signed 64-bit in bencode), each with its neighbours, plus 0, 1 and
/// the largest length.
fn boundary_sizes() -> Vec<u64> {
    let max = i64::MAX as u64;
    let mut sizes: Vec<u64> = (0..128)
        .map(bucket_floor)
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
    // Beside a word, a filter.
    let with_word = Node::And {
        of: vec![word(TextField::Any, "photos"), file(ext("jpg"))],
    };
    assert_eq!(ids(&index, &with_word), vec![1]);
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
