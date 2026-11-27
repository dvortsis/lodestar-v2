//! X-import-verified-no: a stored torrent is indexed `verified:yes` only
//! when its info dict was hashed to its key here. One whose metadata came
//! from another indexer's database (Magnetico, bitmagnet) is
//! `verified:imported`: shown by default, left out of `verified:yes`.

use std::path::Path;

use lodestar_bencode::{OwnedValue, to_vec};
use lodestar_metainfo::Info;
use lodestar_search::query::Verified;
use lodestar_search::tokenizer::{NAME_TOKENIZER, NameTokenizer};
use lodestar_search::{Availability, SearchIndex, TorrentDoc, index_dir, parse, stored_verified};
use lodestar_store::import::{
    BatchCommitted, Completeness, Decision, ImportBatch, ImportRecord, ImportTorrent, NewImportJob,
    Phase, Privacy, Verification,
};
use lodestar_store::{Prepared, PreparedTorrent, Store, StoreConfig, WriteBatch, flags};
use tantivy::TantivyDocument;

const HEAP: usize = 15 << 20;
const NOW: i64 = 1_700_000_100;
const T: i64 = 1_700_000_000;
const PASS: Decision<'static> = Decision {
    hits: &[],
    marks: 0,
};

fn prepared(store: &Store, name: &str) -> Prepared {
    let b = |s: &[u8]| OwnedValue::Bytes(s.to_vec());
    let info = to_vec(&OwnedValue::Dict(
        [
            (b"length".to_vec(), OwnedValue::Int(5)),
            (b"name".to_vec(), b(name.as_bytes())),
            (b"piece length".to_vec(), OwnedValue::Int(16_384)),
            (b"pieces".to_vec(), b(&[0; 20])),
        ]
        .into_iter()
        .collect(),
    ));
    store
        .preparer()
        .prepare(&Info::parse(&info).unwrap(), None)
        .unwrap()
}

/// What another indexer's database gives an adapter: no info dict, so
/// no `info_size` (IMP-2, IMP-3).
fn from_database(store: &Store, name: &str) -> Prepared {
    let mut p = prepared(store, name);
    p.info_size = 0;
    p
}

/// Writes `p` the way the writer does, with `extra_flags`.
fn write(store: &mut Store, p: &Prepared, extra_flags: i64) {
    let source = store.register_source("dht", "1", T).unwrap();
    let rows = [PreparedTorrent {
        torrent: p,
        discovered_at: T,
        metadata_at: T + 1,
        extra_flags,
        source,
        verified: true,
    }];
    store
        .write(&WriteBatch {
            prepared: &rows,
            ..WriteBatch::default()
        })
        .unwrap();
}

/// An IMP-1 job of `adapter` that commits `records` as one batch.
fn import(store: &mut Store, adapter: &str, records: &[(Prepared, Completeness)]) {
    let src = store.register_source(adapter, "1", T).unwrap();
    let job = store
        .create_import_job(&NewImportJob {
            adapter,
            adapter_ver: 1,
            dataset: b"dataset",
            schema_sig: b"sig",
            options: b"{}",
            credential: None,
            at: T,
        })
        .unwrap();
    let job = store
        .set_import_phase(job.id, job.revision, Phase::Ready, T)
        .unwrap();
    let job = store
        .set_import_phase(job.id, job.revision, Phase::Reading, T + 10)
        .unwrap();
    let records: Vec<ImportRecord<'_>> = records
        .iter()
        .map(|(torrent, completeness)| {
            ImportRecord::Torrent(ImportTorrent {
                torrent,
                completeness: *completeness,
                verification: Verification::Unverified,
                privacy: Privacy::Unknown,
                repair: 0,
                source_at: Some(T - 100),
                files_dropped: 0,
                decision: PASS,
            })
        })
        .collect();
    let r = store
        .commit_import_batch(&ImportBatch {
            job: job.id,
            seq: 1,
            position: b"pos",
            source: src,
            source_bytes: 1,
            at: T + 20,
            class_gen: None,
            records: &records,
        })
        .unwrap();
    assert!(matches!(r, BatchCommitted::Committed { .. }), "{r:?}");
}

fn catch_up(index: &SearchIndex, store: &mut Store) {
    let mut w = index.writer(HEAP).unwrap();
    while w.catch_up(store, 1000).unwrap() > 0 {}
    w.commit().unwrap();
    w.finish().unwrap();
}

/// The names matching `q`, sorted.
fn names(index: &SearchIndex, q: &str) -> Vec<String> {
    names_of(index, parse(q).unwrap())
}

/// The same, as a search runs it: unverified items hidden unless asked.
fn shown(index: &SearchIndex, q: &str) -> Vec<String> {
    names_of(index, parse(q).unwrap().without_unverified())
}

fn names_of(index: &SearchIndex, node: lodestar_search::Node) -> Vec<String> {
    let hits = index.search(&index.query(&node, NOW), 1000).unwrap().hits;
    let mut out = index.names(&hits).unwrap();
    out.sort();
    out
}

/// One store with each kind of stored torrent, indexed.
fn library(root: &Path) -> (Store, SearchIndex) {
    let mut store = Store::open(root.join("s.db"), StoreConfig::default()).unwrap();
    // The crawler's: fetched and hashed.
    let p = prepared(&store, "network");
    write(&mut store, &p, 0);
    // A full `.torrent` (a torrent cache's or a refetch's): an info dict
    // hashed when stored, flagged imported.
    let p = prepared(&store, "torrent-file");
    write(&mut store, &p, flags::IMPORTED);
    // The legacy bitmagnet importer: no info dict, no import record.
    let p = from_database(&store, "bitmagnet-legacy");
    write(&mut store, &p, flags::IMPORTED | flags::FILES_PARTIAL);
    // IMP-2 and IMP-3 jobs: unverified import records. `archived-dict`
    // carries an info dict's size but its record says unverified, so only
    // the record (read by the loader) keeps it from reading as verified.
    let m = from_database(&store, "magnetico");
    let a = prepared(&store, "archived-dict");
    import(
        &mut store,
        "magnetico",
        &[
            (m, Completeness::ReportedComplete),
            (a, Completeness::ReportedComplete),
        ],
    );
    let b = from_database(&store, "bitmagnet");
    import(&mut store, "bitmagnet", &[(b, Completeness::Partial)]);
    let index = SearchIndex::open(&index_dir(root)).unwrap();
    catch_up(&index, &mut store);
    drop(index);
    // No producer indexes unverified items yet (SRC-3's decisions), so
    // play one beside the torrents, leaving the loader's documents as
    // `catch_up` wrote them.
    rewrite(&index_dir(root), &mut store, None);
    (store, SearchIndex::open(&index_dir(root)).unwrap())
}

/// Adds one unverified item named `unverified` to the torrent index
/// under `dir`. With `old_verified`, it first rewrites every torrent
/// from `store` with that `verified` value and records the field at
/// generation 1, as the build before this slice left an index; without,
/// the indexer's own documents and the payload are kept.
fn rewrite(dir: &Path, store: &mut Store, old_verified: Option<Verified>) {
    let index = tantivy::Index::open_in_dir(dir).unwrap();
    index
        .tokenizers()
        .register(NAME_TOKENIZER, NameTokenizer::default());
    index.tokenizers().register(
        lodestar_search::schema::TRIGRAM_TOKENIZER,
        lodestar_search::schema::trigram_analyzer(),
    );
    let payload = index.load_metas().unwrap().payload.unwrap();
    let payload = match old_verified {
        Some(_) => payload.replace("verified=2", "verified=1"),
        None => payload,
    };
    let (_, f) = lodestar_search::schema::build();
    let mut w = index.writer::<TantivyDocument>(HEAP).unwrap();
    let ids = store.ids_after(0, 1000).unwrap();
    if let Some(v) = old_verified {
        w.delete_all_documents().unwrap();
        for id in &ids {
            let row = store.torrent(*id).unwrap().unwrap();
            let tree = store.files(*id).unwrap().unwrap();
            let mut doc = TorrentDoc::new(&row, &tree);
            doc.provenance.verified = v;
            w.add_document(doc.to_document(&f)).unwrap();
        }
    }
    let row = store.torrent(ids[0]).unwrap().unwrap();
    let tree = store.files(ids[0]).unwrap().unwrap();
    let mut doc = TorrentDoc::new(&row, &tree);
    doc.id = 9_999;
    doc.name = "unverified".into();
    doc.provenance.verified = Verified::No;
    w.add_document(doc.to_document(&f)).unwrap();
    let mut prepared = w.prepare_commit().unwrap();
    prepared.set_payload(&payload);
    prepared.commit().unwrap();
}

const IMPORTS: [&str; 4] = [
    "archived-dict",
    "bitmagnet",
    "bitmagnet-legacy",
    "magnetico",
];
const VERIFIED: [&str; 2] = ["network", "torrent-file"];
const ALL: [&str; 6] = [
    "archived-dict",
    "bitmagnet",
    "bitmagnet-legacy",
    "magnetico",
    "network",
    "torrent-file",
];

/// Every kind, the unverified item too.
const EVERYTHING: [&str; 7] = [
    "archived-dict",
    "bitmagnet",
    "bitmagnet-legacy",
    "magnetico",
    "network",
    "torrent-file",
    "unverified",
];

#[test]
fn each_kind_of_stored_torrent_indexes_its_own_value() {
    let tmp = tempfile::tempdir().unwrap();
    let (_store, index) = library(tmp.path());
    // As the live loader (`torrent_doc`) wrote them.
    assert_eq!(names(&index, "verified:yes"), VERIFIED);
    assert_eq!(names(&index, "verified:imported"), IMPORTS);
    assert_eq!(names(&index, "verified:no"), ["unverified"]);
    assert_eq!(
        names(&index, "-verified:imported"),
        ["network", "torrent-file", "unverified"]
    );
    assert_eq!(names(&index, "verified:yes,imported"), ALL);
    assert_eq!(names(&index, "verified:imported size:5"), IMPORTS);
}

/// Every list of values, plain and negated, against every kind: a list
/// matches exactly the documents holding one of its values.
#[test]
fn every_list_of_values_matches_exactly_its_kinds() {
    let tmp = tempfile::tempdir().unwrap();
    let (_store, index) = library(tmp.path());
    let kind = |name: &str| match name {
        "network" | "torrent-file" => Verified::Yes,
        "unverified" => Verified::No,
        _ => Verified::Imported,
    };
    for mask in 1..8u8 {
        let any: Vec<Verified> = Verified::ALL
            .into_iter()
            .enumerate()
            .filter(|(i, _)| mask >> i & 1 == 1)
            .map(|(_, v)| v)
            .collect();
        let list: Vec<&str> = any.iter().map(|v| v.as_str()).collect();
        let q = format!("verified:{}", list.join(","));
        let want: Vec<&str> = EVERYTHING
            .into_iter()
            .filter(|n| any.contains(&kind(n)))
            .collect();
        let not: Vec<&str> = EVERYTHING
            .into_iter()
            .filter(|n| !any.contains(&kind(n)))
            .collect();
        assert_eq!(names(&index, &q), want, "{q}");
        assert_eq!(names(&index, &format!("-{q}")), not, "-{q}");
        assert_eq!(names(&index, &format!("item | {q}")), want, "under |");
    }
}

#[test]
fn imports_are_shown_by_default_and_with_unverified() {
    let tmp = tempfile::tempdir().unwrap();
    let (_store, index) = library(tmp.path());
    // The default clause hides only `verified:no`.
    assert_eq!(shown(&index, ""), ALL);
    assert_eq!(shown(&index, "verified:no"), ["unverified"], "asked for");
    assert_eq!(
        parse("x").unwrap().without_unverified().to_query(),
        "x verified:yes,imported"
    );
    // "Show unverified" shows all three.
    assert_eq!(shown(&index, "verified:yes,imported,no"), EVERYTHING);
    // An older "Show unverified" query, as typed, leaves imports out.
    assert_eq!(
        shown(&index, "verified:yes,no"),
        ["network", "torrent-file", "unverified"]
    );
    // Asking for one value is left as asked.
    assert_eq!(shown(&index, "verified:yes"), VERIFIED);
    assert_eq!(shown(&index, "verified:imported"), IMPORTS);
}

#[test]
fn a_network_fetch_verifies_a_partial_import_at_its_next_index_write() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut store, index) = library(tmp.path());
    // The crawler fetches the bitmagnet job's partial import: the store
    // upgrades it (`upgrade_partial`) and writes its record as verified.
    let fetched = prepared(&store, "bitmagnet");
    write(&mut store, &fetched, 0);
    catch_up(&index, &mut store);
    assert_eq!(
        names(&index, "verified:yes"),
        ["bitmagnet", "network", "torrent-file"]
    );
    assert_eq!(
        names(&index, "verified:imported"),
        ["archived-dict", "bitmagnet-legacy", "magnetico"]
    );
}

/// Every flag combination that bears on it, with and without an import
/// record: `yes` exactly when the row isn't imported, the record says
/// verified, or there's no record and the row has an info dict.
#[test]
fn stored_verified_over_every_combination() {
    let records = [
        None,
        Some(Verification::Unknown),
        Some(Verification::Unverified),
        Some(Verification::Verified),
        Some(Verification::Conflicting),
    ];
    let others = [
        0,
        flags::FILES_PARTIAL,
        flags::PRIVATE,
        flags::REMOTE_UNVERIFIED,
        flags::BACKFILL | flags::V2,
    ];
    for imported in [false, true] {
        for other in others {
            for info_size in [0, 1, 4096] {
                for record in records {
                    let row_flags = other | if imported { flags::IMPORTED } else { 0 };
                    let want = if !imported
                        || record == Some(Verification::Verified)
                        || (record.is_none() && info_size > 0)
                    {
                        Verified::Yes
                    } else {
                        Verified::Imported
                    };
                    assert_eq!(
                        stored_verified(row_flags, info_size, record),
                        want,
                        "flags {row_flags:#x}, info_size {info_size}, {record:?}"
                    );
                }
            }
        }
    }
}

/// An index the build before this slice made says `yes` for every
/// torrent, imports included. Opening it with this build walks the
/// `verified` field: meanwhile the default search and lists that take
/// `yes` and `imported` together stay exact, a list with one of them is
/// reported partial, and a restart resumes from the last commit.
#[test]
fn an_older_index_rewrites_imports_to_imported_across_a_restart() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = index_dir(tmp.path());
    let (mut store, index) = library(tmp.path());
    drop(index);
    rewrite(&dir, &mut store, Some(Verified::Yes));
    let old = lodestar_search::Generations::current().with("verified", 1);
    let index = SearchIndex::open(&dir)
        .unwrap()
        .with_generations(old)
        .unwrap();
    assert_eq!(index.availability("verified"), Availability::Ready);
    assert_eq!(names(&index, "verified:yes"), ALL, "the old claim");
    drop(index);

    let index = SearchIndex::open(&dir).unwrap();
    assert!(
        matches!(
            index.availability("verified"),
            Availability::Backfilling { done: 0, .. }
        ),
        "{:?}",
        index.availability("verified")
    );
    let exact = |index: &SearchIndex| {
        assert_eq!(shown(index, ""), ALL);
        assert_eq!(names(index, "verified:yes,imported"), ALL);
        assert_eq!(names(index, "verified:no"), ["unverified"]);
        assert_eq!(shown(index, "verified:yes,imported,no"), EVERYTHING);
    };
    exact(&index);
    for (q, partial) in [
        ("verified:yes", true),
        ("verified:imported", true),
        ("verified:yes,imported", false),
        ("verified:no", false),
    ] {
        let fields = parse(q).unwrap().population_fields();
        assert_eq!(fields.contains(&"verified"), partial, "{q}");
    }

    let mut w = index.writer(HEAP).unwrap();
    assert_eq!(w.catch_up_counted(&mut store, 2).unwrap().backfilled, 2);
    w.commit().unwrap();
    exact(&index);
    // More, lost to a crash before its commit.
    assert_eq!(w.catch_up_counted(&mut store, 2).unwrap().backfilled, 2);
    drop(w);
    drop(index);

    let index = SearchIndex::open(&dir).unwrap();
    assert!(
        matches!(
            index.availability("verified"),
            Availability::Backfilling { done: 2, .. }
        ),
        "resumed from the commit: {:?}",
        index.availability("verified")
    );
    exact(&index);
    let mut w = index.writer(HEAP).unwrap();
    let mut steps = 0;
    while index.availability("verified") != Availability::Ready {
        assert!(steps < 10, "the backfill never finished");
        w.catch_up(&mut store, 2).unwrap();
        w.commit().unwrap();
        exact(&index);
        steps += 1;
    }
    w.finish().unwrap();
    assert_eq!(names(&index, "verified:yes"), VERIFIED);
    assert_eq!(names(&index, "verified:imported"), IMPORTS);
}
