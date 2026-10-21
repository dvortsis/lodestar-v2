//! The query language end to end: text → AST → Tantivy over a small
//! corpus.

use lodestar_bencode::{OwnedValue, to_vec};
use lodestar_metainfo::Info;
use lodestar_search::{SearchIndex, parse};
use lodestar_store::{NewTorrent, SourceId, Store, StoreConfig};

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
