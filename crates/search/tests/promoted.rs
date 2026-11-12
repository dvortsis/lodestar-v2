//! A promoted unverified item (SRC-2) keeps an id below the index's
//! watermark. The live index picks it up through `reindex_log`, which
//! `Store::promote` writes, and a percolation over its id sees it.

use lodestar_bencode::{OwnedValue, to_vec};
use lodestar_metainfo::Info;
use lodestar_search::{CancelToken, Due, PercolateLimits, SearchIndex, parse, percolate};
use lodestar_store::claims::Claims;
use lodestar_store::unverified::{Network, NewUnverified, Promoted};
use lodestar_store::{NewTorrent, PreparedTorrent, SearchableCodes, SourceId, Store, StoreConfig};

const NOW: i64 = 1_700_000_100;
const CODES: SearchableCodes = SearchableCodes {
    stage: 5,
    newly: 10,
    delay: 11,
};

fn info(name: &str) -> Vec<u8> {
    let b = |s: &[u8]| OwnedValue::Bytes(s.to_vec());
    let e: Vec<(Vec<u8>, OwnedValue)> = vec![
        (b"length".to_vec(), OwnedValue::Int(2_000_000)),
        (b"name".to_vec(), b(name.as_bytes())),
        (b"piece length".to_vec(), OwnedValue::Int(65_536)),
        (b"pieces".to_vec(), b(&[0; 20])),
    ];
    to_vec(&OwnedValue::Dict(e.into_iter().collect()))
}

fn names(index: &SearchIndex, q: &str) -> Vec<String> {
    let hits = index
        .search(&index.query(&parse(q).unwrap(), NOW), 50)
        .unwrap()
        .hits;
    index.names(&hits).unwrap()
}

#[test]
fn a_promoted_torrent_below_the_watermark_becomes_searchable() {
    let mut store = Store::open_in_memory(StoreConfig::default()).unwrap();
    let dht = store.register_source("dht", "1", NOW).unwrap();
    let feed = store.register_source("feed", "1", NOW).unwrap();
    let raw = info("Promoted.Gamma.Show");
    let parsed = Info::parse(&raw).unwrap();
    let prepared = store.preparer().prepare(&parsed, None).unwrap();
    let claims = Claims {
        name: Some("Promoted Gamma Show".into()),
        ..Claims::default()
    };
    store
        .put_unverified(&[NewUnverified {
            network: Network::Bt,
            key: &prepared.info_hash.0,
            source: feed,
            private_source: false,
            claims: Some(&claims),
            at: NOW,
        }])
        .unwrap();
    let id = store
        .unverified(Network::Bt, &prepared.info_hash.0)
        .unwrap()
        .unwrap()
        .id;
    // Torrents crawled after it take higher ids, and the index passes it.
    let later: Vec<Vec<u8>> = (0..3).map(|i| info(&format!("Later.Show.{i}"))).collect();
    let infos: Vec<Info> = later.iter().map(|r| Info::parse(r).unwrap()).collect();
    let batch: Vec<NewTorrent<'_>> = infos
        .iter()
        .zip(&later)
        .map(|(info, raw)| NewTorrent {
            info,
            raw_info: Some(raw),
            discovered_at: NOW,
            metadata_at: NOW,
            extra_flags: 0,
            source: dht,
        })
        .collect();
    store.insert(&batch).unwrap();
    let index = SearchIndex::in_memory().unwrap();
    let mut w = index.writer(15 << 20).unwrap();
    while w.catch_up(&mut store, 100).unwrap() > 0 {}
    w.commit().unwrap();
    let watermark = index.watermark().unwrap();
    assert!(watermark > id);
    assert!(names(&index, "gamma").is_empty());

    let promoted = store
        .promote(
            id,
            &PreparedTorrent {
                torrent: &prepared,
                discovered_at: NOW,
                metadata_at: NOW,
                extra_flags: 0,
                source: SourceId::new(0).unwrap(),
                verified: true,
            },
            CODES,
            NOW,
        )
        .unwrap();
    assert_eq!(promoted, Promoted::Kept(id));
    // The live indexer's catch-up replays `reindex_log`: the promoted id,
    // never indexed before, is added under its own id.
    while w.catch_up(&mut store, 100).unwrap() > 0 {}
    w.commit().unwrap();
    assert_eq!(
        names(&index, "gamma"),
        vec!["Promoted.Gamma.Show".to_string()]
    );
    assert_eq!(index.watermark().unwrap(), watermark, "the watermark stays");

    // A percolation over its id finds it (the percolator's own pickup of
    // ids below a search's watermark is `2026-11-02-SRC-2.md`'s handoff).
    let q = parse("gamma").unwrap();
    let due = [Due {
        id: 1,
        watermark: id - 1,
        query: &q,
        utc_offset: 0,
        count_only: false,
        stems: false,
        stoppable: false,
        hold_unresolved: true,
    }];
    let limits = PercolateLimits {
        keep: 100,
        max_range: 1_000,
    };
    let run = percolate(&index, &due, id, limits, NOW, &CancelToken::new()).unwrap();
    assert_eq!(run.matched[0].torrent_ids, vec![id]);
}
