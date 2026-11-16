//! GS-3: collapse-by-group against a real store, end to end. Torrents are
//! stored and classified, so GS-2b's producer writes `group_key` and
//! `title_identity` from their records. A lease then collapses them: one
//! row per work, its other versions counted, groups counted beside the
//! torrents, every page under one lease while more torrents arrive.

use std::collections::HashSet;
use std::sync::Arc;

use lodestar_bencode::{OwnedValue, to_vec};
use lodestar_metainfo::Info;
use lodestar_search::collapse::{self, Unavailable};
use lodestar_search::{
    CancelToken, Generations, Lease, SearchIndex, Sort, Start, Total, index_dir, parse,
};
use lodestar_store::{Enriched, NewTorrent, SourceId, Store, StoreConfig};

const NOW: i64 = 1_700_000_100;
const WORKS: [&str; 6] = ["Alpha", "Bravo", "Charlie", "Delta", "Echo", "Foxtrot"];

fn raw(name: &str) -> Vec<u8> {
    let b = |s: &str| OwnedValue::Bytes(s.as_bytes().to_vec());
    let d = |e: Vec<(&str, OwnedValue)>| {
        OwnedValue::Dict(
            e.into_iter()
                .map(|(k, v)| (k.as_bytes().to_vec(), v))
                .collect(),
        )
    };
    to_vec(&d(vec![
        ("length", OwnedValue::Int(1 << 30)),
        ("name", b(name)),
        ("piece length", OwnedValue::Int(1 << 16)),
        ("pieces", OwnedValue::Bytes(vec![0; 20])),
    ]))
}

/// Torrent `k`: a version of work `k % 6`.
fn name(k: usize) -> String {
    let res = ["720p", "1080p", "2160p", "480p"][(k / WORKS.len()) % 4];
    format!("{}.Film.2012.{res}.WEB-GRP{k}", WORKS[k % WORKS.len()])
}

/// Stores and classifies torrents `range`.
fn add(store: &mut Store, range: std::ops::Range<usize>) {
    let raws: Vec<Vec<u8>> = range.clone().map(|k| raw(&name(k))).collect();
    let infos: Vec<Info> = raws.iter().map(|r| Info::parse(r).unwrap()).collect();
    let batch: Vec<NewTorrent<'_>> = infos
        .iter()
        .zip(&raws)
        .zip(range)
        .map(|((info, raw), k)| NewTorrent {
            info,
            raw_info: Some(raw),
            discovered_at: 1_700_000_000 + k as i64,
            metadata_at: 1_700_000_001,
            extra_flags: 0,
            source: SourceId::new(0).unwrap(),
        })
        .collect();
    let ids: Vec<i64> = store
        .insert(&batch)
        .unwrap()
        .into_iter()
        .map(|i| i.id())
        .collect();
    let enriched: Vec<(i64, Enriched)> = ids
        .iter()
        .map(|&id| {
            let row = store.torrent(id).unwrap().unwrap();
            let tree = store.files(id).unwrap().unwrap();
            let files = lodestar_classify::file_infos(&tree.files, tree.encoding);
            let c = lodestar_classify::classify(&row.name, &files);
            let e = Enriched {
                version: lodestar_classify::ENRICHMENT_VERSION,
                content_type: c.content_type.code(),
                junk_score: c.junk.score,
                comp: c.composition.encode(),
                data: lodestar_classify::Record::new(&c).encode(),
            };
            (id, e)
        })
        .collect();
    store.set_enrichments(&enriched).unwrap();
}

fn catch_up(index: &SearchIndex, store: &mut Store) {
    let mut w = index.writer(15 << 20).unwrap();
    while w.catch_up(store, 1000).unwrap() > 0 {}
    w.commit().unwrap();
    w.finish().unwrap();
}

/// The work a stored torrent's name is a version of.
fn work_of(store: &Store, id: u64) -> String {
    let row = store.torrent(id as i64).unwrap().unwrap();
    row.name.split('.').next().unwrap().to_owned()
}

#[test]
fn stored_versions_collapse_to_one_row_per_work_across_pages_and_commits() {
    let tmp = tempfile::tempdir().unwrap();
    let mut store = Store::open(tmp.path().join("s.db"), StoreConfig::default()).unwrap();
    store.register_source("dht", "1", 1).unwrap();
    add(&mut store, 0..120);
    let index = Arc::new(SearchIndex::open(&index_dir(tmp.path())).unwrap());
    catch_up(&index, &mut store);
    let cancel = CancelToken::new();
    for sort in [Sort::Relevance, Sort::Size { desc: true }, Sort::Discovered { desc: false }] {
        let node = parse("film").unwrap();
        let lease = Lease::new(index.clone(), &node, NOW, sort, &cancel).unwrap();
        collapse::available(&lease).expect("GS-2b's producer wrote group data");
        let n = lease.total().value;
        assert_eq!(
            collapse::groups(&lease, false, &cancel).unwrap(),
            Total {
                value: WORKS.len(),
                approx: false
            }
        );
        let mut rows = Vec::new();
        let mut page = collapse::page(&lease, Start::Offset(0), 4, &cancel).unwrap();
        let mut more = 120;
        loop {
            rows.extend(page.rows.iter().cloned());
            // Torrents arrive and are indexed between pages.
            add(&mut store, more..more + 7);
            catch_up(&index, &mut store);
            more += 7;
            let Some(next) = page.next else { break };
            page = collapse::page(&lease, Start::After(next), 4, &cancel).unwrap();
        }
        let works: Vec<String> = rows.iter().map(|r| work_of(&store, r.hit.id)).collect();
        let distinct: HashSet<&String> = works.iter().collect();
        assert_eq!(works.len(), WORKS.len(), "{sort:?}: {works:?}");
        assert_eq!(distinct.len(), works.len(), "{sort:?}: {works:?}");
        let summed: u64 = rows.iter().map(|r| r.others + 1).sum();
        assert_eq!(summed as usize, n, "{sort:?}");
    }
}

#[test]
fn an_index_without_group_data_reports_it() {
    let tmp = tempfile::tempdir().unwrap();
    let mut store = Store::open(tmp.path().join("s.db"), StoreConfig::default()).unwrap();
    store.register_source("dht", "1", 1).unwrap();
    add(&mut store, 0..20);
    let index = SearchIndex::open(&index_dir(tmp.path()))
        .unwrap()
        .with_generations(Generations::current().with("group_key", 0))
        .unwrap();
    let index = Arc::new(index);
    catch_up(&index, &mut store);
    let lease = Lease::new(index, &parse("film").unwrap(), NOW, Sort::Relevance, &CancelToken::new())
        .unwrap();
    assert_eq!(
        collapse::available(&lease),
        Err(Unavailable::GroupKeyNotReady)
    );
}
