//! GS-2: the `group_key` and `group_minhash` producers against a real
//! store, and their backfill under the population contract
//! (`docs/decisions/2026-11-04-SB-6.md`): an index built before GS-2
//! (both fields at generation 0, no document carrying them), then this
//! build, with a restart mid-walk. Neither field reads Ready until the
//! walk is done, so no consumer takes "no term" for "no match"
//! meanwhile.
//!
//! The fields are read here with Tantivy itself: neither is in the query
//! language. `group_key` is the fast-column copy of `title_identity`
//! (plan Q8) and `group_minhash` holds the band terms
//! `lodestar_classify::group::band_terms` gives (plan Q9).

use std::path::Path;

use lodestar_bencode::{OwnedValue, to_vec};
use lodestar_classify::group::{BANDS, band_terms};
use lodestar_metainfo::Info;
use lodestar_search::tokenizer::{NAME_TOKENIZER, NameTokenizer};
use lodestar_search::{Availability, Generations, SearchIndex, index_dir};
use lodestar_store::{Enriched, NewTorrent, SourceId, Store, StoreConfig};
use tantivy::collector::Count;
use tantivy::query::TermQuery;
use tantivy::schema::IndexRecordOption;
use tantivy::{TantivyDocument, Term};

const HEAP: usize = 15 << 20;
const N: usize = 30;
const GIB: i64 = 1 << 30;

/// Torrent `i` is in family `i % 3`:
/// - 0: a 6-episode season, every fourth one with episode 6 swapped for
///   another size (similar, not identical);
/// - 1: another season of 5 episodes;
/// - 2: one film and its sample: fewer than 3 major files, no signature.
///
/// Each has 3 small files (an `.nfo`, a sample of 100 KiB, a 2 MiB
/// subtitle pack), which are never major.
fn sizes(i: usize) -> Vec<i64> {
    let mut s: Vec<i64> = match i % 3 {
        0 => {
            let mut s: Vec<i64> = (0..6).map(|e| GIB + 1_000 * e).collect();
            if i.is_multiple_of(4) {
                s[5] = 2 * GIB + 77;
            }
            s
        }
        1 => (0..5).map(|e| 3 * GIB + 7_000 * e).collect(),
        _ => vec![8 * GIB + 3, 50 << 20],
    };
    s.extend([4_000, 100 << 10, 2 << 20]);
    if i % 2 == 1 {
        s.reverse();
    }
    s
}

fn name(i: usize) -> String {
    match i % 3 {
        0 => format!("Family.Show.S01.1080p.WEB-GRP{i}"),
        1 => format!("Other.Show.S02.720p.HDTV-GRP{i}"),
        _ => format!("A.Film.2010.1080p.BluRay-GRP{i}"),
    }
}

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
    let files: Vec<OwnedValue> = sizes(i)
        .into_iter()
        .enumerate()
        .map(|(j, len)| {
            d(vec![
                ("length", OwnedValue::Int(len)),
                ("path", OwnedValue::List(vec![b(&format!("f{i}-{j}.mkv"))])),
            ])
        })
        .collect();
    to_vec(&d(vec![
        ("files", OwnedValue::List(files)),
        ("name", b(&name(i))),
        ("piece length", OwnedValue::Int(1 << 16)),
        ("pieces", OwnedValue::Bytes(vec![0; 20])),
    ]))
}

/// The store, every torrent classified except the last (no record, so
/// no title identity and no `group_key`).
fn store(root: &Path) -> (Store, Vec<Info>) {
    let mut store = Store::open(root.join("s.db"), StoreConfig::default()).unwrap();
    store.register_source("dht", "1", 1).unwrap();
    let raws: Vec<Vec<u8>> = (0..N).map(torrent).collect();
    let infos: Vec<Info> = raws.iter().map(|t| Info::parse(t).unwrap()).collect();
    insert(&mut store, &infos, &raws, 0);
    (store, infos)
}

fn insert(store: &mut Store, infos: &[Info], raws: &[Vec<u8>], skip: usize) -> Vec<i64> {
    let batch: Vec<NewTorrent<'_>> = infos
        .iter()
        .zip(raws)
        .skip(skip)
        .map(|(info, raw)| NewTorrent {
            info,
            raw_info: Some(raw),
            discovered_at: 1_700_000_000,
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
        .filter(|&&id| id != N as i64)
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
    ids
}

/// Torrent `i`'s band terms, from its info dict.
fn bands(infos: &[Info], i: usize) -> Vec<u64> {
    let sizes: Vec<u64> = infos[i]
        .files
        .iter()
        .filter(|f| !f.is_padding())
        .map(|f| f.size)
        .collect();
    band_terms(&sizes)
}

/// The title identity the store's record of torrent id `id` gives.
fn identity(store: &Store, id: i64) -> Option<u64> {
    let e = store.enrichment(id).unwrap()?;
    let r = lodestar_classify::Record::decode(&e.data).unwrap();
    lodestar_search::title_identity_parts(&r).map(|p| p.key())
}

fn raw(dir: &Path) -> tantivy::Index {
    let index = tantivy::Index::open_in_dir(dir).unwrap();
    index
        .tokenizers()
        .register(NAME_TOKENIZER, NameTokenizer::default());
    index.tokenizers().register(
        lodestar_search::schema::TRIGRAM_TOKENIZER,
        lodestar_search::schema::trigram_analyzer(),
    );
    index
}

/// Documents with `value` in the u64 field `field`, on the index's
/// latest commit.
fn with_term(dir: &Path, field: &str, value: u64) -> usize {
    let index = raw(dir);
    let f = index.schema().get_field(field).unwrap();
    let searcher = index.reader().unwrap().searcher();
    let q = TermQuery::new(Term::from_field_u64(f, value), IndexRecordOption::Basic);
    searcher.search(&q, &Count).unwrap()
}

/// Every document's (id, `group_key` column value).
fn group_keys(dir: &Path) -> Vec<(u64, Option<u64>)> {
    let index = raw(dir);
    let searcher = index.reader().unwrap().searcher();
    let mut out = Vec::new();
    for seg in searcher.segment_readers() {
        let ids = seg.fast_fields().u64("id").unwrap();
        let keys = seg.fast_fields().u64("group_key").unwrap();
        for doc in seg.doc_ids_alive() {
            out.push((ids.first(doc).unwrap(), keys.first(doc)));
        }
    }
    out.sort_unstable();
    out
}

fn complete_index(root: &Path, store: &mut Store) -> SearchIndex {
    let index = SearchIndex::open(&index_dir(root)).unwrap();
    let mut w = index.writer(HEAP).unwrap();
    while w.catch_up(store, 1000).unwrap() > 0 {}
    w.commit().unwrap();
    w.finish().unwrap();
    index
}

/// The torrents whose band terms include `band`, by the classify
/// function (a swapped episode may or may not change a band: that's the
/// hashes' choice).
fn sharing(infos: &[Info], band: u64) -> usize {
    (0..N).filter(|&j| bands(infos, j).contains(&band)).count()
}

#[test]
fn every_torrent_carries_its_group_fields() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = index_dir(tmp.path());
    let (mut store, infos) = store(tmp.path());
    let index = complete_index(tmp.path(), &mut store);
    for f in ["group_key", "group_minhash"] {
        assert_eq!(index.availability(f), Availability::Ready, "{f}");
    }
    // Band terms: families 0 and 1 have BANDS each, family 2 none.
    for i in 0..N {
        let b = bands(&infos, i);
        assert_eq!(b.len(), if i % 3 == 2 { 0 } else { BANDS }, "torrent {i}");
        for t in &b {
            assert_eq!(with_term(&dir, "group_minhash", *t), sharing(&infos, *t));
        }
    }
    // The unswapped family-0 torrents are one payload: all their bands
    // agree, and no other family shares any.
    let plain: Vec<usize> = (0..N)
        .filter(|i| i % 3 == 0 && !i.is_multiple_of(4))
        .collect();
    for t in bands(&infos, plain[0]) {
        assert!(with_term(&dir, "group_minhash", t) >= plain.len());
        for j in (0..N).filter(|j| j % 3 != 0) {
            assert!(!bands(&infos, j).contains(&t), "torrent {j}");
        }
    }
    // `group_key` is the title identity, in the column and as a term.
    let keys = group_keys(&dir);
    assert_eq!(keys.len(), N);
    for (id, key) in &keys {
        assert_eq!(*key, identity(&store, *id as i64), "id {id}");
    }
    assert_eq!(keys[N - 1].1, None, "the unclassified torrent has none");
    let family0 = identity(&store, 1).unwrap();
    assert_eq!(with_term(&dir, "group_key", family0), N / 3);
    assert_eq!(with_term(&dir, "title_identity", family0), N / 3);
}

/// A torrent written after the index is complete carries both fields
/// (the live indexer's new-document path).
#[test]
fn new_torrents_are_written_with_both_fields() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = index_dir(tmp.path());
    let mut store = Store::open(tmp.path().join("s.db"), StoreConfig::default()).unwrap();
    store.register_source("dht", "1", 1).unwrap();
    let raws: Vec<Vec<u8>> = (0..N).map(torrent).collect();
    let infos: Vec<Info> = raws.iter().map(|t| Info::parse(t).unwrap()).collect();
    insert(&mut store, &infos[..10], &raws[..10], 0);
    let index = complete_index(tmp.path(), &mut store);
    let t = bands(&infos, 1)[0];
    let before = with_term(&dir, "group_minhash", t);
    insert(&mut store, &infos[..13], &raws[..13], 10);
    let mut w = index.writer(HEAP).unwrap();
    let c = w.catch_up_counted(&mut store, 100).unwrap();
    assert_eq!((c.added, c.backfilled), (3, 0));
    w.commit().unwrap();
    // Torrents 10..13: family 1, 2, 0. Torrent 10 is family 1.
    assert_eq!(with_term(&dir, "group_minhash", t), before + 1);
    let keys = group_keys(&dir);
    assert_eq!(keys.len(), 13);
    assert_eq!(keys[10].1, identity(&store, 11));
    assert!(keys[10].1.is_some());
    assert_eq!(index.availability("group_minhash"), Availability::Ready);
}

/// Rewrites the torrent index under `dir` as GS-0's build left it: no
/// document carries `group_key` or `group_minhash`, and the payload
/// records both at generation 0.
fn unpopulate(dir: &Path, store: &mut Store) {
    let index = raw(dir);
    let payload = index.load_metas().unwrap().payload.unwrap();
    let (watermarks, pop) = payload.split_once(";pop:").unwrap();
    let old_pop = pop
        .split(',')
        .map(|e| match e.split_once('=').unwrap().0 {
            f @ ("group_key" | "group_minhash") => format!("{f}=0"),
            _ => e.to_owned(),
        })
        .collect::<Vec<_>>()
        .join(",");
    assert!(old_pop.contains("group_key=0") && old_pop.contains("group_minhash=0"));
    let (_, f) = lodestar_search::schema::build();
    let mut w = index.writer::<TantivyDocument>(HEAP).unwrap();
    w.delete_all_documents().unwrap();
    for id in store.ids_after(0, 100_000).unwrap() {
        let row = store.torrent(id).unwrap().unwrap();
        let tree = store.files(id).unwrap().unwrap();
        let mut doc = lodestar_search::TorrentDoc::new(&row, &tree);
        doc.group_minhash.clear();
        let full = doc.to_document(&f);
        let mut old = TantivyDocument::new();
        for (field, v) in full.field_values() {
            if field != f.group_key && field != f.group_minhash {
                old.add_field_value(field, v);
            }
        }
        w.add_document(old).unwrap();
    }
    let mut prepared = w.prepare_commit().unwrap();
    prepared.set_payload(&format!("{watermarks};pop:{old_pop}"));
    prepared.commit().unwrap();
}

#[test]
fn the_backfill_resumes_after_a_restart_and_reports_partial_meanwhile() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = index_dir(tmp.path());
    let (mut store, infos) = store(tmp.path());
    drop(complete_index(tmp.path(), &mut store));
    unpopulate(&dir, &mut store);
    // Family 1's first band: torrents 1, 4, …, 28 (ids 2, 5, …, 29).
    let t = bands(&infos, 1)[0];
    let family1 = identity(&store, 2).unwrap();
    let done_of = |done: usize| (0..done).filter(|i| i % 3 == 1).count();

    // GS-0's build: no producer, nothing to walk.
    let old = Generations::current()
        .with("group_key", 0)
        .with("group_minhash", 0);
    let index = SearchIndex::open(&dir)
        .unwrap()
        .with_generations(old)
        .unwrap();
    for f in ["group_key", "group_minhash"] {
        assert_eq!(index.availability(f), Availability::NoProducer);
    }
    assert_eq!(with_term(&dir, "group_minhash", t), 0);
    drop(index);

    // This build: both walk the 30 torrents in one pass.
    let index = SearchIndex::open(&dir).unwrap();
    let backfilling = |done| Availability::Backfilling {
        done,
        upto: N as i64,
    };
    for f in ["group_key", "group_minhash"] {
        assert_eq!(index.availability(f), backfilling(0), "{f}");
        assert!(!index.availability(f).complete(), "no prefilter on it");
        assert!(index.unpopulated().iter().any(|u| u.field == f), "{f}");
    }
    let mut w = index.writer(HEAP).unwrap();
    let c = w.catch_up_counted(&mut store, 12).unwrap();
    assert_eq!((c.added, c.replayed, c.backfilled), (0, 0, 12));
    w.commit().unwrap();
    assert_eq!(index.availability("group_minhash"), backfilling(12));
    assert_eq!(with_term(&dir, "group_minhash", t), done_of(12));
    assert_eq!(with_term(&dir, "group_key", family1), done_of(12));
    // Partial before ready: the term finds only what the walk reached.
    assert!(done_of(12) < N / 3);
    // More, lost to a crash before its commit.
    assert_eq!(w.catch_up_counted(&mut store, 12).unwrap().backfilled, 12);
    drop(w);
    drop(index);

    let index = SearchIndex::open(&dir).unwrap();
    assert_eq!(
        index.availability("group_key"),
        backfilling(12),
        "resumed from the commit"
    );
    let mut w = index.writer(HEAP).unwrap();
    let mut steps = 0;
    while index.availability("group_minhash") != Availability::Ready {
        assert!(steps < 10, "the backfill never finished");
        w.catch_up(&mut store, 10).unwrap();
        w.commit().unwrap();
        steps += 1;
    }
    assert_eq!(steps, 2, "18 left in steps of 10");
    assert_eq!(index.availability("group_key"), Availability::Ready);
    assert_eq!(index.num_docs(), N as u64, "replaced, not duplicated");
    assert_eq!(with_term(&dir, "group_minhash", t), N / 3);
    assert_eq!(with_term(&dir, "group_key", family1), N / 3);
    assert_eq!(w.catch_up_counted(&mut store, 10).unwrap().backfilled, 0);
    drop(w);
    drop(index);
    let index = SearchIndex::open(&dir).unwrap();
    assert_eq!(index.availability("group_minhash"), Availability::Ready);
}
