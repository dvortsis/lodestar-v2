//! The index against a real store: catch-up, queries, the watermark and
//! crash safety.

use lodestar_bencode::{OwnedValue, to_vec};
use lodestar_metainfo::{Info, InfoHash};
use lodestar_search::{SearchIndex, index_dir};
use lodestar_store::{NewTorrent, SourceId, Store, StoreConfig};

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

/// A multi-file v1 torrent.
fn torrent(name: &str, files: &[&str]) -> Vec<u8> {
    let files = files
        .iter()
        .map(|p| {
            d(vec![
                ("length", OwnedValue::Int(1000)),
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
    add(&mut store, torrents);
    store
}

fn add(store: &mut Store, torrents: &[Vec<u8>]) {
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
}

const NOW: i64 = 1_700_000_100;

fn names(index: &SearchIndex, q: &str) -> Vec<String> {
    let n = index
        .search(
            index
                .query(&lodestar_search::parse(q).unwrap(), NOW)
                .as_ref(),
            50,
        )
        .unwrap()
        .hits;
    let mut n = index.names(&n).unwrap();
    n.sort();
    n
}

#[test]
fn indexes_the_store_and_finds_names_and_paths() {
    let mut store = store_with(&[
        torrent(
            "Show.Name.S01E02.1080p.WEB-DL.DD+5.1.x265-GRP",
            &["Show.Name.S01E02.mkv", "Sample/sample.mkv"],
        ),
        torrent("a8f3x", &["Some Documentary 2019/part1.mkv"]),
        torrent("千と千尋の神隠し", &["movie.mkv"]),
    ]);
    let index = SearchIndex::in_memory().unwrap();
    let mut w = index.writer(15 << 20).unwrap();
    assert_eq!(w.catch_up(&mut store, 100).unwrap(), 3);
    w.commit().unwrap();
    assert_eq!(index.num_docs(), 3);
    assert_eq!(index.watermark().unwrap(), 3);

    let show = vec!["Show.Name.S01E02.1080p.WEB-DL.DD+5.1.x265-GRP".to_string()];
    for q in [
        "web-dl",
        "webdl",
        "WEB DL",
        "s01e02",
        "x265",
        "dd+",
        "5.1",
        "show name 1080p",
    ] {
        assert_eq!(names(&index, q), show, "{q}");
    }
    // A name that says nothing is found through its files.
    assert_eq!(names(&index, "documentary 2019"), vec!["a8f3x".to_string()]);
    assert_eq!(
        names(&index, "千と千尋"),
        vec!["千と千尋の神隠し".to_string()]
    );
    assert!(
        names(&index, "documentary 2020").is_empty(),
        "every word must match"
    );
    assert_eq!(names(&index, "mkv").len(), 3);
    // The name field outranks a path-only match.
    let hits = index
        .search(
            index
                .query(&lodestar_search::parse("sample").unwrap(), NOW)
                .as_ref(),
            10,
        )
        .unwrap();
    assert_eq!(hits.total, 1);
}

#[test]
fn paths_match_words_and_compounds() {
    let mut store = store_with(&[torrent(
        "x",
        &[
            "Season 1/Show.S01E02.WEB-DL.mkv",
            "Season 1/extras one.txt",
            "two.txt",
        ],
    )]);
    let index = SearchIndex::in_memory().unwrap();
    let mut w = index.writer(15 << 20).unwrap();
    w.catch_up(&mut store, 10).unwrap();
    w.commit().unwrap();
    for q in ["webdl", "web-dl", "s01e02", "season", "one two", "extras"] {
        assert_eq!(names(&index, q), vec!["x".to_string()], "{q}");
    }
    assert!(names(&index, "three").is_empty());
}

#[test]
fn finds_torrents_by_any_swarm_hash() {
    let raw = torrent("by hash", &["f.bin"]);
    let hash = Info::parse(&raw).unwrap().primary_hash();
    let mut store = store_with(&[raw]);
    let index = SearchIndex::in_memory().unwrap();
    let mut w = index.writer(15 << 20).unwrap();
    w.catch_up(&mut store, 10).unwrap();
    w.commit().unwrap();
    assert_eq!(index.find_hash(&hash).unwrap(), Some(1));
    assert_eq!(index.find_hash(&InfoHash([7; 20])).unwrap(), None);
}

#[test]
fn the_watermark_survives_restarts_and_crashes() {
    let dir = tempfile::tempdir().unwrap();
    let path = index_dir(dir.path());
    let mut store = store_with(&[torrent("one", &["1.bin"]), torrent("two", &["2.bin"])]);
    {
        let index = SearchIndex::open(&path).unwrap();
        let mut w = index.writer(15 << 20).unwrap();
        w.catch_up(&mut store, 1).unwrap();
        w.commit().unwrap();
        // Added but never committed: as if we crashed here.
        w.catch_up(&mut store, 10).unwrap();
        assert_eq!(w.pending(), 1);
    }
    let index = SearchIndex::open(&path).unwrap();
    assert_eq!(index.watermark().unwrap(), 1);
    assert_eq!(index.num_docs(), 1);
    let mut w = index.writer(15 << 20).unwrap();
    add(&mut store, &[torrent("three", &["3.bin"])]);
    assert_eq!(
        w.catch_up(&mut store, 10).unwrap(),
        2,
        "two and three, once each"
    );
    w.commit().unwrap();
    assert_eq!(index.num_docs(), 3);
    assert_eq!(names(&index, "two"), vec!["two".to_string()]);
    assert_eq!(w.catch_up(&mut store, 10).unwrap(), 0);
}

#[test]
fn the_live_indexer_follows_the_store_within_seconds() {
    use lodestar_search::{LiveConfig, LiveIndexer};
    use std::time::{Duration, Instant};
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("l.db");
    let mut writer = Store::open(&db, StoreConfig::default()).unwrap();
    add(&mut writer, &[torrent("first", &["a.bin"])]);
    let reader = Store::open_reader(&db, StoreConfig::default()).unwrap();
    let index = SearchIndex::open(&index_dir(&dir.path().join("index"))).unwrap();
    let live = LiveIndexer::spawn(
        index.clone(),
        reader,
        LiveConfig {
            heap_bytes: 15 << 20,
            poll: Duration::from_millis(20),
            commit_every: Duration::from_millis(100),
            ..LiveConfig::default()
        },
    )
    .unwrap();
    let wait = |n: u64| {
        let start = Instant::now();
        while index.num_docs() < n {
            assert!(start.elapsed() < Duration::from_secs(5), "NRT budget");
            std::thread::sleep(Duration::from_millis(10));
        }
    };
    wait(1);
    add(&mut writer, &[torrent("second", &["b.bin"])]);
    wait(2);
    assert_eq!(names(&index, "second"), vec!["second".to_string()]);
    let stats = live.stats();
    live.stop().unwrap();
    assert_eq!(
        stats.watermark.load(std::sync::atomic::Ordering::Relaxed),
        2
    );
    assert_eq!(index.watermark().unwrap(), 2);
}
