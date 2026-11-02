//! The unverified pipeline (SRC-2, `docs/decisions/2026-11-02-SRC-2.md`):
//! claims pass the full filter before storage, a floor or core hit always
//! holds the hash, stored claims are judged again when the rules change,
//! and the verification cursor reads the store a page at a time.

use std::sync::atomic::Ordering;

use lodestar_bencode::{OwnedValue, to_vec};
use lodestar_filter::{
    Action, Core, CsList, Engine, Handle, Lists, Origin, Sealer, Switches, TermSet,
};
use lodestar_metainfo::{Info, InfoHash};
use lodestar_pipeline::unverified::{
    self, Ctx, Phase, REFILE_PAGE, UnverifiedStats, VERIFY_ROUND_SECS, Verify,
};
use lodestar_sources::{ClaimedFile, Claims, Discovery, Found};
use lodestar_store::unverified::{Network, NewUnverified};
use lodestar_store::{PreparedTorrent, SourceId, SourceState, Store, StoreConfig, WriteBatch};

const T: i64 = 1_700_000_000;

/// The core with stand-in term lists: `lexa` is sexual, `rolea` a child
/// role, so "rolea lexa" is a CS-4a pattern hit.
fn stand_in() -> Handle {
    let lists = Lists {
        sexual: TermSet::of(&["lexa"]),
        child_roles: TermSet::of(&["rolea"]),
        codes: TermSet::of(&["codex"]),
    };
    Handle::new(Engine::new(Core::new(
        lists,
        CsList::default(),
        Switches::default(),
    )))
}

fn rule(filter: &Handle, query: &str, action: Action) {
    filter.update_engine(|e| {
        let notes = e.load_rules(&lodestar_store::RuleSet {
            rules: vec![lodestar_store::RuleRow {
                id: 1,
                query: query.into(),
                action: action.code(),
                enabled: true,
                shadow_until: None,
                unless_query: None,
            }],
            ..Default::default()
        });
        assert!(notes.is_empty(), "{notes:?}");
        e.version += 1;
    });
}

fn mem() -> (Store, SourceId, SourceId) {
    let mut store = Store::open_in_memory(StoreConfig::default()).unwrap();
    let dht = store.register_source("dht", "1", T).unwrap();
    let nyaa = store.register_source("nyaa", "1", T).unwrap();
    (store, dht, nyaa)
}

fn key(n: u8) -> [u8; 20] {
    [n; 20]
}

fn named(name: &str, files: &[&str]) -> Claims {
    Claims {
        name: Some(name.into()),
        files: files
            .iter()
            .map(|p| ClaimedFile {
                path: (*p).into(),
                size: Some(1 << 20),
            })
            .collect(),
        ..Claims::default()
    }
}

fn found(k: [u8; 20], source: SourceId, claims: Claims) -> Discovery {
    Discovery {
        source,
        found: Found::key(InfoHash(k), 0).with_claims(claims),
        at: T,
        reply: false,
    }
}

fn ingest(
    store: &mut Store,
    filter: &Handle,
    sealer: Option<&Sealer>,
    batch: &[Discovery],
) -> (UnverifiedStats, unverified::Ingested) {
    let stats = UnverifiedStats::default();
    let ctx = Ctx {
        sealer,
        quarantine_secs: 86_400,
    };
    let done =
        unverified::ingest(store, &filter.current(), ctx, batch, &stats, T, &|_, _| {}).unwrap();
    (stats, done)
}

fn held(store: &Store, k: [u8; 20]) -> bool {
    store.held_among(&[k]).unwrap()[0]
}

fn n(c: &std::sync::atomic::AtomicU64) -> u64 {
    c.load(Ordering::Relaxed)
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

/// Stores a crawled torrent; its id and swarm hash.
fn crawled(store: &mut Store, name: &str, source: SourceId) -> (i64, [u8; 20]) {
    let p = store
        .preparer()
        .prepare(&Info::parse(&info_bytes(name)).unwrap(), None)
        .unwrap();
    let id = store
        .write(&WriteBatch {
            prepared: &[PreparedTorrent {
                torrent: &p,
                discovered_at: T,
                metadata_at: T,
                extra_flags: 0,
                source,
                verified: true,
            }],
            ..WriteBatch::default()
        })
        .unwrap()
        .prepared[0]
        .id();
    (id, p.info_hash.0)
}

/// Runs the refile sweep to its end under the current rules; the number
/// of steps it took and what it removed.
fn sweep(store: &mut Store, filter: &Handle, sealer: Option<&Sealer>) -> (usize, usize) {
    let version = filter.current().version;
    let ctx = Ctx {
        sealer,
        quarantine_secs: 86_400,
    };
    let (mut phase, mut after_item, mut after_claims) = (Phase::Items, 0, (0, 0));
    let (mut steps, mut removed) = (0, 0);
    loop {
        let s = unverified::refile_step(
            store,
            filter,
            version,
            phase,
            after_item,
            after_claims,
            ctx,
            T,
            &|_, _| {},
        )
        .unwrap();
        assert!(!s.rejected);
        steps += 1;
        removed += s.removed;
        after_item = s.after_item;
        after_claims = s.after_claims;
        if s.end {
            match phase {
                Phase::Items => phase = Phase::Claims,
                Phase::Claims => return (steps, removed),
            }
        }
    }
}

#[test]
fn clean_claims_are_stored_as_unverified_items() {
    let (mut store, _, nyaa) = mem();
    let filter = stand_in();
    let (stats, _) = ingest(
        &mut store,
        &filter,
        None,
        &[found(
            key(1),
            nyaa,
            named("Plain Film 2020", &["Plain Film.mkv"]),
        )],
    );
    assert_eq!(n(&stats.stored), 1);
    let item = store.unverified(Network::Bt, &key(1)).unwrap().unwrap();
    assert_eq!(
        item.claims.unwrap().name.as_deref(),
        Some("Plain Film 2020")
    );
    assert!(!held(&store, key(1)));
}

/// The safety injection test (plan SRC-2 gate): a floor hash, a core
/// pattern in the claimed name, and one only in a claimed file path. Each
/// is held (tombstone or quarantine), never stored, and counted.
#[test]
fn floor_and_core_claim_hits_hold_and_are_never_stored() {
    let (mut store, _, nyaa) = mem();
    let filter = stand_in();
    filter.update(|c| c.cs.add_hash(key(1), Origin::Reported));
    let sealer = Sealer::new(&[9; 32]);
    let batch = [
        // CS-1a: the key itself.
        found(key(1), nyaa, named("Plain Film", &[])),
        // CS-4a in the claimed name.
        found(key(2), nyaa, named("Rolea Lexa clip", &[])),
        // CS-4 in a claimed file path only, under an innocent name.
        found(key(3), nyaa, named("Holiday", &["rolea lexa/clip.mkv"])),
    ];
    let (stats, _) = ingest(&mut store, &filter, Some(&sealer), &batch);
    assert_eq!(n(&stats.held), 3);
    assert_eq!(n(&stats.stored), 0);
    for k in [1, 2, 3] {
        assert!(held(&store, key(k)), "{k} is held");
        assert!(store.unverified(Network::Bt, &key(k)).unwrap().is_none());
    }
    assert!(
        store
            .tombstones(10)
            .unwrap()
            .iter()
            .any(|t| t.hash == key(1))
    );
    let q = store.quarantined(10).unwrap();
    let q2 = q.iter().find(|q| q.hash == key(2)).expect("quarantined");
    assert!(q2.rule.starts_with("CS-"), "{}", q2.rule);
    // The name is kept only sealed.
    let sealed = q2.sealed.as_deref().unwrap();
    assert!(!sealed.windows(5).any(|w| w.eq_ignore_ascii_case(b"rolea")));
    assert_eq!(sealer.open(sealed).unwrap(), "Rolea Lexa clip");
    // A second sighting finds the hold, and the store's guard refuses it
    // even past the filter.
    let (again, _) = ingest(
        &mut store,
        &stand_in(),
        None,
        &[found(key(2), nyaa, named("Plain", &[]))],
    );
    assert_eq!(n(&again.refused), 1);
    assert!(store.unverified(Network::Bt, &key(2)).unwrap().is_none());
}

#[test]
fn a_core_hit_on_a_stored_torrents_key_holds_and_deletes_it() {
    let (mut store, dht, nyaa) = mem();
    let (id, h) = crawled(&mut store, "Holiday Film", dht);
    let filter = stand_in();
    let (stats, done) = ingest(
        &mut store,
        &filter,
        None,
        &[found(h, nyaa, named("Rolea Lexa clip", &[]))],
    );
    assert_eq!(n(&stats.held), 1);
    assert_eq!(done.deleted, vec![id]);
    assert!(store.torrent(id).unwrap().is_none());
    assert!(held(&store, h));
    assert!(store.torrent_claims(id).unwrap().is_empty());
}

/// Your rule (or a pack, or a list) on claim text drops that source's
/// claims and holds nothing, in either order (runner, #383): a feed can't
/// get an unrelated torrent refused by claiming its hash under a name your
/// rule drops. The torrent is judged on its own verified metadata.
#[test]
fn your_drop_rule_drops_the_claims_and_holds_nothing_in_either_order() {
    // The torrent first: the claim is dropped, the torrent stays.
    let (mut store, dht, nyaa) = mem();
    let (id, h) = crawled(&mut store, "Holiday Film", dht);
    let filter = stand_in();
    rule(&filter, "badword", Action::Drop);
    let (stats, _) = ingest(
        &mut store,
        &filter,
        None,
        &[found(h, nyaa, named("Badword Film", &[]))],
    );
    assert_eq!((n(&stats.held), n(&stats.dropped)), (0, 1));
    assert!(!held(&store, h));
    assert!(store.torrent(id).unwrap().is_some());
    assert!(store.torrent_claims(id).unwrap().is_empty());

    // The claim first: nothing is stored or held, so the crawl later
    // stores the torrent as usual.
    let (mut store, dht, nyaa) = mem();
    let p = store
        .preparer()
        .prepare(&Info::parse(&info_bytes("Holiday Film")).unwrap(), None)
        .unwrap();
    let h = p.info_hash.0;
    let (stats, _) = ingest(
        &mut store,
        &filter,
        None,
        &[found(h, nyaa, named("Badword Film", &[]))],
    );
    assert_eq!((n(&stats.held), n(&stats.dropped)), (0, 1));
    assert!(!held(&store, h));
    assert!(store.unverified(Network::Bt, &h).unwrap().is_none());
    let (id, again) = crawled(&mut store, "Holiday Film", dht);
    assert_eq!(again, h);
    assert!(store.torrent(id).unwrap().is_some());

    // The source's own earlier claims go with it; another source's stay.
    let (mut store, _, nyaa) = mem();
    let feed2 = store.register_source("feed2", "1", T).unwrap();
    ingest(
        &mut store,
        &filter,
        None,
        &[
            found(key(8), nyaa, named("Plain Film", &[])),
            found(key(9), feed2, named("Plain Film", &[])),
        ],
    );
    ingest(
        &mut store,
        &filter,
        None,
        &[
            found(key(8), nyaa, named("Badword Film", &[])),
            found(key(9), nyaa, named("Badword Film", &[])),
        ],
    );
    assert!(store.unverified(Network::Bt, &key(8)).unwrap().is_none());
    assert!(!held(&store, key(8)));
    let kept = store.unverified(Network::Bt, &key(9)).unwrap().unwrap();
    assert_eq!(kept.source, feed2, "feed2 owns 9's claims: kept");
}

/// Every stored claim text passes the filter (runner, #383): the
/// description and the publisher are judged as names are, and a core hit
/// there holds. Tracker URLs are validated as URLs, not judged as text.
#[test]
fn the_description_and_publisher_are_judged_and_trackers_validated() {
    let (mut store, _, nyaa) = mem();
    let filter = stand_in();
    rule(&filter, "badword", Action::Drop);
    let sealer = Sealer::new(&[9; 32]);
    let described = Claims {
        description: Some("A clip. Rolea Lexa.".into()),
        ..named("Holiday", &[])
    };
    let published = Claims {
        publisher: Some("badword-uploads".into()),
        ..named("Holiday", &[])
    };
    let trackers = Claims {
        trackers: vec![
            "udp://tracker.example:1337/announce".into(),
            "https://t.example/a?passkey=x".into(),
            "javascript:alert(1)".into(),
            "http:///nohost".into(),
            "http://bad host/announce".into(),
            "file:///etc/passwd".into(),
        ],
        ..named("Holiday", &[])
    };
    let (stats, _) = ingest(
        &mut store,
        &filter,
        Some(&sealer),
        &[
            found(key(1), nyaa, described),
            found(key(2), nyaa, published),
            found(key(3), nyaa, trackers),
        ],
    );
    assert_eq!(n(&stats.held), 1, "the description's core hit holds");
    assert!(held(&store, key(1)));
    let q = store.quarantined(10).unwrap();
    assert!(
        q.iter()
            .any(|q| q.hash == key(1) && q.rule.starts_with("CS-"))
    );
    assert_eq!(n(&stats.dropped), 1, "your rule on the publisher drops");
    assert!(!held(&store, key(2)));
    assert!(store.unverified(Network::Bt, &key(2)).unwrap().is_none());
    let item = store.unverified(Network::Bt, &key(3)).unwrap().unwrap();
    assert_eq!(
        item.claims.unwrap().trackers,
        vec![
            "udp://tracker.example:1337/announce".to_string(),
            "https://t.example/a?passkey=x".to_string()
        ]
    );
}

#[test]
fn weaker_actions_store_the_claims() {
    let (mut store, dht, nyaa) = mem();
    let (id, h) = crawled(&mut store, "Holiday Film", dht);
    let filter = stand_in();
    rule(&filter, "badword", Action::Flag);
    let (stats, done) = ingest(
        &mut store,
        &filter,
        None,
        &[
            found(key(7), nyaa, named("Badword Film", &[])),
            found(h, nyaa, named("Badword Film", &[])),
        ],
    );
    assert_eq!(n(&stats.stored), 1);
    assert_eq!(n(&stats.on_torrent), 1);
    assert_eq!(done.touched, vec![(id, T, nyaa, 0)]);
    assert_eq!(store.torrent_claims(id).unwrap().len(), 1);
}

#[test]
fn a_disabled_source_is_refused_alone() {
    let (mut store, dht, nyaa) = mem();
    store.set_source_state(dht, SourceState::Disabled).unwrap();
    let (stats, _) = ingest(
        &mut store,
        &stand_in(),
        None,
        &[
            found(key(1), dht, named("One", &[])),
            found(key(2), nyaa, named("Two", &[])),
        ],
    );
    assert_eq!(n(&stats.refused), 1);
    assert_eq!(n(&stats.stored), 1);
    assert!(store.unverified(Network::Bt, &key(1)).unwrap().is_none());
    assert!(store.unverified(Network::Bt, &key(2)).unwrap().is_some());
}

/// The refile gate (plan SRC-2): a rule added after storage removes a
/// stored unverified name and a stored claim, and holds what it must.
#[test]
fn a_rule_added_after_storage_removes_stored_items_and_claims() {
    let (mut store, dht, nyaa) = mem();
    let (kept_id, kept_h) = crawled(&mut store, "Kept Film", dht);
    let (gone_id, gone_h) = crawled(&mut store, "Other Film", dht);
    let filter = stand_in();
    ingest(
        &mut store,
        &filter,
        None,
        &[
            found(key(1), nyaa, named("Badword Film", &[])),
            found(key(2), nyaa, named("Rolea Lexa", &[])),
            found(key(3), nyaa, named("Plain Film", &[])),
            // Claims on stored torrents: one your rule matches, one the
            // core does once its term exists.
            found(kept_h, nyaa, named("Badword Film", &[])),
            found(gone_h, nyaa, named("Codez clip", &[])),
        ],
    );
    // Stored before the core knew its pattern: "Rolea Lexa" was held at
    // ingest, so only 1 and 3 are items.
    assert!(store.unverified(Network::Bt, &key(2)).unwrap().is_none());
    assert_eq!(store.torrent_claims(kept_id).unwrap().len(), 1);
    assert_eq!(store.torrent_claims(gone_id).unwrap().len(), 1);

    // Nothing matches yet: nothing changes.
    assert_eq!(sweep(&mut store, &filter, None).1, 0);

    rule(&filter, "badword", Action::Drop);
    filter.update(|c| {
        let mut lists = c.lists().clone();
        lists.codes = TermSet::of(&["codez"]);
        c.set_lists(lists);
    });
    let (_, removed) = sweep(&mut store, &filter, None);
    assert_eq!(removed, 3, "an item and two claims");
    // The item: removed. Your rule holds nothing (runner, #383).
    assert!(store.unverified(Network::Bt, &key(1)).unwrap().is_none());
    assert!(!held(&store, key(1)));
    assert!(store.unverified(Network::Bt, &key(3)).unwrap().is_some());
    // Your rule on a stored torrent's claims: only the claims go.
    assert!(store.torrent(kept_id).unwrap().is_some());
    assert!(store.torrent_claims(kept_id).unwrap().is_empty());
    assert!(!held(&store, kept_h));
    // The core on a stored torrent's claims: held and deleted.
    assert!(store.torrent(gone_id).unwrap().is_none());
    assert!(held(&store, gone_h));
}

#[test]
fn a_sweep_under_stale_rules_writes_nothing() {
    let (mut store, _, nyaa) = mem();
    let filter = stand_in();
    ingest(
        &mut store,
        &filter,
        None,
        &[found(key(1), nyaa, named("Badword Film", &[]))],
    );
    let stale = filter.current().version;
    rule(&filter, "badword", Action::Drop);
    let ctx = Ctx {
        sealer: None,
        quarantine_secs: 86_400,
    };
    let s = unverified::refile_step(
        &mut store,
        &filter,
        stale,
        Phase::Items,
        0,
        (0, 0),
        ctx,
        T,
        &|_, _| {},
    )
    .unwrap();
    assert!(s.rejected);
    assert!(store.unverified(Network::Bt, &key(1)).unwrap().is_some());
}

#[test]
fn the_sweep_reads_one_bounded_page_per_step() {
    let (mut store, _, nyaa) = mem();
    let claims = lodestar_store::claims::Claims {
        name: Some("Plain".into()),
        ..Default::default()
    };
    let keys: Vec<[u8; 20]> = (0..(2 * REFILE_PAGE + 5) as u32)
        .map(|i| {
            let mut k = [0u8; 20];
            k[..4].copy_from_slice(&i.to_be_bytes());
            k
        })
        .collect();
    let items: Vec<NewUnverified<'_>> = keys
        .iter()
        .map(|k| NewUnverified {
            network: Network::Bt,
            key: k,
            source: nyaa,
            private_source: false,
            claims: Some(&claims),
            at: T,
        })
        .collect();
    store.put_unverified(&items).unwrap();
    // Three item pages, an empty one, then an empty claims page.
    assert_eq!(sweep(&mut store, &stand_in(), None).0, 5);
}

#[test]
fn verification_reads_the_store_a_page_at_a_time() {
    let (mut store, _, nyaa) = mem();
    let claims = lodestar_store::claims::Claims {
        name: Some("Plain".into()),
        ..Default::default()
    };
    let mut items = Vec::new();
    let keys = [key(1), key(2), key(3)];
    for (i, k) in keys.iter().enumerate() {
        items.push(NewUnverified {
            network: Network::Bt,
            key: k,
            source: nyaa,
            private_source: i == 1,
            claims: Some(&claims),
            at: T,
        });
    }
    store.put_unverified(&items).unwrap();

    // Off (the default): nothing.
    let mut v = Verify::default();
    assert!(v.next(&store, 2, T).unwrap().is_empty());

    v.on = true;
    let first = v.next(&store, 2, T).unwrap();
    assert_eq!(first.len(), 2);
    assert_eq!(first[0].found.key.0, key(1));
    assert_eq!(first[0].source, nyaa);
    assert!(first[0].found.claims.is_none(), "a bare fetch request");
    // A private item stays private: never sent to public peer finding.
    assert!(first[1].found.private_source);
    let second = v.next(&store, 2, T).unwrap();
    assert_eq!(second.len(), 1);
    // The round is over: the next starts later, from the first item.
    assert!(v.next(&store, 2, T).unwrap().is_empty());
    assert!(v.next(&store, 2, T + 60).unwrap().is_empty());
    let again = v.next(&store, 2, T + VERIFY_ROUND_SECS).unwrap();
    assert_eq!(again[0].found.key.0, key(1));

    // A disabled source's items aren't verified.
    store.set_source_state(nyaa, SourceState::Disabled).unwrap();
    let mut v = Verify {
        on: true,
        ..Verify::default()
    };
    assert!(v.next(&store, 10, T).unwrap().is_empty());
}

/// Resident memory, from `/proc/self/statm` (Linux only).
fn rss_bytes() -> u64 {
    let s = std::fs::read_to_string("/proc/self/statm").unwrap_or_default();
    let pages: u64 = s
        .split_whitespace()
        .nth(1)
        .and_then(|p| p.parse().ok())
        .unwrap_or(0);
    pages * 4096
}

/// The plan's RSS measurement (§3, "Hot columns"): unverified ids take
/// slots from `id_seq`, so the hot arrays, sized by the highest id, span
/// them too. Three layouts of 1M crawled torrents and 1M unverified ids.
/// Run: `cargo test --release -p lodestar-pipeline --test unverified
/// rss_delta_at_1m_unverified_ids -- --ignored --nocapture`.
#[test]
#[ignore = "a measurement"]
fn rss_delta_at_1m_unverified_ids() {
    use lodestar_store::HotColumns;
    const M: u64 = 1_000_000;
    let measure = |label: &str, ids: &mut dyn Iterator<Item = u64>| {
        let before = rss_bytes();
        let hot = HotColumns::new();
        for id in ids {
            hot.touch(id, 1);
        }
        let after = rss_bytes();
        println!(
            "{label}: hot arrays {:.1} MiB allocated, RSS +{:.1} MiB, highest id {}",
            hot.memory_bytes() as f64 / 1048576.0,
            after.saturating_sub(before) as f64 / 1048576.0,
            hot.len()
        );
        hot.memory_bytes()
    };
    let base = measure("1M torrents, no unverified ids", &mut (1..=M));
    let block = measure(
        "1M unverified ids in a block, then 1M torrents",
        &mut (M + 1..=2 * M),
    );
    let mixed = measure(
        "1M unverified ids interleaved with 1M torrents",
        &mut (1..=M).map(|i| 2 * i),
    );
    println!(
        "delta: block {:+.1} MiB, interleaved {:+.1} MiB",
        (block as f64 - base as f64) / 1048576.0,
        (mixed as f64 - base as f64) / 1048576.0
    );
}

/// The plan's per-insert measurement: the writer's duplicate check is one
/// probe of `unverified_items`' `(network, key)` index per swarm hash on
/// every crawler insert. On a file store holding 1M unverified items, it
/// times misses (the crawler's usual case) and hits, against the cost of
/// the insert itself. Run: `cargo test --release -p lodestar-pipeline
/// --test unverified duplicate_check_cost -- --ignored --nocapture`.
#[test]
#[ignore = "a measurement"]
fn duplicate_check_cost() {
    use std::time::Instant;
    const ITEMS: u32 = 1_000_000;
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::open(dir.path().join("t.db"), StoreConfig::default()).unwrap();
    let dht = store.register_source("dht", "1", T).unwrap();
    let nyaa = store.register_source("nyaa", "1", T).unwrap();
    let claims = lodestar_store::claims::Claims {
        name: Some("Typical.Feed.Name.2024.1080p.WEB-DL.x264-GRP".into()),
        size: Some(1 << 30),
        ..Default::default()
    };
    let k = |i: u32, salt: u8| {
        let mut h = [salt; 20];
        h[..4].copy_from_slice(&i.to_be_bytes());
        // Spread keys over the index as real hashes are.
        h[4..8].copy_from_slice(&i.wrapping_mul(2_654_435_761).to_le_bytes());
        h
    };
    let t0 = Instant::now();
    for chunk in (0..ITEMS).collect::<Vec<_>>().chunks(10_000) {
        let keys: Vec<[u8; 20]> = chunk.iter().map(|&i| k(i, 0xAA)).collect();
        let items: Vec<NewUnverified<'_>> = keys
            .iter()
            .map(|key| NewUnverified {
                network: Network::Bt,
                key,
                source: nyaa,
                private_source: false,
                claims: Some(&claims),
                at: T,
            })
            .collect();
        store.put_unverified(&items).unwrap();
    }
    println!("built 1M items in {:.1?}", t0.elapsed());

    let probes = 200_000u32;
    let t = Instant::now();
    let mut found = 0;
    for i in 0..probes {
        found += usize::from(
            store
                .unverified(Network::Bt, &k(i, 0x55))
                .unwrap()
                .is_some(),
        );
    }
    let miss = t.elapsed().as_nanos() as f64 / f64::from(probes);
    assert_eq!(found, 0);
    let t = Instant::now();
    for i in 0..probes {
        found += usize::from(
            store
                .unverified(Network::Bt, &k(i * 5 % ITEMS, 0xAA))
                .unwrap()
                .is_some(),
        );
    }
    let hit = t.elapsed().as_nanos() as f64 / f64::from(probes);
    assert_eq!(found, probes as usize);

    // The insert it's added to: crawler-sized batches of 256.
    let raws: Vec<Vec<u8>> = (0..20_480)
        .map(|i| info_bytes(&format!("Crawled.{i}")))
        .collect();
    let prepared: Vec<_> = raws
        .iter()
        .map(|r| {
            store
                .preparer()
                .prepare(&Info::parse(r).unwrap(), None)
                .unwrap()
        })
        .collect();
    let t = Instant::now();
    for batch in prepared.chunks(256) {
        let pts: Vec<PreparedTorrent<'_>> = batch
            .iter()
            .map(|p| PreparedTorrent {
                torrent: p,
                discovered_at: T,
                metadata_at: T,
                extra_flags: 0,
                source: dht,
                verified: true,
            })
            .collect();
        store
            .write(&WriteBatch {
                prepared: &pts,
                ..WriteBatch::default()
            })
            .unwrap();
    }
    let insert = t.elapsed().as_nanos() as f64 / prepared.len() as f64;
    println!(
        "per insert: {:.2} µs; duplicate check: miss {:.2} µs, hit {:.2} µs ({:.1}% of an insert per v1 hash)",
        insert / 1000.0,
        miss / 1000.0,
        hit / 1000.0,
        100.0 * miss / insert
    );
}
