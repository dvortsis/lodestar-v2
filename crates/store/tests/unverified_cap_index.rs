//! X-unverified-cap-index (V16, `docs/decisions/2026-11-23-X-unverified-cap-index.md`):
//! the kept per-network count agrees with the rows through every path
//! that adds or removes an unverified item. The query-plan test is beside
//! the queries, in `src/unverified.rs`.

use lodestar_bencode::{OwnedValue, to_vec};
use lodestar_metainfo::Info;
use lodestar_store::claims::Claims;
use lodestar_store::unverified::{
    Network, NewUnverified, PurgeCursor, RefiledUnverified, UsenetKey,
};
use lodestar_store::{
    Filtered, Held, Prepared, PreparedTorrent, SourceId, SourceState, Store, StoreConfig,
    WriteBatch,
};
use proptest::prelude::*;

const T: i64 = 1_700_000_000;
const KEYS: u8 = 10;
const CODES: lodestar_store::SearchableCodes = lodestar_store::SearchableCodes {
    stage: 5,
    newly: 10,
    delay: 11,
};

fn prepared(store: &Store, n: u8) -> Prepared {
    let b = |s: &[u8]| OwnedValue::Bytes(s.to_vec());
    let e: Vec<(Vec<u8>, OwnedValue)> = vec![
        (b"length".to_vec(), OwnedValue::Int(5)),
        (b"name".to_vec(), b(format!("t{n}").as_bytes())),
        (b"piece length".to_vec(), OwnedValue::Int(16_384)),
        (b"pieces".to_vec(), b(&[0; 20])),
    ];
    let info = Info::parse(&to_vec(&OwnedValue::Dict(e.into_iter().collect()))).unwrap();
    store.preparer().prepare(&info, None).unwrap()
}

fn pt(p: &Prepared, source: SourceId) -> PreparedTorrent<'_> {
    PreparedTorrent {
        torrent: p,
        discovered_at: T,
        metadata_at: T,
        extra_flags: 0,
        source,
        verified: true,
    }
}

fn filtered(hashes: &[[u8; 20]], held: Held) -> Filtered<'_> {
    Filtered {
        hashes,
        rule: "CS-2",
        action: 0,
        held: Some(held),
        sealed: None,
        evidence: &[],
        at: T,
        expires_at: T + 86_400,
        adult_hold: false,
    }
}

fn count(store: &Store, sql: &str) -> i64 {
    store.connection().query_row(sql, [], |r| r.get(0)).unwrap()
}

/// Every network's kept count equals its rows, and no network has a kept
/// count without rows other than 0.
fn assert_counts_agree(store: &Store) -> Result<(), TestCaseError> {
    for network in [Network::Bt, Network::Usenet] {
        let rows = count(
            store,
            &format!(
                "SELECT count(*) FROM unverified_items WHERE network = {}",
                network as i64
            ),
        );
        prop_assert_eq!(
            store.unverified_count(network).unwrap(),
            rows as u64,
            "{:?}",
            network
        );
    }
    let off = count(
        store,
        "SELECT count(*) FROM unverified_counts c WHERE n != (
             SELECT count(*) FROM unverified_items u WHERE u.network = c.network)",
    );
    prop_assert_eq!(off, 0);
    let missing = count(
        store,
        "SELECT count(*) FROM (SELECT DISTINCT network FROM unverified_items) u
         WHERE NOT EXISTS (SELECT 1 FROM unverified_counts c WHERE c.network = u.network)",
    );
    prop_assert_eq!(missing, 0);
    Ok(())
}

#[derive(Debug, Clone)]
enum Op {
    /// Source `s` (0 or 1) finds BitTorrent key `n`, with claims or bare.
    FindBt(u8, u8, bool),
    /// Source `s` finds usenet key `n`.
    FindUsenet(u8, u8),
    /// A hold (tombstone, quarantine or floor hash) on key `n`.
    Hold(u8, u8),
    /// The re-judge sweep removes item `n`, holding it or not.
    Refile(u8, bool),
    /// Key `n`'s metadata arrives.
    Promote(u8),
    /// The crawler stores torrent `n`, then a source finds it.
    Crawl(u8),
    /// The usenet tier purged, in steps of `limit`.
    PurgeUsenet(usize),
    /// The second source disabled and purged (once; later finds by it are
    /// skipped).
    PurgeSource,
    /// An item's network changed (BitTorrent to usenet), outside the API.
    Move(u8),
    /// An item deleted outside the API (as the snapshot purge does).
    Delete(u8),
}

fn op() -> impl Strategy<Value = Op> {
    prop_oneof![
        4 => (0..KEYS, 0u8..2, any::<bool>()).prop_map(|(n, s, c)| Op::FindBt(n, s, c)),
        4 => (0..KEYS, 0u8..2).prop_map(|(n, s)| Op::FindUsenet(n, s)),
        1 => (0..KEYS, 0u8..3).prop_map(|(n, h)| Op::Hold(n, h)),
        1 => (0..KEYS, any::<bool>()).prop_map(|(n, h)| Op::Refile(n, h)),
        1 => (0..KEYS).prop_map(Op::Promote),
        1 => (0..KEYS).prop_map(Op::Crawl),
        1 => (1usize..4).prop_map(Op::PurgeUsenet),
        1 => Just(Op::PurgeSource),
        1 => (0..KEYS).prop_map(Op::Move),
        1 => (0..KEYS).prop_map(Op::Delete),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]

    /// Whatever the mix of writes, holds, purges, promotions and raw
    /// deletes, each network's kept count is its `count(*)`.
    #[test]
    fn the_kept_count_matches_the_rows(ops in proptest::collection::vec(op(), 1..60)) {
        let mut store = Store::open_in_memory(StoreConfig::default()).unwrap();
        let dht = store.register_source("dht", "1", T).unwrap();
        let src = [
            store.register_source("nyaa", "1", T).unwrap(),
            store.register_source("prowlarr", "1", T).unwrap(),
        ];
        let mut purged = false;
        let ts: Vec<Prepared> = (0..KEYS).map(|n| prepared(&store, n)).collect();
        let uk: Vec<UsenetKey> = (0..KEYS)
            .map(|n| UsenetKey::new("7", &format!("guid-{n}")).unwrap())
            .collect();
        let claims = Claims { name: Some("x".into()), ..Claims::default() };
        let id_of = |store: &Store, network: Network, key: &[u8]| {
            store.unverified(network, key).unwrap().map(|i| i.id)
        };
        for op in ops {
            match op {
                Op::FindBt(n, s, with) => {
                    if s == 1 && purged {
                        continue;
                    }
                    store.put_unverified(&[NewUnverified {
                        network: Network::Bt,
                        key: &ts[n as usize].info_hash.0,
                        source: src[s as usize],
                        private_source: false,
                        claims: with.then_some(&claims),
                        at: T,
                    }]).unwrap();
                }
                Op::FindUsenet(n, s) => {
                    if s == 1 && purged {
                        continue;
                    }
                    store.put_unverified(&[NewUnverified {
                        network: Network::Usenet,
                        key: uk[n as usize].as_bytes(),
                        source: src[s as usize],
                        private_source: true,
                        claims: Some(&claims),
                        at: T,
                    }]).unwrap();
                }
                Op::Hold(n, how) => {
                    let hashes = [ts[n as usize].info_hash.0];
                    match how {
                        0 | 1 => {
                            let held = if how == 0 { Held::Tombstone } else { Held::Quarantine };
                            store.write(&WriteBatch {
                                filtered: &[filtered(&hashes, held)],
                                ..WriteBatch::default()
                            }).unwrap();
                        }
                        _ => store.add_cs_hashes(&hashes, 1, T).unwrap(),
                    }
                }
                Op::Refile(n, held) => {
                    let hashes = [ts[n as usize].info_hash.0];
                    let ids = [
                        id_of(&store, Network::Bt, &hashes[0]),
                        id_of(&store, Network::Usenet, uk[n as usize].as_bytes()),
                    ];
                    let r: Vec<RefiledUnverified<'_>> = ids
                        .into_iter()
                        .flatten()
                        .map(|id| RefiledUnverified {
                            id,
                            filtered: held.then(|| filtered(&hashes, Held::Quarantine)),
                        })
                        .collect();
                    store.refile_unverified(&r, || true).unwrap();
                }
                Op::Promote(n) => {
                    let p = &ts[n as usize];
                    if let Some(id) = id_of(&store, Network::Bt, &p.info_hash.0) {
                        store.promote(id, &pt(p, dht), CODES, T).unwrap();
                    }
                }
                Op::Crawl(n) => {
                    store.write(&WriteBatch {
                        prepared: &[pt(&ts[n as usize], dht)],
                        ..WriteBatch::default()
                    }).unwrap();
                }
                Op::PurgeUsenet(limit) => {
                    let mut after = 0;
                    loop {
                        let step = store.purge_network(Network::Usenet, after, limit).unwrap();
                        match step.next {
                            Some(next) => after = next,
                            None => break,
                        }
                    }
                }
                Op::PurgeSource => {
                    if purged {
                        continue;
                    }
                    purged = true;
                    store.set_source_state(src[1], SourceState::Disabled).unwrap();
                    let mut cursor = PurgeCursor::START;
                    loop {
                        let step = store.purge_source(src[1], cursor, 2).unwrap();
                        match step.next {
                            Some(c) => cursor = c,
                            None => break,
                        }
                    }
                }
                Op::Move(n) => {
                    store.connection().execute(
                        "UPDATE unverified_items SET network = 4 WHERE network = 0 AND key = ?1",
                        [&ts[n as usize].info_hash.0[..]],
                    ).unwrap();
                }
                Op::Delete(n) => {
                    store.connection().execute(
                        "DELETE FROM unverified_items WHERE key IN (?1, ?2)",
                        [&ts[n as usize].info_hash.0[..], &uk[n as usize].as_bytes()[..]],
                    ).unwrap();
                }
            }
            assert_counts_agree(&store)?;
        }
    }
}

/// The paths one at a time, each checked against `count(*)`: an insert
/// counts once, a sighting of a known key adds nothing, and each kind of
/// removal takes one off.
#[test]
fn each_path_keeps_the_count() {
    let mut store = Store::open_in_memory(StoreConfig::default()).unwrap();
    let dht = store.register_source("dht", "1", T).unwrap();
    let nyaa = store.register_source("nyaa", "1", T).unwrap();
    let claims = Claims {
        name: Some("x".into()),
        ..Claims::default()
    };
    let ts: Vec<Prepared> = (0..6).map(|n| prepared(&store, n)).collect();
    let uk: Vec<UsenetKey> = (0..3)
        .map(|n| UsenetKey::new("7", &format!("g{n}")).unwrap())
        .collect();
    let items: Vec<NewUnverified<'_>> = ts
        .iter()
        .map(|p| NewUnverified {
            network: Network::Bt,
            key: &p.info_hash.0,
            source: nyaa,
            private_source: false,
            claims: Some(&claims),
            at: T,
        })
        .chain(uk.iter().map(|k| NewUnverified {
            network: Network::Usenet,
            key: k.as_bytes(),
            source: nyaa,
            private_source: true,
            claims: Some(&claims),
            at: T,
        }))
        .collect();
    let counts = |store: &Store| {
        (
            store.unverified_count(Network::Bt).unwrap(),
            store.unverified_count(Network::Usenet).unwrap(),
        )
    };
    assert_eq!(counts(&store), (0, 0), "no rows, no count");
    store.put_unverified(&items).unwrap();
    assert_eq!(counts(&store), (6, 3));
    // Seen again: nothing new.
    store.put_unverified(&items).unwrap();
    assert_eq!(counts(&store), (6, 3));
    // A content-safety hold (the V14 trigger's delete).
    store
        .write(&WriteBatch {
            filtered: &[filtered(&[ts[0].info_hash.0], Held::Tombstone)],
            ..WriteBatch::default()
        })
        .unwrap();
    store.add_cs_hashes(&[ts[1].info_hash.0], 1, T).unwrap();
    assert_eq!(counts(&store), (4, 3));
    // A promotion takes the item out of the tier.
    let id = store.unverified(Network::Bt, &ts[2].info_hash.0).unwrap();
    store
        .promote(id.unwrap().id, &pt(&ts[2], dht), CODES, T)
        .unwrap();
    assert_eq!(counts(&store), (3, 3));
    // The re-judge sweep.
    let id = store
        .unverified(Network::Bt, &ts[3].info_hash.0)
        .unwrap()
        .unwrap()
        .id;
    store
        .refile_unverified(&[RefiledUnverified { id, filtered: None }], || true)
        .unwrap();
    assert_eq!(counts(&store), (2, 3));
    // The tier purge.
    let step = store.purge_network(Network::Usenet, 0, 2).unwrap();
    assert_eq!(counts(&store), (2, 1));
    store
        .purge_network(Network::Usenet, step.next.unwrap(), 2)
        .unwrap();
    assert_eq!(counts(&store), (2, 0));
    // A network change moves the count.
    store
        .connection()
        .execute(
            "UPDATE unverified_items SET network = 4 WHERE key = ?1",
            [&ts[4].info_hash.0[..]],
        )
        .unwrap();
    assert_eq!(counts(&store), (1, 1));
    // The source purge.
    store.set_source_state(nyaa, SourceState::Disabled).unwrap();
    let mut cursor = PurgeCursor::START;
    while let Some(c) = store.purge_source(nyaa, cursor, 1).unwrap().next {
        cursor = c;
    }
    assert_eq!(counts(&store), (0, 0));
    assert_eq!(count(&store, "SELECT count(*) FROM unverified_items"), 0);
}
