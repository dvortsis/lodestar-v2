//! The result cache (architecture §8, B5c) against a real store: hits and
//! misses by index generation, visibility in the key, filter changes
//! reaching it through the index, bounds, concurrent readers across
//! commits, and paging after a cached first page.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use lodestar_bencode::{OwnedValue, to_vec};
use lodestar_metainfo::Info;
use lodestar_search::{
    CacheConfig, CancelToken, FacetParams, FacetSpec, Lease, Node, Outcome, ResultCache,
    SearchIndex, SearchSpec, Sort, Start, parse,
};
use lodestar_store::{Filtered, NewTorrent, Refiled, SourceId, Store, StoreConfig, flags};

const NOW: i64 = 1_700_000_100;

fn torrent(name: &str) -> Vec<u8> {
    let d = |e: Vec<(&str, OwnedValue)>| {
        OwnedValue::Dict(
            e.into_iter()
                .map(|(k, v)| (k.as_bytes().to_vec(), v))
                .collect(),
        )
    };
    let b = |s: &str| OwnedValue::Bytes(s.as_bytes().to_vec());
    to_vec(&d(vec![
        (
            "files",
            OwnedValue::List(vec![d(vec![
                ("length", OwnedValue::Int(1000)),
                ("path", OwnedValue::List(vec![b(&format!("{name}.mkv"))])),
            ])]),
        ),
        ("name", b(name)),
        ("piece length", OwnedValue::Int(1 << 16)),
        ("pieces", OwnedValue::Bytes(vec![0; 20])),
    ]))
}

fn add(store: &mut Store, names: &[String]) {
    let raws: Vec<Vec<u8>> = names.iter().map(|n| torrent(n)).collect();
    let infos: Vec<Info> = raws.iter().map(|r| Info::parse(r).unwrap()).collect();
    let batch: Vec<NewTorrent<'_>> = infos
        .iter()
        .zip(&raws)
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

/// A store and an index of `n` torrents named `Day {i}`, every third also
/// `Night`.
fn fixture(n: usize) -> (Store, Arc<SearchIndex>, lodestar_search::Indexer) {
    let mut store = Store::open_in_memory(StoreConfig::default()).unwrap();
    add(&mut store, &names(0..n));
    let index = SearchIndex::in_memory().unwrap();
    let mut w = index.writer(15 << 20).unwrap();
    w.catch_up(&mut store, 10_000).unwrap();
    w.commit().unwrap();
    (store, Arc::new(index), w)
}

fn names(r: std::ops::Range<usize>) -> Vec<String> {
    r.map(|i| {
        if i % 3 == 0 {
            format!("Day {i} Night")
        } else {
            format!("Day {i}")
        }
    })
    .collect()
}

fn commit(store: &mut Store, w: &mut lodestar_search::Indexer) {
    w.catch_up(store, 10_000).unwrap();
    w.commit().unwrap();
}

fn spec<'a>(node: &'a Node, scope: &'a str) -> SearchSpec<'a> {
    SearchSpec {
        node,
        scope,
        sort: Sort::Relevance,
        limit: 20,
        now: NOW,
        utc_offset: 0,
    }
}

fn ids(index: &SearchIndex, cache: &ResultCache, s: &SearchSpec<'_>) -> (Vec<u64>, Outcome) {
    let (got, o) = cache
        .search(&Arc::new(index.clone()), s, &CancelToken::new())
        .unwrap();
    (got.page.hits.iter().map(|h| h.id).collect(), o)
}

#[test]
fn a_commit_is_a_miss_and_a_quiet_reload_is_not() {
    let (mut store, index, mut w) = fixture(30);
    let cache = ResultCache::default();
    let node = parse("night").unwrap();
    let s = spec(&node, "");
    let (first, o) = ids(&index, &cache, &s);
    assert_eq!((first.len(), o), (10, Outcome::Miss));
    assert_eq!(ids(&index, &cache, &s), (first.clone(), Outcome::Hit));
    // A commit with nothing new changes no segment: still a hit.
    commit(&mut store, &mut w);
    assert_eq!(ids(&index, &cache, &s).1, Outcome::Hit);
    add(&mut store, &names(30..33));
    commit(&mut store, &mut w);
    let (after, o) = ids(&index, &cache, &s);
    assert_eq!((after.len(), o), (11, Outcome::Miss));
    let stats = cache.stats().search;
    assert_eq!((stats.hits, stats.misses, stats.retired), (2, 2, 1));
}

#[test]
fn requests_that_may_see_different_torrents_never_share() {
    let (_store, index, _w) = fixture(30);
    let cache = ResultCache::default();
    let shown = parse("day").unwrap();
    let hidden_out = parse("day").unwrap().without_hidden();
    ids(&index, &cache, &spec(&shown, "hidden=true"));
    // The same query text under another scope, or another query as run.
    assert_eq!(
        ids(&index, &cache, &spec(&shown, "hidden=false")).1,
        Outcome::Miss
    );
    assert_eq!(
        ids(&index, &cache, &spec(&hidden_out, "hidden=true")).1,
        Outcome::Miss
    );
    assert_eq!(
        ids(&index, &cache, &spec(&shown, "hidden=true")).1,
        Outcome::Hit
    );
    // Another sort or page size is another entry.
    let other = SearchSpec {
        sort: Sort::Size { desc: true },
        ..spec(&shown, "hidden=true")
    };
    assert_eq!(ids(&index, &cache, &other).1, Outcome::Miss);
    let other = SearchSpec {
        limit: 5,
        ..spec(&shown, "hidden=true")
    };
    assert_eq!(ids(&index, &cache, &other).1, Outcome::Miss);
}

/// A hide rule reaches stored torrents through the store's refiling
/// path (`Store::set_enrichments_at`, as the filter engine and the
/// `classify` corrections write it): the `hidden` mark and a
/// `reindex_log` entry in one transaction. The index picks it up at its
/// next commit, which is a new generation. A deletion (reported
/// material, §20.5.2) goes the same way.
#[test]
fn a_filter_change_never_shows_what_it_hides() {
    let (mut store, index, mut w) = fixture(30);
    let cache = ResultCache::default();
    let node = parse("night").unwrap().without_hidden();
    let s = spec(&node, "hidden=false");
    let (before, _) = ids(&index, &cache, &s);
    let gone = before[0];
    assert_eq!(ids(&index, &cache, &s).1, Outcome::Hit);
    let hash = store.torrent(gone as i64).unwrap().unwrap().info_hash.0;
    let hashes = [hash];
    let refiled = Refiled {
        id: gone as i64,
        filtered: Filtered {
            hashes: &hashes,
            rule: "U-1",
            action: 3,
            held: None,
            sealed: None,
            evidence: &[],
            at: NOW,
            expires_at: 0,
            adult_hold: false,
        },
        flags: flags::HIDDEN,
    };
    let generation = store.class_gen().unwrap();
    let written = store
        .set_enrichments_at(generation, &[], &[refiled])
        .unwrap();
    assert!(written.is_some());
    // Before the commit the index hasn't changed: still the cached page,
    // as the uncached path would show it.
    assert_eq!(ids(&index, &cache, &s).1, Outcome::Hit);
    commit(&mut store, &mut w);
    let (after, o) = ids(&index, &cache, &s);
    assert_eq!(o, Outcome::Miss);
    assert!(!after.contains(&gone), "{after:?}");
    assert_eq!(after.len(), before.len() - 1);
    // With hidden torrents asked for, it's there: another scope.
    let shown = parse("night").unwrap();
    let (all, o) = ids(&index, &cache, &spec(&shown, "hidden=true"));
    assert_eq!(o, Outcome::Miss);
    assert!(all.contains(&gone));
    // Deleted, it's gone from that too, from a miss.
    store.delete_torrents(&hashes).unwrap();
    commit(&mut store, &mut w);
    let (deleted, o) = ids(&index, &cache, &spec(&shown, "hidden=true"));
    assert_eq!(o, Outcome::Miss);
    assert!(!deleted.contains(&gone));
}

#[test]
fn memory_stays_within_its_bounds() {
    let (_store, index, _w) = fixture(30);
    let cache = ResultCache::new(CacheConfig {
        max_entries: 4,
        ..CacheConfig::default()
    });
    let queries: Vec<Node> = (0..10)
        .map(|i| parse(&format!("day {i}")).unwrap())
        .collect();
    for q in &queries {
        ids(&index, &cache, &spec(q, ""));
    }
    let s = cache.stats().search;
    assert_eq!((s.entries, s.evictions), (4, 6));
    assert!(s.bytes > 0 && s.bytes < 4 * 4096, "{s:?}");
    // The most recent are kept.
    assert_eq!(ids(&index, &cache, &spec(&queries[9], "")).1, Outcome::Hit);
    assert_eq!(ids(&index, &cache, &spec(&queries[0], "")).1, Outcome::Miss);
    let cache = ResultCache::new(CacheConfig {
        max_bytes: 6_000,
        ..CacheConfig::default()
    });
    for q in &queries {
        ids(&index, &cache, &spec(q, ""));
    }
    assert!(cache.stats().search.bytes <= 6_000);
}

#[test]
fn readers_never_see_a_generation_older_than_the_last_commit() {
    let (mut store, index, mut w) = fixture(10);
    let cache = Arc::new(ResultCache::default());
    // Torrents visible, published once their commit has returned.
    let visible = Arc::new(AtomicUsize::new(10));
    let stop = Arc::new(AtomicBool::new(false));
    let readers: Vec<_> = (0..4)
        .map(|_| {
            let (index, cache, visible, stop) =
                (index.clone(), cache.clone(), visible.clone(), stop.clone());
            std::thread::spawn(move || {
                let node = parse("day").unwrap();
                let mut hits = 0;
                while !stop.load(Ordering::Relaxed) {
                    let floor = visible.load(Ordering::Acquire);
                    let (got, o) = cache
                        .search(&index, &spec(&node, ""), &CancelToken::new())
                        .unwrap();
                    let total = got.lease.total().value;
                    assert!(total >= floor, "saw {total} after {floor} were committed");
                    hits += usize::from(o != Outcome::Miss);
                }
                hits
            })
        })
        .collect();
    for i in 0..40 {
        add(&mut store, &names(10 + i..11 + i));
        commit(&mut store, &mut w);
        visible.store(11 + i, Ordering::Release);
        std::thread::sleep(Duration::from_millis(2));
    }
    stop.store(true, Ordering::Relaxed);
    let hits: usize = readers.into_iter().map(|r| r.join().unwrap()).sum();
    assert!(hits > 0, "the readers shared some results");
}

#[test]
fn page_two_after_a_cached_page_one_reads_the_same_snapshot() {
    let (mut store, index, mut w) = fixture(90);
    let cache = ResultCache::default();
    let node = parse("day").unwrap();
    let c = CancelToken::new();
    for sort in [Sort::Relevance, Sort::Discovered { desc: true }] {
        let s = SearchSpec {
            sort,
            ..spec(&node, "")
        };
        let (cold, _) = cache.search(&index, &s, &c).unwrap();
        let (warm, o) = cache.search(&index, &s, &c).unwrap();
        assert_eq!(o, Outcome::Hit);
        // A reference lease on the same snapshot, scrolled whole.
        let reference = Lease::new(index.clone(), &node, NOW, sort, &c).unwrap();
        let all: Vec<u64> = reference
            .page(Start::Offset(0), 1_000, &c)
            .unwrap()
            .hits
            .iter()
            .map(|h| h.id)
            .collect();
        // New matches land between page 1 and page 2.
        add(&mut store, &names(90 + all.len()..120 + all.len()));
        commit(&mut store, &mut w);
        let mut seen: Vec<u64> = warm.page.hits.iter().map(|h| h.id).collect();
        let mut next = warm.page.next;
        while let Some(p) = next {
            assert!(warm.lease.fits(&p));
            let page = warm.lease.page(Start::After(p), 20, &c).unwrap();
            seen.extend(page.hits.iter().map(|h| h.id));
            next = page.next;
        }
        assert_eq!(seen, all, "{sort:?}");
        assert!(Arc::ptr_eq(&cold.lease, &warm.lease));
    }
}

#[test]
fn facets_are_cached_per_generation_and_shared_with_a_searchs_lease() {
    let (mut store, index, mut w) = fixture(30);
    let cache = ResultCache::default();
    let node = parse("day").unwrap();
    let params = FacetParams::default();
    let fs = FacetSpec {
        query: &node,
        node: &node,
        filters: &[],
        params: &params,
        scope: "",
        sort: Sort::Relevance,
        now: NOW,
        utc_offset: 0,
    };
    let c = CancelToken::new();
    let (a, o) = cache.facets(&index, &fs, &c).unwrap();
    assert_eq!(o, Outcome::Miss);
    assert_eq!(a.total.value, 30);
    assert_eq!(cache.facets(&index, &fs, &c).unwrap().1, Outcome::Hit);
    // The same facets on a search's lease of this snapshot: a hit.
    let (search, _) = cache.search(&index, &spec(&node, ""), &c).unwrap();
    let (b, o) = cache.facets_on(&index, &search.lease, &fs, &c).unwrap();
    assert_eq!((o, &b.facets), (Outcome::Hit, &a.facets));
    // Other parameters are other entries.
    let hide = FacetParams::new(Default::default(), None, true);
    let other = FacetSpec {
        params: &hide,
        ..fs
    };
    assert_eq!(cache.facets(&index, &other, &c).unwrap().1, Outcome::Miss);
    add(&mut store, &names(30..40));
    commit(&mut store, &mut w);
    let (fresh, o) = cache.facets(&index, &fs, &c).unwrap();
    assert_eq!((o, fresh.total.value), (Outcome::Miss, 40));
    // The old lease still reads its snapshot (and its own count).
    let (old, o) = cache.facets_on(&index, &search.lease, &fs, &c).unwrap();
    assert_eq!(
        o,
        Outcome::Miss,
        "an older lease is counted without the cache"
    );
    assert_eq!(old.total.value, 30);
}

/// The §3 budget for a cached browse and cached facets (≤ 2 ms p95), on
/// the cache's side of `/search` and `/facets`. Timing: run it in a
/// release build (`cargo test --release -p lodestar-search --test cache
/// -- --ignored`).
#[test]
#[ignore = "timing; run in release"]
fn cached_hits_are_within_two_milliseconds() {
    let (_store, index, _w) = fixture(5_000);
    let cache = ResultCache::default();
    let browse = parse("").unwrap().without_hidden();
    let params = FacetParams::default();
    let fs = FacetSpec {
        query: &browse,
        node: &browse,
        filters: &[],
        params: &params,
        scope: "",
        sort: Sort::Relevance,
        now: NOW,
        utc_offset: 0,
    };
    let c = CancelToken::new();
    let s = SearchSpec {
        limit: 50,
        sort: Sort::Discovered { desc: true },
        ..spec(&browse, "")
    };
    cache.search(&index, &s, &c).unwrap();
    cache.facets(&index, &fs, &c).unwrap();
    let mut times = Vec::new();
    for _ in 0..1_000 {
        let t = Instant::now();
        assert_eq!(cache.search(&index, &s, &c).unwrap().1, Outcome::Hit);
        assert_eq!(cache.facets(&index, &fs, &c).unwrap().1, Outcome::Hit);
        times.push(t.elapsed());
    }
    times.sort();
    let p95 = times[times.len() * 95 / 100];
    println!(
        "cached browse + facets: p50 {:?}, p95 {p95:?}",
        times[times.len() / 2]
    );
    assert!(p95 <= Duration::from_millis(2), "{p95:?}");
}
