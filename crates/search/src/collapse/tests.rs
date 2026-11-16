use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use proptest::prelude::*;
use tantivy::TantivyDocument;
use tantivy::indexer::NoMergePolicy;

use super::*;
use crate::SearchIndex;
use crate::index::Sort;
use crate::lease::Page;
use crate::population::Generations;

const NOW: i64 = 1_700_000_100;

/// Torrent `id`'s group in the fixtures: every 11th has no key (a group
/// of its own); the rest fall into `groups` groups, unevenly (a group's
/// members spread over the whole id range, so over segments too).
fn group_of(id: u64, groups: u64) -> Option<u64> {
    if id.is_multiple_of(11) {
        None
    } else {
        // Squared, so low groups are big and high ones small.
        let g = (id * 7919 % (groups * groups)).isqrt();
        Some(0xfeed_0000 + g)
    }
}

/// Torrent `id`'s name: every one has `Common` and its group's word;
/// some carry more words, so relevance scores differ within a group.
fn name_of(id: u64, groups: u64) -> String {
    let mut words = vec![format!(
        "Show{}",
        group_of(id, groups).map_or(0, |g| g & 0xffff)
    )];
    words.push("Common".into());
    for extra in 0..(id % 4) {
        words.push(format!("Pad{extra}"));
    }
    if id.is_multiple_of(3) {
        words.push("Common".into());
    }
    words.push(format!("v{id}"));
    words.join(".")
}

/// Adds torrents `ids` in one new segment, with their group in both
/// `group_key` and `title_identity` (as GS-2b writes them).
fn append(index: &SearchIndex, ids: std::ops::Range<u64>, groups: u64) {
    let f = *index.fields();
    let (raw, reader) = index.raw();
    let mut w: tantivy::IndexWriter = raw.writer_with_num_threads(1, 15 << 20).unwrap();
    w.set_merge_policy(Box::new(NoMergePolicy));
    for id in ids {
        let name = name_of(id, groups);
        let mut d = TantivyDocument::new();
        d.add_u64(f.id, id);
        d.add_text(f.name, &name);
        d.add_text(f.text, &name);
        // Sizes repeat, so the field sorts have ties to break.
        d.add_u64(f.size, 1000 + id % 97);
        d.add_u64(f.files, 1 + id % 7);
        d.add_i64(f.discovered, 1_700_000_000 + id as i64);
        if let Some(g) = group_of(id, groups) {
            d.add_u64(f.group_key, g);
            d.add_u64(f.title_identity, g);
        }
        w.add_document(d).unwrap();
    }
    w.commit().unwrap();
    reader.reload().unwrap();
}

/// `n` torrents in `segments` segments, `groups` groups.
fn fixture(n: u64, segments: u64, groups: u64) -> Arc<SearchIndex> {
    let index = SearchIndex::in_memory().unwrap();
    let per = n.div_ceil(segments.max(1)).max(1);
    let mut from = 1;
    while from <= n {
        let to = (from + per).min(n + 1);
        append(&index, from..to, groups);
        from = to;
    }
    Arc::new(index)
}

fn lease(index: &Arc<SearchIndex>, q: &str, sort: Sort) -> Lease {
    let node = crate::parse(q).unwrap();
    Lease::new(index.clone(), &node, NOW, sort, &CancelToken::new()).unwrap()
}

/// Every hit uncollapsed, in the lease's order.
fn uncollapsed(l: &Lease) -> Vec<u64> {
    let cancel = CancelToken::new();
    let mut out = Vec::new();
    let mut page: Page = l.page(Start::Offset(0), 37, &cancel).unwrap();
    loop {
        out.extend(page.hits.iter().map(|h| h.id));
        let Some(next) = page.next else { return out };
        page = l.page(Start::After(next), 37, &cancel).unwrap();
    }
}

/// What a collapsed walk must give: each group's first hit in the
/// uncollapsed order, with its other members' number.
fn expected(all: &[u64], groups: u64) -> Vec<(u64, u64)> {
    let mut size: HashMap<u64, u64> = HashMap::new();
    for id in all {
        if let Some(g) = group_of(*id, groups) {
            *size.entry(g).or_default() += 1;
        }
    }
    let mut seen = HashSet::new();
    all.iter()
        .filter_map(|&id| match group_of(id, groups) {
            None => Some((id, 0)),
            Some(g) => seen.insert(g).then(|| (id, size[&g] - 1)),
        })
        .collect()
}

/// Every row, a cursor page of `size` at a time; `between` runs after
/// each page.
fn scroll(l: &Lease, size: usize, mut between: impl FnMut()) -> Vec<(u64, u64)> {
    let cancel = CancelToken::new();
    let mut out = Vec::new();
    let mut page = super::page(l, Start::Offset(0), size, &cancel).unwrap();
    let mut n = 0;
    loop {
        assert!(page.rows.len() <= size);
        out.extend(page.rows.iter().map(|r| (r.hit.id, r.others)));
        between();
        let Some(next) = page.next else { return out };
        assert!(
            l.fits(&next),
            "a collapsed cursor is a position of the lease"
        );
        page = super::page(l, Start::After(next), size, &cancel).unwrap();
        n += 1;
        assert!(n < 10_000, "paging doesn't end");
    }
}

/// Every row, an offset page at a time.
fn by_offset(l: &Lease, size: usize) -> Vec<(u64, u64)> {
    let cancel = CancelToken::new();
    let mut out = Vec::new();
    loop {
        let page = super::page(l, Start::Offset(out.len()), size, &cancel).unwrap();
        out.extend(page.rows.iter().map(|r| (r.hit.id, r.others)));
        if page.next.is_none() {
            return out;
        }
    }
}

const SORTS: [Sort; 5] = [
    Sort::Relevance,
    Sort::Size { desc: true },
    Sort::Size { desc: false },
    Sort::Discovered { desc: true },
    Sort::Files { desc: false },
];

#[test]
fn every_group_once_in_order_with_its_others_whatever_the_page_size() {
    let groups = 40;
    let index = fixture(1_500, 3, groups);
    // "common": two-phase relevance; "": phase-1 order; the field sorts.
    for q in ["common", "", "pad0 OR pad2", "-pad1 common"] {
        for sort in SORTS {
            let l = lease(&index, q, sort);
            available(&l).expect("ready");
            let all = uncollapsed(&l);
            let want = expected(&all, groups);
            assert!(want.len() < all.len(), "{q}: groups actually collapse");
            // `others` + 1 over every row is the torrent count.
            let summed: u64 = want.iter().map(|(_, o)| o + 1).sum();
            assert_eq!(summed as usize, all.len());
            assert_eq!(summed as usize, l.total().value);
            for size in [1, 7, 50, 200] {
                let got = scroll(&l, size, || {});
                assert_eq!(got, want, "{q} {sort:?} cursor pages of {size}");
            }
            assert_eq!(by_offset(&l, 13), want, "{q} {sort:?} offsets");
        }
    }
}

#[test]
fn the_best_version_follows_the_sort_with_a_stable_tie_break() {
    let groups = 10;
    let index = fixture(600, 2, groups);
    let cancel = CancelToken::new();
    let l = lease(&index, "", Sort::Size { desc: true });
    let page = super::page(&l, Start::Offset(0), 200, &cancel).unwrap();
    // Each row is its group's largest member; among equal sizes, the
    // lowest address (the order `sorted.rs` breaks ties in), here the
    // lowest id, since ids were written in order.
    for r in &page.rows {
        let Some(g) = group_of(r.hit.id, groups) else {
            continue;
        };
        let best = (1..=600u64)
            .filter(|&id| group_of(id, groups) == Some(g))
            .max_by_key(|&id| (id % 97, std::cmp::Reverse(id)))
            .unwrap();
        assert_eq!(r.hit.id, best, "group {g:x}");
    }
    // The same rows every time.
    for _ in 0..3 {
        assert_eq!(
            super::page(&l, Start::Offset(0), 200, &cancel).unwrap(),
            page
        );
    }
}

#[test]
fn pages_across_an_nrt_commit_under_one_lease_have_no_duplicates_or_gaps() {
    let groups = 30;
    let index = fixture(900, 2, groups);
    for sort in [Sort::Relevance, Sort::Size { desc: false }] {
        let l = lease(&index, "common", sort);
        let want = expected(&uncollapsed(&l), groups);
        let mut next_id = 10_000 + if sort == Sort::Relevance { 0 } else { 1_000 };
        // A commit after every page: new torrents in new and existing
        // groups, invisible to the lease.
        let got = scroll(&l, 9, || {
            append(&index, next_id..next_id + 25, groups);
            next_id += 25;
        });
        assert_eq!(got, want, "{sort:?}");
        let ids: Vec<u64> = got.iter().map(|(id, _)| *id).collect();
        let distinct: HashSet<u64> = ids.iter().copied().collect();
        assert_eq!(distinct.len(), ids.len());
    }
}

#[test]
fn a_page_is_bounded_by_the_groups_it_decides_and_resumes_after_them() {
    // One big group first in the order, then others: a page from a cursor
    // checks groups served before, and a page that decides more than
    // MAX_SCAN_GROUPS groups ends short with a cursor, never skipping.
    let groups = 3;
    let index = fixture(400, 1, groups);
    let l = lease(&index, "", Sort::Discovered { desc: false });
    let want = expected(&uncollapsed(&l), groups);
    assert_eq!(scroll(&l, 1, || {}), want);
}

#[test]
fn collapsed_and_uncollapsed_pages_share_positions() {
    // The positions a collapsed page hands out are the uncollapsed
    // order's: an uncollapsed page from one continues right after it.
    let groups = 25;
    let index = fixture(700, 2, groups);
    let cancel = CancelToken::new();
    for sort in SORTS {
        let l = lease(&index, "common", sort);
        let all = uncollapsed(&l);
        let page = super::page(&l, Start::Offset(0), 10, &cancel).unwrap();
        let last = page.rows.last().unwrap().hit.id;
        let at = all.iter().position(|id| *id == last).unwrap();
        let rest = l
            .page(Start::After(page.next.unwrap()), 5, &cancel)
            .unwrap();
        let ids: Vec<u64> = rest.hits.iter().map(|h| h.id).collect();
        assert_eq!(ids, all[at + 1..at + 6], "{sort:?}");
    }
}

#[test]
fn page_ranked_gives_page_at_exactly_with_a_position_per_hit() {
    // Uncollapsed search is unchanged: the same hits and next cursor, and
    // a cursor at hit i continues at hit i + 1.
    let index = fixture(800, 3, 20);
    let cancel = CancelToken::new();
    for q in ["common", ""] {
        for sort in SORTS {
            let l = lease(&index, q, sort);
            for start in [Start::Offset(0), Start::Offset(150), Start::Offset(390)] {
                let (page, addrs) = l.page_at(start, 60, &cancel).unwrap();
                let (ranked, raddrs, positions) = l.page_ranked(start, 60, &cancel).unwrap();
                assert_eq!(page, ranked);
                assert_eq!(addrs, raddrs);
                assert_eq!(positions.len(), page.hits.len());
                for i in [0, 17, 58] {
                    let after = l.page(Start::After(positions[i]), 1, &cancel).unwrap();
                    assert_eq!(after.hits[0].id, page.hits[i + 1].id, "{q} {sort:?} {i}");
                }
            }
        }
    }
}

#[test]
fn groups_are_counted_exactly_below_the_threshold() {
    let groups = 40;
    let index = fixture(2_000, 3, groups);
    let cancel = CancelToken::new();
    for q in ["common", "pad1", "-pad0"] {
        let l = lease(&index, q, Sort::Relevance);
        let want = expected(&uncollapsed(&l), groups).len();
        let t = super::groups(&l, false, &cancel).unwrap();
        assert_eq!(
            t,
            Total {
                value: want,
                approx: false
            },
            "{q}"
        );
        // Kept on the lease.
        assert_eq!(l.kept_groups(), Some(t));
    }
    let l = lease(&index, "nothingmatches", Sort::Relevance);
    assert_eq!(super::groups(&l, false, &cancel).unwrap().value, 0);
}

/// The estimate's relative error on a seeded corpus with groups of
/// every size, two ways:
/// - the draw alone: every match visited (no strata slices), so the error
///   is the Horvitz–Thompson draw's (HT_SAMPLE keyed matches);
/// - with the count's slices at a 2,048-document threshold (as the
///   count's own test): that adds the slices' sampling error on the
///   keyless share, which on 40,000 documents is far larger than at the
///   200,000 matches below which nothing is estimated.
///
/// The decisions file states the bounds held here.
#[test]
fn the_groups_estimate_is_within_its_stated_error() {
    let index = fixture(40_000, 2, 300);
    let cancel = CancelToken::new();
    let (mut drawn, mut sliced) = (0f64, 0f64);
    for q in ["common", "pad1", "-pad0", "pad2 OR pad3", "pad0 OR pad1"] {
        let l = lease(&index, q, Sort::Relevance);
        let torrents = l.total();
        let exact = groups_with(&l, torrents, false, u32::MAX, &cancel).unwrap();
        assert!(!exact.approx);
        let err = |sample_above| {
            let est = estimate(&l, torrents.value, sample_above, &cancel).unwrap();
            assert!(est.approx);
            let e = (est.value as f64 - exact.value as f64).abs() / exact.value as f64;
            eprintln!(
                "{q} ({sample_above}): ≈{} vs {} ({e:.4})",
                est.value, exact.value
            );
            e
        };
        drawn = drawn.max(err(u32::MAX));
        sliced = sliced.max(err(2_048));
    }
    eprintln!("worst relative error: drawn {drawn:.4}, with slices {sliced:.4}");
    assert!(drawn < 0.05, "{drawn}");
    assert!(sliced < 0.12, "{sliced}");
}

#[test]
fn an_approximate_torrent_count_gives_an_approximate_groups_count() {
    let index = fixture(5_000, 1, 50);
    let l = lease(&index, "common", Sort::Relevance);
    let cancel = CancelToken::new();
    let t = Total {
        value: l.total().value,
        approx: true,
    };
    let g = groups_with(&l, t, false, 1_024, &cancel).unwrap();
    assert!(g.approx);
    assert!(g.value > 0 && g.value <= t.value);
}

#[test]
fn not_ready_answers_ungrouped_with_a_reason() {
    // `group_key` without a producer (as a build before GS-2b, or a v7
    // index serving during the rebuild, reports it): not Ready, so
    // collapse isn't available; the same for `title_identity`.
    for field in ["group_key", "title_identity"] {
        let index = SearchIndex::in_memory().unwrap();
        append(&index, 1..50, 5);
        let gens = Generations::current().with(field, 0);
        let index = Arc::new(index.with_generations(gens).unwrap());
        assert_eq!(index.availability(field), Availability::NoProducer);
        let l = lease(&index, "common", Sort::Relevance);
        assert_eq!(available(&l), Err(Unavailable::GroupKeyNotReady), "{field}");
    }
    assert_eq!(Unavailable::GroupKeyNotReady.code(), "group_key_not_ready");
    // A fresh index is complete at this build's generations.
    let ready = fixture(10, 1, 5);
    assert_eq!(available(&lease(&ready, "", Sort::Relevance)), Ok(()));
}

#[test]
fn a_cancelled_collapse_stops() {
    let index = fixture(3_000, 2, 30);
    let l = lease(&index, "common", Sort::Relevance);
    let cancel = CancelToken::new();
    cancel.cancel();
    assert!(matches!(
        super::page(&l, Start::Offset(0), 20, &cancel),
        Err(Error::Cancelled)
    ));
    assert!(matches!(
        groups_with(&l, l.total(), false, 1_024, &cancel),
        Err(Error::Cancelled)
    ));
    assert!(matches!(
        estimate(&l, l.total().value, 1_024, &cancel),
        Err(Error::Cancelled)
    ));
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(24))]

    /// Any corpus shape, page size, sort and a commit after any page: the
    /// groups served are exactly the distinct groups of a full scan, each
    /// once, in order (no repeats, no gaps).
    #[test]
    fn walking_every_page_serves_each_group_once(
        n in 50u64..600,
        segments in 1u64..4,
        groups in 1u64..30,
        size in 1usize..40,
        sort in 0usize..SORTS.len(),
        q in 0usize..3,
        commit_after in 0usize..5,
    ) {
        let index = fixture(n, segments, groups);
        let q = ["common", "", "pad1 OR v7"][q];
        let l = lease(&index, q, SORTS[sort]);
        let want = expected(&uncollapsed(&l), groups);
        let mut pages = 0;
        let mut next_id = 100_000;
        let got = scroll(&l, size, || {
            if pages == commit_after {
                append(&index, next_id..next_id + 10, groups);
                next_id += 10;
            }
            pages += 1;
        });
        prop_assert_eq!(got, want);
    }
}
