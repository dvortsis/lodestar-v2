//! Sources' store (SRC-1, V14): unverified items, claims, plugin state and
//! purges, through the public API. Content safety first: a held hash never
//! lives in or comes back through either table, on every path.

use lodestar_bencode::{OwnedValue, to_vec};
use lodestar_metainfo::Info;
use lodestar_store::claims::{ClaimedFile, Claims, Confidence, ExternalId, Provenance};
use lodestar_store::unverified::{
    ClaimsRefiled, MAX_PAGE, Network, NewUnverified, PurgeCursor, PurgeStep, Put, RefiledClaims,
    RefiledUnverified,
};
use lodestar_store::{
    Error, Filtered, Held, Prepared, PreparedTorrent, SourceId, SourceState, Store, StoreConfig,
    WriteBatch, flags,
};
use rusqlite::params;

const T: i64 = 1_700_000_000;

fn mem() -> Store {
    Store::open_in_memory(StoreConfig::default()).unwrap()
}

/// The DHT (0) and two feed sources (1, 2).
fn sources(store: &mut Store) -> (SourceId, SourceId, SourceId) {
    (
        store.register_source("dht", "1", T).unwrap(),
        store.register_source("nyaa", "1", T).unwrap(),
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
        trackers: vec!["udp://tracker.example:1337/announce".into()],
        ids: vec![ExternalId {
            scheme: "imdb".into(),
            value: "tt1".into(),
        }],
        description: None,
        publisher: Some("someone".into()),
        provenance: Provenance {
            external_ref: Some("https://feed.example/1".into()),
            published_at: Some(T - 60),
            confidence: Confidence::Medium,
        },
    }
}

fn item<'a>(key: &'a [u8; 20], source: SourceId, claims: Option<&'a Claims>) -> NewUnverified<'a> {
    NewUnverified {
        network: Network::Bt,
        key,
        source,
        private_source: false,
        claims,
        at: T,
    }
}

fn info_bytes(name: &str) -> Vec<u8> {
    let b = |s: &[u8]| OwnedValue::Bytes(s.to_vec());
    let e: Vec<(Vec<u8>, OwnedValue)> = vec![
        (b"length".to_vec(), OwnedValue::Int(5)),
        (b"name".to_vec(), b(name.as_bytes())),
        (b"piece length".to_vec(), OwnedValue::Int(16_384)),
        (b"pieces".to_vec(), b(&[0; 20])),
    ];
    to_vec(&OwnedValue::Dict(e.into_iter().collect()))
}

fn prepared(store: &Store, name: &str) -> Prepared {
    store
        .preparer()
        .prepare(&Info::parse(&info_bytes(name)).unwrap(), None)
        .unwrap()
}

fn pt(p: &Prepared, source: SourceId) -> PreparedTorrent<'_> {
    PreparedTorrent {
        torrent: p,
        discovered_at: T + 10,
        metadata_at: T + 10,
        extra_flags: 0,
        source,
        verified: true,
    }
}

/// Stores a torrent through the writer; its id.
fn store_torrent(store: &mut Store, p: &Prepared, source: SourceId) -> i64 {
    store
        .write(&WriteBatch {
            prepared: &[pt(p, source)],
            ..WriteBatch::default()
        })
        .unwrap()
        .prepared[0]
        .id()
}

/// A torrent's claims as `(source, private, claims)`.
fn claims_of(store: &Store, id: i64) -> Vec<(SourceId, bool, Claims)> {
    store
        .torrent_claims(id)
        .unwrap()
        .into_iter()
        .map(|c| (c.source, c.private_source, c.claims.unwrap()))
        .collect()
}

fn count(store: &Store, sql: &str) -> i64 {
    store.connection().query_row(sql, [], |r| r.get(0)).unwrap()
}

fn journalled(store: &Store, hash: &[u8; 20]) -> bool {
    store
        .connection()
        .query_row(
            "SELECT count(*) FROM snapshot_deletions WHERE hash = ?1",
            [&hash[..]],
            |r| r.get::<_, i64>(0),
        )
        .unwrap()
        == 1
}

fn next_id(store: &Store) -> i64 {
    count(store, "SELECT next FROM id_seq")
}

#[test]
fn put_records_finds_and_merges_sources() {
    let mut store = mem();
    let (_, nyaa, prowlarr) = sources(&mut store);
    let (a, b) = (named("Alpha"), named("Beta"));
    let key = [1u8; 20];
    let first = next_id(&store);

    let put = store.put_unverified(&[item(&key, nyaa, Some(&a))]).unwrap();
    assert_eq!(put, vec![Put::New(first)]);
    assert_eq!(next_id(&store), first + 1, "the id came from id_seq");

    // Another source's claims don't replace the owner's; its bit is added.
    let put = store
        .put_unverified(&[NewUnverified {
            at: T + 5,
            ..item(&key, prowlarr, Some(&b))
        }])
        .unwrap();
    assert_eq!(put, vec![Put::Existing(first)]);
    let got = store.unverified(Network::Bt, &key).unwrap().unwrap();
    assert_eq!(got.id, first);
    assert_eq!(got.source, nyaa);
    assert_eq!(got.sources, (1 << nyaa.get()) | (1 << prowlarr.get()));
    assert_eq!(got.claims.as_ref(), Some(&a));
    assert_eq!((got.first_at, got.last_at), (T, T + 5));

    // The owner's newer claims replace its own.
    store.put_unverified(&[item(&key, nyaa, Some(&b))]).unwrap();
    let got = store.unverified(Network::Bt, &key).unwrap().unwrap();
    assert_eq!(got.claims.as_ref(), Some(&b));
    assert_eq!(got.last_at, T + 5, "last_at never goes back");

    // A bare new key isn't stored; a bare sighting of a known one is.
    assert_eq!(
        store
            .put_unverified(&[item(&[2; 20], nyaa, None), item(&key, nyaa, None)])
            .unwrap(),
        vec![Put::Bare, Put::Existing(first)]
    );
    assert_eq!(count(&store, "SELECT count(*) FROM unverified_items"), 1);
    assert_eq!(next_id(&store), first + 1);
}

#[test]
fn a_public_sighting_clears_private_and_a_private_one_never_sets_it() {
    let mut store = mem();
    let (_, nyaa, prowlarr) = sources(&mut store);
    let c = named("Gamma");
    let private = |k: &'static [u8; 20], s| NewUnverified {
        private_source: true,
        ..item(k, s, Some(&c))
    };
    store
        .put_unverified(&[private(&[3; 20], prowlarr)])
        .unwrap();
    let get = |store: &Store, k: &[u8; 20]| {
        store
            .unverified(Network::Bt, k)
            .unwrap()
            .unwrap()
            .private_source
    };
    assert!(get(&store, &[3; 20]));
    store.put_unverified(&[item(&[3; 20], nyaa, None)]).unwrap();
    assert!(!get(&store, &[3; 20]));
    store
        .put_unverified(&[private(&[3; 20], prowlarr)])
        .unwrap();
    assert!(!get(&store, &[3; 20]));
}

#[test]
fn a_key_already_stored_gets_claims_on_the_torrent() {
    let mut store = mem();
    let (dht, nyaa, _) = sources(&mut store);
    let p = prepared(&store, "delta");
    let id = store_torrent(&mut store, &p, dht);
    let c = named("Delta");
    let put = store
        .put_unverified(&[item(&p.info_hash.0, nyaa, Some(&c))])
        .unwrap();
    assert_eq!(put, vec![Put::Torrent(id)]);
    assert_eq!(claims_of(&store, id), vec![(nyaa, false, c)]);
    let row = store.torrent(id).unwrap().unwrap();
    assert_eq!(row.sources, (1 << dht.get()) | (1 << nyaa.get()));
    assert_eq!(count(&store, "SELECT count(*) FROM unverified_items"), 0);
}

#[test]
fn claims_over_the_limits_write_nothing() {
    let mut store = mem();
    let (_, nyaa, _) = sources(&mut store);
    let bad = Claims {
        name: Some(String::new()),
        ..Claims::default()
    };
    let ok = named("ok");
    let err = store
        .put_unverified(&[
            item(&[4; 20], nyaa, Some(&ok)),
            item(&[5; 20], nyaa, Some(&bad)),
        ])
        .unwrap_err();
    assert!(matches!(err, Error::Invalid(_)), "{err:?}");
    assert_eq!(count(&store, "SELECT count(*) FROM unverified_items"), 0);
    let err = store
        .put_unverified(&[NewUnverified {
            key: &[1; 32],
            ..item(&[4; 20], nyaa, Some(&ok))
        }])
        .unwrap_err();
    assert!(matches!(err, Error::Invalid(_)), "{err:?}");
}

/// The three hold tables, each written the way the store writes it.
#[derive(Debug, Clone, Copy)]
enum Hold {
    Tombstone,
    Quarantine,
    CsHash,
}

const HOLDS: [Hold; 3] = [Hold::Tombstone, Hold::Quarantine, Hold::CsHash];

fn hold(store: &mut Store, how: Hold, hash: [u8; 20]) {
    let hashes = [hash];
    let filtered = |held| Filtered {
        hashes: &hashes,
        rule: "CS-2",
        action: 0,
        held: Some(held),
        sealed: None,
        evidence: &[],
        at: T,
        expires_at: T + 86_400,
        adult_hold: false,
    };
    match how {
        Hold::Tombstone => {
            store
                .write(&WriteBatch {
                    filtered: &[filtered(Held::Tombstone)],
                    ..WriteBatch::default()
                })
                .unwrap();
        }
        Hold::Quarantine => {
            store
                .write(&WriteBatch {
                    filtered: &[filtered(Held::Quarantine)],
                    ..WriteBatch::default()
                })
                .unwrap();
        }
        Hold::CsHash => store.add_cs_hashes(&hashes, 1, T).unwrap(),
    }
}

/// A hash that enters any hold table takes its unverified item with it,
/// journalled for the snapshot purge, and the claims of a torrent under
/// it (by v1 or truncated v2 hash).
#[test]
fn every_hold_removes_unverified_items_and_claims() {
    for how in HOLDS {
        let mut store = mem();
        let (dht, nyaa, _) = sources(&mut store);
        let c = named("held-name");
        let key = [6u8; 20];
        store.put_unverified(&[item(&key, nyaa, Some(&c))]).unwrap();
        let p = prepared(&store, "held torrent");
        let id = store_torrent(&mut store, &p, dht);
        assert!(store.put_claims(id, nyaa, &c, false).unwrap());

        hold(&mut store, how, key);
        assert!(
            store.unverified(Network::Bt, &key).unwrap().is_none(),
            "{how:?}"
        );
        assert!(journalled(&store, &key), "{how:?}");

        hold(&mut store, how, p.info_hash.0);
        assert!(store.torrent_claims(id).unwrap().is_empty(), "{how:?}");
    }
}

/// A truncated v2 hold reaches a hybrid torrent's claims.
#[test]
fn a_truncated_v2_hold_removes_a_hybrids_claims() {
    let mut store = mem();
    let (dht, nyaa, _) = sources(&mut store);
    let p = prepared(&store, "hybrid");
    let id = store_torrent(&mut store, &p, dht);
    let v2 = [0xabu8; 32];
    store
        .connection()
        .execute(
            "UPDATE torrents SET info_hash_v2 = ?2 WHERE id = ?1",
            params![id, &v2[..]],
        )
        .unwrap();
    assert!(store.put_claims(id, nyaa, &named("hybrid"), false).unwrap());
    let mut t = [0u8; 20];
    t.copy_from_slice(&v2[..20]);
    hold(&mut store, Hold::Tombstone, t);
    assert!(store.torrent_claims(id).unwrap().is_empty());
}

/// While a hash is held, nothing comes back through any write path.
#[test]
fn a_held_hash_never_comes_back() {
    for how in HOLDS {
        let mut store = mem();
        let (dht, nyaa, _) = sources(&mut store);
        let c = named("again");
        let p = prepared(&store, "held again");
        let key = p.info_hash.0;
        store.put_unverified(&[item(&key, nyaa, Some(&c))]).unwrap();
        let id = store.unverified(Network::Bt, &key).unwrap().unwrap().id;
        hold(&mut store, how, key);

        assert_eq!(
            store.put_unverified(&[item(&key, nyaa, Some(&c))]).unwrap(),
            vec![Put::Held],
            "{how:?}"
        );
        assert_eq!(
            store.promote(id, &pt(&p, dht)).unwrap(),
            lodestar_store::unverified::Promoted::Gone
        );
        // A torrent the writer stored before the hold (or by mistake).
        let tid = store_torrent(&mut store, &p, dht);
        assert!(!store.put_claims(tid, nyaa, &c, false).unwrap(), "{how:?}");
        assert_eq!(count(&store, "SELECT count(*) FROM unverified_items"), 0);
        assert_eq!(count(&store, "SELECT count(*) FROM torrent_claims"), 0);
    }
}

#[test]
fn promotion_keeps_the_id_and_moves_the_claims() {
    use lodestar_store::unverified::Promoted;
    let mut store = mem();
    let (dht, nyaa, prowlarr) = sources(&mut store);
    let p = prepared(&store, "epsilon");
    let c = named("Epsilon");
    let Put::New(id) = store
        .put_unverified(&[item(&p.info_hash.0, nyaa, Some(&c))])
        .unwrap()[0]
    else {
        panic!()
    };
    store
        .put_unverified(&[item(&p.info_hash.0, prowlarr, None)])
        .unwrap();
    // Another key stored meanwhile takes a later id.
    let other = prepared(&store, "zeta");
    let other_id = store_torrent(&mut store, &other, dht);
    assert!(other_id > id);
    let next = next_id(&store);
    let reindex = count(&store, "SELECT count(*) FROM reindex_log");

    assert_eq!(store.promote(id, &pt(&p, dht)).unwrap(), Promoted::Kept(id));
    let row = store.torrent(id).unwrap().unwrap();
    assert_eq!(row.info_hash, p.info_hash);
    assert_eq!(
        row.sources,
        (1 << dht.get()) | (1 << nyaa.get()) | (1 << prowlarr.get())
    );
    assert_eq!(row.first_source, Some(nyaa));
    assert_eq!(row.discovered_at, T, "first found as unverified");
    assert_eq!(row.flags & flags::PRIVATE_SOURCE, 0);
    assert_eq!(claims_of(&store, id), vec![(nyaa, false, c)]);
    assert!(
        store
            .unverified(Network::Bt, &p.info_hash.0)
            .unwrap()
            .is_none()
    );
    assert!(
        !journalled(&store, &p.info_hash.0),
        "a promotion isn't a deletion"
    );
    assert_eq!(next_id(&store), next, "the sequence is untouched");
    assert_eq!(
        count(&store, "SELECT count(*) FROM reindex_log"),
        reindex + 1,
        "below the index watermark, so logged for re-indexing"
    );
    assert_eq!(store.promote(id, &pt(&p, dht)).unwrap(), Promoted::Gone);
}

#[test]
fn promotion_merges_into_a_torrent_stored_meanwhile() {
    use lodestar_store::unverified::Promoted;
    let mut store = mem();
    let (dht, nyaa, _) = sources(&mut store);
    let p = prepared(&store, "eta");
    store
        .put_unverified(&[NewUnverified {
            private_source: true,
            ..item(&p.info_hash.0, nyaa, Some(&named("Eta")))
        }])
        .unwrap();
    let id = store
        .unverified(Network::Bt, &p.info_hash.0)
        .unwrap()
        .unwrap()
        .id;
    // The crawler stored it under a new id without the item (SRC-2's
    // check is what prevents this; the store copes anyway).
    let tid = store
        .write(&WriteBatch {
            prepared: &[PreparedTorrent {
                extra_flags: flags::PRIVATE_SOURCE,
                ..pt(&p, dht)
            }],
            ..WriteBatch::default()
        })
        .unwrap()
        .prepared[0]
        .id();
    assert_ne!(tid, id);
    assert_eq!(
        store.promote(id, &pt(&p, dht)).unwrap(),
        Promoted::Merged(tid)
    );
    assert_eq!(store.torrent_claims(tid).unwrap().len(), 1);
    assert!(store.torrent(id).unwrap().is_none());
    let row = store.torrent(tid).unwrap().unwrap();
    assert_eq!(
        row.flags & flags::PRIVATE_SOURCE,
        0,
        "a public fetch clears it"
    );
}

#[test]
fn promotion_refuses_another_key_and_honours_holds() {
    use lodestar_store::unverified::Promoted;
    let mut store = mem();
    let (dht, nyaa, _) = sources(&mut store);
    let p = prepared(&store, "theta");
    let wrong = prepared(&store, "iota");
    store
        .put_unverified(&[item(&p.info_hash.0, nyaa, Some(&named("Theta")))])
        .unwrap();
    let id = store
        .unverified(Network::Bt, &p.info_hash.0)
        .unwrap()
        .unwrap()
        .id;
    let err = store.promote(id, &pt(&wrong, dht)).unwrap_err();
    assert!(matches!(err, Error::Invalid(_)), "{err:?}");
    assert!(
        store
            .unverified(Network::Bt, &p.info_hash.0)
            .unwrap()
            .is_some()
    );

    // A hybrid whose other hash is held: the item goes, nothing is stored.
    let mut hybrid = p.clone();
    hybrid.info_hash_v2 = Some(lodestar_metainfo::InfoHashV2([0xcd; 32]));
    hold(&mut store, Hold::CsHash, [0xcd; 20]);
    assert_eq!(
        store.promote(id, &pt(&hybrid, dht)).unwrap(),
        Promoted::Held
    );
    assert!(
        store
            .unverified(Network::Bt, &p.info_hash.0)
            .unwrap()
            .is_none()
    );
    assert!(journalled(&store, &p.info_hash.0));
    assert_eq!(count(&store, "SELECT count(*) FROM torrents"), 0);
}

#[test]
fn re_judge_pages_are_bounded_and_resumable() {
    let mut store = mem();
    let (_, nyaa, _) = sources(&mut store);
    let claims: Vec<Claims> = (0..1500).map(|i| named(&format!("n{i}"))).collect();
    let keys: Vec<[u8; 20]> = (0..1500u32)
        .map(|i| {
            let mut k = [0u8; 20];
            k[..4].copy_from_slice(&i.to_be_bytes());
            k[19] = 1;
            k
        })
        .collect();
    let items: Vec<_> = keys
        .iter()
        .zip(&claims)
        .map(|(k, c)| item(k, nyaa, Some(c)))
        .collect();
    store.put_unverified(&items).unwrap();
    let page = store.unverified_after(0, usize::MAX).unwrap();
    assert_eq!(page.len(), MAX_PAGE, "capped");
    let mut seen = 0;
    let mut after = 0;
    loop {
        let page = store.unverified_after(after, 400).unwrap();
        let Some(last) = page.last() else { break };
        after = last.id;
        seen += page.len();
    }
    assert_eq!(seen, 1500);

    // Large claims: a page stops once its decoded text passes the budget.
    let mut big = mem();
    let (_, nyaa, _) = sources(&mut big);
    let huge = Claims {
        description: Some("x".repeat(60 * 1024)),
        files: (0..240)
            .map(|i| ClaimedFile {
                path: format!("{i:04}").repeat(1000),
                size: None,
            })
            .collect(),
        ..Claims::default()
    };
    assert!(huge.text_len() > 1_000_000);
    let items: Vec<_> = keys[..20]
        .iter()
        .map(|k| item(k, nyaa, Some(&huge)))
        .collect();
    big.put_unverified(&items).unwrap();
    let page = big.unverified_after(0, 20).unwrap();
    assert!(page.len() < 20 && page.len() >= 8, "{}", page.len());
}

#[test]
fn refiling_holds_back_items_and_claims_under_current_rules_only() {
    let mut store = mem();
    let (dht, nyaa, _) = sources(&mut store);
    let bad = named("bad-name-marker");
    let key = [9u8; 20];
    store
        .put_unverified(&[item(&key, nyaa, Some(&bad))])
        .unwrap();
    let id = store.unverified(Network::Bt, &key).unwrap().unwrap().id;
    let hashes = [key];
    let f = Filtered {
        hashes: &hashes,
        rule: "user:1",
        action: 0,
        held: Some(Held::Tombstone),
        sealed: None,
        evidence: &[],
        at: T,
        expires_at: T,
        adult_hold: false,
    };
    let r = [RefiledUnverified {
        id,
        filtered: Some(f),
    }];
    // The rules moved on: nothing is written.
    assert_eq!(store.refile_unverified(&r, || false).unwrap(), None);
    assert!(store.unverified(Network::Bt, &key).unwrap().is_some());
    assert_eq!(count(&store, "SELECT count(*) FROM tombstones"), 0);
    assert_eq!(
        store.refile_unverified(&r, || true).unwrap(),
        Some(vec![id])
    );
    assert!(store.unverified(Network::Bt, &key).unwrap().is_none());
    assert_eq!(count(&store, "SELECT count(*) FROM tombstones"), 1);
    assert_eq!(count(&store, "SELECT count(*) FROM filter_hits"), 1);
    assert!(journalled(&store, &key));
    assert_eq!(store.refile_unverified(&r, || true).unwrap(), Some(vec![]));

    // Torrent claims: the claim goes, the torrent stays (it's judged on
    // its verified metadata).
    let p = prepared(&store, "kappa");
    let tid = store_torrent(&mut store, &p, dht);
    store.put_claims(tid, nyaa, &bad, false).unwrap();
    let page = store.claims_after((0, dht), 10).unwrap();
    assert_eq!(page.len(), 1);
    assert_eq!(page[0].claims.as_ref(), Some(&bad));
    let rc = [RefiledClaims {
        torrent_id: tid,
        source: nyaa,
        filtered: None,
    }];
    assert_eq!(store.refile_claims(&rc, || false).unwrap(), None);
    assert_eq!(
        store.refile_claims(&rc, || true).unwrap(),
        Some(ClaimsRefiled {
            claims: 1,
            torrents_deleted: vec![]
        })
    );
    assert!(store.torrent_claims(tid).unwrap().is_empty());
    assert!(store.torrent(tid).unwrap().is_some());
    assert!(store.claims_after((tid, nyaa), 10).unwrap().is_empty());
}

#[test]
fn an_unreadable_blob_is_reported_for_removal() {
    let mut store = mem();
    let (_, nyaa, _) = sources(&mut store);
    let key = [10u8; 20];
    store
        .put_unverified(&[item(&key, nyaa, Some(&named("x")))])
        .unwrap();
    store
        .connection()
        .execute(
            "UPDATE unverified_items SET claims = x'0900000000000000'",
            [],
        )
        .unwrap();
    let page = store.unverified_after(0, 10).unwrap();
    assert!(page[0].unreadable && page[0].claims.is_none());
    let r = [RefiledUnverified {
        id: page[0].id,
        filtered: None,
    }];
    assert_eq!(
        store.refile_unverified(&r, || true).unwrap(),
        Some(vec![page[0].id])
    );
}

/// Runs a purge to the end in steps of `limit`, summing the steps.
fn purge_all(store: &mut Store, source: SourceId, limit: usize) -> (PurgeStep, usize) {
    let mut total = PurgeStep::default();
    let mut cursor = PurgeCursor::START;
    let mut steps = 0;
    loop {
        let s = store.purge_source(source, cursor, limit).unwrap();
        steps += 1;
        total.unverified_deleted += s.unverified_deleted;
        total.unverified_kept += s.unverified_kept;
        total.claims_deleted += s.claims_deleted;
        total.torrents_deleted.extend(&s.torrents_deleted);
        total.torrents_kept += s.torrents_kept;
        total.state_deleted += s.state_deleted;
        total.queue_deleted += s.queue_deleted;
        match s.next {
            Some(c) => cursor = c,
            None => return (total, steps),
        }
    }
}

#[test]
fn purge_removes_only_what_the_source_alone_found() {
    let mut store = mem();
    let (dht, nyaa, prowlarr) = sources(&mut store);
    // Index watermark past every id, so removals count in Insights.
    store
        .connection()
        .execute(
            "INSERT INTO index_watermark_hour (hour, last_id) VALUES (1, 1000000)",
            [],
        )
        .ok();
    store
        .connection()
        .execute("UPDATE index_watermark_hour SET last_id = 1000000", [])
        .unwrap();
    let (cn, cp) = (named("only-nyaa-marker"), named("prowlarr"));
    let keys: Vec<[u8; 20]> = (1..=7u8).map(|i| [0x40 | i; 20]).collect();
    // 0–2 only Nyaa; 3 Nyaa then Prowlarr (Nyaa owns the claims); 4 the
    // other way round; 5 only Prowlarr.
    for k in &keys[..5] {
        store.put_unverified(&[item(k, nyaa, Some(&cn))]).unwrap();
    }
    store
        .put_unverified(&[item(&keys[3], prowlarr, Some(&cp))])
        .unwrap();
    store
        .put_unverified(&[item(&keys[5], prowlarr, Some(&cp))])
        .unwrap();
    store
        .put_unverified(&[item(&keys[4], prowlarr, None)])
        .unwrap();
    // Torrents: one only Nyaa found, one Nyaa and the DHT, one the DHT.
    let pa = prepared(&store, "nyaa only");
    let pb = prepared(&store, "both");
    let pc = prepared(&store, "dht only");
    let ta = store_torrent(&mut store, &pa, nyaa);
    let tb = store_torrent(&mut store, &pb, dht);
    store_torrent(&mut store, &pb, nyaa);
    let tc = store_torrent(&mut store, &pc, dht);
    store.put_claims(tb, nyaa, &cn, false).unwrap();
    store.put_claims(tc, prowlarr, &cp, false).unwrap();
    store.set_plugin_state(nyaa, "cursor", Some(b"42")).unwrap();
    store
        .set_plugin_state(prowlarr, "cursor", Some(b"7"))
        .unwrap();
    let torrents = count(&store, "SELECT count(*) FROM torrents");
    let removed = |store: &Store| {
        count(
            store,
            "SELECT coalesce(sum(value), 0) FROM rollup_hour WHERE metric = 15",
        )
    };
    let removed_before = removed(&store);

    let err = store
        .purge_source(nyaa, PurgeCursor::START, 10)
        .unwrap_err();
    assert!(
        matches!(err, Error::Invalid(_)),
        "an enabled source: {err:?}"
    );
    store.set_source_state(nyaa, SourceState::Disabled).unwrap();

    let (total, steps) = purge_all(&mut store, nyaa, 2);
    assert!(steps > 4, "chunked: {steps}");
    assert_eq!(total.unverified_deleted, 3);
    assert_eq!(total.unverified_kept, 2);
    assert_eq!(total.torrents_deleted, vec![ta]);
    assert_eq!(total.torrents_kept, 1);
    assert_eq!(total.claims_deleted, 1);
    assert_eq!(total.state_deleted, 1);

    for k in &keys[..3] {
        assert!(store.unverified(Network::Bt, k).unwrap().is_none());
        assert!(journalled(&store, k));
    }
    // Nyaa owned 3's claims: they go, and Prowlarr is left as the finder.
    let k3 = store.unverified(Network::Bt, &keys[3]).unwrap().unwrap();
    assert_eq!(
        (k3.source, k3.sources, k3.claims),
        (prowlarr, 1 << prowlarr.get(), None)
    );
    // 4's claims were Nyaa's too; Prowlarr's later claims now take it.
    store
        .put_unverified(&[item(&keys[4], prowlarr, Some(&cp))])
        .unwrap();
    let k4 = store.unverified(Network::Bt, &keys[4]).unwrap().unwrap();
    assert_eq!((k4.source, k4.claims.as_ref()), (prowlarr, Some(&cp)));
    assert!(store.unverified(Network::Bt, &keys[5]).unwrap().is_some());

    assert!(store.torrent(ta).unwrap().is_none());
    assert_eq!(store.torrent(tb).unwrap().unwrap().sources, 1 << dht.get());
    assert!(store.torrent_claims(tb).unwrap().is_empty());
    assert_eq!(
        store.torrent_claims(tc).unwrap().len(),
        1,
        "Prowlarr's stay"
    );
    assert_eq!(store.plugin_state(nyaa, "cursor").unwrap(), None);
    assert_eq!(
        store.plugin_state(prowlarr, "cursor").unwrap(),
        Some(b"7".to_vec())
    );

    // The rollups reconcile: the store lost exactly what Insights counts
    // as removed.
    assert_eq!(count(&store, "SELECT count(*) FROM torrents"), torrents - 1);
    assert_eq!(removed(&store) - removed_before, 1);
    // A second purge finds nothing.
    let (again, _) = purge_all(&mut store, nyaa, 100);
    assert_eq!(again, PurgeStep::default());
}

#[test]
fn purged_names_leave_the_database_file() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("lodestar.db");
    let mut store = Store::open(&db, StoreConfig::default()).unwrap();
    let (_, nyaa, _) = sources(&mut store);
    let marker = "purge-marker-name-31f7";
    let c = Claims {
        name: Some(marker.into()),
        ..Claims::default()
    };
    store
        .put_unverified(&[item(&[0x51; 20], nyaa, Some(&c))])
        .unwrap();
    store.set_source_state(nyaa, SourceState::Disabled).unwrap();
    purge_all(&mut store, nyaa, 100);
    drop(store);
    for entry in std::fs::read_dir(dir.path()).unwrap() {
        let bytes = std::fs::read(entry.unwrap().path()).unwrap();
        assert!(
            !bytes.windows(marker.len()).any(|w| w == marker.as_bytes()),
            "secure_delete zeroes the name"
        );
    }
}

#[test]
fn plugin_state_round_trips_within_its_limits() {
    let mut store = mem();
    let (_, nyaa, prowlarr) = sources(&mut store);
    assert_eq!(store.plugin_state(nyaa, "etag").unwrap(), None);
    store.set_plugin_state(nyaa, "etag", Some(b"W/1")).unwrap();
    assert_eq!(
        store.plugin_state(nyaa, "etag").unwrap(),
        Some(b"W/1".to_vec())
    );
    assert_eq!(
        store.plugin_state(prowlarr, "etag").unwrap(),
        None,
        "per source"
    );
    // Compare-and-set.
    assert!(
        !store
            .compare_and_set_plugin_state(nyaa, "etag", Some(b"W/0"), Some(b"W/2"))
            .unwrap()
    );
    assert!(
        store
            .compare_and_set_plugin_state(nyaa, "etag", Some(b"W/1"), Some(b"W/2"))
            .unwrap()
    );
    assert!(
        store
            .compare_and_set_plugin_state(nyaa, "new", None, Some(b""))
            .unwrap()
    );
    assert!(
        store
            .compare_and_set_plugin_state(nyaa, "new", Some(b""), None)
            .unwrap()
    );
    assert_eq!(store.plugin_state(nyaa, "new").unwrap(), None);
    store.set_plugin_state(nyaa, "etag", None).unwrap();
    store.set_plugin_state(nyaa, "absent", None).unwrap();

    let invalid = |r: lodestar_store::Result<()>| matches!(r, Err(Error::Invalid(_)));
    assert!(invalid(store.set_plugin_state(nyaa, "", Some(b"x"))));
    assert!(invalid(store.set_plugin_state(
        nyaa,
        &"k".repeat(129),
        Some(b"x")
    )));
    assert!(invalid(store.set_plugin_state(
        nyaa,
        "v",
        Some(&vec![0; 64 * 1024 + 1])
    )));
    store
        .set_plugin_state(nyaa, &"k".repeat(128), Some(&vec![0; 64 * 1024]))
        .unwrap();
    for i in 1..1024 {
        store
            .set_plugin_state(nyaa, &format!("key{i}"), Some(b"1"))
            .unwrap();
    }
    assert!(invalid(store.set_plugin_state(
        nyaa,
        "one-more",
        Some(b"1")
    )));
    // Replacing a key when full is fine.
    store.set_plugin_state(nyaa, "key1", Some(b"2")).unwrap();
}

/// SRC-1's space measurement (`docs/decisions/2026-11-02-SRC-1.md`):
/// bytes per row of `unverified_items` and `torrent_claims` with typical
/// feed claims (a name, a size, a few files, a tracker, a publisher and a
/// GUID). Run with `cargo test --release -p lodestar-store --test
/// unverified space_per_row -- --ignored --nocapture`.
#[test]
#[ignore = "measurement"]
fn space_per_row() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::open(dir.path().join("m.db"), StoreConfig::default()).unwrap();
    let (_, nyaa, _) = sources(&mut store);
    let n = 100_000u32;
    let claims: Vec<Claims> = (0..n)
        .map(|i| Claims {
            name: Some(format!(
                "[Group] Some Show - {:02} (1080p) [{i:08X}]",
                i % 24
            )),
            size: Some(1_400_000_000 + u64::from(i)),
            files: (0..3u64)
                .map(|f| ClaimedFile {
                    path: format!("Some Show - {:02} part {f}.mkv", i % 24),
                    size: Some(400_000_000 + f),
                })
                .collect(),
            trackers: vec!["http://nyaa.tracker.wf:7777/announce".into()],
            publisher: Some("Group".into()),
            provenance: Provenance {
                external_ref: Some(format!("https://nyaa.si/view/{}", 1_000_000 + i)),
                published_at: Some(T + i64::from(i)),
                confidence: Confidence::Medium,
            },
            ..Claims::default()
        })
        .collect();
    let keys: Vec<[u8; 20]> = (0..n)
        .map(|i| {
            let mut k = [7u8; 20];
            k[..4].copy_from_slice(&i.to_be_bytes());
            k
        })
        .collect();
    let blob: usize = claims
        .iter()
        .map(|c| lodestar_store::claims::encode(c, 3).unwrap().len())
        .sum();
    for (k, c) in keys.chunks(1000).zip(claims.chunks(1000)) {
        let items: Vec<_> = k
            .iter()
            .zip(c)
            .map(|(k, c)| item(k, nyaa, Some(c)))
            .collect();
        store.put_unverified(&items).unwrap();
    }
    let size = |store: &Store, name: &str| -> i64 {
        store
            .connection()
            .query_row(
                "SELECT coalesce(sum(pgsize), 0) FROM dbstat WHERE name = ?1",
                [name],
                |r| r.get(0),
            )
            .unwrap_or(-1)
    };
    let items =
        size(&store, "unverified_items") + size(&store, "sqlite_autoindex_unverified_items_1");
    // Promote them all, which moves every claim to `torrent_claims`.
    store
        .connection()
        .execute_batch(
            "INSERT INTO torrents (id, info_hash, name, size, piece_len, piece_count, file_count,
                 info_size, flags, discovered_at, metadata_at, last_seen_at, sources)
             SELECT id, key, 'x', 1, 1, 1, 1, 1, 0, 0, 0, 0, 2 FROM unverified_items;
             INSERT INTO torrent_claims (torrent_id, source_id, flags, data)
             SELECT id, source_id, 0, claims FROM unverified_items;",
        )
        .unwrap();
    let tc = size(&store, "torrent_claims")
        + size(&store, "sqlite_autoindex_torrent_claims_1")
        + size(&store, "torrent_claims_source");
    eprintln!(
        "{n} rows; claim blob avg {} B; unverified_items {} B/row (incl. its unique index); \
         torrent_claims {} B/row (incl. both indexes)",
        blob / n as usize,
        items / i64::from(n),
        tc / i64::from(n),
    );
}

/// An item found and fetched only privately stays private-source when
/// promoted, and its claims keep their private mark.
#[test]
fn a_private_promotion_stays_private() {
    use lodestar_store::unverified::Promoted;
    let mut store = mem();
    let (_, _, prowlarr) = sources(&mut store);
    let p = prepared(&store, "private one");
    store
        .put_unverified(&[NewUnverified {
            private_source: true,
            ..item(&p.info_hash.0, prowlarr, Some(&named("Private")))
        }])
        .unwrap();
    let id = store
        .unverified(Network::Bt, &p.info_hash.0)
        .unwrap()
        .unwrap()
        .id;
    let fetched = PreparedTorrent {
        extra_flags: flags::PRIVATE_SOURCE,
        ..pt(&p, prowlarr)
    };
    assert_eq!(store.promote(id, &fetched).unwrap(), Promoted::Kept(id));
    let row = store.torrent(id).unwrap().unwrap();
    assert_ne!(row.flags & flags::PRIVATE_SOURCE, 0);
    assert_eq!(
        claims_of(&store, id),
        vec![(prowlarr, true, named("Private"))]
    );
}

/// Claims that came through a private source keep that mark when the key
/// is later seen publicly; only public claims replacing them clear it. A
/// stored torrent's claims carry the mark of the sighting that wrote them.
#[test]
fn private_claims_stay_marked() {
    let mut store = mem();
    let (dht, nyaa, prowlarr) = sources(&mut store);
    let key = [0x70u8; 20];
    let private = |c| NewUnverified {
        private_source: true,
        ..item(&key, prowlarr, Some(c))
    };
    let (a, b) = (named("indexer name"), named("public name"));
    store.put_unverified(&[private(&a)]).unwrap();
    store.put_unverified(&[item(&key, nyaa, None)]).unwrap();
    let got = store.unverified(Network::Bt, &key).unwrap().unwrap();
    assert!(!got.private_source, "seen publicly");
    assert!(got.claims_private, "the claims are still the indexer's");
    store
        .put_unverified(&[item(&key, prowlarr, Some(&b))])
        .unwrap();
    assert!(
        !store
            .unverified(Network::Bt, &key)
            .unwrap()
            .unwrap()
            .claims_private
    );

    let p = prepared(&store, "public torrent");
    let tid = store_torrent(&mut store, &p, dht);
    store
        .put_unverified(&[NewUnverified {
            private_source: true,
            ..item(&p.info_hash.0, prowlarr, Some(&a))
        }])
        .unwrap();
    assert_eq!(claims_of(&store, tid), vec![(prowlarr, true, a)]);
    let row = store.torrent(tid).unwrap().unwrap();
    assert_eq!(
        row.flags & flags::PRIVATE_SOURCE,
        0,
        "a public torrent stays public"
    );
}

#[test]
fn promotion_needs_verified_metadata() {
    let mut store = mem();
    let (dht, nyaa, _) = sources(&mut store);
    let p = prepared(&store, "unverified meta");
    store
        .put_unverified(&[item(&p.info_hash.0, nyaa, Some(&named("x")))])
        .unwrap();
    let id = store
        .unverified(Network::Bt, &p.info_hash.0)
        .unwrap()
        .unwrap()
        .id;
    let err = store
        .promote(
            id,
            &PreparedTorrent {
                verified: false,
                ..pt(&p, dht)
            },
        )
        .unwrap_err();
    assert!(matches!(err, Error::Invalid(_)), "{err:?}");
}

/// A held decision on a torrent's claims deletes the torrent too, so
/// nothing stays stored under a held hash.
#[test]
fn a_held_claim_decision_deletes_the_torrent() {
    for held in [Held::Tombstone, Held::Quarantine] {
        let mut store = mem();
        let (dht, nyaa, _) = sources(&mut store);
        let p = prepared(&store, "claimed");
        let tid = store_torrent(&mut store, &p, dht);
        store
            .put_claims(tid, nyaa, &named("blocked"), false)
            .unwrap();
        let hashes = [p.info_hash.0];
        let rc = [RefiledClaims {
            torrent_id: tid,
            source: nyaa,
            filtered: Some(Filtered {
                hashes: &hashes,
                rule: "CS-2",
                action: 0,
                held: Some(held),
                sealed: None,
                evidence: &[],
                at: T,
                expires_at: T + 86_400,
                adult_hold: false,
            }),
        }];
        let reindex = count(&store, "SELECT count(*) FROM reindex_log");
        assert_eq!(
            store.refile_claims(&rc, || true).unwrap(),
            Some(ClaimsRefiled {
                claims: 1,
                torrents_deleted: vec![tid]
            })
        );
        assert!(store.torrent(tid).unwrap().is_none(), "{held:?}");
        assert!(journalled(&store, &p.info_hash.0));
        assert_eq!(
            count(&store, "SELECT count(*) FROM reindex_log"),
            reindex + 1
        );
    }
}

/// Source 63 is the sign bit of SQLite's 64-bit integers: the bitset, the
/// schema's check and the purge all handle it.
#[test]
fn source_63_works_everywhere() {
    let mut store = mem();
    let ids: Vec<SourceId> = (0..64)
        .map(|i| store.register_source(&format!("s{i}"), "1", T).unwrap())
        .collect();
    let (s1, s63) = (ids[1], ids[63]);
    assert_eq!(s63.get(), 63);
    let c = named("sixty-three");
    let (only, shared) = ([0x63u8; 20], [0x64u8; 20]);
    store.put_unverified(&[item(&only, s63, Some(&c))]).unwrap();
    store
        .put_unverified(&[item(&shared, s63, Some(&c))])
        .unwrap();
    store.put_unverified(&[item(&shared, s1, None)]).unwrap();
    let got = store.unverified(Network::Bt, &shared).unwrap().unwrap();
    assert_eq!(got.sources, (1 << 63) | (1 << 1));
    let p = prepared(&store, "found by 63");
    let tid = store_torrent(&mut store, &p, s63);
    assert_eq!(store.torrent(tid).unwrap().unwrap().sources, 1 << 63);
    store.put_claims(tid, s63, &c, false).unwrap();

    store.set_source_state(s63, SourceState::Disabled).unwrap();
    let (total, _) = purge_all(&mut store, s63, 1);
    assert_eq!(total.unverified_deleted, 1);
    assert_eq!(total.unverified_kept, 1);
    assert_eq!(total.torrents_deleted, vec![tid]);
    assert_eq!(total.claims_deleted, 1);
    let got = store.unverified(Network::Bt, &shared).unwrap().unwrap();
    assert_eq!((got.source, got.sources, got.claims), (s1, 1 << 1, None));
    assert!(store.unverified(Network::Bt, &only).unwrap().is_none());
}

/// A purge also drops the keys the source queued for fetching, so none is
/// stored under its bit afterwards.
#[test]
fn purge_drops_the_sources_queued_keys() {
    let mut store = mem();
    let (dht, nyaa, _) = sources(&mut store);
    let key = |b: u8| lodestar_store::QueuedKey {
        info_hash: lodestar_metainfo::InfoHash([b; 20]),
        source: if b == 1 { nyaa } else { dht },
        cohort: 0,
        strategy: 0,
        priority: 0,
        attempts: 0,
        private_source: false,
    };
    store.save_fetch_queue(&[key(1), key(2)]).unwrap();
    store.set_source_state(nyaa, SourceState::Disabled).unwrap();
    let (total, _) = purge_all(&mut store, nyaa, 10);
    assert_eq!(total.queue_deleted, 1);
    let left = store.load_fetch_queue().unwrap();
    assert_eq!(left.len(), 1);
    assert_eq!(left[0].source, dht);
}
