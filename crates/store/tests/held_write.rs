//! The write path's holds (FILTER-15): a held decision over a torrent
//! already stored deletes it in the hold's own transaction, a write that
//! fails leaves neither, a held hash is never queued for a refetch, and
//! the one-time sweep deletes what an earlier write left stored under a
//! held hash.

use lodestar_bencode::{OwnedValue, to_vec};
use lodestar_metainfo::Info;
use lodestar_store::{
    Filtered, HELD_SWEEP_KEY, Held, NewTorrent, SourceId, Store, StoreConfig, WriteBatch, flags,
};

fn info(name: &str, v2: bool) -> Vec<u8> {
    let b = |s: &[u8]| OwnedValue::Bytes(s.to_vec());
    let mut entries = vec![
        (b"length".to_vec(), OwnedValue::Int(100)),
        (b"name".to_vec(), b(name.as_bytes())),
        (b"piece length".to_vec(), OwnedValue::Int(16_384)),
        (b"pieces".to_vec(), b(&[0; 20])),
    ];
    if v2 {
        let leaf = OwnedValue::Dict(
            [(
                b"".to_vec(),
                OwnedValue::Dict(
                    [
                        (b"length".to_vec(), OwnedValue::Int(100)),
                        (b"pieces root".to_vec(), b(&[5; 32])),
                    ]
                    .into_iter()
                    .collect(),
                ),
            )]
            .into_iter()
            .collect(),
        );
        entries.push((
            b"file tree".to_vec(),
            OwnedValue::Dict([(name.as_bytes().to_vec(), leaf)].into_iter().collect()),
        ));
        entries.push((b"meta version".to_vec(), OwnedValue::Int(2)));
    }
    to_vec(&OwnedValue::Dict(entries.into_iter().collect()))
}

/// Stores a torrent named `name`: its id, primary hash and, for a hybrid,
/// its truncated v2 hash.
fn stored(store: &mut Store, name: &str, v2: bool, extra_flags: i64) -> (i64, [u8; 20], [u8; 20]) {
    let raw = info(name, v2);
    let info = Info::parse(&raw).unwrap();
    let id = store
        .insert(&[NewTorrent {
            info: &info,
            raw_info: Some(&raw),
            discovered_at: 1,
            metadata_at: 1,
            extra_flags,
            source: SourceId::new(0).unwrap(),
        }])
        .unwrap()[0]
        .id();
    let primary = info.primary_hash().0;
    let other = info.v2.map_or(primary, |v| v.truncated().0);
    (id, primary, other)
}

fn hold<'a>(hashes: &'a [[u8; 20]], held: Option<Held>) -> Filtered<'a> {
    Filtered {
        hashes,
        rule: "U-1",
        action: 2,
        held,
        sealed: None,
        evidence: &[],
        at: 10,
        expires_at: 20,
        adult_hold: false,
    }
}

fn is_stored(store: &Store, id: i64) -> bool {
    store.torrent(id).unwrap().is_some()
}

fn reindexed(store: &Store) -> Vec<i64> {
    store
        .reindex_after(0, 100)
        .unwrap()
        .into_iter()
        .map(|(_, id)| id)
        .collect()
}

/// A held decision over a stored torrent, under either of its swarm
/// hashes, deletes it with the hold: the id comes back, the search
/// indexer is told, and nothing is found under the hash once the write
/// returns. A decision that doesn't hold leaves it stored.
#[test]
fn a_held_decision_deletes_the_stored_torrent_with_its_hold() {
    let mut store = Store::open_in_memory(StoreConfig::default()).unwrap();
    let (dropped, dropped_hash, _) = stored(&mut store, "a", false, 0);
    let (quarantined, _, v2_hash) = stored(&mut store, "b", true, 0);
    let (marked, marked_hash, _) = stored(&mut store, "c", false, 0);
    let before = reindexed(&store);
    let written = store
        .write(&WriteBatch {
            filtered: &[
                hold(&[dropped_hash], Some(Held::Tombstone)),
                hold(&[v2_hash], Some(Held::Quarantine)),
                hold(&[marked_hash], None),
            ],
            ..Default::default()
        })
        .unwrap();
    assert_eq!(written.deleted, [dropped, quarantined]);
    assert!(!is_stored(&store, dropped) && !is_stored(&store, quarantined));
    assert!(is_stored(&store, marked));
    for h in [dropped_hash, v2_hash] {
        assert_eq!(store.find(&lodestar_metainfo::InfoHash(h)).unwrap(), None);
    }
    let after = reindexed(&store);
    assert_eq!(&after[before.len()..], [dropped, quarantined]);
    assert_eq!(
        store.quarantine_rule(&v2_hash).unwrap().as_deref(),
        Some("U-1")
    );
}

/// A held import's row with a partial file list is deleted like any
/// other.
#[test]
fn a_partial_row_under_a_held_hash_is_deleted() {
    let mut store = Store::open_in_memory(StoreConfig::default()).unwrap();
    let (id, hash, _) = stored(&mut store, "a", false, flags::FILES_PARTIAL);
    let written = store
        .write(&WriteBatch {
            filtered: &[hold(&[hash], Some(Held::Tombstone))],
            ..Default::default()
        })
        .unwrap();
    assert_eq!(written.deleted, [id]);
    assert!(!is_stored(&store, id));
}

/// A write that fails after the delete leaves the torrent stored, no
/// hold and no re-index entry: the hold and the delete commit together or
/// not at all.
#[test]
fn a_failed_write_keeps_neither_the_hold_nor_the_delete() {
    let mut store = Store::open_in_memory(StoreConfig::default()).unwrap();
    let (id, hash, _) = stored(&mut store, "a", false, 0);
    let before = reindexed(&store);
    // Fails the transaction once the torrent is deleted, after the hold.
    store
        .connection()
        .execute_batch(
            "CREATE TEMP TRIGGER fail_after_delete AFTER DELETE ON torrents
             BEGIN SELECT RAISE(ABORT, 'test'); END",
        )
        .unwrap();
    let r = store.write(&WriteBatch {
        filtered: &[hold(&[hash], Some(Held::Tombstone))],
        ..Default::default()
    });
    assert!(r.is_err());
    assert!(is_stored(&store, id));
    assert_eq!(reindexed(&store), before);
    let tombstones: i64 = store
        .connection()
        .query_row("SELECT count(*) FROM tombstones", [], |r| r.get(0))
        .unwrap();
    assert_eq!(tombstones, 0);
}

/// A partial row under a held hash is never queued for a refetch, and the
/// rest still are.
#[test]
fn a_held_hash_is_never_refetched() {
    let mut store = Store::open_in_memory(StoreConfig::default()).unwrap();
    let (_, held, _) = stored(&mut store, "a", false, flags::FILES_PARTIAL);
    let (_, _, held_v2) = stored(&mut store, "b", true, flags::FILES_PARTIAL);
    let (_, free, _) = stored(&mut store, "c", false, flags::FILES_PARTIAL);
    let c = store.connection();
    c.execute(
        "INSERT INTO tombstones (hash, rule, at) VALUES (?1, 'U-1', 1)",
        [&held[..]],
    )
    .unwrap();
    c.execute(
        "INSERT INTO quarantine (hash, rule, at, expires_at) VALUES (?1, 'U-1', 1, 2)",
        [&held_v2[..]],
    )
    .unwrap();
    let queued: Vec<[u8; 20]> = store
        .partial_hashes(10)
        .unwrap()
        .into_iter()
        .map(|h| h.0)
        .collect();
    assert_eq!(queued, [free]);
}

/// The sweep deletes, once, what's stored under a held hash (either swarm
/// hash, either kind of hold), leaves every other torrent, and is marked
/// done in the same transaction.
#[test]
fn the_sweep_deletes_stored_torrents_under_held_hashes_once() {
    let mut store = Store::open_in_memory(StoreConfig::default()).unwrap();
    let (tombed, tombed_hash, _) = stored(&mut store, "a", false, 0);
    let (quarantined, _, v2_hash) = stored(&mut store, "b", true, 0);
    let (kept, _, _) = stored(&mut store, "c", false, 0);
    let c = store.connection();
    c.execute(
        "INSERT INTO tombstones (hash, rule, at) VALUES (?1, 'U-1', 1), (?2, 'U-1', 1)",
        [&tombed_hash[..], &[7; 20][..]],
    )
    .unwrap();
    c.execute(
        "INSERT INTO quarantine (hash, rule, at, expires_at) VALUES (?1, 'U-1', 1, 2)",
        [&v2_hash[..]],
    )
    .unwrap();
    let before = reindexed(&store).len();
    let mut deleted = store.sweep_held_stored().unwrap().unwrap();
    deleted.sort_unstable();
    assert_eq!(deleted, [tombed, quarantined]);
    assert!(!is_stored(&store, tombed) && !is_stored(&store, quarantined));
    assert!(is_stored(&store, kept));
    assert_eq!(reindexed(&store).len(), before + 2);
    assert_eq!(store.setting_i64(HELD_SWEEP_KEY).unwrap(), Some(1));
    // Once only: a row stored under a held hash later isn't its to delete.
    let (later, later_hash, _) = stored(&mut store, "d", false, 0);
    store
        .connection()
        .execute(
            "INSERT INTO tombstones (hash, rule, at) VALUES (?1, 'U-1', 1)",
            [&later_hash[..]],
        )
        .unwrap();
    assert_eq!(store.sweep_held_stored().unwrap(), None);
    assert!(is_stored(&store, later));
}

/// A fetched torrent whose hash was held after it was judged (a hold the
/// write itself didn't carry) is neither stored nor merged into a stored
/// row: the write reads the holds in its own transaction.
#[test]
fn a_fetch_under_a_hash_held_since_is_never_stored() {
    let mut store = Store::open_in_memory(StoreConfig::default()).unwrap();
    let fresh_raw = info("fresh", false);
    let fresh = Info::parse(&fresh_raw).unwrap();
    let (stored_id, stored_hash, _) = stored(&mut store, "kept", false, flags::FILES_PARTIAL);
    let stored_raw = info("kept", false);
    let stored_info = Info::parse(&stored_raw).unwrap();
    let c = store.connection();
    for h in [fresh.primary_hash().0, stored_hash] {
        c.execute(
            "INSERT INTO quarantine (hash, rule, at, expires_at) VALUES (?1, 'U-1', 1, 2)",
            [&h[..]],
        )
        .unwrap();
    }
    let mut preparer = store.preparer();
    let prepared = [
        preparer.prepare(&fresh, Some(&fresh_raw)).unwrap(),
        preparer.prepare(&stored_info, Some(&stored_raw)).unwrap(),
    ];
    let batch: Vec<lodestar_store::PreparedTorrent<'_>> = prepared
        .iter()
        .map(|p| lodestar_store::PreparedTorrent {
            torrent: p,
            discovered_at: 1,
            metadata_at: 1,
            extra_flags: 0,
            source: SourceId::new(0).unwrap(),
            verified: true,
        })
        .collect();
    let written = store
        .write(&WriteBatch {
            prepared: &batch,
            ..Default::default()
        })
        .unwrap();
    assert_eq!(
        written.prepared,
        [
            lodestar_store::Inserted::Held,
            lodestar_store::Inserted::Held
        ]
    );
    assert_eq!(
        store.find(&fresh.primary_hash()).unwrap(),
        None,
        "not stored"
    );
    // The partial row isn't upgraded either.
    let row = store.torrent(stored_id).unwrap().unwrap();
    assert_ne!(row.flags & flags::FILES_PARTIAL, 0);
}
