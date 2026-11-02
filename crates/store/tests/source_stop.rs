//! A disabled or purged source's fetches aren't written (SRC-14b): the
//! check in `Store::write`'s transaction that keeps a fetch already on its
//! way from putting back what a purge removes.

use lodestar_bencode::{OwnedValue, to_vec};
use lodestar_metainfo::{Info, InfoHash};
use lodestar_store::unverified::PurgeCursor;
use lodestar_store::{
    Inserted, PreparedTorrent, SourceId, SourceState, Store, StoreConfig, Touch, WriteBatch,
};

const T: i64 = 1_700_000_000;

fn info(name: &str) -> Vec<u8> {
    let d = |e: Vec<(&str, OwnedValue)>| {
        OwnedValue::Dict(
            e.into_iter()
                .map(|(k, v)| (k.as_bytes().to_vec(), v))
                .collect(),
        )
    };
    to_vec(&d(vec![
        ("length", OwnedValue::Int(1_000_000)),
        ("name", OwnedValue::Bytes(name.as_bytes().to_vec())),
        ("piece length", OwnedValue::Int(1 << 20)),
        ("pieces", OwnedValue::Bytes(vec![0; 20])),
    ]))
}

fn setup() -> (Store, SourceId, SourceId) {
    let mut store = Store::open_in_memory(StoreConfig::default()).unwrap();
    let dht = store.register_source("dht", "1", T).unwrap();
    let nyaa = store.register_source("nyaa", "1", T).unwrap();
    (store, dht, nyaa)
}

/// One fetched torrent from `source`, with `others` reporting it too in
/// the same batch, as the writer sends them.
fn write(store: &mut Store, raw: &[u8], source: SourceId, others: u64) -> Inserted {
    let info = Info::parse(raw).unwrap();
    let p = store.preparer().prepare(&info, Some(raw)).unwrap();
    let touches: Vec<Touch> = (others != 0)
        .then(|| Touch {
            hash: info.swarm_hashes().next().unwrap(),
            sources: others | 1 << source.get(),
            at: T,
            public: true,
        })
        .into_iter()
        .collect();
    let w = store
        .write(&WriteBatch {
            prepared: &[PreparedTorrent {
                torrent: &p,
                discovered_at: T,
                metadata_at: T,
                extra_flags: 0,
                source,
                verified: true,
            }],
            touches: &touches,
            ..WriteBatch::default()
        })
        .unwrap();
    w.prepared[0]
}

fn bits(store: &Store, hash: InfoHash) -> Option<(i64, i64)> {
    store
        .connection()
        .query_row(
            "SELECT sources, first_source FROM torrents WHERE info_hash = ?1",
            [&hash.0[..]],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .ok()
}

fn hash(raw: &[u8]) -> InfoHash {
    Info::parse(raw).unwrap().swarm_hashes().next().unwrap()
}

#[test]
fn a_fetch_written_after_its_source_stops_is_refused() {
    let (mut store, _, nyaa) = setup();
    let raw = info("Only.Nyaa");
    store.set_source_state(nyaa, SourceState::Disabled).unwrap();
    assert_eq!(write(&mut store, &raw, nyaa, 0), Inserted::Refused);
    assert_eq!(bits(&store, hash(&raw)), None, "nothing is stored");
    assert_eq!(store.count().unwrap(), 0);
    // A running source stores it as new later.
    store.enable_source(nyaa, None).unwrap();
    assert!(matches!(write(&mut store, &raw, nyaa, 0), Inserted::New(_)));
}

#[test]
fn a_torrent_another_source_found_keeps_all_but_the_stopped_bit() {
    let (mut store, dht, nyaa) = setup();
    store.set_source_state(nyaa, SourceState::Purged).unwrap();
    let (n, d) = (1i64 << nyaa.get(), 1i64 << dht.get());
    // First found by the stopped source, also by the DHT: stored under
    // the DHT alone.
    let raw = info("Both");
    let Inserted::New(_) = write(&mut store, &raw, nyaa, d as u64) else {
        panic!("stored")
    };
    assert_eq!(bits(&store, hash(&raw)), Some((d, i64::from(dht.get()))));
    // Fetched by the DHT, reported by the stopped one too: no stopped bit.
    let raw = info("Dht.First");
    write(&mut store, &raw, dht, n as u64);
    assert_eq!(bits(&store, hash(&raw)).map(|b| b.0), Some(d));
    // A stopped source's rediscovery of a stored torrent adds nothing.
    let touch = Touch {
        hash: hash(&raw),
        sources: n as u64,
        at: T + 7200,
        public: true,
    };
    let w = store
        .write(&WriteBatch {
            touches: &[touch],
            ..WriteBatch::default()
        })
        .unwrap();
    assert!(w.touched[0].is_some(), "still reported as stored");
    assert_eq!(bits(&store, hash(&raw)).map(|b| b.0), Some(d));
}

/// The race a purge runs: a batch the writer already holds commits after
/// the source was disabled, during or after the purge. It leaves no
/// torrent attributed to the purged source.
#[test]
fn a_batch_racing_a_purge_leaves_nothing_of_the_source() {
    let (mut store, dht, nyaa) = setup();
    let early = info("Stored.Before");
    write(&mut store, &early, nyaa, 0);
    // The purge starts: the source is disabled, then purged in steps.
    store.set_source_state(nyaa, SourceState::Disabled).unwrap();
    let mut cursor = Some(PurgeCursor::START);
    let mut first = true;
    while let Some(c) = cursor {
        cursor = store.purge_source(nyaa, c, 1).unwrap().next;
        if first {
            // The writer's batch lands mid-purge.
            assert_eq!(
                write(&mut store, &info("In.Flight"), nyaa, 0),
                Inserted::Refused
            );
            write(&mut store, &info("Shared"), nyaa, 1 << dht.get());
            first = false;
        }
    }
    store.set_source_state(nyaa, SourceState::Purged).unwrap();
    // And one after it ends.
    assert_eq!(write(&mut store, &info("Late"), nyaa, 0), Inserted::Refused);
    let bit = 1i64 << nyaa.get();
    let left: i64 = store
        .connection()
        .query_row(
            "SELECT count(*) FROM torrents WHERE sources & ?1 != 0 OR first_source = ?2",
            [bit, i64::from(nyaa.get())],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(left, 0);
    assert_eq!(bits(&store, hash(&early)), None, "the purge deleted it");
    assert!(
        bits(&store, hash(&info("Shared"))).is_some(),
        "the DHT's stays"
    );
}

/// A pool snapshot taken before a purge dropped the source's keys can't
/// save them after it: a stopped source's keys aren't persisted.
#[test]
fn a_stopped_sources_queued_keys_arent_saved() {
    let (mut store, dht, nyaa) = setup();
    let key = |n: u8, source| lodestar_store::QueuedKey {
        info_hash: InfoHash([n; 20]),
        source,
        cohort: 0,
        strategy: 0,
        priority: 0,
        attempts: 0,
        private_source: false,
    };
    store.set_source_state(nyaa, SourceState::Disabled).unwrap();
    store
        .save_fetch_queue(&[key(1, nyaa), key(2, dht)])
        .unwrap();
    let saved: Vec<_> = store
        .load_fetch_queue()
        .unwrap()
        .into_iter()
        .map(|k| k.source)
        .collect();
    assert_eq!(saved, [dht]);
}

/// A torrent that passes to a live finder because its own stopped is
/// stored private: a public sighting by the stopped finder can't make it
/// public through the live one (runner, #400). A live public sighting
/// makes it public again.
#[test]
fn a_torrent_passed_to_a_live_finder_is_stored_private() {
    let (mut store, dht, nyaa) = setup();
    store.set_source_state(nyaa, SourceState::Purged).unwrap();
    let raw = info("Public.Through.The.Purged.One");
    write(&mut store, &raw, nyaa, 1 << dht.get());
    let flags = |store: &Store| -> i64 {
        store
            .connection()
            .query_row(
                "SELECT flags FROM torrents WHERE info_hash = ?1",
                [&hash(&raw).0[..]],
                |r| r.get(0),
            )
            .unwrap()
    };
    assert_ne!(flags(&store) & lodestar_store::flags::PRIVATE_SOURCE, 0);
    store
        .write(&WriteBatch {
            touches: &[Touch {
                hash: hash(&raw),
                sources: 1 << dht.get(),
                at: T + 60,
                public: true,
            }],
            ..WriteBatch::default()
        })
        .unwrap();
    assert_eq!(flags(&store) & lodestar_store::flags::PRIVATE_SOURCE, 0);
}
