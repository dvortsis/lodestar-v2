//! X-refetch-private: `partial_hashes` feeds the start's refetch, which
//! queues every key it returns for public peer finding. A key with any
//! `flags::PRIVACY` bit must never be among them (§18.7, BEP 27), and the
//! cap counts only keys that can be refetched.

use lodestar_bencode::{OwnedValue, to_vec};
use lodestar_metainfo::{Info, InfoHash};
use lodestar_store::claims::Claims;
use lodestar_store::unverified::{Network, NewUnverified};
use lodestar_store::{NewTorrent, SourceId, Store, StoreConfig, flags};

const DHT: SourceId = SourceId::new(0).unwrap();

fn raw(name: &str) -> Vec<u8> {
    let d = |e: Vec<(&str, OwnedValue)>| {
        OwnedValue::Dict(
            e.into_iter()
                .map(|(k, v)| (k.as_bytes().to_vec(), v))
                .collect(),
        )
    };
    let file = d(vec![
        ("length", OwnedValue::Int(7)),
        (
            "path",
            OwnedValue::List(vec![OwnedValue::Bytes(b"a.mkv".to_vec())]),
        ),
    ]);
    to_vec(&d(vec![
        ("files", OwnedValue::List(vec![file])),
        ("name", OwnedValue::Bytes(name.as_bytes().to_vec())),
        ("piece length", OwnedValue::Int(16_384)),
        ("pieces", OwnedValue::Bytes(vec![0; 20])),
    ]))
}

/// Stores a partial import of `name` with `extra` flags (and the BEP 27
/// bit when `bep27`), checks the row really carries them, and returns its
/// hash.
fn partial(store: &mut Store, name: &str, extra: i64, bep27: bool) -> InfoHash {
    let mut info = Info::parse(&raw(name)).unwrap();
    info.private = bep27;
    info.files.clear();
    let id = store
        .insert(&[NewTorrent {
            info: &info,
            raw_info: None,
            discovered_at: 1_700_000_000,
            metadata_at: 1_700_000_000,
            extra_flags: flags::IMPORTED | flags::FILES_PARTIAL | extra,
            source: DHT,
        }])
        .unwrap()[0]
        .id();
    let row = store.torrent(id).unwrap().unwrap();
    let want = flags::FILES_PARTIAL | extra | if bep27 { flags::PRIVATE } else { 0 };
    assert_eq!(row.flags & want, want, "{name} is stored as built");
    row.info_hash
}

#[test]
fn partial_hashes_never_returns_a_private_key() {
    let mut store = Store::open_in_memory(StoreConfig::default()).unwrap();
    // The private ones first, so they'd be the oldest under the cap.
    let private_source = partial(&mut store, "Private.Source", flags::PRIVATE_SOURCE, false);
    let bep27 = partial(&mut store, "Bep27", 0, true);
    let bep27_flag = partial(&mut store, "Bep27.Flag", flags::PRIVATE, false);
    let remote = partial(&mut store, "Remote", flags::REMOTE_UNVERIFIED, false);
    let public = partial(&mut store, "Public", 0, false);
    let all = store.partial_hashes(10).unwrap();
    assert_eq!(all, vec![public]);
    for key in [private_source, bep27, bep27_flag, remote] {
        assert!(!all.contains(&key));
    }
    // The cap counts only refetchable keys: four older private partials
    // don't crowd out the public one.
    assert_eq!(store.partial_hashes(1).unwrap(), vec![public]);
}

#[test]
fn public_partials_are_returned_oldest_first_as_before() {
    let mut store = Store::open_in_memory(StoreConfig::default()).unwrap();
    let a = partial(&mut store, "A", 0, false);
    let _private = partial(&mut store, "P", flags::PRIVATE_SOURCE, false);
    let b = partial(&mut store, "B", 0, false);
    assert_eq!(store.partial_hashes(10).unwrap(), vec![a, b]);
    assert_eq!(store.partial_hashes(1).unwrap(), vec![a]);
    assert!(store.partial_hashes(0).unwrap().is_empty());
}

/// An item a private source reported under the hash keeps the key private
/// beside a public import (`Store::insert` leaves the item in place), as
/// the torrent cache's gate and the writer's commit already hold. A public
/// item beside one changes nothing.
#[test]
fn a_private_item_under_the_hash_keeps_a_public_partial_out() {
    let mut store = Store::open_in_memory(StoreConfig::default()).unwrap();
    let feed = store.register_source("feed", "1", 0).unwrap();
    let claims = Claims {
        name: Some("Named".into()),
        ..Claims::default()
    };
    let hash = |name: &str| Info::parse(&raw(name)).unwrap().primary_hash();
    let (private, public) = (hash("Private.Item"), hash("Public.Item"));
    for (key, private_source) in [(&private, true), (&public, false)] {
        store
            .put_unverified(&[NewUnverified {
                network: Network::Bt,
                key: &key.0,
                source: feed,
                private_source,
                claims: Some(&claims),
                at: 1_700_000_000,
            }])
            .unwrap();
    }
    assert!(
        store
            .unverified(Network::Bt, &private.0)
            .unwrap()
            .unwrap()
            .private_source
    );
    assert_eq!(partial(&mut store, "Private.Item", 0, false), private);
    assert_eq!(partial(&mut store, "Public.Item", 0, false), public);
    assert!(
        store.unverified(Network::Bt, &private.0).unwrap().is_some(),
        "the import leaves the item in place"
    );
    assert_eq!(store.partial_hashes(10).unwrap(), vec![public]);
    assert_eq!(store.partial_hashes(1).unwrap(), vec![public]);
}

/// A hybrid (v1 + v2) info dict named `name`.
fn hybrid(name: &str) -> Vec<u8> {
    let d = |e: Vec<(&str, OwnedValue)>| {
        OwnedValue::Dict(
            e.into_iter()
                .map(|(k, v)| (k.as_bytes().to_vec(), v))
                .collect(),
        )
    };
    let leaf = d(vec![(
        "",
        d(vec![
            ("length", OwnedValue::Int(100)),
            ("pieces root", OwnedValue::Bytes(vec![5; 32])),
        ]),
    )]);
    to_vec(&d(vec![
        ("file tree", d(vec![("a.bin", leaf)])),
        ("length", OwnedValue::Int(100)),
        ("meta version", OwnedValue::Int(2)),
        ("name", OwnedValue::Bytes(name.as_bytes().to_vec())),
        ("piece length", OwnedValue::Int(16_384)),
        ("pieces", OwnedValue::Bytes(vec![0; 20])),
    ]))
}

/// The v2 half of the rule: an item a private source reported under a
/// hybrid's truncated v2 hash alone keeps it out; a public hybrid partial
/// is kept.
#[test]
fn a_private_item_under_the_v2_hash_alone_keeps_a_hybrid_partial_out() {
    let mut store = Store::open_in_memory(StoreConfig::default()).unwrap();
    let feed = store.register_source("feed", "1", 0).unwrap();
    let claims = Claims {
        name: Some("Named".into()),
        ..Claims::default()
    };
    let import = |store: &mut Store, name: &str| {
        let mut info = Info::parse(&hybrid(name)).unwrap();
        info.files.clear();
        let (v1, v2) = (info.v1.unwrap(), info.v2.unwrap().truncated());
        assert_ne!(v1, v2);
        store
            .insert(&[NewTorrent {
                info: &info,
                raw_info: None,
                discovered_at: 1_700_000_000,
                metadata_at: 1_700_000_000,
                extra_flags: flags::IMPORTED | flags::FILES_PARTIAL,
                source: DHT,
            }])
            .unwrap();
        (v1, v2)
    };
    let private_v2 = Info::parse(&hybrid("Private.Hybrid"))
        .unwrap()
        .v2
        .unwrap()
        .truncated();
    store
        .put_unverified(&[NewUnverified {
            network: Network::Bt,
            key: &private_v2.0,
            source: feed,
            private_source: true,
            claims: Some(&claims),
            at: 1_700_000_000,
        }])
        .unwrap();
    let (private_v1, v2) = import(&mut store, "Private.Hybrid");
    assert_eq!(v2, private_v2);
    assert!(
        store
            .unverified(Network::Bt, &private_v1.0)
            .unwrap()
            .is_none(),
        "nothing under the v1 hash: only the v2 term can exclude it"
    );
    assert!(store.unverified(Network::Bt, &v2.0).unwrap().is_some());
    let (public_v1, _) = import(&mut store, "Public.Hybrid");
    assert_eq!(store.partial_hashes(10).unwrap(), vec![public_v1]);
}
