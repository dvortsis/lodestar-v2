//! Usenet items in the unverified tier (UN-0): the key, and the rules the
//! store enforces whatever the caller passes. They're always
//! private-source, never verified or promoted, never looked up as a swarm
//! hash, and purged per tier.

use lodestar_bencode::{OwnedValue, to_vec};
use lodestar_metainfo::Info;
use lodestar_store::claims::{ClaimedFile, Claims, Confidence, Provenance};
use lodestar_store::unverified::{
    MAX_USENET_BATCH, Network, NetworkPurgeStep, NewUnverified, PurgeCursor, Put, USENET_MAX_PART,
    UsenetKey,
};
use lodestar_store::{Error, PreparedTorrent, SourceId, SourceState, Store, StoreConfig};
use rusqlite::params;

const T: i64 = 1_700_000_000;
const CODES: lodestar_store::SearchableCodes = lodestar_store::SearchableCodes {
    stage: 5,
    newly: 10,
    delay: 11,
};

fn mem() -> Store {
    Store::open_in_memory(StoreConfig::default()).unwrap()
}

/// The DHT (0) and Prowlarr (1).
fn sources(store: &mut Store) -> (SourceId, SourceId) {
    (
        store.register_source("dht", "1", T).unwrap(),
        store.register_source("prowlarr", "1", T).unwrap(),
    )
}

fn named(name: &str) -> Claims {
    Claims {
        name: Some(name.into()),
        size: Some(5),
        files: vec![ClaimedFile {
            path: format!("{name}.mkv"),
            size: Some(5),
        }],
        trackers: vec![],
        ids: vec![],
        description: None,
        publisher: None,
        provenance: Provenance {
            external_ref: None,
            published_at: Some(T - 60),
            confidence: Confidence::Medium,
        },
        adult: None,
        details: None,
    }
}

fn key(guid: &str) -> UsenetKey {
    UsenetKey::new("7", guid).unwrap()
}

fn usenet<'a>(k: &'a UsenetKey, source: SourceId, claims: &'a Claims) -> NewUnverified<'a> {
    NewUnverified {
        network: Network::Usenet,
        key: k.as_bytes(),
        source,
        private_source: false,
        claims: Some(claims),
        at: T,
    }
}

fn new_id(put: &Put) -> i64 {
    match put {
        Put::New(id) => *id,
        other => panic!("not new: {other:?}"),
    }
}

fn count(store: &Store, sql: &str) -> i64 {
    store.connection().query_row(sql, [], |r| r.get(0)).unwrap()
}

#[test]
fn a_usenet_item_round_trips() {
    let mut store = mem();
    let (_, prowlarr) = sources(&mut store);
    let k = key("guid-1");
    let c = named("Steel.Dawn.2024.1080p");
    let id = new_id(&store.put_unverified(&[usenet(&k, prowlarr, &c)]).unwrap()[0]);
    let item = store
        .unverified(Network::Usenet, k.as_bytes())
        .unwrap()
        .unwrap();
    assert_eq!(item.id, id);
    assert_eq!(item.network, Network::Usenet);
    assert_eq!(item.key, k.as_bytes().to_vec());
    assert_eq!(item.source, prowlarr);
    assert_eq!(item.claims.as_ref(), Some(&c));
    assert!(item.private_source && item.claims_private);
    assert_eq!(item.network.to_string(), "usenet");
    // The re-judge sweep still sees it: the filter judges usenet claims.
    let page = store.unverified_after(0, 10).unwrap();
    assert_eq!(page.len(), 1);
    assert_eq!(page[0], item);
    // The same key under another network is another item.
    assert!(
        store
            .unverified(Network::Bt, k.as_bytes())
            .unwrap()
            .is_none()
    );
    // Seen again: the same item.
    assert_eq!(
        store.put_unverified(&[usenet(&k, prowlarr, &c)]).unwrap(),
        vec![Put::Existing(id)]
    );
    assert_eq!(store.unverified_count(Network::Usenet).unwrap(), 1);
}

#[test]
fn a_usenet_key_is_deterministic_and_unambiguous() {
    assert_eq!(UsenetKey::new("7", "abc").unwrap(), key("abc"));
    // The boundary between the parts moves: different keys.
    let pairs = [("ab", "c"), ("a", "bc"), ("abc", "d"), ("ab", "cd")];
    for (n, a) in pairs.iter().enumerate() {
        for b in &pairs[n + 1..] {
            assert_ne!(
                UsenetKey::new(a.0, a.1).unwrap(),
                UsenetKey::new(b.0, b.1).unwrap(),
                "{a:?} {b:?}"
            );
        }
    }
    // A length prefix can't be forged from inside a part.
    assert_ne!(
        UsenetKey::new("a", "b").unwrap(),
        UsenetKey::new("a\0\0\0\x01b", "x").unwrap()
    );
    // Case and spaces are kept as sent.
    assert_ne!(key("ABC"), key("abc"));
    assert_ne!(key("abc "), key("abc"));
    // Empty or oversized parts are refused, not hashed.
    let long = "x".repeat(USENET_MAX_PART + 1);
    for (i, g) in [
        ("", "g"),
        ("7", ""),
        (long.as_str(), "g"),
        ("7", long.as_str()),
    ] {
        assert!(matches!(UsenetKey::new(i, g), Err(Error::Invalid(_))));
    }
    let edge = "x".repeat(USENET_MAX_PART);
    assert!(UsenetKey::new(&edge, &edge).is_ok());
    // Debug shows a prefix only.
    assert!(format!("{:?}", key("abc")).len() < 24);
}

#[test]
fn a_usenet_key_must_be_32_bytes() {
    let mut store = mem();
    let (_, prowlarr) = sources(&mut store);
    let c = named("x");
    for len in [0, 20, 31, 33, 64] {
        let raw = vec![7u8; len];
        let item = NewUnverified {
            network: Network::Usenet,
            key: &raw,
            source: prowlarr,
            private_source: true,
            claims: Some(&c),
            at: T,
        };
        assert!(
            matches!(store.put_unverified(&[item]), Err(Error::Invalid(_))),
            "{len}"
        );
    }
    assert_eq!(count(&store, "SELECT count(*) FROM unverified_items"), 0);
}

#[test]
fn private_source_is_forced_on() {
    let mut store = mem();
    let (_, prowlarr) = sources(&mut store);
    let k = key("guid-1");
    let c = named("x");
    // The caller says public, twice: it stays private.
    store.put_unverified(&[usenet(&k, prowlarr, &c)]).unwrap();
    store.put_unverified(&[usenet(&k, prowlarr, &c)]).unwrap();
    let flags = count(&store, "SELECT flags FROM unverified_items");
    assert_eq!(flags, 3, "private and private claims");
    let item = store
        .unverified(Network::Usenet, k.as_bytes())
        .unwrap()
        .unwrap();
    assert!(item.private_source && item.claims_private);
    // A row stored without the flag (written outside the API) still reads
    // back private.
    store
        .connection()
        .execute("UPDATE unverified_items SET flags = 0", [])
        .unwrap();
    let item = store
        .unverified(Network::Usenet, k.as_bytes())
        .unwrap()
        .unwrap();
    assert!(item.private_source && item.claims_private);
}

#[test]
fn a_usenet_item_is_never_verified_or_promoted() {
    let mut store = mem();
    let (dht, prowlarr) = sources(&mut store);
    let k = key("guid-1");
    let c = named("x");
    let id = new_id(&store.put_unverified(&[usenet(&k, prowlarr, &c)]).unwrap()[0]);
    // The verification cursor never offers it.
    assert!(store.pending_keys_after(0, 100).unwrap().is_empty());
    // A Bt item after it still comes through.
    let bt = [9u8; 20];
    let bt_id = new_id(
        &store
            .put_unverified(&[NewUnverified {
                network: Network::Bt,
                key: &bt,
                source: dht,
                private_source: false,
                claims: Some(&c),
                at: T,
            }])
            .unwrap()[0],
    );
    let pending = store.pending_keys_after(0, 100).unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].id, bt_id);
    // Promotion is refused with a typed error before any hash is
    // compared, and the item is left as it was.
    let mut d = vec![
        (b"length".to_vec(), OwnedValue::Int(5)),
        (b"name".to_vec(), OwnedValue::Bytes(b"x".to_vec())),
        (b"piece length".to_vec(), OwnedValue::Int(16_384)),
        (b"pieces".to_vec(), OwnedValue::Bytes(vec![0; 20])),
    ];
    d.sort();
    let p = store
        .preparer()
        .prepare(
            &Info::parse(&to_vec(&OwnedValue::Dict(d.into_iter().collect()))).unwrap(),
            None,
        )
        .unwrap();
    let t = PreparedTorrent {
        torrent: &p,
        discovered_at: T,
        metadata_at: T,
        extra_flags: 0,
        source: prowlarr,
        verified: true,
    };
    assert!(matches!(
        store.promote(id, &t, CODES, T),
        Err(Error::NeverVerified)
    ));
    assert!(
        store
            .unverified(Network::Usenet, k.as_bytes())
            .unwrap()
            .is_some()
    );
    assert_eq!(count(&store, "SELECT count(*) FROM torrents"), 0);
    assert_eq!(count(&store, "SELECT count(*) FROM reindex_log"), 0);
}

#[test]
fn a_usenet_key_is_never_matched_as_a_swarm_hash() {
    let mut store = mem();
    let (_, prowlarr) = sources(&mut store);
    let k = key("guid-1");
    // A tombstone on the key's first 20 bytes holds nothing: it isn't a
    // swarm hash, and nothing writes or deletes it as one.
    let prefix = &k.as_bytes()[..20];
    store
        .connection()
        .execute(
            "INSERT INTO tombstones (hash, rule, at) VALUES (?1, 'CS-2', 0)",
            [prefix],
        )
        .unwrap();
    let c = named("x");
    assert!(matches!(
        store.put_unverified(&[usenet(&k, prowlarr, &c)]).unwrap()[0],
        Put::New(_)
    ));
    assert_eq!(count(&store, "SELECT count(*) FROM snapshot_deletions"), 0);
}

#[test]
fn purge_per_tier_removes_usenet_items_only() {
    let mut store = mem();
    let (dht, prowlarr) = sources(&mut store);
    let c = named("x");
    let keys: Vec<UsenetKey> = (0..5).map(|n| key(&format!("g{n}"))).collect();
    let items: Vec<_> = keys.iter().map(|k| usenet(k, prowlarr, &c)).collect();
    store.put_unverified(&items).unwrap();
    let bt = [9u8; 20];
    store
        .put_unverified(&[NewUnverified {
            network: Network::Bt,
            key: &bt,
            source: dht,
            private_source: false,
            claims: Some(&c),
            at: T,
        }])
        .unwrap();
    assert_eq!(store.unverified_count(Network::Usenet).unwrap(), 5);
    // In steps of two: 2, 2, then 1 and done.
    let mut after = 0;
    let mut steps = Vec::new();
    loop {
        let step = store.purge_network(Network::Usenet, after, 2).unwrap();
        steps.push(step.deleted);
        match step.next {
            Some(n) => after = n,
            None => break,
        }
    }
    assert_eq!(steps, [2, 2, 1]);
    assert_eq!(store.unverified_count(Network::Usenet).unwrap(), 0);
    assert_eq!(store.unverified_count(Network::Bt).unwrap(), 1);
    // An empty tier is one empty step.
    assert_eq!(
        store.purge_network(Network::Usenet, 0, 10).unwrap(),
        NetworkPurgeStep {
            next: None,
            deleted: 0
        }
    );
    // The clearnet tier isn't purged this way.
    assert!(matches!(
        store.purge_network(Network::Bt, 0, 10),
        Err(Error::Invalid(_))
    ));
    assert_eq!(store.unverified_count(Network::Bt).unwrap(), 1);
}

#[test]
fn a_source_purge_covers_usenet_items() {
    let mut store = mem();
    let (_, prowlarr) = sources(&mut store);
    let c = named("x");
    let k = key("g");
    store.put_unverified(&[usenet(&k, prowlarr, &c)]).unwrap();
    store
        .set_source_state(prowlarr, SourceState::Disabled)
        .unwrap();
    let mut from = PurgeCursor::START;
    let mut deleted = 0;
    loop {
        let step = store.purge_source(prowlarr, from, 100).unwrap();
        deleted += step.unverified_deleted;
        match step.next {
            Some(n) => from = n,
            None => break,
        }
    }
    assert_eq!(deleted, 1);
    assert_eq!(store.unverified_count(Network::Usenet).unwrap(), 0);
}

#[test]
fn a_batch_of_usenet_items_is_bounded() {
    let mut store = mem();
    let (_, prowlarr) = sources(&mut store);
    let c = named("x");
    let keys: Vec<UsenetKey> = (0..=MAX_USENET_BATCH)
        .map(|n| key(&format!("g{n}")))
        .collect();
    let items: Vec<_> = keys.iter().map(|k| usenet(k, prowlarr, &c)).collect();
    assert!(matches!(
        store.put_unverified(&items),
        Err(Error::Invalid(_))
    ));
    assert_eq!(store.unverified_count(Network::Usenet).unwrap(), 0);
    store.put_unverified(&items[..MAX_USENET_BATCH]).unwrap();
    assert_eq!(
        store.unverified_count(Network::Usenet).unwrap(),
        MAX_USENET_BATCH as u64
    );
}

#[test]
fn unknown_networks_still_fail_to_decode() {
    let mut store = mem();
    let (_, prowlarr) = sources(&mut store);
    // Networks 1–3 and 5–15 pass V14's CHECK but no build knows them yet.
    for (n, net) in [1i64, 2, 3, 5, 15].into_iter().enumerate() {
        store
            .connection()
            .execute(
                "INSERT INTO unverified_items (id, network, key, source_id, sources, flags,
                     claims, first_at, last_at) VALUES (?1, ?2, ?3, ?4, ?5, 1, NULL, 0, 0)",
                params![
                    1000 + n as i64,
                    net,
                    &[n as u8; 32][..],
                    prowlarr.get(),
                    1i64 << prowlarr.get()
                ],
            )
            .unwrap();
        assert!(
            matches!(
                store.unverified_after(999 + n as i64, 1),
                Err(Error::Corrupt("unknown network"))
            ),
            "{net}"
        );
    }
}

#[test]
fn a_reopened_v14_store_accepts_usenet_rows() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("lodestar.db");
    let k = key("guid-1");
    let c = named("x");
    {
        let mut store = Store::open(&path, StoreConfig::default()).unwrap();
        let (_, prowlarr) = sources(&mut store);
        store.put_unverified(&[usenet(&k, prowlarr, &c)]).unwrap();
    }
    let store = Store::open(&path, StoreConfig::default()).unwrap();
    let item = store
        .unverified(Network::Usenet, k.as_bytes())
        .unwrap()
        .unwrap();
    assert_eq!(item.claims.as_ref(), Some(&c));
    assert_eq!(
        count(
            &store,
            "SELECT count(*) FROM unverified_items WHERE network = 4 AND length(key) = 32"
        ),
        1
    );
}
