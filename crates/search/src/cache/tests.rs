use std::sync::Arc;
use std::time::Duration;

use super::*;
use crate::facets::FacetParams;
use crate::index::Error;
use crate::testing::{append, delete, index_with};

const NOW: i64 = 1_700_000_100;
/// One snapshot order for keys made by hand: none is ever older.
const SEEN: Seen = Seen { index: 0, id: 0 };

fn key(what: &str, at: &Generation) -> Key {
    Key {
        what: what.into(),
        at: Snapshot {
            torrents: at.clone(),
            files: None,
        },
        now: None,
        utc_offset: None,
        hot: None,
    }
}

fn computed(v: u32, key: &Key, bytes: usize) -> Result<Computed<u32>> {
    Ok(Computed {
        value: v,
        key: Some(key.clone()),
        bytes,
    })
}

fn config(max_entries: usize, max_bytes: usize) -> CacheConfig {
    CacheConfig {
        enabled: true,
        max_entries,
        max_bytes,
        ttl: Duration::from_secs(600),
    }
}

fn generation(index: &SearchIndex) -> Generation {
    Generation::of(&index.searcher())
}

#[test]
fn a_hit_needs_the_same_key_and_generation() {
    let (index, _) = index_with(10, 1);
    let g1 = generation(&index);
    let lru: Lru<u32> = Lru::new(CacheConfig::default());
    let c = CancelToken::new();
    let get = |k: &Key, v| lru.get_or_compute(k.clone(), SEEN, true, &c, |k| computed(v, k, 10));
    assert_eq!(get(&key("a", &g1), 1).unwrap(), (1, Outcome::Miss));
    assert_eq!(get(&key("a", &g1), 2).unwrap(), (1, Outcome::Hit));
    assert_eq!(get(&key("b", &g1), 3).unwrap(), (3, Outcome::Miss));
    // A commit: a new generation misses, and retires the old one's.
    append(&index, 11..12, 11);
    let g2 = generation(&index);
    assert_ne!(g1, g2);
    assert_eq!(get(&key("a", &g2), 4).unwrap(), (4, Outcome::Miss));
    let s = lru.stats();
    assert_eq!((s.hits, s.misses, s.retired, s.entries), (1, 3, 2, 1));
    // Deletes alone are a new generation too.
    delete(&index, [3]);
    assert_ne!(generation(&index), g2);
    // A reload that changed nothing keeps it.
    let g3 = generation(&index);
    index.raw().1.reload().unwrap();
    assert_eq!(generation(&index), g3);
}

#[test]
fn bounds_hold_in_entries_and_bytes_least_recently_used_first() {
    let (index, _) = index_with(10, 1);
    let g = generation(&index);
    let lru: Lru<u32> = Lru::new(config(3, 1_000));
    let c = CancelToken::new();
    let get = |w: &str, bytes| {
        lru.get_or_compute(key(w, &g), SEEN, true, &c, |k| computed(0, k, bytes))
            .unwrap()
            .1
    };
    for w in ["a", "b", "c"] {
        get(w, 100);
    }
    assert_eq!(get("a", 100), Outcome::Hit);
    get("d", 100);
    // `b` was the least recently used.
    assert_eq!(get("b", 100), Outcome::Miss);
    let s = lru.stats();
    assert_eq!((s.entries, s.evictions), (3, 2));
    assert_eq!(get("a", 100), Outcome::Hit);
    // 900 bytes: the two least recently used go to make room.
    get("big", 900);
    let s = lru.stats();
    assert_eq!((s.entries, s.bytes, s.evictions), (2, 1_000, 4));
    assert_eq!(get("a", 100), Outcome::Hit);
    // Larger than the whole budget: computed, not kept.
    get("huge", 2_000);
    assert_eq!(get("huge", 2_000), Outcome::Miss);
    assert_eq!(get("big", 900), Outcome::Hit);
    assert!(lru.stats().bytes <= 1_000);
}

#[test]
fn identical_misses_compute_once() {
    let (index, _) = index_with(10, 1);
    let g = generation(&index);
    let lru: Lru<u32> = Lru::new(CacheConfig::default());
    let k = key("q", &g);
    let computes = std::sync::atomic::AtomicUsize::new(0);
    const FOLLOWERS: usize = 3;
    let outcomes: Vec<Outcome> = std::thread::scope(|s| {
        let lead = s.spawn(|| {
            lru.get_or_compute(k.clone(), SEEN, true, &CancelToken::new(), |key| {
                computes.fetch_add(1, Ordering::Relaxed);
                // Hold the flight until every follower waits on it: the
                // map and the leader hold it, plus one per follower.
                let start = Instant::now();
                while lock(&lru.inner)
                    .flights
                    .get(key)
                    .is_none_or(|f| Arc::strong_count(f) < 2 + FOLLOWERS)
                {
                    assert!(start.elapsed() < Duration::from_secs(60));
                    std::thread::yield_now();
                }
                computed(7, key, 10)
            })
            .unwrap()
        });
        // Followers start once the flight exists.
        while lock(&lru.inner).flights.is_empty() && !lead.is_finished() {
            std::thread::yield_now();
        }
        let followers: Vec<_> = (0..FOLLOWERS)
            .map(|_| {
                s.spawn(|| {
                    lru.get_or_compute(k.clone(), SEEN, true, &CancelToken::new(), |key| {
                        computes.fetch_add(1, Ordering::Relaxed);
                        computed(8, key, 10)
                    })
                    .unwrap()
                })
            })
            .collect();
        let mut out = vec![lead.join().unwrap()];
        out.extend(followers.into_iter().map(|f| f.join().unwrap()));
        assert!(out.iter().all(|(v, _)| *v == 7), "{out:?}");
        out.into_iter().map(|(_, o)| o).collect()
    });
    assert_eq!(computes.load(Ordering::Relaxed), 1);
    assert_eq!(outcomes[0], Outcome::Miss);
    assert!(outcomes[1..].iter().all(|o| *o == Outcome::Joined));
    assert_eq!(lru.stats().joined, FOLLOWERS as u64);
    assert!(lock(&lru.inner).flights.is_empty());
}

#[test]
fn a_failed_leader_lets_the_waiters_compute() {
    let (index, _) = index_with(10, 1);
    let g = generation(&index);
    let lru: Lru<u32> = Lru::new(CacheConfig::default());
    let k = key("q", &g);
    std::thread::scope(|s| {
        let lead = s.spawn(|| {
            lru.get_or_compute(k.clone(), SEEN, true, &CancelToken::new(), |key| {
                let start = Instant::now();
                while lock(&lru.inner)
                    .flights
                    .get(key)
                    .is_none_or(|f| Arc::strong_count(f) < 3)
                {
                    assert!(start.elapsed() < Duration::from_secs(60));
                    std::thread::yield_now();
                }
                Err::<Computed<u32>, _>(Error::Cancelled)
            })
        });
        while lock(&lru.inner).flights.is_empty() && !lead.is_finished() {
            std::thread::yield_now();
        }
        let follower = s.spawn(|| {
            lru.get_or_compute(k.clone(), SEEN, true, &CancelToken::new(), |key| {
                computed(9, key, 10)
            })
        });
        assert!(matches!(lead.join().unwrap(), Err(Error::Cancelled)));
        assert_eq!(follower.join().unwrap().unwrap(), (9, Outcome::Miss));
    });
    // And a waiter whose own token stops gives up.
    let stopped = CancelToken::new();
    stopped.cancel();
    let flight: Arc<Flight<u32>> = Arc::new(Flight {
        done: Mutex::new(None),
        ready: Condvar::new(),
    });
    assert!(matches!(flight.wait(&stopped), Err(Error::Cancelled)));
}

#[test]
fn entries_go_after_their_ttl_and_when_swept_or_cleared() {
    let (index, _) = index_with(10, 1);
    let g = generation(&index);
    let lru: Lru<u32> = Lru::new(CacheConfig {
        ttl: Duration::ZERO,
        ..CacheConfig::default()
    });
    let c = CancelToken::new();
    let get = |w: &str| {
        lru.get_or_compute(key(w, &g), SEEN, true, &c, |k| computed(0, k, 10))
            .unwrap()
            .1
    };
    assert_eq!(get("a"), Outcome::Miss);
    assert_eq!(get("a"), Outcome::Miss);
    lru.sweep(&g);
    assert_eq!(lru.stats().entries, 0);

    let lru: Lru<u32> = Lru::new(CacheConfig::default());
    let get = |w: &str, at: &Generation| {
        lru.get_or_compute(key(w, at), SEEN, true, &c, |k| computed(0, k, 10))
            .unwrap()
            .1
    };
    get("a", &g);
    get("b", &g);
    lru.sweep(&g);
    assert_eq!(lru.stats().entries, 2, "current and fresh: kept");
    append(&index, 11..12, 11);
    lru.sweep(&generation(&index));
    assert_eq!(lru.stats().entries, 0, "another generation serves");
    get("a", &g);
    lru.clear();
    assert_eq!(get("a", &g), Outcome::Miss);
}

#[test]
fn turned_off_it_keeps_nothing() {
    let (index, _) = index_with(10, 1);
    let g = generation(&index);
    let lru: Lru<u32> = Lru::new(CacheConfig {
        enabled: false,
        ..CacheConfig::default()
    });
    let c = CancelToken::new();
    for _ in 0..2 {
        let got = lru.get_or_compute(key("a", &g), SEEN, true, &c, |k| computed(1, k, 10));
        assert_eq!(got.unwrap(), (1, Outcome::Miss));
    }
    assert_eq!(lru.stats().entries, 0);
}

#[test]
fn a_cached_first_page_is_the_leases_and_page_two_reads_its_snapshot() {
    let (index, _) = index_with(300, 2);
    let index = Arc::new(index);
    let cache = ResultCache::default();
    let node = crate::parse("common").unwrap();
    let spec = SearchSpec {
        node: &node,
        scope: "v",
        sort: Sort::Relevance,
        limit: 20,
        now: NOW,
        utc_offset: 0,
    };
    let c = CancelToken::new();
    let (first, o) = cache.search(&index, &spec, &c).unwrap();
    assert_eq!(o, Outcome::Miss);
    let (again, o) = cache.search(&index, &spec, &c).unwrap();
    assert_eq!(o, Outcome::Hit);
    assert!(Arc::ptr_eq(&first, &again));
    let fresh = Lease::new(index.clone(), &node, NOW, Sort::Relevance, &c).unwrap();
    assert_eq!(first.page, fresh.page(Start::Offset(0), 20, &c).unwrap());
    // New torrents that match land; page 2 of the cached lease still
    // reads page 1's snapshot.
    append(&index, 301..400, 400);
    let next = first.page.next.expect("more");
    let page2 = first.lease.page(Start::After(next), 20, &c).unwrap();
    assert_eq!(page2, fresh.page(Start::Offset(20), 20, &c).unwrap());
    // And the search itself now misses, with the new torrents counted.
    let (now, o) = cache.search(&index, &spec, &c).unwrap();
    assert_eq!(o, Outcome::Miss);
    assert!(now.lease.total().value > first.lease.total().value);
}

#[test]
fn relative_dates_are_keyed_by_the_second() {
    let (index, _) = index_with(50, 1);
    let index = Arc::new(index);
    let cache = ResultCache::default();
    let node = crate::parse("discovered:<7d").unwrap();
    let c = CancelToken::new();
    let at = |now| SearchSpec {
        node: &node,
        scope: "",
        sort: Sort::Relevance,
        limit: 10,
        now,
        utc_offset: 0,
    };
    assert_eq!(cache.search(&index, &at(NOW), &c).unwrap().1, Outcome::Miss);
    assert_eq!(cache.search(&index, &at(NOW), &c).unwrap().1, Outcome::Hit);
    assert_eq!(
        cache.search(&index, &at(NOW + 1), &c).unwrap().1,
        Outcome::Miss
    );
    // Without relative dates, `now` isn't in the key.
    let plain = crate::parse("common").unwrap();
    let spec = |now| SearchSpec {
        node: &plain,
        ..at(now)
    };
    cache.search(&index, &spec(NOW), &c).unwrap();
    assert_eq!(
        cache.search(&index, &spec(NOW + 9), &c).unwrap().1,
        Outcome::Hit
    );
}

#[test]
fn time_of_day_is_keyed_by_the_zone() {
    let (index, _) = index_with(200, 1);
    let index = Arc::new(index);
    let cache = ResultCache::default();
    let node = crate::parse("time:9..17").unwrap();
    let c = CancelToken::new();
    let at = |utc_offset| SearchSpec {
        node: &node,
        scope: "",
        sort: Sort::Relevance,
        limit: 10,
        now: NOW,
        utc_offset,
    };
    assert_eq!(cache.search(&index, &at(0), &c).unwrap().1, Outcome::Miss);
    assert_eq!(cache.search(&index, &at(0), &c).unwrap().1, Outcome::Hit);
    let (east, o) = cache.search(&index, &at(5 * 3600), &c).unwrap();
    assert_eq!(o, Outcome::Miss, "another zone is another result");
    let fresh = Lease::new_at(index.clone(), &node, NOW, 5 * 3600, Sort::Relevance, &c).unwrap();
    assert_eq!(east.lease.total(), fresh.total());
    assert_eq!(east.page, fresh.page(Start::Offset(0), 10, &c).unwrap());
    // Without `time:`, the zone isn't in the key.
    let plain = crate::parse("common").unwrap();
    let spec = |utc_offset| SearchSpec {
        node: &plain,
        ..at(utc_offset)
    };
    cache.search(&index, &spec(0), &c).unwrap();
    assert_eq!(
        cache.search(&index, &spec(3600), &c).unwrap().1,
        Outcome::Hit
    );
}

#[test]
fn results_that_read_hot_columns_last_until_their_epoch_moves() {
    let (index, _) = index_with(200, 1);
    let hot = Arc::new(lodestar_store::HotColumns::with_epoch_every(Duration::ZERO));
    let swarm = |seeders| lodestar_store::SwarmHot {
        seeders,
        leechers: 1,
        trackers: 1,
        dht: lodestar_store::NEVER,
    };
    for id in 1..=200 {
        hot.set_swarm(id, swarm(id as u32 % 10));
    }
    let index = Arc::new(index.with_hot(hot.clone()));
    let cache = ResultCache::default();
    let c = CancelToken::new();
    let common = crate::parse("common").unwrap();
    let spec = |node, sort| SearchSpec {
        node,
        scope: "",
        sort,
        limit: 10,
        now: NOW,
        utc_offset: 0,
    };
    let by_seeders = spec(&common, Sort::Seeders { desc: true });
    let relevance = spec(&common, Sort::Relevance);
    let by_size = spec(&common, Sort::Size { desc: true });
    let filtered_node = crate::parse("seeders:>=5").unwrap();
    let filtered = spec(&filtered_node, Sort::Size { desc: true });
    for s in [&by_seeders, &relevance, &by_size, &filtered] {
        assert_eq!(cache.search(&index, s, &c).unwrap().1, Outcome::Miss);
        assert_eq!(cache.search(&index, s, &c).unwrap().1, Outcome::Hit);
    }
    let params = FacetParams::default();
    let facet = |query| FacetSpec {
        query,
        node: query,
        filters: &[],
        params: &params,
        scope: "",
        sort: Sort::Relevance,
        now: NOW,
        utc_offset: 0,
    };
    for q in [&common, &filtered_node] {
        assert_eq!(
            cache.facets(&index, &facet(q), &c).unwrap().1,
            Outcome::Miss
        );
        assert_eq!(cache.facets(&index, &facet(q), &c).unwrap().1, Outcome::Hit);
    }

    // A scrape: one torrent jumps to the top, with no index commit.
    let top = cache.search(&index, &by_seeders, &c).unwrap().0;
    let new_top = top.page.hits.last().unwrap().id;
    hot.set_swarm(new_top, swarm(1_000));
    let (moved, o) = cache.search(&index, &by_seeders, &c).unwrap();
    assert_eq!(o, Outcome::Miss, "a hot sort reads the new counts");
    assert_eq!(moved.page.hits[0].id, new_top);
    assert_eq!(
        cache.search(&index, &relevance, &c).unwrap().1,
        Outcome::Miss,
        "relevance ranks with the swarm priors"
    );
    assert_eq!(
        cache.search(&index, &filtered, &c).unwrap().1,
        Outcome::Miss,
        "a swarm filter reads them"
    );
    assert_eq!(
        cache.search(&index, &by_size, &c).unwrap().1,
        Outcome::Hit,
        "a field sort without swarm clauses doesn't"
    );
    assert_eq!(
        cache.facets(&index, &facet(&filtered_node), &c).unwrap().1,
        Outcome::Miss
    );
    assert_eq!(
        cache.facets(&index, &facet(&common), &c).unwrap().1,
        Outcome::Hit,
        "counts without swarm clauses don't depend on the columns"
    );
}

#[test]
fn an_older_leases_facets_never_retire_the_serving_generations() {
    let (index, _) = index_with(100, 1);
    let index = Arc::new(index);
    let cache = ResultCache::default();
    let node = crate::parse("common").unwrap();
    let params = FacetParams::default();
    let spec = FacetSpec {
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
    let old = Arc::new(Lease::new(index.clone(), &node, NOW, Sort::Relevance, &c).unwrap());
    append(&index, 101..110, 110);
    assert_eq!(cache.facets(&index, &spec, &c).unwrap().1, Outcome::Miss);
    assert_eq!(cache.facets(&index, &spec, &c).unwrap().1, Outcome::Hit);
    // The lease from before the commit: counted on its own snapshot,
    // without the cache, twice.
    for _ in 0..2 {
        let (got, o) = cache.facets_on(&index, &old, &spec, &c).unwrap();
        assert_eq!(o, Outcome::Miss);
        assert_eq!(got.total, old.total());
    }
    // The serving generation's entry is still there.
    assert_eq!(cache.facets(&index, &spec, &c).unwrap().1, Outcome::Hit);
    assert_eq!(cache.stats().facets.retired, 0);
    // A lease of the serving snapshot shares it.
    let now = Arc::new(Lease::new(index.clone(), &node, NOW, Sort::Relevance, &c).unwrap());
    assert_eq!(
        cache.facets_on(&index, &now, &spec, &c).unwrap().1,
        Outcome::Hit
    );
    // And a lookup on a searcher older than the latest one seen doesn't
    // move the cache back to it.
    let stale = Seen::of(&index, &old.searcher().clone());
    let latest = Seen::of(&index, &index.searcher());
    assert!(stale.before(&latest) && !latest.before(&stale));
}

#[test]
fn a_search_with_a_file_clause_isnt_kept() {
    let (index, _) = index_with(20, 1);
    let index = Arc::new(index);
    let cache = ResultCache::default();
    let node = crate::parse("common file:(ext:mkv)").unwrap();
    let spec = SearchSpec {
        node: &node,
        scope: "",
        sort: Sort::Relevance,
        limit: 10,
        now: NOW,
        utc_offset: 0,
    };
    let c = CancelToken::new();
    for _ in 0..2 {
        assert_eq!(cache.search(&index, &spec, &c).unwrap().1, Outcome::Miss);
    }
    assert_eq!(cache.stats().search.entries, 0);
}
