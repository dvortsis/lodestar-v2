//! Sources' store (SRC-1, V14): unverified items, claims, plugin state and
//! purges, through the public API. Content safety first: a held hash never
//! lives in or comes back through either table, on every path.

use lodestar_bencode::{OwnedValue, to_vec};
use lodestar_metainfo::Info;
use lodestar_store::claims::{
    AdultEnrichment, AdultKind, AltTitle, ClaimedFile, Claims, Confidence, Details, ExternalId,
    Provenance,
};
use lodestar_store::identify_sweep::TERMS_KEY;
use lodestar_store::unverified::{
    ClaimsRefiled, MAX_PAGE, MAX_ROW, Network, NewUnverified, PAGE_BYTES, PurgeCursor,
    PurgePreview, PurgeStep, Put, RefiledClaims, RefiledUnverified, UnverifiedRefiled,
};
use lodestar_store::{
    AdultMark, ClassCorrection, Error, Filtered, Held, Prepared, PreparedTorrent, SourceId,
    SourceState, Store, StoreConfig, WriteBatch, flags,
};
use rusqlite::params;

const T: i64 = 1_700_000_000;
/// The codes Insights counts searchable torrents under (`lodestar::insights::CODES`).
const CODES: lodestar_store::SearchableCodes = lodestar_store::SearchableCodes {
    stage: 5,
    newly: 10,
    delay: 11,
};

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
        adult: None,
        details: None,
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
            store.promote(id, &pt(&p, dht), CODES, T).unwrap(),
            lodestar_store::unverified::Promoted::Gone
        );
        // The write path reads a tombstone or quarantine row in its own
        // transaction: a fetch under the held hash isn't stored. (A CS hash
        // list entry is the filter's to judge at commit.)
        if !matches!(how, Hold::CsHash) {
            let written = store
                .write(&WriteBatch {
                    prepared: &[pt(&p, dht)],
                    ..WriteBatch::default()
                })
                .unwrap();
            assert_eq!(
                written.prepared,
                [lodestar_store::Inserted::Held],
                "{how:?}"
            );
        }
        // A torrent stored anyway (by mistake), outside the write path.
        let raw = info_bytes("held again");
        let info = Info::parse(&raw).unwrap();
        let tid = store
            .insert(&[lodestar_store::NewTorrent {
                info: &info,
                raw_info: None,
                discovered_at: T,
                metadata_at: T,
                extra_flags: 0,
                source: dht,
            }])
            .unwrap()[0]
            .id();
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

    assert_eq!(
        store.promote(id, &pt(&p, dht), CODES, T).unwrap(),
        Promoted::Kept(id)
    );
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
    assert_eq!(
        store.promote(id, &pt(&p, dht), CODES, T).unwrap(),
        Promoted::Gone
    );
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
        store.promote(id, &pt(&p, dht), CODES, T).unwrap(),
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
    let err = store.promote(id, &pt(&wrong, dht), CODES, T).unwrap_err();
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
        store.promote(id, &pt(&hybrid, dht), CODES, T).unwrap(),
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

    // Many entries and no text: weighed by decoded size, not text. 10,000
    // empty paths decode to ~400 KB each, so a page stops well before
    // 1,024 rows, and stays under its bound.
    let mut empty = mem();
    let (dht, nyaa, _) = sources(&mut empty);
    let hollow = Claims {
        files: vec![
            ClaimedFile {
                path: String::new(),
                size: None
            };
            10_000
        ],
        ..Claims::default()
    };
    assert_eq!(hollow.text_len(), 0);
    let keys: Vec<[u8; 20]> = (0..60u8).map(|i| [i ^ 0x80; 20]).collect();
    let items: Vec<_> = keys.iter().map(|k| item(k, nyaa, Some(&hollow))).collect();
    empty.put_unverified(&items).unwrap();
    let page = empty.unverified_after(0, MAX_PAGE).unwrap();
    let held: usize = page
        .iter()
        .map(|i| i.claims.as_ref().unwrap().decoded_size())
        .sum();
    assert!(page.len() < 60, "{}", page.len());
    assert!(held < PAGE_BYTES + MAX_ROW, "{held}");
    // The same for torrent claims.
    for k in &keys {
        let p = prepared(&empty, &format!("{k:?}"));
        let tid = store_torrent(&mut empty, &p, dht);
        empty.put_claims(tid, nyaa, &hollow, false).unwrap();
    }
    let page = empty.claims_after((0, dht), MAX_PAGE).unwrap();
    assert!(page.len() < 60, "{}", page.len());
}

/// A held decision on an unverified item also deletes a torrent the
/// crawler stored under the same hash before the writer merged them.
#[test]
fn a_held_item_decision_deletes_a_torrent_under_its_hash() {
    let mut store = mem();
    let (dht, nyaa, _) = sources(&mut store);
    let p = prepared(&store, "crawled meanwhile");
    store
        .put_unverified(&[item(&p.info_hash.0, nyaa, Some(&named("blocked")))])
        .unwrap();
    let id = store
        .unverified(Network::Bt, &p.info_hash.0)
        .unwrap()
        .unwrap()
        .id;
    let tid = store_torrent(&mut store, &p, dht);
    let hashes = [p.info_hash.0];
    let r = [RefiledUnverified {
        id,
        filtered: Some(Filtered {
            hashes: &hashes,
            rule: "CS-2",
            action: 0,
            held: Some(Held::Quarantine),
            sealed: None,
            evidence: &[],
            at: T,
            expires_at: T + 86_400,
            adult_hold: false,
        }),
    }];
    let reindex = count(&store, "SELECT count(*) FROM reindex_log");
    assert_eq!(
        store.refile_unverified(&r, || true).unwrap(),
        Some(UnverifiedRefiled {
            ids: vec![id],
            torrents_deleted: vec![tid]
        })
    );
    assert!(store.torrent(tid).unwrap().is_none());
    assert_eq!(
        count(&store, "SELECT count(*) FROM reindex_log"),
        reindex + 1
    );
}

/// A disabled or purged source can't write items or claims: a poll in
/// flight can't put back what a purge removed. A paused one can finish.
#[test]
fn a_disabled_source_cannot_write() {
    let mut store = mem();
    let (dht, nyaa, _) = sources(&mut store);
    let p = prepared(&store, "late");
    let tid = store_torrent(&mut store, &p, dht);
    let c = named("late");
    store.set_source_state(nyaa, SourceState::Paused).unwrap();
    store
        .put_unverified(&[item(&[0x77; 20], nyaa, Some(&c))])
        .unwrap();
    for state in [SourceState::Disabled, SourceState::Purged] {
        store.set_source_state(nyaa, state).unwrap();
        let err = store
            .put_unverified(&[item(&[0x78; 20], nyaa, Some(&c))])
            .unwrap_err();
        assert!(matches!(err, Error::Invalid(_)), "{state:?}: {err:?}");
        let err = store.put_claims(tid, nyaa, &c, false).unwrap_err();
        assert!(matches!(err, Error::Invalid(_)), "{state:?}: {err:?}");
    }
    assert!(
        store
            .unverified(Network::Bt, &[0x78; 20])
            .unwrap()
            .is_none()
    );
    assert!(store.torrent_claims(tid).unwrap().is_empty());
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
        store.refile_unverified(&r, || true).unwrap().unwrap().ids,
        vec![id]
    );
    assert!(store.unverified(Network::Bt, &key).unwrap().is_none());
    assert_eq!(count(&store, "SELECT count(*) FROM tombstones"), 1);
    assert_eq!(count(&store, "SELECT count(*) FROM filter_hits"), 1);
    assert!(journalled(&store, &key));
    assert_eq!(
        store.refile_unverified(&r, || true).unwrap(),
        Some(UnverifiedRefiled::default())
    );

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
            torrents_deleted: vec![],
            rederive: false,
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
        store.refile_unverified(&r, || true).unwrap().unwrap().ids,
        vec![page[0].id]
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
        total.rederive |= s.rederive;
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
    // One key Nyaa queued while it ran, one the DHT did.
    let queued = |b: u8, source| lodestar_store::QueuedKey {
        info_hash: lodestar_metainfo::InfoHash([b; 20]),
        source,
        cohort: 0,
        strategy: 0,
        priority: 0,
        attempts: 0,
        private_source: false,
    };
    store
        .save_fetch_queue(&[queued(0x77, nyaa), queued(0x78, dht)])
        .unwrap();
    store.set_source_state(nyaa, SourceState::Disabled).unwrap();

    // The preview counts what the purge then removes, and changes nothing
    // (X-sources-api-spec).
    let tables = |store: &Store| {
        [
            "unverified_items",
            "torrents",
            "torrent_claims",
            "source_state",
            "fetch_queue",
        ]
        .map(|t| count(store, &format!("SELECT count(*) FROM {t}")))
    };
    let before = tables(&store);
    let preview = store.purge_preview(nyaa).unwrap();
    assert_eq!(tables(&store), before, "the preview wrote nothing");
    assert_eq!(store.purge_preview(nyaa).unwrap(), preview, "and repeats");
    assert!(matches!(
        store.purge_preview(SourceId::new(40).unwrap()),
        Err(Error::NotFound(_))
    ));

    let (total, steps) = purge_all(&mut store, nyaa, 2);
    assert!(steps > 4, "chunked: {steps}");
    assert_eq!(
        preview,
        PurgePreview {
            unverified_deleted: total.unverified_deleted as u64,
            unverified_kept: total.unverified_kept as u64,
            claims_deleted: total.claims_deleted as u64,
            torrents_deleted: total.torrents_deleted.len() as u64,
            torrents_kept: total.torrents_kept as u64,
            state_deleted: total.state_deleted as u64,
            queue_deleted: total.queue_deleted as u64,
        }
    );
    assert_eq!(total.queue_deleted, 1);
    assert_eq!(store.purge_preview(nyaa).unwrap(), PurgePreview::default());
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

/// SRC-1's reconciliation, for promoted torrents (SRC-2): a promotion
/// below the searchable watermark is counted as newly searchable in its
/// own transaction, one above it is left to `record_searchable`, and
/// either way a purge that deletes it leaves Insights' newly searchable
/// minus removed equal to the torrents stored.
#[test]
fn purge_reconciles_promoted_torrents() {
    let mut store = mem();
    let (_, nyaa, _) = sources(&mut store);
    let c = named("promoted");
    let newly = |store: &Store| {
        count(
            store,
            "SELECT coalesce(sum(value), 0) FROM rollup_hour WHERE metric = 10",
        )
    };
    let removed = |store: &Store| {
        count(
            store,
            "SELECT coalesce(sum(value), 0) FROM rollup_hour WHERE metric = 15",
        )
    };
    // Two items; the index then passes the first one's id only.
    let (ka, kb) = (
        prepared(&store, "promoted a"),
        prepared(&store, "promoted b"),
    );
    store
        .put_unverified(&[item(&ka.info_hash.0, nyaa, Some(&c))])
        .unwrap();
    let a = store
        .unverified(Network::Bt, &ka.info_hash.0)
        .unwrap()
        .unwrap()
        .id;
    store.record_searchable(a, T, CODES).unwrap();
    store
        .put_unverified(&[item(&kb.info_hash.0, nyaa, Some(&c))])
        .unwrap();
    let b = store
        .unverified(Network::Bt, &kb.info_hash.0)
        .unwrap()
        .unwrap()
        .id;
    assert!(b > a);
    assert_eq!(newly(&store), 0, "no torrents yet");

    // Fetched through the feed itself (a verification fetch), so only
    // the feed found them.
    assert_eq!(
        store.promote(a, &pt(&ka, nyaa), CODES, T).unwrap(),
        lodestar_store::unverified::Promoted::Kept(a)
    );
    assert_eq!(newly(&store), 1, "below the watermark: counted at once");
    assert_eq!(
        store.promote(b, &pt(&kb, nyaa), CODES, T).unwrap(),
        lodestar_store::unverified::Promoted::Kept(b)
    );
    assert_eq!(newly(&store), 1, "above it: not yet");
    store.record_searchable(b, T + 60, CODES).unwrap();
    assert_eq!(newly(&store), 2, "counted once, when the index passes it");
    let torrents = count(&store, "SELECT count(*) FROM torrents");
    assert_eq!(newly(&store) - removed(&store), torrents);

    store.set_source_state(nyaa, SourceState::Disabled).unwrap();
    let (total, _) = purge_all(&mut store, nyaa, 100);
    assert_eq!(total.torrents_deleted.len(), 2);
    assert_eq!(count(&store, "SELECT count(*) FROM torrents"), 0);
    assert_eq!(newly(&store) - removed(&store), 0, "the rollups reconcile");
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
    assert_eq!(
        store.promote(id, &fetched, CODES, T).unwrap(),
        Promoted::Kept(id)
    );
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
            CODES,
            T,
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
                torrents_deleted: vec![tid],
                rederive: false,
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

/// A promotion adds no stopped source's bit or claims, takes a live
/// finder as `first_source`, and with no live finder left leaves the
/// torrent to the usual insert, which refuses it (SRC-14b).
#[test]
fn a_promotion_writes_nothing_of_a_stopped_source() {
    let mut store = mem();
    let (dht, nyaa, prowlarr) = sources(&mut store);
    let p = prepared(&store, "eta");
    let c = named("Eta");
    let Put::New(id) = store
        .put_unverified(&[item(&p.info_hash.0, nyaa, Some(&c))])
        .unwrap()[0]
    else {
        panic!()
    };
    store
        .put_unverified(&[item(&p.info_hash.0, prowlarr, None)])
        .unwrap();
    // The item's finder (and claims' owner) is purged; the fetching
    // source too.
    store.set_source_state(nyaa, SourceState::Purged).unwrap();
    store.set_source_state(dht, SourceState::Disabled).unwrap();
    assert_eq!(
        store.promote(id, &pt(&p, dht), CODES, T).unwrap(),
        lodestar_store::unverified::Promoted::Kept(id)
    );
    let row: (i64, i64) = store
        .connection()
        .query_row(
            "SELECT sources, first_source FROM torrents WHERE id = ?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(row, (1 << prowlarr.get(), i64::from(prowlarr.get())));
    assert!(
        claims_of(&store, id).is_empty(),
        "the purged source's claims stay out"
    );

    // Only stopped finders: not promoted, and the insert refuses it.
    let q = prepared(&store, "theta");
    let Put::New(id) = store
        .put_unverified(&[item(&q.info_hash.0, prowlarr, Some(&c))])
        .unwrap()[0]
    else {
        panic!()
    };
    store
        .set_source_state(prowlarr, SourceState::Disabled)
        .unwrap();
    assert_eq!(
        store.promote(id, &pt(&q, dht), CODES, T).unwrap(),
        lodestar_store::unverified::Promoted::Gone
    );
    let w = store
        .write(&WriteBatch {
            prepared: &[pt(&q, dht)],
            ..WriteBatch::default()
        })
        .unwrap();
    assert_eq!(w.prepared, [lodestar_store::Inserted::Refused]);
}

/// An item whose public sighting may be a stopped finder's is promoted
/// private: only a live public sighting makes it public (SRC-14b).
#[test]
fn a_promotion_ignores_a_stopped_finders_public_mark() {
    let mut store = mem();
    let (dht, nyaa, prowlarr) = sources(&mut store);
    let p = prepared(&store, "iota");
    let c = named("Iota");
    let Put::New(id) = store
        .put_unverified(&[item(&p.info_hash.0, nyaa, Some(&c))])
        .unwrap()[0]
    else {
        panic!()
    };
    let private = NewUnverified {
        private_source: true,
        ..item(&p.info_hash.0, prowlarr, None)
    };
    store.put_unverified(&[private]).unwrap();
    store.set_source_state(nyaa, SourceState::Purged).unwrap();
    // A private fetch by a live source.
    let fetch = PreparedTorrent {
        extra_flags: flags::PRIVATE_SOURCE,
        ..pt(&p, dht)
    };
    store.promote(id, &fetch, CODES, T).unwrap();
    let row = store.torrent(id).unwrap().unwrap();
    assert_ne!(row.flags & flags::PRIVATE_SOURCE, 0, "stored private");
}

/// ENR-G: purging a source marks for re-derivation (`parser_ver = 0`) each
/// torrent that lost a claim that shaped its record (an adult match naming
/// a kind, here), so a purged match doesn't stay in the record or the
/// index. A torrent that lost only a plain feed claim, or that only another
/// source claims, keeps its version. One row a step pairs every mark with
/// its delete.
#[test]
fn a_purge_marks_the_torrents_that_lost_a_record_shaping_claim() {
    let ver = |store: &Store, id: i64| -> i64 {
        store
            .connection()
            .query_row(
                "SELECT parser_ver FROM torrent_parsed WHERE torrent_id = ?1",
                [id],
                |r| r.get(0),
            )
            .unwrap()
    };
    let matched = |_: &str| Claims {
        adult: Some(AdultEnrichment {
            scene_id: "1".into(),
            kind: Some(AdultKind::Scene),
            title: None,
            studio: None,
            released: None,
            performers: Vec::new(),
            performers_truncated: false,
        }),
        ..Claims::default()
    };
    for limit in [1, 100] {
        let mut store = mem();
        let (dht, enr, other) = sources(&mut store);
        let mut ids = Vec::new();
        for (name, by, adult) in [
            ("one", enr, true),
            ("two", enr, true),
            ("three", enr, false),
            ("four", other, true),
        ] {
            let p = prepared(&store, name);
            let id = store_torrent(&mut store, &p, dht);
            let c = if adult { matched(name) } else { named(name) };
            store
                .put_unverified(&[item(&p.info_hash.0, by, Some(&c))])
                .unwrap();
            // What the enrichment backfill writes once it has derived it.
            store
                .connection()
                .execute(
                    "INSERT INTO torrent_parsed (torrent_id, parser_ver, data) VALUES (?1, 7, x'00')",
                    [id],
                )
                .unwrap();
            ids.push(id);
        }
        // "two" is claimed by both: it loses the purged source's match and
        // keeps the other's plain claims.
        let two = store.torrent(ids[1]).unwrap().unwrap();
        let c = named("two");
        store
            .put_unverified(&[item(&two.info_hash.0, other, Some(&c))])
            .unwrap();
        store.set_source_state(enr, SourceState::Disabled).unwrap();
        // A backfill batch that read the claims before the purge.
        let read_at = store.class_gen().unwrap();
        let (total, _) = purge_all(&mut store, enr, limit);
        assert_eq!(total.claims_deleted, 3, "limit {limit}");
        let stale = lodestar_store::Enriched {
            version: 7,
            content_type: 0,
            junk_score: 0,
            comp: Vec::new(),
            data: vec![0],
        };
        assert_eq!(
            store
                .set_enrichments_at(read_at, &[(ids[0], stale)], &[])
                .unwrap(),
            None,
            "limit {limit}: the stale batch can't write the match back"
        );
        for &id in &ids[..2] {
            assert_eq!(ver(&store, id), 0, "limit {limit}: {id} marked");
        }
        assert_eq!(ver(&store, ids[2]), 7, "limit {limit}: a plain feed claim");
        assert_eq!(ver(&store, ids[3]), 7, "limit {limit}: another source's");
        assert_eq!(store.torrent_claims(ids[1]).unwrap().len(), 1);
    }
}

/// A consented write (ENR-G) reads the plugin's consent record and stored
/// credential names inside its own transaction, and writes nothing when
/// the condition doesn't hold.
#[test]
fn a_consented_write_judges_consent_inside_its_transaction() {
    let mut store = mem();
    let (dht, enr, _) = sources(&mut store);
    let p = prepared(&store, "one");
    let id = store_torrent(&mut store, &p, dht);
    let c = named("one");
    let held = |_: Option<&str>, names: &[String]| names.iter().any(|n| n == "api_key");
    // No credential stored: refused, nothing written.
    let seen = std::cell::RefCell::new(None);
    let record = |r: Option<&str>, names: &[String]| {
        *seen.borrow_mut() = Some((r.map(str::to_owned), names.to_vec()));
        held(r, names)
    };
    assert_eq!(
        store
            .put_claims_at_consented(id, enr, &c, false, T, "nyaa", &record)
            .unwrap(),
        None
    );
    assert_eq!(seen.take(), Some((None, Vec::new())));
    assert!(claims_of(&store, id).is_empty());
    // Stored: written, and the check saw the name.
    store
        .update_source_secrets(enr, &[("api_key", b"sealed")], &[], None)
        .unwrap();
    assert_eq!(
        store
            .put_claims_at_consented(id, enr, &c, false, T, "nyaa", &record)
            .unwrap(),
        Some(true)
    );
    assert_eq!(seen.take().unwrap().1, ["api_key"]);
    assert_eq!(claims_of(&store, id).len(), 1);
    // A disabled source is still refused first (SRC-1).
    store.set_source_state(enr, SourceState::Disabled).unwrap();
    assert!(matches!(
        store.put_claims_at_consented(id, enr, &c, false, T, "nyaa", &held),
        Err(Error::Invalid(_))
    ));
}

/// The stored content-type code the always-adult tests treat as Adult (the
/// store is told it, as the pipeline tells it `ContentType::Adult`).
const ADULT: u8 = 9;

fn bit(s: SourceId) -> u64 {
    1 << s.get()
}

fn parser_ver(store: &Store, id: i64) -> Option<i64> {
    store
        .connection()
        .query_row(
            "SELECT parser_ver FROM torrent_parsed WHERE torrent_id = ?1",
            [id],
            |r| r.get(0),
        )
        .ok()
}

/// What the backfill leaves once it has derived a torrent: its stored
/// type and an up-to-date record.
fn derived(store: &Store, id: i64, content_type: u8) {
    let c = store.connection();
    c.execute(
        "UPDATE torrents SET content_type = ?2 WHERE id = ?1",
        params![id, content_type],
    )
    .unwrap();
    c.execute(
        "INSERT INTO torrent_parsed (torrent_id, parser_ver, data) VALUES (?1, 7, x'00')
         ON CONFLICT (torrent_id) DO UPDATE SET parser_ver = 7",
        [id],
    )
    .unwrap();
}

/// X-purge-rederive: a kept torrent that only the purged always-adult
/// source made Adult is marked to be derived again, and the generation
/// moves so a batch that read its old sources can't store it Adult. One
/// another always-adult source keeps, one stored as something else, one
/// Adult by detection alone and every torrent of a plain source's purge
/// are left as they are.
#[test]
fn a_purge_rederives_what_only_an_always_adult_source_made_adult() {
    for limit in [1, 100] {
        let mut store = mem();
        let (dht, adult, plain) = sources(&mut store);
        let adult2 = store.register_source("sukebei2", "1", T).unwrap();
        let mut ids = Vec::new();
        // (name, extra finders, stored type, enriched)
        let rows: [(&str, &[SourceId], u8, bool); 7] = [
            ("only", &[adult], ADULT, true),
            ("both", &[adult, adult2], ADULT, true),
            ("not adult", &[adult], 2, true),
            ("detected", &[plain], ADULT, true),
            ("pending", &[adult], ADULT, false),
            ("plain only", &[plain, adult], ADULT, true),
            ("alone", &[], 2, true),
        ];
        for (name, by, ty, enriched) in rows {
            let p = prepared(&store, name);
            let id = store_torrent(&mut store, &p, dht);
            for s in by {
                store
                    .put_unverified(&[item(&p.info_hash.0, *s, None)])
                    .unwrap();
            }
            if enriched {
                derived(&store, id, ty);
            }
            ids.push(id);
        }
        // "plain only" is the plain source's purge, below.
        store.set_adult_mark(AdultMark {
            sources: bit(adult) | bit(adult2),
            content_type: ADULT,
        });
        store
            .set_source_state(adult, SourceState::Disabled)
            .unwrap();
        let read_at = store.class_gen().unwrap();
        let (total, _) = purge_all(&mut store, adult, limit);
        assert!(total.rederive, "limit {limit}");
        assert_eq!(total.torrents_kept, 5, "limit {limit}");
        assert_eq!(parser_ver(&store, ids[0]), Some(0), "limit {limit}: only");
        assert_eq!(parser_ver(&store, ids[1]), Some(7), "limit {limit}: both");
        assert_eq!(
            parser_ver(&store, ids[2]),
            Some(7),
            "limit {limit}: not adult"
        );
        assert_eq!(
            parser_ver(&store, ids[3]),
            Some(7),
            "limit {limit}: detected"
        );
        assert_eq!(parser_ver(&store, ids[4]), None, "limit {limit}: pending");
        assert_eq!(
            parser_ver(&store, ids[5]),
            Some(0),
            "limit {limit}: plain + adult"
        );
        assert_eq!(parser_ver(&store, ids[6]), Some(7), "limit {limit}: alone");
        // A backfill batch that read "pending" with the purged bit (or the
        // writer's adult step) is refused: it reruns under the new sources.
        let stale = lodestar_store::Enriched {
            version: 7,
            content_type: ADULT,
            junk_score: 0,
            comp: Vec::new(),
            data: vec![0],
        };
        assert_eq!(
            store
                .set_enrichments_at(read_at, &[(ids[4], stale)], &[])
                .unwrap(),
            None,
            "limit {limit}"
        );
        assert_eq!(
            store.content_type(ids[0]).unwrap(),
            Some(ADULT),
            "until derived"
        );

        // A plain source's purge derives nothing and keeps the generation.
        store
            .set_source_state(plain, SourceState::Disabled)
            .unwrap();
        derived(&store, ids[5], ADULT);
        let gen_before = store.class_gen().unwrap();
        let (total, _) = purge_all(&mut store, plain, limit);
        assert_eq!(total.torrents_kept, 2, "limit {limit}");
        assert!(!total.rederive, "limit {limit}");
        assert_eq!(store.class_gen().unwrap(), gen_before, "limit {limit}");
        assert_eq!(parser_ver(&store, ids[3]), Some(7), "limit {limit}");
        assert_eq!(parser_ver(&store, ids[5]), Some(7), "limit {limit}");
    }
}

/// Without the always-adult mark (a connection the pipeline never told),
/// an always-adult source's purge marks nothing: the mark is what names
/// it. The pipeline's purge sets it (`Control::purge_step`).
#[test]
fn a_purge_without_the_adult_mark_marks_nothing() {
    let mut store = mem();
    let (dht, adult, _) = sources(&mut store);
    let p = prepared(&store, "only");
    let id = store_torrent(&mut store, &p, dht);
    store
        .put_unverified(&[item(&p.info_hash.0, adult, None)])
        .unwrap();
    derived(&store, id, ADULT);
    store
        .set_source_state(adult, SourceState::Disabled)
        .unwrap();
    let gen_before = store.class_gen().unwrap();
    let (total, _) = purge_all(&mut store, adult, 100);
    assert!(!total.rederive);
    assert_eq!(store.class_gen().unwrap(), gen_before);
    assert_eq!(parser_ver(&store, id), Some(7));
}

/// A kept torrent that loses its last always-adult finder moves the
/// generation even when it has no record to mark: a backfill batch may
/// already have read it with the purged source's bit.
#[test]
fn a_kept_unenriched_torrent_moves_the_generation_without_a_mark() {
    let mut store = mem();
    let (dht, adult, _) = sources(&mut store);
    let p = prepared(&store, "unenriched");
    let id = store_torrent(&mut store, &p, dht);
    store
        .put_unverified(&[item(&p.info_hash.0, adult, None)])
        .unwrap();
    assert_eq!(parser_ver(&store, id), None);
    store.set_adult_mark(AdultMark {
        sources: bit(adult),
        content_type: ADULT,
    });
    store
        .set_source_state(adult, SourceState::Disabled)
        .unwrap();
    let stale = lodestar_store::Enriched {
        version: 7,
        content_type: ADULT,
        junk_score: 0,
        comp: Vec::new(),
        data: vec![0],
    };
    let mut cursor = PurgeCursor::START;
    let mut kept_step = false;
    loop {
        let read_at = store.class_gen().unwrap();
        let s = store.purge_source(adult, cursor, 1).unwrap();
        if s.torrents_kept > 0 {
            assert_eq!(s.torrents_kept, 1);
            kept_step = true;
            assert!(s.rederive);
            assert_ne!(store.class_gen().unwrap(), read_at);
            assert_eq!(parser_ver(&store, id), None, "nothing to mark");
            assert_eq!(
                store
                    .set_enrichments_at(read_at, &[(id, stale.clone())], &[])
                    .unwrap(),
                None,
                "a batch that read the purged bit is refused"
            );
        }
        match s.next {
            Some(c) => cursor = c,
            None => break,
        }
    }
    assert!(kept_step);
    assert_eq!(parser_ver(&store, id), None);
}

/// Your own Is adult correction keeps a torrent Adult whatever is purged:
/// it's marked to be derived again (the backfill applies the correction),
/// never cleared by the purge itself.
#[test]
fn a_purge_never_changes_the_stored_type_itself() {
    let mut store = mem();
    let (dht, adult, _) = sources(&mut store);
    let p = prepared(&store, "yours");
    let id = store_torrent(&mut store, &p, dht);
    store
        .put_unverified(&[item(&p.info_hash.0, adult, None)])
        .unwrap();
    derived(&store, id, ADULT);
    let is_adult = ClassCorrection {
        adult: Some(true),
        not_junk: false,
    };
    assert!(store.set_correction(id, is_adult).unwrap());
    derived(&store, id, ADULT);
    store.set_adult_mark(AdultMark {
        sources: bit(adult),
        content_type: ADULT,
    });
    store
        .set_source_state(adult, SourceState::Disabled)
        .unwrap();
    purge_all(&mut store, adult, 100);
    // Marked for the backfill, which applies the correction; the purge
    // itself changes neither.
    assert_eq!(parser_ver(&store, id), Some(0));
    assert_eq!(store.content_type(id).unwrap(), Some(ADULT));
    assert_eq!(store.correction(id).unwrap(), Some(is_adult));
}

/// A purge's Claims stage moves the generation for a claim that shaped a
/// record, even when the torrent has no record yet; plain feed claims leave it.
#[test]
fn a_claims_purge_moves_the_generation_only_for_claims_that_shape_the_record() {
    let matched = Claims {
        adult: Some(AdultEnrichment {
            scene_id: "1".into(),
            kind: Some(AdultKind::Scene),
            title: None,
            studio: None,
            released: None,
            performers: Vec::new(),
            performers_truncated: false,
        }),
        ..Claims::default()
    };
    let mut store = mem();
    let (dht, feed, enr) = sources(&mut store);
    let a = prepared(&store, "plain");
    let a_id = store_torrent(&mut store, &a, dht);
    let c = named("plain");
    store
        .put_unverified(&[item(&a.info_hash.0, feed, Some(&c))])
        .unwrap();
    derived(&store, a_id, 2);
    // A plain feed's claims shape nothing: the generation stays.
    store.set_source_state(feed, SourceState::Disabled).unwrap();
    let gen_before = store.class_gen().unwrap();
    let (total, _) = purge_all(&mut store, feed, 100);
    assert_eq!(total.claims_deleted, 1);
    assert!(!total.rederive);
    assert_eq!(store.class_gen().unwrap(), gen_before);
    // A match on a torrent with no record yet has no row to mark, but a
    // backfill batch may already have read it: the generation moves, so
    // that batch can't write the purged match into a first record.
    let b = prepared(&store, "unenriched");
    let b_id = store_torrent(&mut store, &b, dht);
    store
        .put_unverified(&[item(&b.info_hash.0, enr, Some(&matched))])
        .unwrap();
    assert_eq!(parser_ver(&store, b_id), None);
    let read_at = store.class_gen().unwrap();
    store.set_source_state(enr, SourceState::Disabled).unwrap();
    let (total, _) = purge_all(&mut store, enr, 100);
    assert_eq!(total.claims_deleted, 1);
    assert!(total.rederive);
    assert_ne!(store.class_gen().unwrap(), read_at);
    let stale = lodestar_store::Enriched {
        version: 7,
        content_type: ADULT,
        junk_score: 0,
        comp: Vec::new(),
        data: vec![0],
    };
    assert_eq!(
        store
            .set_enrichments_at(read_at, &[(b_id, stale)], &[])
            .unwrap(),
        None,
        "a batch that read the purged match is refused"
    );
    assert_eq!(parser_ver(&store, b_id), None);
}

/// A terms record that lets a source's answers be passed on
/// (`lodestar_sources::terms::Terms::record`: version 1, the redistribute
/// flag, a retention in seconds).
fn redistributing() -> Vec<u8> {
    let mut v = vec![1, 0b10];
    v.extend((180u64 * 86_400).to_be_bytes());
    v
}

/// [`redistributing`] with the redistribute flag off: the record a terms
/// change writes.
fn closed() -> Vec<u8> {
    let mut v = redistributing();
    v[1] = 0;
    v
}

/// Every claims shape the two predicates read: no section, an adult
/// section with and without a kind, and a details section with each of
/// a canonical title, a match confidence and an alternative title on or
/// off, each with and without an external id. Shapes `Claims::check`
/// refuses are left out by the caller.
fn every_shape() -> Vec<Claims> {
    let mut out = Vec::new();
    let adult = |kind| AdultEnrichment {
        scene_id: "1".into(),
        kind,
        title: None,
        studio: None,
        released: None,
        performers: Vec::new(),
        performers_truncated: false,
    };
    for ids in [false, true] {
        let base = Claims {
            ids: if ids {
                vec![ExternalId {
                    scheme: "imdb".into(),
                    value: "tt0133093".into(),
                }]
            } else {
                Vec::new()
            },
            ..Claims::default()
        };
        out.push(base.clone());
        for kind in [None, Some(AdultKind::Scene)] {
            out.push(Claims {
                adult: Some(adult(kind)),
                ..base.clone()
            });
        }
        for bits in 0..8u8 {
            out.push(Claims {
                details: Some(Box::new(Details {
                    canonical_title: (bits & 1 != 0).then(|| "The Matrix".into()),
                    match_confidence: (bits & 2 != 0).then_some(90),
                    alt_titles: if bits & 4 != 0 {
                        vec![AltTitle {
                            language: "ja".into(),
                            title: "Matrix ja".into(),
                        }]
                    } else {
                        Vec::new()
                    },
                    ..Details::default()
                })),
                ..base.clone()
            });
        }
    }
    out
}

fn logged(store: &Store, id: i64) -> i64 {
    store
        .connection()
        .query_row(
            "SELECT count(*) FROM reindex_log WHERE torrent_id = ?1",
            [id],
            |r| r.get(0),
        )
        .unwrap()
}

/// X-purge-index-shapes: the served identification rule and the claims
/// shape rule agree on every shape. Where everything else allows serving,
/// a row is served exactly when it holds an identification answer, every
/// served row shapes the record, and each path that deletes claims (the
/// source purge, a refresh's drop and a refile) logs the torrent for the
/// index exactly when the deleted row shaped it. The purge and the refile
/// mark it for the backfill on the same test.
#[test]
fn served_identifications_and_record_shapes_agree_on_every_shape() {
    let mut checked = 0;
    for c in every_shape() {
        if c.check().is_err() {
            continue;
        }
        checked += 1;
        let mut store = mem();
        let (dht, enr, _) = sources(&mut store);
        store
            .set_plugin_state(enr, TERMS_KEY, Some(&redistributing()))
            .unwrap();
        let ids: Vec<i64> = ["purged", "dropped", "refiled"]
            .iter()
            .map(|name| {
                let p = prepared(&store, name);
                let id = store_torrent(&mut store, &p, dht);
                assert!(store.put_claims(id, enr, &c, false).unwrap());
                // What the enrichment backfill writes once it has derived it.
                store
                    .connection()
                    .execute(
                        "INSERT INTO torrent_parsed (torrent_id, parser_ver, data)
                         VALUES (?1, 7, x'00')",
                        [id],
                    )
                    .unwrap();
                id
            })
            .collect();
        let before: Vec<i64> = ids.iter().map(|&id| logged(&store, id)).collect();
        let served = !store.served_identifications(ids[0]).unwrap().is_empty();
        assert_eq!(served, c.identification().is_some(), "{c:?}");
        assert!(
            !served || c.shapes_the_record(),
            "served but no shape: {c:?}"
        );
        let want = i64::from(c.shapes_the_record());

        assert!(store.drop_claims(ids[1], enr).unwrap());
        let rc = [RefiledClaims {
            torrent_id: ids[2],
            source: enr,
            filtered: None,
        }];
        let gen_before = store.class_gen().unwrap();
        let refiled = store.refile_claims(&rc, || true).unwrap().unwrap();
        assert_eq!(refiled.claims, 1);
        assert_eq!(refiled.rederive, c.shapes_the_record(), "{c:?}");
        // The refile, as the purge, re-derives what the claim shaped and
        // refuses a batch that read it.
        assert_eq!(
            store.class_gen().unwrap() != gen_before,
            c.shapes_the_record(),
            "{c:?}"
        );
        store.set_source_state(enr, SourceState::Disabled).unwrap();
        purge_all(&mut store, enr, 100);
        for (&id, b) in ids.iter().zip(before) {
            assert_eq!(logged(&store, id) - b, want, "torrent {id}: {c:?}");
            assert!(store.served_identifications(id).unwrap().is_empty());
        }
        let ver = |id: i64| -> i64 {
            store
                .connection()
                .query_row(
                    "SELECT parser_ver FROM torrent_parsed WHERE torrent_id = ?1",
                    [id],
                    |r| r.get(0),
                )
                .unwrap()
        };
        // A drop marks whatever it removed; the purge and the refile only
        // what shaped the record.
        assert_eq!(ver(ids[1]), 0);
        for id in [ids[0], ids[2]] {
            assert_eq!(ver(id) == 0, c.shapes_the_record(), "torrent {id}: {c:?}");
        }
    }
    // 22 built; refused: an empty details section (with and without an
    // id) and an adult section beside an id (with and without a kind).
    assert_eq!(checked, 18);
}

/// X-purge-index-shapes: a purge step's claim deletes and the log entries
/// for the index commit together or not at all, so a crash between the
/// purge and the next index pass still leaves the torrent logged.
#[test]
fn a_purge_deletes_claims_and_logs_them_together_or_not_at_all() {
    let mut store = mem();
    let (dht, enr, _) = sources(&mut store);
    store
        .set_plugin_state(enr, TERMS_KEY, Some(&redistributing()))
        .unwrap();
    let p = prepared(&store, "one");
    let id = store_torrent(&mut store, &p, dht);
    let answer = Claims {
        details: Some(Box::new(Details {
            alt_titles: vec![AltTitle {
                language: "ja".into(),
                title: "One ja".into(),
            }],
            match_confidence: Some(80),
            ..Details::default()
        })),
        ..Claims::default()
    };
    assert!(store.put_claims(id, enr, &answer, false).unwrap());
    assert_eq!(store.served_identifications(id).unwrap().len(), 1);
    store.set_source_state(enr, SourceState::Disabled).unwrap();
    let before = logged(&store, id);
    let claims_stage = {
        // The unverified stage has nothing to do: its cursor is the next.
        let s = store.purge_source(enr, PurgeCursor::START, 100).unwrap();
        s.next.unwrap()
    };
    store
        .connection()
        .execute_batch(
            "CREATE TRIGGER fail_purge_reindex BEFORE INSERT ON reindex_log BEGIN
             SELECT RAISE(ABORT, 'injected reindex failure'); END;",
        )
        .unwrap();
    assert!(store.purge_source(enr, claims_stage, 100).is_err());
    assert_eq!(claims_of(&store, id).len(), 1, "the delete rolled back");
    assert_eq!(logged(&store, id), before);
    store
        .connection()
        .execute_batch("DROP TRIGGER fail_purge_reindex")
        .unwrap();
    let s = store.purge_source(enr, claims_stage, 100).unwrap();
    assert_eq!(s.claims_deleted, 1);
    assert!(claims_of(&store, id).is_empty());
    assert_eq!(logged(&store, id), before + 1);
    assert!(store.served_identifications(id).unwrap().is_empty());
}

/// An identification answer with a canonical title: the shape whose title
/// the backfill writes into the record (ENR-10).
fn titled_answer() -> Claims {
    Claims {
        details: Some(Box::new(Details {
            canonical_title: Some("The Matrix".into()),
            match_confidence: Some(90),
            ..Details::default()
        })),
        ..Claims::default()
    }
}

/// What the enrichment backfill writes once it has derived a torrent.
fn derived_at_7(store: &Store, id: i64) {
    store
        .connection()
        .execute(
            "INSERT INTO torrent_parsed (torrent_id, parser_ver, data) VALUES (?1, 7, x'00')
             ON CONFLICT (torrent_id) DO UPDATE SET parser_ver = 7",
            [id],
        )
        .unwrap();
}

fn ver_of(store: &Store, id: i64) -> i64 {
    store
        .connection()
        .query_row(
            "SELECT parser_ver FROM torrent_parsed WHERE torrent_id = ?1",
            [id],
            |r| r.get(0),
        )
        .unwrap()
}

/// The three paths that take a source's answers out of a record without
/// a purge: a refresh's drop, the retention sweep and a terms change.
#[derive(Debug, Clone, Copy)]
enum Unshape {
    Drop,
    Expire,
    Terms,
}

impl Unshape {
    fn run(self, store: &mut Store, id: i64, source: SourceId) {
        match self {
            Unshape::Drop => assert!(store.drop_claims(id, source).unwrap()),
            Unshape::Expire => {
                let gone = store
                    .drop_claims_outside(source, i64::MAX, i64::MAX, 100)
                    .unwrap();
                assert_eq!(gone, [id]);
            }
            // Every torrent holding the source's claims: counted in
            // `a_terms_change_logs_and_marks_together_or_not_at_all`.
            Unshape::Terms => assert!(store.mark_claims_unenriched(source, &closed()).unwrap() > 0),
        }
    }
}

/// X-claims-refile-rearm: a backfill batch on another connection that read
/// an answer before a drop, an expiry or a terms change is refused by the
/// classification generation, so it can't write the answer's title back
/// over the mark. The two connections interleave on channels, each wait
/// bounded at 30 s. A plain feed claim leaves the generation.
#[test]
fn a_backfill_batch_that_read_an_answer_is_refused_after_it_leaves() {
    use std::sync::mpsc;
    use std::time::Duration;
    const WAIT: Duration = Duration::from_secs(30);
    for how in [Unshape::Drop, Unshape::Expire, Unshape::Terms] {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("s.db");
        let mut store = Store::open(&db, StoreConfig::default()).unwrap();
        let (dht, enr, feed) = sources(&mut store);
        store
            .set_plugin_state(enr, TERMS_KEY, Some(&redistributing()))
            .unwrap();
        let p = prepared(&store, "matrix");
        let id = store_torrent(&mut store, &p, dht);
        assert!(store.put_claims(id, enr, &titled_answer(), false).unwrap());
        derived_at_7(&store, id);

        let (read_tx, read_rx) = mpsc::channel();
        let (go_tx, go_rx) = mpsc::channel::<()>();
        let batch = std::thread::spawn(move || {
            // The backfill's own connection: the generation first, then
            // the claims, as `enrich_stored_marking` reads them.
            let mut other = Store::open(&db, StoreConfig::default()).unwrap();
            let read_at = other.class_gen().unwrap();
            let seen = other.torrent_claims(id).unwrap();
            read_tx.send(seen.len()).unwrap();
            go_rx.recv_timeout(WAIT).expect("the change never came");
            let stale = lodestar_store::Enriched {
                version: 7,
                content_type: 0,
                junk_score: 0,
                comp: Vec::new(),
                data: vec![0],
            };
            other
                .set_enrichments_at(read_at, &[(id, stale)], &[])
                .unwrap()
        });
        assert_eq!(read_rx.recv_timeout(WAIT).expect("the batch never read"), 1);
        how.run(&mut store, id, enr);
        go_tx.send(()).unwrap();
        assert_eq!(batch.join().unwrap(), None, "{how:?}: the batch is refused");
        assert_eq!(ver_of(&store, id), 0, "{how:?}: the mark stays");

        // Plain feed claims shape nothing: the generation stays. Another
        // source, so they're the only claims the path reaches.
        let q = prepared(&store, "plain");
        let plain = store_torrent(&mut store, &q, dht);
        assert!(
            store
                .put_claims(plain, feed, &named("plain"), false)
                .unwrap()
        );
        derived_at_7(&store, plain);
        let before = store.class_gen().unwrap();
        how.run(&mut store, plain, feed);
        assert_eq!(store.class_gen().unwrap(), before, "{how:?}");
    }
}

/// X-claims-refile-rearm: a terms change logs for the index each torrent
/// whose claims could have supplied a served answer, in the transaction
/// that marks them and writes the new record; a failed log rolls the
/// marks, the record and the generation back.
#[test]
fn a_terms_change_logs_and_marks_together_or_not_at_all() {
    let mut store = mem();
    let (dht, enr, other) = sources(&mut store);
    let ids: Vec<i64> = ["answer", "plain", "other"]
        .iter()
        .map(|name| {
            let p = prepared(&store, name);
            let id = store_torrent(&mut store, &p, dht);
            derived_at_7(&store, id);
            id
        })
        .collect();
    assert!(
        store
            .put_claims(ids[0], enr, &titled_answer(), false)
            .unwrap()
    );
    assert!(
        store
            .put_claims(ids[1], enr, &named("plain"), false)
            .unwrap()
    );
    assert!(
        store
            .put_claims(ids[2], other, &titled_answer(), false)
            .unwrap()
    );
    store
        .set_plugin_state(enr, TERMS_KEY, Some(&redistributing()))
        .unwrap();
    let before: Vec<i64> = ids.iter().map(|&id| logged(&store, id)).collect();
    let gen_before = store.class_gen().unwrap();
    let terms = |store: &Store| store.plugin_state(enr, TERMS_KEY).unwrap();

    store
        .connection()
        .execute_batch(
            "CREATE TRIGGER fail_terms_reindex BEFORE INSERT ON reindex_log BEGIN
             SELECT RAISE(ABORT, 'injected reindex failure'); END;",
        )
        .unwrap();
    assert!(store.mark_claims_unenriched(enr, &closed()).is_err());
    for &id in &ids {
        assert_eq!(ver_of(&store, id), 7, "torrent {id}: the mark rolled back");
    }
    assert_eq!(store.class_gen().unwrap(), gen_before);
    assert_eq!(terms(&store), Some(redistributing()), "the record too");
    store
        .connection()
        .execute_batch("DROP TRIGGER fail_terms_reindex")
        .unwrap();

    assert_eq!(store.mark_claims_unenriched(enr, &closed()).unwrap(), 2);
    let after: Vec<i64> = ids.iter().map(|&id| logged(&store, id)).collect();
    assert_eq!(after[0] - before[0], 1, "the answer's torrent is logged");
    assert_eq!(after[1], before[1], "a plain claim serves nothing");
    assert_eq!(after[2], before[2], "another source's");
    assert_eq!(
        ids.iter().map(|&id| ver_of(&store, id)).collect::<Vec<_>>(),
        [0, 0, 7]
    );
    assert_ne!(store.class_gen().unwrap(), gen_before);
    assert_eq!(terms(&store), Some(closed()), "the record with them");
}
