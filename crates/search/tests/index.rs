//! The index against a real store: catch-up, queries, the watermark and
//! crash safety.

use lodestar_bencode::{OwnedValue, to_vec};
use lodestar_metainfo::{Info, InfoHash};
use lodestar_search::{SearchIndex, Sort, index_dir};
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
        .search(&index.query(&lodestar_search::parse(q).unwrap(), NOW), 50)
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
            &index.query(&lodestar_search::parse("sample").unwrap(), NOW),
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

#[test]
fn upgraded_imports_are_reindexed() {
    use lodestar_store::flags;
    let raw = torrent("pack", &["Hidden.Gem.mkv", "other.txt"]);
    let real = Info::parse(&raw).unwrap();
    let mut partial = real.clone();
    partial.files.clear();
    let mut store = Store::open_in_memory(StoreConfig::default()).unwrap();
    let base = NewTorrent {
        info: &real,
        raw_info: Some(&raw),
        discovered_at: 1_700_000_000,
        metadata_at: 1_700_000_001,
        extra_flags: 0,
        source: SourceId::new(0).unwrap(),
    };
    store
        .insert(&[NewTorrent {
            info: &partial,
            raw_info: None,
            extra_flags: flags::IMPORTED | flags::FILES_PARTIAL,
            ..base
        }])
        .unwrap();
    let index = SearchIndex::in_memory().unwrap();
    let mut w = index.writer(15 << 20).unwrap();
    w.catch_up(&mut store, 10).unwrap();
    w.commit().unwrap();
    assert!(names(&index, "gem").is_empty(), "no files known yet");
    store.insert(&[base]).unwrap();
    assert_eq!(w.catch_up(&mut store, 10).unwrap(), 1);
    w.commit().unwrap();
    assert_eq!(names(&index, "gem"), vec!["pack".to_string()]);
    assert_eq!(index.num_docs(), 1, "replaced, not duplicated");
    assert_eq!(w.catch_up(&mut store, 10).unwrap(), 0);
}

#[test]
fn browse_follows_new_segments_and_deletions() {
    use lodestar_search::{Sort, parse};
    use lodestar_store::flags;
    let mut store = Store::open_in_memory(StoreConfig::default()).unwrap();
    add(
        &mut store,
        &[torrent("one", &["a"]), torrent("two", &["a", "b"])],
    );
    let index = SearchIndex::in_memory().unwrap();
    let mut w = index.writer(15 << 20).unwrap();
    w.catch_up(&mut store, 10).unwrap();
    w.commit().unwrap();
    let browse = || {
        let q = index.query(&parse("").unwrap(), NOW);
        let r = index
            .search_page(&q, Sort::Files { desc: true }, 10, 0)
            .unwrap();
        (index.names(&r.hits).unwrap(), r.total)
    };
    assert_eq!(browse(), (vec!["two".into(), "one".into()], 2));

    add(&mut store, &[torrent("three", &["a", "b", "c"])]);
    w.catch_up(&mut store, 10).unwrap();
    w.commit().unwrap();
    assert_eq!(
        browse(),
        (vec!["three".into(), "two".into(), "one".into()], 3)
    );

    // An upgraded import is deleted and re-added in an older segment's
    // place; the cached top of that segment must not survive.
    let raw = torrent("pack", &["a", "b", "c", "d"]);
    let real = Info::parse(&raw).unwrap();
    let mut partial = real.clone();
    partial.files.clear();
    let base = NewTorrent {
        info: &real,
        raw_info: Some(&raw),
        discovered_at: 1_700_000_000,
        metadata_at: 1_700_000_001,
        extra_flags: 0,
        source: SourceId::new(0).unwrap(),
    };
    store
        .insert(&[NewTorrent {
            info: &partial,
            raw_info: None,
            extra_flags: flags::IMPORTED | flags::FILES_PARTIAL,
            ..base
        }])
        .unwrap();
    w.catch_up(&mut store, 10).unwrap();
    w.commit().unwrap();
    assert_eq!(browse().1, 4);
    store.insert(&[base]).unwrap();
    w.catch_up(&mut store, 10).unwrap();
    w.commit().unwrap();
    let (names, total) = browse();
    assert_eq!(total, 4, "replaced, not duplicated");
    assert_eq!(names[0], "pack", "the upgrade's four files sort first");
}

/// A multi-file v1 torrent with file sizes.
fn sized(name: &str, files: &[(&str, i64)]) -> Vec<u8> {
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

const GB: i64 = 1 << 30;

#[test]
fn enrichment_fields_are_searchable_once_enriched() {
    let movie = "The.Dark.Knight.2008.2160p.UHD.BluRay.x265.HDR.DV.Atmos-NTb";
    let show = "Show.Name.S02.1080p.WEB-DL.FRENCH-GRP";
    let doc = "a8f3x";
    let lure = "Free.Movie.2021.1080p";
    let mut store = store_with(&[
        sized(
            movie,
            &[
                (
                    "The.Dark.Knight.2008.2160p.UHD.BluRay.x265-NTb.mkv",
                    40 * GB,
                ),
                ("The.Dark.Knight.2008.2160p.UHD.BluRay.x265-NTb.nfo", 5_000),
            ],
        ),
        sized(
            show,
            &[
                ("Show.Name.S02E01.1080p.mkv", GB),
                ("Show.Name.S02E02.1080p.mkv", GB),
                ("Show.Name.S02E03.1080p.mkv", GB),
                ("Show.Name.S02.srt", 50_000),
            ],
        ),
        sized(
            doc,
            &[(
                "Some Documentary 2019/Some.Documentary.2019.720p.mkv",
                2 * GB,
            )],
        ),
        sized(lure, &[("Free.Movie.2021.1080p.mp4.exe", 2_000_000)]),
    ]);
    let index = SearchIndex::in_memory().unwrap();
    let mut w = index.writer(15 << 20).unwrap();
    w.catch_up(&mut store, 100).unwrap();
    w.commit().unwrap();
    // Not enriched yet: no facets, and every junk score reads 0.
    assert!(names(&index, "res:2160p").is_empty());
    assert_eq!(names(&index, "junk:0").len(), 4);

    // The writer's backfill enriches them and logs them for reindexing.
    let (_, n) = lodestar_pipeline::enrich::enrich_stored(&mut store, 0, 100)
        .unwrap()
        .unwrap();
    assert_eq!(n, 4);
    assert_eq!(w.catch_up(&mut store, 100).unwrap(), 4);
    w.commit().unwrap();
    assert_eq!(index.num_docs(), 4);
    // An index built after the backfill reads the enrichment directly
    // and doesn't replay the log.
    let rebuilt = SearchIndex::in_memory().unwrap();
    let mut r = rebuilt.writer(15 << 20).unwrap();
    assert_eq!(r.catch_up(&mut store, 100).unwrap(), 4);
    assert_eq!(r.catch_up(&mut store, 100).unwrap(), 0);

    let one = |s: &str| vec![s.to_string()];
    let check = |q: &str, want: &[&str]| {
        let mut want: Vec<String> = want.iter().map(|s| s.to_string()).collect();
        want.sort();
        assert_eq!(names(&index, q), want, "{q}");
    };
    for q in [
        "res:2160p",
        "res:4k",
        "codec:x265",
        "hdr:dv audio:atmos",
        "group:NTB",
        "has:nfo",
        "year:2008",
        "year:2000..2010",
        "title:\"dark knight\"",
        "knight type:movie",
    ] {
        assert_eq!(names(&index, q), one(movie), "{q}");
    }
    for q in [
        "s:2",
        "s:1..3 res:1080p",
        "lang:french",
        "type:tv",
        "cat:subtitle",
    ] {
        assert_eq!(names(&index, q), one(show), "{q}");
    }
    check("s:1", &[]);
    // `e:` is the release's own episode; a pack's episodes are its
    // files' (`file:(…)`).
    check("e:2", &[]);
    for q in ["ambiguous:yes", "resolved:files", "title:documentary"] {
        assert_eq!(names(&index, q), one(doc), "{q}");
    }
    check("ambiguous:no", &[movie, show, lure]);
    check("junk:>=50", &[lure]);
    check("junk:<50", &[movie, show, doc]);
    // Shares: by count and by bytes, through the collector (with a word
    // to drive) and in the query (alone).
    check("cat:video=50", &[movie]);
    check("cat:video<=60", &[movie, lure]);
    check("bytes:video>=99", &[movie, show, doc]);
    check("cat:software=0", &[movie, show, doc]);
    check("mkv cat:software=0", &[movie, show, doc]);
    check("mkv bytes:video>=99 cat:video<=80", &[movie, show]);
    check("cat:software", &[lure]);
    check("type:movie,tv -cat:subtitle", &[movie, doc]);
}

fn ids(index: &SearchIndex, q: &str) -> Vec<u64> {
    let node = lodestar_search::parse(q).unwrap();
    let mut ids: Vec<u64> = index
        .search(&index.query(&node, NOW), 50)
        .unwrap()
        .hits
        .into_iter()
        .map(|h| h.id)
        .collect();
    ids.sort();
    ids
}

const MB: i64 = 1 << 20;

fn file_corpus() -> Vec<Vec<u8>> {
    vec![
        // 1: a lossless album with a big and a small track.
        sized(
            "Artist - Album (2019) [FLAC]",
            &[
                ("01 Opening.flac", 60 * MB),
                ("02 Interlude.flac", 20 * MB),
                ("cover.jpg", MB / 2),
            ],
        ),
        // 2: an MP3 album with one big file.
        sized(
            "Artist - Album (2019) [MP3]",
            &[("01 Opening.mp3", 80 * MB), ("02 Interlude.mp3", 8 * MB)],
        ),
        // 3: "dark" and "knight" in different files, not in one name.
        sized(
            "Misc Collection",
            &[
                ("dark matters.mkv", 700 * MB),
                ("knight rider.mkv", 700 * MB),
            ],
        ),
        // 4: both words in one file name.
        sized(
            "Batman Trilogy",
            &[
                ("The Dark Knight (2008).mkv", 30 * GB),
                ("Sample/sample.mkv", 20 * MB),
            ],
        ),
    ]
}

#[test]
fn file_clauses_match_within_one_file() {
    let mut store = store_with(&file_corpus());
    let index = SearchIndex::in_memory().unwrap();
    let mut w = index.writer(30 << 20).unwrap();
    assert_eq!(w.catch_up(&mut store, 100).unwrap(), 4);
    w.commit().unwrap();
    assert_eq!(index.num_files(), 9);
    assert_eq!(index.files_watermark().unwrap(), 4);

    // Every inner clause must hold for the same file.
    assert_eq!(ids(&index, "file:(ext:flac size:>50MB)"), [1]);
    assert_eq!(ids(&index, "file:(ext:flac,mp3 size:>50MB)"), [1, 2]);
    assert!(ids(&index, "file:(ext:jpg size:>50MB)").is_empty());
    // Phrases and NEAR within one file name, which the torrent index's
    // paths (no positions) can't tell apart.
    assert_eq!(ids(&index, "file:\"dark knight\""), [4]);
    assert_eq!(ids(&index, "file:(dark NEAR/1 knight)"), [4]);
    assert_eq!(ids(&index, "file:(dark knight)"), [4]);
    assert_eq!(ids(&index, "path:dark path:knight"), [3, 4]);
    // Words match directories too; path: only directories.
    assert_eq!(ids(&index, "file:sample"), [4]);
    assert_eq!(ids(&index, "file:(path:sample ext:mkv)"), [4]);
    // Beside other clauses (checked in the collector), negated, nested
    // and alone (driving the query).
    assert_eq!(ids(&index, "album file:(opening size:>70MB)"), [2]);
    assert_eq!(ids(&index, "album -file:(ext:flac)"), [2]);
    assert_eq!(ids(&index, "batman | file:(ext:mp3)"), [2, 4]);
    assert_eq!(ids(&index, "file:(ext:mkv) -file:(path:sample)"), [3]);
    // Browse order with a file clause.
    let node = lodestar_search::parse("file:(ext:mkv)").unwrap();
    let page = index
        .search_page(
            &index.query(&node, NOW),
            lodestar_search::Sort::Size { desc: true },
            10,
            0,
        )
        .unwrap();
    assert_eq!(page.total, 2);
    assert_eq!(page.hits[0].id, 4);

    // Matched files are positions in the torrent's file list.
    let node = lodestar_search::parse("album file:(opening | interlude) -x").unwrap();
    assert_eq!(index.matched_files(&node, 1, 10).unwrap(), [0, 1]);
    let node = lodestar_search::parse("file:(ext:flac size:>50MB)").unwrap();
    assert_eq!(index.matched_files(&node, 1, 10).unwrap(), [0]);
    assert!(index.matched_files(&node, 2, 10).unwrap().is_empty());
}

#[test]
fn an_index_left_behind_catches_up_alone() {
    let dir = tempfile::tempdir().unwrap();
    let path = index_dir(dir.path());
    let mut store = store_with(&file_corpus());
    {
        let index = SearchIndex::open(&path).unwrap();
        let mut w = index.writer(30 << 20).unwrap();
        w.catch_up(&mut store, 100).unwrap();
        w.commit().unwrap();
    }
    // As if a crash came between the two commits and the file index lost
    // everything: the torrent index keeps its documents, and the next
    // catch-up fills in the files without adding torrents twice.
    std::fs::remove_dir_all(path.join("files")).unwrap();
    let index = SearchIndex::open(&path).unwrap();
    assert_eq!(
        (index.watermark().unwrap(), index.files_watermark().unwrap()),
        (4, 0)
    );
    assert!(ids(&index, "file:(ext:flac)").is_empty());
    let mut w = index.writer(30 << 20).unwrap();
    w.catch_up(&mut store, 100).unwrap();
    w.commit().unwrap();
    assert_eq!(index.num_docs(), 4);
    assert_eq!(index.num_files(), 9);
    assert_eq!(ids(&index, "file:(ext:flac)"), [1]);
}

#[test]
fn plurals_match_through_the_title_stem_as_a_fallback() {
    let tdk = "The.Dark.Knight.2008.1080p.BluRay.x264-GRP";
    let kotz = "Knights.of.the.Zodiac.2023.1080p.WEB-DL.x264-GRP";
    let mut store = store_with(&[
        sized(tdk, &[("tdk.mkv", 10 * GB)]),
        sized(kotz, &[("kotz.mkv", 3 * GB)]),
        sized(
            "Toy.Story.1995.1080p.BluRay.x264-GRP",
            &[("ts.mkv", 4 * GB)],
        ),
        sized(
            "Ghost.Stories.2017.720p.BluRay.x264-GRP",
            &[("gs.mkv", 2 * GB)],
        ),
        // The word only in a file name: the stem tier reads titles only.
        sized("Misc Stuff", &[("knight notes.txt", 10_000)]),
    ]);
    lodestar_pipeline::enrich::enrich_stored(&mut store, 0, 100)
        .unwrap()
        .unwrap();
    let index = SearchIndex::in_memory().unwrap();
    let mut w = index.writer(15 << 20).unwrap();
    w.catch_up(&mut store, 100).unwrap();
    w.commit().unwrap();
    let ranked = |q: &str| {
        let node = lodestar_search::parse(q).unwrap();
        let (r, stemmed) = index
            .search_with_fallback(&node, NOW, Sort::Relevance, 50, 0)
            .unwrap();
        (index.names(&r.hits).unwrap(), stemmed)
    };

    // The plain query is exact.
    assert_eq!(names(&index, "knights"), [kotz]);
    // Few exact matches: plural ↔ singular, both ways, exact first.
    assert_eq!(ranked("knights"), (vec![kotz.into(), tdk.into()], true));
    assert_eq!(
        ranked("knight"),
        (vec![tdk.into(), "Misc Stuff".into(), kotz.into()], true)
    );
    let (stories, _) = ranked("stories");
    assert_eq!(stories.len(), 2);
    assert!(stories[0].starts_with("Ghost.Stories"));
    // Every word must still match: by its exact form anywhere, or by
    // its stem in the title.
    assert_eq!(ranked("dark knights"), (vec![tdk.into()], true));
    assert_eq!(ranked("knights 1995"), (vec![], false));
    // title: takes stems too; phrases and other fields don't, and a
    // tier that finds nothing more isn't used.
    assert_eq!(ranked("title:knights").0.len(), 2);
    assert_eq!(ranked("\"dark knights\""), (vec![], false));
    assert_eq!(ranked("name:knights"), (vec![kotz.into()], false));
    assert_eq!(ranked("zzzz | knights").0.len(), 2);
    // Negated words stay exact: the tier widens, never excludes more.
    assert_eq!(ranked("1080p -knights").0.len(), 2);

    // With enough exact matches there's no fallback.
    let more: Vec<Vec<u8>> = (0..lodestar_search::STEM_FALLBACK_BELOW)
        .map(|i| sized(&format!("Knights.Collection.Part.{i}"), &[("k.mkv", GB)]))
        .collect();
    add(&mut store, &more);
    lodestar_pipeline::enrich::enrich_stored(&mut store, 0, 100)
        .unwrap()
        .unwrap();
    w.catch_up(&mut store, 100).unwrap();
    w.commit().unwrap();
    let (hits, stemmed) = ranked("knights");
    assert!(!stemmed);
    assert!(!hits.contains(&tdk.to_string()));
}

#[test]
fn hidden_torrents_stay_out_unless_asked_for() {
    use lodestar_store::flags;
    let shown = torrent("Open Day", &["a.mkv"]);
    let hidden = torrent("Hidden Day", &["b.mkv"]);
    let infos = [Info::parse(&shown).unwrap(), Info::parse(&hidden).unwrap()];
    let mut store = Store::open_in_memory(StoreConfig::default()).unwrap();
    let new = |i: usize, extra_flags: i64| NewTorrent {
        info: &infos[i],
        raw_info: None,
        discovered_at: 1_700_000_000,
        metadata_at: 1_700_000_001,
        extra_flags,
        source: SourceId::new(0).unwrap(),
    };
    store
        .insert(&[new(0, 0), new(1, flags::HIDDEN | flags::DEMOTED)])
        .unwrap();
    let index = SearchIndex::in_memory().unwrap();
    let mut w = index.writer(15 << 20).unwrap();
    w.catch_up(&mut store, 10).unwrap();
    w.commit().unwrap();
    let visible = |q: &str| {
        let ast = lodestar_search::parse(q).unwrap().without_hidden();
        let hits = index.search(&index.query(&ast, NOW), 50).unwrap().hits;
        let mut n = index.names(&hits).unwrap();
        n.sort();
        n
    };
    assert_eq!(visible("day"), vec!["Open Day".to_string()]);
    assert_eq!(visible(""), vec!["Open Day".to_string()]);
    assert_eq!(visible("day hidden:yes"), vec!["Hidden Day".to_string()]);
    assert_eq!(
        names(&index, "day demoted:yes"),
        vec!["Hidden Day".to_string()]
    );
    assert_eq!(names(&index, "day").len(), 2);
}

#[test]
fn matching_ids_share_one_limit_across_segments() {
    use lodestar_search::parse;
    let mut store = Store::open_in_memory(StoreConfig::default()).unwrap();
    let index = SearchIndex::in_memory().unwrap();
    let mut w = index.writer(15 << 20).unwrap();
    // Three commits, so three segments of two matching torrents each.
    for batch in 0..3 {
        add(
            &mut store,
            &[
                torrent(&format!("match {batch}a"), &["x"]),
                torrent(&format!("match {batch}b"), &["x"]),
                torrent(&format!("other {batch}"), &["x"]),
            ],
        );
        w.catch_up(&mut store, 100).unwrap();
        w.commit().unwrap();
    }
    let q = index.query(&parse("match").unwrap(), NOW);
    let (all, over) = index.matching_ids(&q, 100).unwrap();
    assert_eq!((all.len(), over), (6, false));
    let (some, over) = index.matching_ids(&q, 3).unwrap();
    assert_eq!((some.len(), over), (3, true), "3 in total, not per segment");
    let (exact, over) = index.matching_ids(&q, 6).unwrap();
    assert_eq!((exact.len(), over), (6, false));
}

/// B4b: the Adult count `catch_up_counted` reports for the "N new"
/// banner is exactly what `type:adult` finds, and re-indexing (a later
/// enrichment) adds to neither count.
#[test]
fn the_adult_count_matches_what_type_adult_finds() {
    let adult = |n: &str| torrent(&format!("{n}.XXX.1080p"), &["scene.mp4"]);
    let other = |n: &str| torrent(&format!("{n}.2019.1080p"), &["movie.mkv"]);
    let mut store = store_with(&[
        other("Big.Movie"),
        adult("Studio.Scene"),
        other("Other.Movie"),
        adult("Another.Clip"),
        other("Third.Movie"),
    ]);
    let (_, n) = lodestar_pipeline::enrich::enrich_stored(&mut store, 0, 100)
        .unwrap()
        .unwrap();
    assert_eq!(n, 5);
    let index = SearchIndex::in_memory().unwrap();
    let mut w = index.writer(15 << 20).unwrap();
    let c = w.catch_up_counted(&mut store, 100).unwrap();
    w.commit().unwrap();
    assert_eq!((c.added, c.adult_added), (5, 2));
    assert_eq!(names(&index, "type:adult").len(), c.adult_added);
    assert_eq!(names(&index, "-type:adult").len(), c.added - c.adult_added);

    // Not enriched yet: not Adult in the index either, so not counted.
    add(&mut store, &[adult("Fresh.Clip"), other("Fresh.Movie")]);
    let c = w.catch_up_counted(&mut store, 100).unwrap();
    w.commit().unwrap();
    assert_eq!((c.added, c.adult_added), (2, 0));
    assert_eq!(names(&index, "type:adult").len(), 2);
    // Enriched later: re-indexed, so neither count moves, though search
    // now finds it as Adult.
    lodestar_pipeline::enrich::enrich_stored(&mut store, 5, 100)
        .unwrap()
        .unwrap();
    let c = w.catch_up_counted(&mut store, 100).unwrap();
    w.commit().unwrap();
    assert_eq!((c.added, c.adult_added, c.replayed), (0, 0, 2));
    assert_eq!(names(&index, "type:adult").len(), 3);
}
