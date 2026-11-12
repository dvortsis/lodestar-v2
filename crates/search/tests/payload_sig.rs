//! GS-0: the `payload_sig` producer and `sig:` against a real store,
//! and the field's backfill under the population contract
//! (`docs/decisions/2026-11-04-SB-6.md`): an index built before GS-0
//! (SB-6's schema, then an import, with no producer), then this build,
//! with a restart mid-walk. `sig:` never answers "no match" for a
//! torrent the walk hasn't reached without the search saying so.

use std::path::Path;

use lodestar_bencode::{OwnedValue, to_vec};
use lodestar_metainfo::Info;
use lodestar_search::tokenizer::{NAME_TOKENIZER, NameTokenizer};
use lodestar_search::{Availability, SearchIndex, index_dir, parse, query::sig_hex};
use lodestar_store::{NewTorrent, SourceId, Store, StoreConfig, flags};
use tantivy::TantivyDocument;

const HEAP: usize = 15 << 20;
const NOW: i64 = 1_700_000_100;
const N: usize = 30;
/// Torrents fall into this many payloads, each under many names.
const PAYLOADS: usize = 3;

/// Torrent `i`'s content file sizes: payload `i % PAYLOADS`. Every one
/// is under its own name, every other one lists its files in reverse,
/// and every fifth also has a BEP 47 padding file, so only the sizes
/// make the payload.
fn sizes(i: usize) -> Vec<i64> {
    let g = (i % PAYLOADS) as i64;
    let s = vec![1000 + g, 2000 + g, 3000];
    if i % 2 == 1 {
        s.into_iter().rev().collect()
    } else {
        s
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
    let mut files: Vec<OwnedValue> = sizes(i)
        .into_iter()
        .enumerate()
        .map(|(j, len)| {
            d(vec![
                ("length", OwnedValue::Int(len)),
                (
                    "path",
                    OwnedValue::List(vec![b(&format!("part{i}-{j}.bin"))]),
                ),
            ])
        })
        .collect();
    if i.is_multiple_of(5) {
        files.insert(
            1,
            d(vec![
                ("attr", b("p")),
                ("length", OwnedValue::Int(500)),
                (
                    "path",
                    OwnedValue::List(vec![b(".pad"), b(&format!("{i}"))]),
                ),
            ]),
        );
    }
    to_vec(&d(vec![
        ("files", OwnedValue::List(files)),
        ("name", b(&format!("Payload.Item.{i}"))),
        ("piece length", OwnedValue::Int(1 << 16)),
        ("pieces", OwnedValue::Bytes(vec![0; 20])),
    ]))
}

/// Torrent 0 (payload 0) is hidden by a filter rule.
fn store(root: &Path) -> (Store, Vec<Info>) {
    let mut store = Store::open(root.join("s.db"), StoreConfig::default()).unwrap();
    store.register_source("dht", "1", 1).unwrap();
    let raws: Vec<Vec<u8>> = (0..N).map(torrent).collect();
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
            extra_flags: if i == 0 { flags::HIDDEN } else { 0 },
            source: SourceId::new(0).unwrap(),
        })
        .collect();
    store.insert(&batch).unwrap();
    (store, infos)
}

/// Payload `g`'s signature, as `lodestar_classify` computes it from a
/// parsed info dict (the function filter's CS-8a reads too).
fn sig_of(infos: &[Info], g: usize) -> u64 {
    let info = &infos[g];
    let files = lodestar_classify::file_infos(&info.files, info.path_encoding);
    lodestar_classify::payload::payload_sig(&files).unwrap()
}

fn hash(infos: &[Info], i: usize) -> String {
    infos[i].v1.unwrap().to_string()
}

fn count(index: &SearchIndex, q: &str) -> usize {
    index
        .search(&index.query(&parse(q).unwrap(), NOW), 1000)
        .unwrap()
        .hits
        .len()
}

/// `q` as a search with hidden torrents left out, and, with `adult`
/// false, Adult too: the visibility clauses `/search` adds.
fn visible(index: &SearchIndex, q: &str, adult: bool) -> usize {
    let node = parse(q).unwrap().without_hidden().without_unverified();
    let node = if adult {
        node
    } else {
        lodestar_search::with_filters(node, &[parse("-type:adult").unwrap()])
    };
    index
        .search(&index.query(&node, NOW), 1000)
        .unwrap()
        .hits
        .len()
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
fn every_torrent_carries_its_payload_signature() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut store, infos) = store(tmp.path());
    let index = complete_index(tmp.path(), &mut store);
    assert_eq!(index.availability("payload_sig"), Availability::Ready);
    let sigs: Vec<u64> = (0..PAYLOADS).map(|g| sig_of(&infos, g)).collect();
    // Distinct payloads, each shared by its torrents whatever their
    // names, file order or padding.
    assert!(sigs[0] != sigs[1] && sigs[1] != sigs[2] && sigs[0] != sigs[2]);
    for (g, sig) in sigs.iter().enumerate() {
        for i in (g..N).step_by(PAYLOADS) {
            assert_eq!(sig_of(&infos, i), *sig, "torrent {i}");
        }
        let q = format!("sig:{}", sig_hex(*sig));
        assert_eq!(count(&index, &q), N / PAYLOADS, "{q}");
        // Upper case reads the same.
        assert_eq!(count(&index, &q.to_uppercase().replace("SIG", "sig")), 10);
    }
    let (a, b) = (sig_hex(sigs[0]), sig_hex(sigs[1]));
    assert_eq!(count(&index, &format!("sig:{a} | sig:{b}")), 20);
    assert_eq!(count(&index, &format!("-sig:{a}")), 20);
    assert_eq!(count(&index, &format!("item sig:{a}")), 10);
    assert_eq!(count(&index, &format!("sig:{a} sig:{b}")), 0);
    // A signature nothing has.
    assert_eq!(count(&index, "sig:0123456789abcdef"), 0);

    // "Same payload as this torrent": by its info hash, itself included.
    for i in [0, 1, 7, 29] {
        let q = format!("sig:{}", hash(&infos, i));
        assert_eq!(count(&index, &q), 10, "{q}");
        let same = format!("sig:{} sig:{}", hash(&infos, i), sig_hex(sig_of(&infos, i)));
        assert_eq!(count(&index, &same), 10, "{same}");
    }
    let magnet = format!("sig:magnet:?xt=urn:btih:{}&dn=x", hash(&infos, 4));
    assert_eq!(count(&index, &magnet), 10);
    // A hash no torrent has matches nothing.
    assert_eq!(
        count(&index, "sig:0123456789abcdef0123456789abcdef01234567"),
        0
    );
    // A query that names `sig:` reports the field, so the API says when
    // it's partial ([`lodestar_search::Node::population_fields`]).
    assert_eq!(
        parse(&format!("sig:{a}")).unwrap().population_fields(),
        ["payload_sig"]
    );
}

/// Rewrites the torrent index under `dir` as a build from before GS-0
/// left it: no document has `payload_sig`, and the payload records the
/// field at generation 0 (no producer).
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
            "payload_sig" => "payload_sig=0".to_owned(),
            _ => e.to_owned(),
        })
        .collect::<Vec<_>>()
        .join(",");
    assert!(old_pop.contains("payload_sig=0"), "{old_pop}");
    let (_, f) = lodestar_search::schema::build();
    let mut w = index.writer::<TantivyDocument>(HEAP).unwrap();
    w.delete_all_documents().unwrap();
    for id in store.ids_after(0, 100_000).unwrap() {
        let row = store.torrent(id).unwrap().unwrap();
        let tree = store.files(id).unwrap().unwrap();
        let mut doc = lodestar_search::TorrentDoc::new(&row, &tree);
        assert!(doc.payload_sig.is_some());
        doc.payload_sig = None;
        w.add_document(doc.to_document(&f)).unwrap();
    }
    let mut prepared = w.prepare_commit().unwrap();
    prepared.set_payload(&format!("{watermarks};pop:{old_pop}"));
    prepared.commit().unwrap();
}

#[test]
fn the_backfill_resumes_after_a_restart_and_reports_sig_partial_meanwhile() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = index_dir(tmp.path());
    let (mut store, infos) = store(tmp.path());
    drop(complete_index(tmp.path(), &mut store));
    unpopulate(&dir, &mut store);
    let sig0 = sig_hex(sig_of(&infos, 0));
    let by_sig = format!("sig:{sig0}");
    // Torrent 3 (id 4): payload 0, which the walk reaches in its first
    // batch; torrent 27 (id 28) is past the crash.
    let (early, late) = (
        format!("sig:{}", hash(&infos, 3)),
        format!("sig:{}", hash(&infos, 27)),
    );
    // Payload 0's torrents with ids in (0, done].
    let done_of = |done: usize| (0..done).filter(|i| i.is_multiple_of(PAYLOADS)).count();

    // The build from before GS-0: no producer, so nothing to walk, and
    // `sig:` finds nothing a search could trust.
    let old = lodestar_search::Generations::current().with("payload_sig", 0);
    let index = SearchIndex::open(&dir)
        .unwrap()
        .with_generations(old)
        .unwrap();
    assert_eq!(index.availability("payload_sig"), Availability::NoProducer);
    assert_eq!(count(&index, &by_sig), 0);
    drop(index);

    // This build: the field walks the 30 torrents.
    let index = SearchIndex::open(&dir).unwrap();
    let backfilling = |done| Availability::Backfilling {
        done,
        upto: N as i64,
    };
    assert_eq!(index.availability("payload_sig"), backfilling(0));
    assert!(
        index.unpopulated().iter().any(|f| f.field == "payload_sig"),
        "reported partial"
    );
    assert_eq!(count(&index, &by_sig), 0, "no document carries it yet");
    // By info hash: a torrent the walk hasn't reached has no signature
    // yet. Not an error: it matches itself, never nothing, and the
    // search reports the field partial (what `/search` returns as
    // `incomplete_fields`).
    assert_eq!(count(&index, &early), 1);
    assert_eq!(count(&index, &late), 1);
    let partial = |q: &str| {
        parse(q)
            .unwrap()
            .population_fields()
            .into_iter()
            .map(|f| index.availability(f))
            .collect::<Vec<_>>()
    };
    assert_eq!(partial(&late), [backfilling(0)]);
    assert_eq!(partial(&format!("item -{by_sig}")), [backfilling(0)]);
    // An info hash no torrent has: empty, still not an error.
    let unknown = "sig:0123456789abcdef0123456789abcdef01234567";
    assert_eq!(count(&index, unknown), 0);
    assert_eq!(partial(unknown), [backfilling(0)]);

    let mut w = index.writer(HEAP).unwrap();
    let c = w.catch_up_counted(&mut store, 12).unwrap();
    assert_eq!((c.added, c.replayed, c.backfilled), (0, 0, 12));
    w.commit().unwrap();
    assert_eq!(index.availability("payload_sig"), backfilling(12));
    assert_eq!(count(&index, &by_sig), done_of(12));
    assert_eq!(count(&index, &early), done_of(12));
    // Its own document doesn't carry its signature yet, so there's
    // nothing to resolve: itself only, and the search says it's partial.
    assert_eq!(count(&index, &late), 1);
    // More, lost to a crash before its commit.
    assert_eq!(w.catch_up_counted(&mut store, 12).unwrap().backfilled, 12);
    drop(w);
    drop(index);

    let index = SearchIndex::open(&dir).unwrap();
    assert_eq!(
        index.availability("payload_sig"),
        backfilling(12),
        "resumed from the commit"
    );
    assert_eq!(count(&index, &by_sig), done_of(12));
    // A lease on this mid-backfill snapshot (a search's later pages).
    let lease = lodestar_search::Lease::new(
        std::sync::Arc::new(index.clone()),
        &parse(&by_sig).unwrap(),
        NOW,
        lodestar_search::Sort::Relevance,
        &lodestar_search::CancelToken::new(),
    )
    .unwrap();
    let mut w = index.writer(HEAP).unwrap();
    let mut steps = 0;
    while index.availability("payload_sig") != Availability::Ready {
        assert!(steps < 10, "the backfill never finished");
        w.catch_up(&mut store, 10).unwrap();
        w.commit().unwrap();
        steps += 1;
    }
    assert_eq!(steps, 2, "18 left in steps of 10");
    assert_eq!(
        index.availability_in("payload_sig", &lease),
        backfilling(22),
        "the lease is still told it's partial"
    );
    assert!(index.unpopulated().iter().all(|f| f.field != "payload_sig"));
    assert_eq!(index.num_docs(), N as u64, "replaced, not duplicated");
    assert_eq!(count(&index, &by_sig), 10);
    assert_eq!(count(&index, &early), 10);
    assert_eq!(count(&index, &late), 10);
    // Recorded: a restart has nothing left to do.
    assert_eq!(w.catch_up_counted(&mut store, 10).unwrap().backfilled, 0);
    drop(w);
    drop(index);
    let index = SearchIndex::open(&dir).unwrap();
    assert_eq!(index.availability("payload_sig"), Availability::Ready);
    assert_eq!(count(&index, &by_sig), 10);
}

/// Adds an Adult torrent with payload 0 under its own hash, a renamed
/// clone of torrents that aren't Adult, to the index under `dir`, and
/// returns its hash.
fn add_adult_clone(dir: &Path, store: &mut Store) -> &'static str {
    let adult_hash = "a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0";
    {
        let raw = tantivy::Index::open_in_dir(dir).unwrap();
        raw.tokenizers()
            .register(NAME_TOKENIZER, NameTokenizer::default());
        raw.tokenizers().register(
            lodestar_search::schema::TRIGRAM_TOKENIZER,
            lodestar_search::schema::trigram_analyzer(),
        );
        let payload = raw.load_metas().unwrap().payload.unwrap();
        let row = store.torrent(4).unwrap().unwrap();
        let tree = store.files(4).unwrap().unwrap();
        let mut doc = lodestar_search::TorrentDoc::new(&row, &tree);
        doc.id = 99;
        doc.hashes = vec![adult_hash.parse().unwrap()];
        doc.enriched.tags = vec!["type:adult".into()];
        let (_, f) = lodestar_search::schema::build();
        let mut w = raw.writer::<TantivyDocument>(HEAP).unwrap();
        w.add_document(doc.to_document(&f)).unwrap();
        let mut prepared = w.prepare_commit().unwrap();
        prepared.set_payload(&payload);
        prepared.commit().unwrap();
    }
    adult_hash
}

/// B13's rule: a torrent the request may not see looks exactly like one
/// the server doesn't have. `sig:` by its hash must not find its clones.
#[test]
fn a_withheld_reference_answers_as_an_unknown_hash() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = index_dir(tmp.path());
    let (mut store, infos) = store(tmp.path());
    drop(complete_index(tmp.path(), &mut store));
    let adult_hash = add_adult_clone(&dir, &mut store);
    let index = SearchIndex::open(&dir).unwrap();
    let unknown = "sig:0123456789abcdef0123456789abcdef01234567";
    let hidden = format!("sig:{}", hash(&infos, 0));
    let adult = format!("sig:{adult_hash}");
    // Payload 0: torrents 0, 3, …, 27 (0 hidden) and the Adult one.
    for (q, adult_shown) in [
        (hidden.clone(), true),
        (hidden.clone(), false),
        (adult.clone(), false),
    ] {
        for shape in ["{q}", "-{q}", "x | {q}", "item {q}"] {
            let with = shape.replace("{q}", &q);
            let without = shape.replace("{q}", unknown);
            assert_eq!(
                visible(&index, &with, adult_shown),
                visible(&index, &without, adult_shown),
                "{with} (adult shown: {adult_shown})"
            );
        }
    }
    assert_eq!(visible(&index, &hidden, true), 0);
    // A reference the request may see finds the visible clones only.
    let shown = format!("sig:{}", hash(&infos, 3));
    assert_eq!(
        visible(&index, &shown, false),
        9,
        "not the hidden one or Adult"
    );
    assert_eq!(visible(&index, &shown, true), 10, "with the Adult clone");
    // Asked for hidden torrents, or Adult shown: the reference is visible.
    assert_eq!(count(&index, &hidden), 11);
    assert_eq!(
        visible(&index, &adult, true),
        10,
        "itself and 9 visible clones"
    );
}

/// A lease pages the answer of its own snapshot for both forms, while a
/// search taken after the walk completes sees the whole field.
#[test]
fn a_lease_from_mid_walk_keeps_its_answer() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = index_dir(tmp.path());
    let (mut store, infos) = store(tmp.path());
    drop(complete_index(tmp.path(), &mut store));
    unpopulate(&dir, &mut store);
    let index = SearchIndex::open(&dir).unwrap();
    let mut w = index.writer(HEAP).unwrap();
    assert_eq!(w.catch_up_counted(&mut store, 12).unwrap().backfilled, 12);
    w.commit().unwrap();
    let late = format!("sig:{}", hash(&infos, 27));
    let by_sig = format!("sig:{}", sig_hex(sig_of(&infos, 0)));
    let cancel = lodestar_search::CancelToken::new();
    let lease = |q: &str| {
        lodestar_search::Lease::new(
            std::sync::Arc::new(index.clone()),
            &parse(q).unwrap(),
            NOW,
            lodestar_search::Sort::Relevance,
            &cancel,
        )
        .unwrap()
    };
    let (late_lease, sig_lease) = (lease(&late), lease(&by_sig));
    while w.catch_up(&mut store, 10).unwrap() > 0 {
        w.commit().unwrap();
    }
    w.commit().unwrap();
    assert_eq!(index.availability("payload_sig"), Availability::Ready);
    // Ids 1, 4, 7, 10 carry payload 0's signature in the lease's snapshot.
    assert_eq!(late_lease.exact_total(&cancel).unwrap().value, 1);
    assert_eq!(sig_lease.exact_total(&cancel).unwrap().value, 4);
    assert_eq!(lease(&late).exact_total(&cancel).unwrap().value, 10);
    assert_eq!(lease(&by_sig).exact_total(&cancel).unwrap().value, 10);
}

/// Saved-search alerts run without a request's visibility (B8), so a
/// `sig:` reference there must pass the default visibility: a withheld
/// one alerts exactly as an unknown hash does.
#[test]
fn alerts_never_resolve_a_withheld_reference() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut store, infos) = store(tmp.path());
    drop(complete_index(tmp.path(), &mut store));
    let adult = format!(
        "sig:{}",
        add_adult_clone(&index_dir(tmp.path()), &mut store)
    );
    let index = SearchIndex::open(&index_dir(tmp.path())).unwrap();
    let alerts = |q: &str| {
        let node = parse(q).unwrap();
        let due = [lodestar_search::Due {
            id: 1,
            watermark: 0,
            query: &node,
            utc_offset: 0,
            count_only: false,
            stems: false,
            stoppable: false,
        }];
        let limits = lodestar_search::PercolateLimits {
            keep: 1000,
            max_range: 1000,
        };
        let cancel = lodestar_search::CancelToken::new();
        let run = lodestar_search::percolate(&index, &due, i64::MAX, limits, NOW, &cancel);
        run.unwrap().matched[0].count
    };
    let unknown = "sig:0123456789abcdef0123456789abcdef01234567";
    assert_eq!(alerts(unknown), 0);
    // Torrent 0 is hidden: as unknown, negated too.
    let hidden = format!("sig:{}", hash(&infos, 0));
    assert_eq!(alerts(&hidden), alerts(unknown));
    assert_eq!(
        alerts(&format!("-{hidden}")),
        alerts(&format!("-{unknown}"))
    );
    // An Adult one too: alerts don't know the account shows Adult, so
    // the strictest holds.
    assert_eq!(alerts(&adult), alerts(unknown));
    // A visible reference alerts on its visible clones.
    assert_eq!(alerts(&format!("sig:{}", hash(&infos, 3))), 9);
    // The 16-digit form names no torrent and runs as saved: every match,
    // the hidden and Adult ones included (left out when alerts are read).
    assert_eq!(alerts(&format!("sig:{}", sig_hex(sig_of(&infos, 0)))), 11);
}

/// While the backfill hasn't reached a `sig:<info hash>` reference, a
/// saved search with it isn't run: it keeps its watermark, so a clone
/// indexed meanwhile still alerts once the field is complete.
#[test]
fn an_alert_waits_for_the_backfill_and_loses_no_clone() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = index_dir(tmp.path());
    let (mut store, infos) = store(tmp.path());
    drop(complete_index(tmp.path(), &mut store));
    unpopulate(&dir, &mut store);
    let index = SearchIndex::open(&dir).unwrap();
    let percolate = |q: &str, watermark: i64| {
        let node = parse(q).unwrap();
        let due = [lodestar_search::Due {
            id: 1,
            watermark,
            query: &node,
            utc_offset: 0,
            count_only: false,
            stems: false,
            stoppable: false,
        }];
        let limits = lodestar_search::PercolateLimits {
            keep: 1000,
            max_range: 1000,
        };
        let cancel = lodestar_search::CancelToken::new();
        lodestar_search::percolate(&index, &due, i64::MAX, limits, NOW, &cancel)
            .unwrap()
            .matched
    };
    // Torrent 27 (id 28): payload 0, past where the walk will be.
    let late = format!("sig:{}", hash(&infos, 27));
    let bare = format!("sig:{}", sig_hex(sig_of(&infos, 0)));
    let mut w = index.writer(HEAP).unwrap();
    assert_eq!(w.catch_up_counted(&mut store, 12).unwrap().backfilled, 12);
    w.commit().unwrap();
    assert!(matches!(
        index.availability("payload_sig"),
        Availability::Backfilling { .. }
    ));
    // Mid-backfill, watermark 1: not run, so the watermark stays.
    assert!(percolate(&late, 1).is_empty(), "not run while backfilling");
    // A bare signature looks nothing up and runs (ids past the walk lack
    // it here only because this test unpopulated them).
    let ran = percolate(&bare, 1);
    assert_eq!(ran.len(), 1);
    assert_eq!(ran[0].through, N as i64);
    while w.catch_up(&mut store, 10).unwrap() > 0 {
        w.commit().unwrap();
    }
    w.commit().unwrap();
    assert_eq!(index.availability("payload_sig"), Availability::Ready);
    // From the same watermark: every visible clone after it, 28 included.
    let ran = percolate(&late, 1);
    assert_eq!(ran.len(), 1);
    assert_eq!(ran[0].through, N as i64);
    assert!(ran[0].torrent_ids.contains(&28), "{:?}", ran[0].torrent_ids);
    // Ids 4, 7, …, 28 (id 1 is hidden and below the watermark anyway).
    assert_eq!(ran[0].count, 9);
}
