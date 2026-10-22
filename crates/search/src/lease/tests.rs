use std::collections::HashSet;
use std::time::{Duration, Instant};

use proptest::prelude::*;

use super::*;
use crate::testing::{append, delete, index_with, slow};

const NOW: i64 = 1_700_000_100;

fn lease(index: &Arc<SearchIndex>, q: &str, sort: Sort) -> Lease {
    let node = crate::parse(q).unwrap();
    Lease::new(index.clone(), &node, NOW, sort, &CancelToken::new()).unwrap()
}

fn ids(page: &Page) -> Vec<u64> {
    page.hits.iter().map(|h| h.id).collect()
}

/// Every hit, a cursor page at a time; `between` runs after each page.
fn scroll(l: &Lease, size: usize, mut between: impl FnMut(usize)) -> Vec<u64> {
    let cancel = CancelToken::new();
    let mut out = Vec::new();
    let mut page = l.page(Start::Offset(0), size, &cancel).unwrap();
    let mut n = 0;
    loop {
        out.extend(ids(&page));
        between(n);
        n += 1;
        let Some(next) = page.next else { break };
        assert!(l.fits(&next));
        page = l.page(Start::After(next), size, &cancel).unwrap();
        assert!(n < 100_000, "paging doesn't end");
    }
    out
}

/// Every hit, an offset page at a time.
fn by_offset(l: &Lease, size: usize) -> Vec<u64> {
    let cancel = CancelToken::new();
    let mut out = Vec::new();
    loop {
        let page = l.page(Start::Offset(out.len()), size, &cancel).unwrap();
        let more = page.next.is_some();
        out.extend(ids(&page));
        if !more {
            return out;
        }
    }
}

const SORTS: [Sort; 7] = [
    Sort::Relevance,
    Sort::Discovered { desc: true },
    Sort::Discovered { desc: false },
    Sort::Size { desc: true },
    Sort::Size { desc: false },
    Sort::Files { desc: true },
    Sort::Files { desc: false },
];

/// Plain relevance (a browse, a fielded word), two-phase relevance
/// (unfielded words) and filters.
const QUERIES: [&str; 5] = [
    "",
    "common",
    "name:half",
    "half size:>1100",
    "common -recent",
];

#[test]
fn paging_through_a_lease_while_commits_land_has_no_duplicates_or_gaps() {
    let n = 1_500;
    for q in QUERIES {
        for sort in SORTS {
            let (index, _) = index_with(n, 4);
            let index = Arc::new(index);
            let mut next_id = n + 1;
            let mut deleted = HashSet::new();
            let l = lease(&index, q, sort);
            let want = by_offset(&l, 500);
            assert_eq!(want.len(), l.total().value, "{q} {sort:?}");
            // Pages of 37 (not a divisor of the 200-candidate windows),
            // with torrents added and deleted between them.
            let got = scroll(&l, 37, |page| {
                if page % 3 == 0 {
                    append(&index, next_id..next_id + 40, n);
                    next_id += 40;
                }
                if page % 4 == 1 {
                    let ids: Vec<u64> = (1..=n).filter(|id| id % 13 == page as u64 % 13).collect();
                    deleted.extend(ids.iter().copied());
                    delete(&index, ids);
                }
            });
            let unique: HashSet<u64> = got.iter().copied().collect();
            assert_eq!(unique.len(), got.len(), "{q} {sort:?}: a duplicate");
            // The same hits in the same order as offset paging before
            // anything changed: nothing skipped, nothing moved.
            assert_eq!(got, want, "{q} {sort:?}");
            assert_eq!(l.total().value, want.len(), "{q} {sort:?}: the total moved");
            // And a new lease sees the changes.
            let fresh = lease(&index, "", Sort::Relevance);
            let alive = (next_id - 1) as usize - deleted.len();
            assert_eq!(fresh.total().value, alive, "{q} {sort:?}");
        }
    }
}

#[test]
fn offset_pages_and_cursor_pages_agree_at_every_size() {
    let (index, _) = index_with(700, 3);
    let index = Arc::new(index);
    for q in ["common", "", "half"] {
        for sort in [Sort::Relevance, Sort::Size { desc: false }] {
            let l = lease(&index, q, sort);
            let all = by_offset(&l, 1_000);
            for size in [1, 7, 50, 199, 200, 201] {
                assert_eq!(
                    scroll(&l, size, |_| {}),
                    all,
                    "{q} {sort:?} pages of {size}"
                );
                assert_eq!(by_offset(&l, size), all, "{q} {sort:?} offsets of {size}");
            }
        }
    }
}

#[test]
fn two_phase_order_is_exact_within_each_window() {
    // Within a window of 200 phase-1 candidates, hits come in re-ranked
    // order; the windows themselves come in phase-1 order.
    let (index, _) = index_with(900, 2);
    let index = Arc::new(index);
    let l = lease(&index, "common", Sort::Relevance);
    assert!(matches!(l.mode(), Mode::TwoPhase));
    let cancel = CancelToken::new();
    let page = l.page(Start::Offset(0), 200, &cancel).unwrap();
    let scores: Vec<f32> = page.hits.iter().map(|h| h.score).collect();
    assert!(scores.windows(2).all(|w| w[0] >= w[1]), "{scores:?}");
    let page = l.page(Start::Offset(200), 200, &cancel).unwrap();
    let scores: Vec<f32> = page.hits.iter().map(|h| h.score).collect();
    assert!(scores.windows(2).all(|w| w[0] >= w[1]), "{scores:?}");
}

#[test]
fn a_swap_of_the_indexes_never_invalidates_a_lease() {
    // The lease holds its index; dropping every other handle (as a swap
    // does) leaves it working and the index alive.
    let (index, _) = index_with(300, 2);
    let index = Arc::new(index);
    let holders = index.holders();
    let l = lease(&index, "", Sort::Relevance);
    let want = by_offset(&l, 1_000);
    drop(index);
    assert!(holders.strong_count() > 0, "the lease keeps the index");
    assert_eq!(scroll(&l, 64, |_| {}), want);
    drop(l);
    assert_eq!(holders.strong_count(), 0);
}

#[test]
fn a_cursor_of_another_mode_is_refused() {
    let (index, _) = index_with(100, 1);
    let index = Arc::new(index);
    let l = lease(&index, "common", Sort::Size { desc: true });
    let wrong = Position::Score {
        score: 1.0,
        addr: DocAddress::new(0, 0),
    };
    assert!(!l.fits(&wrong));
    assert!(matches!(
        l.page(Start::After(wrong), 10, &CancelToken::new()),
        Err(Error::Corrupt(_))
    ));
}

fn slow_lease(index: &Arc<SearchIndex>, sort: Sort, delay: Duration) -> Lease {
    Lease {
        index: index.clone(),
        searcher: index.searcher(),
        compiled: slow(index, delay),
        stemmed: false,
        sort,
        total: Mutex::new(Total {
            value: 0,
            approx: true,
        }),
    }
}

/// Runs `f` with a token cancelled after 30 ms; returns its result and
/// how long it took.
fn cancelled_after_30ms<T>(f: impl FnOnce(&CancelToken) -> T) -> (T, Duration) {
    let cancel = CancelToken::new();
    let c = cancel.clone();
    let stopper = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(30));
        c.cancel();
    });
    let start = Instant::now();
    let r = f(&cancel);
    let took = start.elapsed();
    stopper.join().unwrap();
    (r, took)
}

#[test]
fn a_cancelled_search_stops_early() {
    // 50,000 documents at 20 µs each: a full scan takes a second.
    let (index, _) = index_with(50_000, 2);
    let index = Arc::new(index);
    let delay = Duration::from_micros(20);
    let after_field = Position::Field {
        present: true,
        key: u64::MAX,
        addr: DocAddress::new(0, 0),
    };
    let cases: [(Sort, Start); 3] = [
        (Sort::Relevance, Start::Offset(0)),
        (Sort::Size { desc: true }, Start::Offset(0)),
        (Sort::Size { desc: true }, Start::After(after_field)),
    ];
    for (sort, start) in cases {
        let l = slow_lease(&index, sort, delay);
        let (r, took) = cancelled_after_30ms(|c| l.page(start, 50, c));
        assert!(
            matches!(r, Err(Error::Cancelled)),
            "{sort:?} {start:?}: {r:?}"
        );
        assert!(
            took < Duration::from_millis(400),
            "{sort:?} {start:?}: took {took:?}"
        );
        // Cancelled before it starts, it does nothing.
        let done = CancelToken::new();
        done.cancel();
        assert!(matches!(l.page(start, 50, &done), Err(Error::Cancelled)));
    }
}

#[test]
fn the_exact_count_on_demand_is_true_and_kept_or_gives_way_to_the_budget() {
    let (index, names) = index_with(3_000, 3);
    let index = Arc::new(index);
    let l = lease(&index, "half", Sort::Relevance);
    // Pretend the count was estimated.
    *lock(&l.total) = Total {
        value: 1,
        approx: true,
    };
    let t = l.exact_total(&CancelToken::new()).unwrap();
    let want = names.iter().filter(|n| n.contains("Half")).count();
    assert_eq!(
        t,
        Total {
            value: want,
            approx: false
        }
    );
    assert_eq!(l.total(), t, "kept for later pages");
    // Out of time, the estimate stands; cancelled, it's an error.
    // 20,000 documents at 50 µs each: a second for all of them.
    let (big, _) = index_with(20_000, 1);
    let slowly = slow_lease(&Arc::new(big), Sort::Relevance, Duration::from_micros(50));
    let t = slowly
        .exact_total_within(&CancelToken::new(), Duration::from_millis(20))
        .unwrap();
    assert!(t.approx);
    let (r, _) = cancelled_after_30ms(|c| slowly.exact_total(c));
    assert!(matches!(r, Err(Error::Cancelled)), "{r:?}");
}

fn tiny(ttl_ms: u64, max_leases: usize, per_client: usize) -> Leases {
    Leases::new(LeaseConfig {
        ttl: Duration::from_millis(ttl_ms),
        max_age: Duration::from_secs(3600),
        max_leases,
        per_client,
    })
}

#[test]
fn leases_expire_after_their_last_use() {
    // On an explicit clock: sleeps are too coarse on a loaded runner.
    let (index, _) = index_with(10, 1);
    let index = Arc::new(index);
    let ms = |n: u64| Duration::from_millis(n);
    let t0 = Instant::now();
    let leases = tiny(150, 10, 10);
    let id = leases.insert_at(Arc::new(lease(&index, "", Sort::Relevance)), 1, t0);
    // Used within the ttl, again and again: still there, past one ttl.
    for k in 1..=4 {
        leases.get_at(&id, 1, t0 + ms(100 * k)).unwrap();
    }
    // 149 ms after the last use it's there; 150 ms after, it's gone.
    leases.get_at(&id, 1, t0 + ms(549)).unwrap();
    let gone = t0 + ms(549 + 150);
    assert!(matches!(
        leases.get_at(&id, 1, gone),
        Err(Error::LeaseExpired)
    ));
    assert_eq!(leases.sweep_at(gone), 0);
    // And at most `max_age` after it was taken, however busy.
    let aged = Leases::new(LeaseConfig {
        max_age: ms(100),
        ..LeaseConfig::default()
    });
    let id = aged.insert_at(Arc::new(lease(&index, "", Sort::Relevance)), 1, t0);
    aged.get_at(&id, 1, t0 + ms(40)).unwrap();
    aged.get_at(&id, 1, t0 + ms(99)).unwrap();
    assert!(matches!(
        aged.get_at(&id, 1, t0 + ms(100)),
        Err(Error::LeaseExpired)
    ));
    // A sweep drops the expired, not the live.
    let live = aged.insert_at(
        Arc::new(lease(&index, "", Sort::Relevance)),
        1,
        t0 + ms(100),
    );
    assert_eq!(aged.sweep_at(t0 + ms(150)), 1);
    aged.get_at(&live, 1, t0 + ms(150)).unwrap();
}

#[test]
fn unknown_and_other_clients_leases_are_expired() {
    let (index, _) = index_with(10, 1);
    let leases = Leases::default();
    let id = leases.insert(Arc::new(lease(&Arc::new(index), "", Sort::Relevance)), 7);
    assert!(matches!(
        leases.get(&LeaseId([0; 16]), 7),
        Err(Error::LeaseExpired)
    ));
    assert!(matches!(leases.get(&id, 8), Err(Error::LeaseExpired)));
    // Someone else asking doesn't evict it.
    leases.get(&id, 7).unwrap();
}

#[test]
fn past_the_caps_the_least_recently_used_go() {
    let (index, _) = index_with(10, 1);
    let index = Arc::new(index);
    let new = || Arc::new(lease(&index, "", Sort::Relevance));
    let t0 = Instant::now();
    let at = |n: u64| t0 + Duration::from_millis(n);
    // Per client: 3.
    let leases = tiny(60_000, 100, 3);
    let a: Vec<LeaseId> = (0..3).map(|k| leases.insert_at(new(), 1, at(k))).collect();
    leases.get_at(&a[0], 1, at(10)).unwrap(); // a[1] is now the least recent
    let other = leases.insert_at(new(), 2, at(11));
    leases.insert_at(new(), 1, at(12));
    assert!(matches!(
        leases.get_at(&a[1], 1, at(13)),
        Err(Error::LeaseExpired)
    ));
    for id in [a[0], a[2]] {
        leases.get_at(&id, 1, at(14)).unwrap();
    }
    leases.get_at(&other, 2, at(14)).unwrap();
    assert_eq!(leases.len(), 4);
    // In all: 4.
    let leases = tiny(60_000, 4, 100);
    let ids: Vec<LeaseId> = (0..4).map(|c| leases.insert_at(new(), c, at(c))).collect();
    leases.insert_at(new(), 9, at(5));
    assert_eq!(leases.len(), 4);
    assert!(matches!(
        leases.get_at(&ids[0], 0, at(6)),
        Err(Error::LeaseExpired)
    ));
    leases.get_at(&ids[1], 1, at(6)).unwrap();
}

#[test]
fn lease_ids_are_distinct() {
    let leases = Leases::default();
    let ids: HashSet<LeaseId> = (0..10_000).map(|_| leases.new_id()).collect();
    assert_eq!(ids.len(), 10_000);
    assert!(!ids.contains(&LeaseId([0; 16])));
}

#[derive(Debug, Clone)]
enum Op {
    Page(usize),
    Add(u64),
    Delete(u64),
}

fn op() -> impl Strategy<Value = Op> {
    prop_oneof![
        3 => (1usize..90).prop_map(Op::Page),
        1 => (1u64..120).prop_map(Op::Add),
        1 => (2u64..9).prop_map(Op::Delete),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(24))]

    /// Random interleavings of commits (adds, deletes) and cursor pages
    /// of random sizes never yield a duplicate or skip an id.
    #[test]
    fn random_commit_and_scroll_interleavings_never_duplicate_or_skip(
        n in 50u64..700,
        q in 0usize..QUERIES.len(),
        sort in 0usize..SORTS.len(),
        ops in proptest::collection::vec(op(), 1..60),
    ) {
        let (index, _) = index_with(n, 3);
        let index = Arc::new(index);
        let l = lease(&index, QUERIES[q], SORTS[sort]);
        let want = by_offset(&l, 500);
        let cancel = CancelToken::new();
        let mut got = Vec::new();
        let mut next = Some(Start::Offset(0));
        let mut next_id = n + 1;
        let mut ops = ops;
        ops.push(Op::Page(25));
        let mut ops = ops.into_iter().cycle();
        while let Some(start) = next {
            match ops.next().unwrap() {
                Op::Page(size) => {
                    let page = l.page(start, size, &cancel).unwrap();
                    got.extend(ids(&page));
                    next = page.next.map(Start::After);
                }
                Op::Add(k) => {
                    append(&index, next_id..next_id + k, n);
                    next_id += k;
                }
                Op::Delete(m) => delete(&index, (1..next_id).filter(|id| id % m == 0)),
            }
        }
        let unique: HashSet<u64> = got.iter().copied().collect();
        prop_assert_eq!(unique.len(), got.len(), "a duplicate");
        prop_assert_eq!(got, want);
    }
}
