//! The query language end to end: text → AST → Tantivy over a small
//! corpus.

use std::sync::Arc;
use std::time::Duration;

use lodestar_bencode::{OwnedValue, to_vec};
use lodestar_metainfo::Info;
use lodestar_search::{SearchIndex, Sort, parse};
use lodestar_store::{HotColumns, NewTorrent, SourceId, Store, StoreConfig, SwarmHot};

const NOW: i64 = 1_767_225_600 + 40 * 86_400; // 2026-02-10
const DAY: i64 = 86_400;

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

struct T {
    name: &'static str,
    files: &'static [(&'static str, i64)],
    private: bool,
    age_days: i64,
}

fn info(t: &T) -> Vec<u8> {
    let files = t
        .files
        .iter()
        .map(|(p, len)| {
            d(vec![
                ("length", OwnedValue::Int(*len)),
                ("path", OwnedValue::List(p.split('/').map(b).collect())),
            ])
        })
        .collect();
    let mut e = vec![
        ("files", OwnedValue::List(files)),
        ("name", b(t.name)),
        ("piece length", OwnedValue::Int(1 << 20)),
        ("pieces", OwnedValue::Bytes(vec![0; 20])),
    ];
    if t.private {
        e.push(("private", OwnedValue::Int(1)));
    }
    to_vec(&d(e))
}

const GB: i64 = 1 << 30;
const MB: i64 = 1 << 20;

fn corpus() -> (SearchIndex, Vec<String>) {
    let ts = [
        T {
            name: "The.Dark.Knight.2008.1080p.BluRay.x264",
            files: &[("The.Dark.Knight.2008.mkv", 8 * GB)],
            private: false,
            age_days: 1,
        },
        T {
            name: "Batman Begins 2005 720p",
            files: &[("Batman.Begins.mkv", 4 * GB), ("Batman.Begins.srt", MB)],
            private: false,
            age_days: 10,
        },
        T {
            name: "Joker 2019 CAM",
            files: &[("Joker.2019.CAM.mp4", 700 * MB)],
            private: true,
            age_days: 3,
        },
        T {
            name: "Knight Rider Complete",
            files: &[
                ("S01/E01.avi", 300 * MB),
                ("S01/E02.avi", 300 * MB),
                ("S01/E03.avi", 300 * MB),
            ],
            private: false,
            age_days: 100,
        },
        T {
            name: "Computer Science Lectures",
            files: &[("lecture01.pdf", 5 * MB)],
            private: false,
            age_days: 39,
        },
    ];
    let mut store = Store::open_in_memory(StoreConfig::default()).unwrap();
    let raws: Vec<Vec<u8>> = ts.iter().map(info).collect();
    let infos: Vec<Info> = raws.iter().map(|r| Info::parse(r).unwrap()).collect();
    let batch: Vec<NewTorrent<'_>> = infos
        .iter()
        .zip(&raws)
        .zip(&ts)
        .map(|((info, raw), t)| NewTorrent {
            info,
            raw_info: Some(raw),
            discovered_at: NOW - t.age_days * DAY,
            metadata_at: NOW,
            extra_flags: 0,
            source: SourceId::new(0).unwrap(),
        })
        .collect();
    store.insert(&batch).unwrap();
    let index = SearchIndex::in_memory().unwrap();
    let mut w = index.writer(15 << 20).unwrap();
    w.catch_up(&mut store, 100).unwrap();
    w.commit().unwrap();
    let hashes = infos.iter().map(|i| hex(&i.primary_hash().0)).collect();
    (index, hashes)
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn find(index: &SearchIndex, q: &str) -> Vec<String> {
    let ast = parse(q).unwrap_or_else(|e| panic!("{q}: {e}"));
    let hits = index.search(&index.query(&ast, NOW), 50).unwrap().hits;
    let mut names: Vec<String> = index
        .names(&hits)
        .unwrap()
        .into_iter()
        .map(|name| {
            name.split(&[' ', '.'][..])
                .take(2)
                .collect::<Vec<_>>()
                .join(" ")
        })
        .collect();
    names.sort();
    names
}

#[test]
fn text_operators() {
    let (ix, _) = corpus();
    assert_eq!(find(&ix, "knight"), ["Knight Rider", "The Dark"]);
    assert_eq!(find(&ix, "\"dark knight\""), ["The Dark"]);
    assert_eq!(find(&ix, "\"knight dark\""), Vec::<String>::new());
    assert_eq!(find(&ix, "batman | joker"), ["Batman Begins", "Joker 2019"]);
    assert_eq!(find(&ix, "knight -rider"), ["The Dark"]);
    assert_eq!(find(&ix, "-knight -batman -joker"), ["Computer Science"]);
    assert_eq!(find(&ix, "(batman | joker) -cam"), ["Batman Begins"]);
    assert_eq!(find(&ix, "the NEAR/2 knight"), ["The Dark"]);
    assert_eq!(find(&ix, "the NEAR/1 knight"), ["The Dark"]);
    assert_eq!(find(&ix, "rider NEAR/1 dark"), Vec::<String>::new());
    assert_eq!(find(&ix, "comput*"), ["Computer Science"]);
    assert_eq!(find(&ix, "~knigth"), ["Knight Rider", "The Dark"]);
    assert_eq!(find(&ix, "name:srt"), Vec::<String>::new());
    assert_eq!(find(&ix, "path:srt"), ["Batman Begins"]);
    assert_eq!(find(&ix, "").len(), 5, "empty query browses everything");
}

#[test]
fn filters() {
    let (ix, hashes) = corpus();
    assert_eq!(find(&ix, "size:>5GB"), ["The Dark"]);
    assert_eq!(find(&ix, "size:>=4GB"), ["Batman Begins", "The Dark"]);
    assert_eq!(find(&ix, "size:700MB..1GB"), ["Joker 2019", "Knight Rider"]);
    assert_eq!(find(&ix, "files:>=3"), ["Knight Rider"]);
    assert_eq!(find(&ix, "largest:<10MB"), ["Computer Science"]);
    assert_eq!(
        find(&ix, "ext:srt,pdf"),
        ["Batman Begins", "Computer Science"]
    );
    assert_eq!(find(&ix, "private:yes"), ["Joker 2019"]);
    assert_eq!(find(&ix, "private:no").len(), 4);
    assert_eq!(find(&ix, "discovered:<7d"), ["Joker 2019", "The Dark"]);
    assert_eq!(find(&ix, "age:>30d"), ["Computer Science", "Knight Rider"]);
    assert_eq!(
        find(&ix, "discovered:2026-01-01..2026-01-31"),
        ["Batman Begins", "Computer Science"]
    );
    assert_eq!(find(&ix, "discovered:2026-01-31"), ["Batman Begins"]);
    assert_eq!(find(&ix, &format!("hash:{}", hashes[2])), ["Joker 2019"]);
    assert_eq!(find(&ix, &hashes[0]), ["The Dark"]);
    assert_eq!(
        find(&ix, &format!("magnet:?xt=urn:btih:{}&dn=x", hashes[1])),
        ["Batman Begins"]
    );
    assert_eq!(find(&ix, "knight size:<1GB ext:avi"), ["Knight Rider"]);
}

#[test]
fn sorting_and_paging() {
    use lodestar_search::Sort;
    let (ix, _) = corpus();
    let all = ix.query(&parse("").unwrap(), NOW);
    let names = |sort, limit, offset| -> Vec<String> {
        let hits = ix.search_page(&all, sort, limit, offset).unwrap().hits;
        ix.names(&hits)
            .unwrap()
            .into_iter()
            .map(|n| n.split(&[' ', '.'][..]).next().unwrap().to_string())
            .collect()
    };
    assert_eq!(
        names(Sort::Discovered { desc: true }, 2, 0),
        ["The", "Joker"]
    );
    assert_eq!(
        names(Sort::Discovered { desc: true }, 2, 2),
        ["Batman", "Computer"]
    );
    assert_eq!(names(Sort::Size { desc: false }, 1, 0), ["Computer"]);
    assert_eq!(names(Sort::Files { desc: true }, 1, 0), ["Knight"]);
    let page = ix.search_page(&all, Sort::Relevance, 2, 4).unwrap();
    assert_eq!((page.hits.len(), page.total), (1, 5));
}

#[test]
fn lifted_ranges_match_ranges_in_the_query() {
    // Top-level ranges are checked in the collector; inside `|` they stay
    // in the Tantivy query. Both must select the same torrents.
    let (ix, _) = corpus();
    for q in [
        "knight size:<1GB",
        "batman discovered:<60d",
        "files:>=2 size:<1GB",
        "private:no size:1GB..8GB",
        "-joker size:>1MB",
    ] {
        let wrapped = format!("({q}) | zzznomatch");
        assert_eq!(find(&ix, q), find(&ix, &wrapped), "{q}");
    }
}

#[test]
fn browse_sorts_with_filters() {
    use lodestar_search::Sort;
    let (ix, _) = corpus();
    let q = ix.query(&parse("size:<1GB").unwrap(), NOW);
    let page = ix
        .search_page(&q, Sort::Discovered { desc: true }, 50, 0)
        .unwrap();
    assert_eq!(page.total, find(&ix, "size:<1GB").len());
    // Cached the second time; same answer.
    assert_eq!(
        ix.search_page(&q, Sort::Discovered { desc: true }, 50, 0)
            .unwrap(),
        page
    );
}

#[test]
fn reranked_pages_follow_the_full_order() {
    use lodestar_search::Sort;
    let (ix, _) = corpus();
    for q in ["knight", "mkv", "2008 mkv", "s01 avi"] {
        let compiled = ix.query(&parse(q).unwrap(), NOW);
        let full = ix.search_page(&compiled, Sort::Relevance, 50, 0).unwrap();
        let paged: Vec<_> = (0..full.total)
            .flat_map(|i| {
                ix.search_page(&compiled, Sort::Relevance, 1, i)
                    .unwrap()
                    .hits
            })
            .collect();
        assert!(full.total > 0, "{q}");
        assert_eq!(paged, full.hits, "{q}");
    }
}

// ---- hot columns (§7): swarm stats and last seen ---------------------------

const HOUR: i64 = 3600;

/// Hot columns for the corpus (ids 1–5 in insertion order):
/// The Dark is a big fresh swarm, Batman a dead one, Joker small, Knight
/// Rider never scraped and long unseen, Computer scraped but never seen.
fn hot(epoch_every: Duration) -> Arc<HotColumns> {
    let hot = Arc::new(HotColumns::with_epoch_every(epoch_every));
    let swarm = |id: u64, seeders, leechers, trackers, dht| {
        hot.set_swarm(
            id,
            SwarmHot {
                seeders,
                leechers,
                trackers,
                dht,
            },
        )
    };
    swarm(1, 120, 30, 5, 300);
    swarm(2, 0, 0, 0, 0);
    swarm(3, 7, 2, 1, 10);
    swarm(5, 3, 0, 2, 4);
    for (id, ago) in [(1, HOUR), (2, 20 * DAY), (3, 2 * DAY), (4, 100 * DAY)] {
        hot.touch(id, (NOW - ago) as u32);
    }
    hot
}

fn hot_corpus() -> SearchIndex {
    corpus().0.with_hot(hot(Duration::ZERO))
}

fn first_words(ix: &SearchIndex, hits: &[lodestar_search::Hit]) -> Vec<String> {
    ix.names(hits)
        .unwrap()
        .into_iter()
        .map(|n| n.split(&[' ', '.'][..]).next().unwrap().to_string())
        .collect()
}

#[test]
fn swarm_filters() {
    let ix = hot_corpus();
    assert_eq!(find(&ix, "seeders:>=5"), ["Joker 2019", "The Dark"]);
    assert_eq!(find(&ix, "seeders:0"), ["Batman Begins"]);
    // Never scraped isn't zero: it matches no range.
    assert_eq!(
        find(&ix, "seeders:<5"),
        ["Batman Begins", "Computer Science"]
    );
    // …but a negated range keeps it.
    assert_eq!(find(&ix, "-seeders:0").len(), 4);
    assert_eq!(find(&ix, "trackers:>=2"), ["Computer Science", "The Dark"]);
    assert_eq!(find(&ix, "dht:>100"), ["The Dark"]);
    assert_eq!(find(&ix, "leechers:>0"), ["Joker 2019", "The Dark"]);
    assert_eq!(find(&ix, "seen:<24h"), ["The Dark"]);
    assert_eq!(find(&ix, "seen:>30d"), ["Knight Rider"]);
    assert_eq!(find(&ix, "knight seeders:>=1"), ["The Dark"]);
    assert_eq!(
        find(&ix, "batman | seeders:>100"),
        ["Batman Begins", "The Dark"]
    );

    // Without hot columns, swarm clauses match nothing.
    let (plain, _) = corpus();
    assert!(find(&plain, "seeders:>=0").is_empty());
    assert!(find(&plain, "seen:<10y").is_empty());
    assert_eq!(find(&plain, "-seeders:>=0").len(), 5);
}

#[test]
fn time_of_day() {
    // Every torrent was discovered at 00:00 UTC.
    let ix = hot_corpus();
    assert_eq!(find(&ix, "time:00:00..00:59").len(), 5);
    assert_eq!(find(&ix, "time:23:00..00:30").len(), 5, "wraps midnight");
    assert!(find(&ix, "time:12..13").is_empty());
    assert_eq!(find(&ix, "time:0 knight").len(), 2);
    // In UTC+1 that's 01:00.
    let at = |q: &str, offset| {
        let c = ix.query_at(&parse(q).unwrap(), NOW, offset);
        ix.search(&c, 50).unwrap().total
    };
    assert_eq!(at("time:01:00", 3600), 5);
    assert_eq!(at("time:00:00", 3600), 0);
    assert_eq!(at("time:19:00..19:59", -5 * 3600), 5);
}

#[test]
fn lifted_swarm_clauses_match_clauses_in_the_query() {
    // At the top level they're collector checks; inside `|` they're a
    // query of their own. Both must select the same torrents.
    let ix = hot_corpus();
    for q in [
        "seeders:>=5",
        "knight seeders:>=1",
        "-joker seeders:<10",
        "trackers:1..3 private:no",
        "seen:<3d",
        "dht:0",
        "time:00:00..00:10 size:<1GB",
        "-time:12..13 -seeders:0",
    ] {
        let wrapped = format!("({q}) | zzznomatch");
        assert_eq!(find(&ix, q), find(&ix, &wrapped), "{q}");
    }
}

#[test]
fn swarm_sorts_put_never_scraped_last() {
    let ix = hot_corpus();
    let all = ix.query(&parse("").unwrap(), NOW);
    let order = |sort| first_words(&ix, &ix.search_page(&all, sort, 10, 0).unwrap().hits);
    assert_eq!(
        order(Sort::Seeders { desc: true }),
        ["The", "Joker", "Computer", "Batman", "Knight"]
    );
    assert_eq!(
        order(Sort::Seeders { desc: false }),
        ["Batman", "Computer", "Joker", "The", "Knight"]
    );
    assert_eq!(
        order(Sort::Trackers { desc: true })[..2],
        ["The", "Computer"]
    );
    assert_eq!(order(Sort::Leechers { desc: true })[0], "The");
    assert_eq!(
        order(Sort::Seen { desc: true }),
        ["The", "Joker", "Batman", "Knight", "Computer"]
    );
    let filtered = ix.query(&parse("seeders:>0").unwrap(), NOW);
    let page = ix
        .search_page(&filtered, Sort::Seeders { desc: false }, 10, 0)
        .unwrap();
    assert_eq!(page.total, 3);
    assert_eq!(first_words(&ix, &page.hits), ["Computer", "Joker", "The"]);
}

#[test]
fn hot_epoch_expires_cached_browse() {
    for (every, fresh) in [(Duration::ZERO, true), (Duration::from_secs(3600), false)] {
        let cols = hot(every);
        cols.epoch(); // settle the loading writes
        let ix = corpus().0.with_hot(cols.clone());
        let all = ix.query(&parse("seeders:>=0").unwrap(), NOW);
        let top = |ix: &SearchIndex| {
            let hits = ix
                .search_page(&all, Sort::Seeders { desc: true }, 1, 0)
                .unwrap()
                .hits;
            first_words(ix, &hits)
        };
        assert_eq!(top(&ix), ["The"]);
        cols.set_swarm(
            4,
            SwarmHot {
                seeders: 1000,
                leechers: 0,
                trackers: 1,
                dht: 0,
            },
        );
        // A new epoch drops the cached tops; within one they stand.
        let expect = if fresh { "Knight" } else { "The" };
        assert_eq!(top(&ix), [expect], "epoch every {every:?}");
    }
}

#[test]
fn ranking_uses_swarm_priors() {
    // "knight" matches The Dark (big, fresh swarm) and Knight Rider
    // (never scraped, unseen for 100 days).
    let ix = hot_corpus();
    let q = ix.query(&parse("knight").unwrap(), NOW);
    let page = ix
        .search_page_explained(&q, Sort::Relevance, 10, 0)
        .unwrap();
    assert_eq!(first_words(&ix, &page.hits), ["The", "Knight"]);
    assert_eq!(page.explain.len(), page.hits.len());
    for (hit, p) in page.hits.iter().zip(&page.explain) {
        assert_eq!(hit.score, p.score);
        let product = p.text * p.seeders * p.freshness * p.trackers * p.junk * p.penalty;
        assert!((product - p.score).abs() <= p.score * 1e-5, "{p:?}");
    }
    let (dark, rider) = (page.explain[0], page.explain[1]);
    assert!(dark.seeders > rider.seeders && dark.freshness > rider.freshness);
    assert!(rider.seeders > 1.0, "never scraped is neutral, not dead");

    // A penalty (say, fake seeding) can sink it.
    let ix = hot_corpus().with_penalty(Arc::new(|id| if id == 1 { 0.01 } else { 1.0 }));
    let q = ix.query(&parse("knight").unwrap(), NOW);
    let hits = ix.search(&q, 10).unwrap().hits;
    assert_eq!(first_words(&ix, &hits), ["Knight", "The"]);

    // Plain searches report no explanation; sorted pages have none.
    assert!(ix.search(&q, 10).unwrap().explain.is_empty());
    let sorted = ix
        .search_page_explained(&q, Sort::Seeders { desc: true }, 10, 0)
        .unwrap();
    assert!(sorted.explain.is_empty());
}

#[test]
fn ranking_without_hot_columns_is_text_only() {
    let (plain, _) = corpus();
    let q = plain.query(&parse("knight").unwrap(), NOW);
    let page = plain
        .search_page_explained(&q, Sort::Relevance, 10, 0)
        .unwrap();
    assert_eq!(
        page.hits,
        plain.search_page(&q, Sort::Relevance, 10, 0).unwrap().hits
    );
    for p in &page.explain {
        assert_eq!(
            (p.seeders, p.freshness, p.trackers, p.junk, p.penalty),
            (1.0, 1.0, 1.0, 1.0, 1.0)
        );
        assert_eq!(p.score, p.text);
    }
}
