//! SRC-3: `verified:`, `source:` and `network:` against a real store,
//! unverified items hidden by default, and the fields' backfill under
//! the population contract (`docs/decisions/2026-11-04-SB-6.md`) on an
//! index built before SRC-3, with a restart mid-walk.

use std::path::Path;

use lodestar_bencode::{OwnedValue, to_vec};
use lodestar_metainfo::Info;
use lodestar_search::tokenizer::{NAME_TOKENIZER, NameTokenizer};
use lodestar_search::{Availability, Node, SearchIndex, index_dir, parse};
use lodestar_store::{NewTorrent, SourceId, Store, StoreConfig};
use tantivy::TantivyDocument;

const HEAP: usize = 15 << 20;
const NOW: i64 = 1_700_000_100;

fn torrent(i: usize) -> Vec<u8> {
    let d = |entries: Vec<(&str, OwnedValue)>| {
        OwnedValue::Dict(
            entries
                .into_iter()
                .map(|(k, v)| (k.as_bytes().to_vec(), v))
                .collect(),
        )
    };
    let b = |s: &str| OwnedValue::Bytes(s.as_bytes().to_vec());
    to_vec(&d(vec![
        ("length", OwnedValue::Int(1000)),
        ("name", b(&format!("Provenance.Item.{i}.mkv"))),
        ("piece length", OwnedValue::Int(1 << 16)),
        ("pieces", OwnedValue::Bytes(vec![0; 20])),
    ]))
}

/// A store of `n` torrents: even ones found by `dht` (source 0), odd
/// ones by `Nyaa` (source 1, registered with a capital to check the
/// term is lowercased).
fn store(root: &Path, n: usize) -> Store {
    let mut store = Store::open(root.join("s.db"), StoreConfig::default()).unwrap();
    assert_eq!(store.register_source("dht", "1", 1).unwrap().get(), 0);
    assert_eq!(store.register_source("Nyaa", "1", 1).unwrap().get(), 1);
    let raws: Vec<Vec<u8>> = (0..n).map(torrent).collect();
    let infos: Vec<Info> = raws.iter().map(|t| Info::parse(t).unwrap()).collect();
    let batch: Vec<NewTorrent<'_>> = infos
        .iter()
        .zip(&raws)
        .enumerate()
        .map(|(i, (info, raw))| NewTorrent {
            info,
            raw_info: Some(raw),
            discovered_at: 1_700_000_000,
            metadata_at: 1_700_000_001,
            extra_flags: 0,
            source: SourceId::new((i % 2) as u8).unwrap(),
        })
        .collect();
    store.insert(&batch).unwrap();
    store
}

fn count(index: &SearchIndex, q: &str) -> usize {
    count_node(index, &parse(q).unwrap())
}

fn count_node(index: &SearchIndex, node: &Node) -> usize {
    index
        .search(&index.query(node, NOW), 1000)
        .unwrap()
        .hits
        .len()
}

/// The query a search runs: unverified items hidden unless asked for.
fn shown(index: &SearchIndex, q: &str) -> usize {
    count_node(index, &parse(q).unwrap().without_unverified())
}

fn complete_index(root: &Path, store: &mut Store) -> SearchIndex {
    let index = SearchIndex::open(&index_dir(root)).unwrap();
    let mut w = index.writer(HEAP).unwrap();
    while w.catch_up(store, 1000).unwrap() > 0 {}
    w.commit().unwrap();
    w.finish().unwrap();
    index
}

#[test]
fn every_torrent_is_verified_clearnet_and_carries_its_sources() {
    let tmp = tempfile::tempdir().unwrap();
    let mut store = store(tmp.path(), 10);
    let index = complete_index(tmp.path(), &mut store);
    for f in ["verified", "source", "network"] {
        assert_eq!(index.availability(f), Availability::Ready, "{f}");
    }
    assert_eq!(count(&index, "verified:yes"), 10);
    assert_eq!(
        count(&index, "verified:no"),
        0,
        "nothing unverified is indexed"
    );
    assert_eq!(count(&index, "verified:yes,no"), 10);
    assert_eq!(count(&index, "network:bt"), 10);
    assert_eq!(count(&index, "network:ed2k,ipfs"), 0);
    assert_eq!(count(&index, "network:bt,usenet"), 10);
    assert_eq!(count(&index, "source:dht"), 5);
    assert_eq!(count(&index, "source:nyaa"), 5, "the id is lowercased");
    assert_eq!(count(&index, "source:NYAA"), 5, "and so is the query");
    assert_eq!(count(&index, "source:dht,nyaa"), 10);
    assert_eq!(count(&index, "-source:dht"), 5);
    // An id no source has parses and matches nothing.
    assert_eq!(count(&index, "source:prowlarr"), 0);
    assert_eq!(count(&index, "item source:prowlarr"), 0);
    assert_eq!(count(&index, "item (source:prowlarr | source:dht)"), 5);
    // An unknown network is a parse error, not an empty result.
    assert!(parse("network:tor").is_err());
    // Combined with words and ranges.
    assert_eq!(count(&index, "item network:bt verified:yes size:>100"), 10);
    assert_eq!(shown(&index, "item"), 10);
    assert_eq!(shown(&index, ""), 10);
}

#[test]
fn a_torrent_gaining_a_source_has_it_once_reindexed() {
    let tmp = tempfile::tempdir().unwrap();
    let mut store = store(tmp.path(), 4);
    let index = complete_index(tmp.path(), &mut store);
    // A full reindex (the walk or a `reindex_log` replay) reads the
    // torrent's current bitset.
    let gens = lodestar_search::Generations::current().with("source", 2);
    let index = index.with_generations(gens).unwrap();
    assert_eq!(
        index.availability("source"),
        Availability::Backfilling { done: 0, upto: 4 }
    );
    let mut w = index.writer(HEAP).unwrap();
    while w.catch_up(&mut store, 1).unwrap() > 0 {
        w.commit().unwrap();
    }
    w.commit().unwrap();
    assert_eq!(index.availability("source"), Availability::Ready);
    assert_eq!(count(&index, "source:dht"), 2);
    assert_eq!(index.num_docs(), 4, "replaced, not duplicated");
}

/// Rewrites the torrent index under `dir` as a v6 build from before
/// SRC-3 left it: no document has `verified`, `source` or `network`, and
/// the payload records those fields at generation 0 (no producer).
fn unpopulate(dir: &Path, store: &mut Store) {
    let index = tantivy::Index::open_in_dir(dir).unwrap();
    index
        .tokenizers()
        .register(NAME_TOKENIZER, NameTokenizer::default());
    index.tokenizers().register(
        lodestar_search::schema::TRIGRAM_TOKENIZER,
        lodestar_search::schema::trigram_analyzer(),
    );
    let payload = index.load_metas().unwrap().payload.unwrap();
    let (watermarks, pop) = payload.split_once(";pop:").unwrap();
    let old_pop = pop
        .split(',')
        .map(|e| match e.split_once('=').unwrap().0 {
            f @ ("network" | "source" | "verified") => format!("{f}=0"),
            _ => e.to_owned(),
        })
        .collect::<Vec<_>>()
        .join(",");
    let (_, f) = lodestar_search::schema::build();
    let mut w = index.writer::<TantivyDocument>(HEAP).unwrap();
    w.delete_all_documents().unwrap();
    for id in store.ids_after(0, 100_000).unwrap() {
        let row = store.torrent(id).unwrap().unwrap();
        let tree = store.files(id).unwrap().unwrap();
        let doc = lodestar_search::TorrentDoc::new(&row, &tree).to_document(&f);
        let mut stripped = TantivyDocument::new();
        for (field, v) in doc.field_values() {
            if ![f.verified, f.source, f.network].contains(&field) {
                stripped.add_field_value(field, v);
            }
        }
        w.add_document(stripped).unwrap();
    }
    let mut prepared = w.prepare_commit().unwrap();
    prepared.set_payload(&format!("{watermarks};pop:{old_pop}"));
    prepared.commit().unwrap();
}

#[test]
fn the_backfill_resumes_after_a_restart_and_hides_nothing_meanwhile() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = index_dir(tmp.path());
    let mut store = store(tmp.path(), 40);
    drop(complete_index(tmp.path(), &mut store));
    unpopulate(&dir, &mut store);

    // A build from before SRC-3 reports the fields as having no
    // producer.
    let old = lodestar_search::Generations::current()
        .with("verified", 0)
        .with("source", 0)
        .with("network", 0);
    let index = SearchIndex::open(&dir)
        .unwrap()
        .with_generations(old)
        .unwrap();
    assert_eq!(index.availability("source"), Availability::NoProducer);
    drop(index);

    // This build: every field walks the 40 torrents.
    let index = SearchIndex::open(&dir).unwrap();
    let backfilling = |done| Availability::Backfilling { done, upto: 40 };
    for f in ["verified", "source", "network"] {
        assert_eq!(index.availability(f), backfilling(0), "{f}");
    }
    let partial: Vec<String> = index.unpopulated().into_iter().map(|f| f.field).collect();
    for f in ["network", "source", "verified"] {
        assert!(partial.iter().any(|p| p == f), "{f} in {partial:?}");
    }
    // Before any walk: the hidden-by-default clause, `verified:yes` and
    // `network:bt` hide no torrent the walk hasn't reached; `source:`
    // finds only what it has.
    let whole = |index: &SearchIndex| {
        assert_eq!(shown(index, ""), 40);
        assert_eq!(shown(index, "item"), 40);
        assert_eq!(count(index, "verified:yes"), 40);
        assert_eq!(count(index, "network:bt"), 40);
        assert_eq!(count(index, "verified:no"), 0);
        assert_eq!(count(index, "network:ipfs"), 0);
    };
    whole(&index);
    assert_eq!(count(&index, "source:dht"), 0);

    let mut w = index.writer(HEAP).unwrap();
    let c = w.catch_up_counted(&mut store, 16).unwrap();
    assert_eq!((c.added, c.replayed, c.backfilled), (0, 0, 16));
    w.commit().unwrap();
    assert_eq!(index.availability("source"), backfilling(16));
    whole(&index);
    assert_eq!(count(&index, "source:dht,nyaa"), 16);
    // More, lost to a crash before its commit.
    assert_eq!(w.catch_up_counted(&mut store, 16).unwrap().backfilled, 16);
    drop(w);
    drop(index);

    let index = SearchIndex::open(&dir).unwrap();
    assert_eq!(
        index.availability("source"),
        backfilling(16),
        "resumed from the commit"
    );
    whole(&index);
    // A lease on this mid-backfill snapshot (a search's later pages).
    let lease = lodestar_search::Lease::new(
        std::sync::Arc::new(index.clone()),
        &parse("source:dht").unwrap(),
        NOW,
        lodestar_search::Sort::Relevance,
        &lodestar_search::CancelToken::new(),
    )
    .unwrap();
    let mut w = index.writer(HEAP).unwrap();
    let mut steps = 0;
    while index.availability("source") != Availability::Ready {
        assert!(steps < 10, "the backfill never finished");
        w.catch_up(&mut store, 10).unwrap();
        w.commit().unwrap();
        whole(&index);
        steps += 1;
    }
    assert_eq!(steps, 3, "24 left in steps of 10");
    // The lease still reads the snapshot from mid-backfill, and is told
    // so; a search taken now is complete.
    // (It reports the walk as it stood just before completing.)
    assert_eq!(index.availability_in("source", &lease), backfilling(36));
    let fresh = lodestar_search::Lease::new(
        std::sync::Arc::new(index.clone()),
        &parse("source:dht").unwrap(),
        NOW,
        lodestar_search::Sort::Relevance,
        &lodestar_search::CancelToken::new(),
    )
    .unwrap();
    assert_eq!(index.availability_in("source", &fresh), Availability::Ready);
    for f in ["verified", "source", "network"] {
        assert_eq!(index.availability(f), Availability::Ready, "{f}");
    }
    assert!(
        index
            .unpopulated()
            .iter()
            .all(|f| !matches!(f.field.as_str(), "verified" | "source" | "network"))
    );
    assert_eq!(index.num_docs(), 40);
    assert_eq!(count(&index, "source:dht"), 20);
    assert_eq!(count(&index, "source:nyaa"), 20);
    // Recorded: a restart has nothing left to do.
    assert_eq!(w.catch_up_counted(&mut store, 10).unwrap().backfilled, 0);
    drop(w);
    drop(index);
    let index = SearchIndex::open(&dir).unwrap();
    assert_eq!(index.availability("source"), Availability::Ready);
}

#[test]
fn verified_no_finds_unverified_documents_only_when_asked() {
    // No producer indexes unverified items yet (see the decisions file),
    // so play one: a document written with `verified:no` beside the
    // torrents'.
    let tmp = tempfile::tempdir().unwrap();
    let dir = index_dir(tmp.path());
    let mut store = store(tmp.path(), 1);
    drop(complete_index(tmp.path(), &mut store));
    let row = store.torrent(1).unwrap().unwrap();
    let tree = store.files(1).unwrap().unwrap();
    let mut doc = lodestar_search::TorrentDoc::new(&row, &tree);
    doc.id = 99;
    doc.provenance.verified = lodestar_search::query::Verified::No;
    doc.provenance.sources = vec!["nyaa".into()];
    {
        let raw = tantivy::Index::open_in_dir(&dir).unwrap();
        raw.tokenizers()
            .register(NAME_TOKENIZER, NameTokenizer::default());
        raw.tokenizers().register(
            lodestar_search::schema::TRIGRAM_TOKENIZER,
            lodestar_search::schema::trigram_analyzer(),
        );
        let payload = raw.load_metas().unwrap().payload.unwrap();
        let (_, f) = lodestar_search::schema::build();
        let mut w = raw.writer::<TantivyDocument>(HEAP).unwrap();
        w.add_document(doc.to_document(&f)).unwrap();
        let mut prepared = w.prepare_commit().unwrap();
        prepared.set_payload(&payload);
        prepared.commit().unwrap();
    }
    let index = SearchIndex::open(&dir).unwrap();

    assert_eq!(shown(&index, "item"), 1, "hidden by default");
    assert_eq!(shown(&index, ""), 1);
    assert_eq!(shown(&index, "source:nyaa"), 0);
    assert_eq!(shown(&index, "verified:no"), 1, "asked for");
    assert_eq!(shown(&index, "item verified:no source:nyaa"), 1);
    assert_eq!(shown(&index, "verified:yes,no"), 2);
    assert_eq!(shown(&index, "item (verified:no | verified:yes)"), 2);
    assert_eq!(shown(&index, "-verified:yes"), 1);
    assert_eq!(shown(&index, "verified:yes"), 1);
    assert_eq!(shown(&index, "-verified:no"), 1);
}
