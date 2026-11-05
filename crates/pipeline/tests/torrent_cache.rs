//! The pipeline's side of torrent-cache enrichment (SRC-13,
//! `docs/decisions/2026-11-05-SRC-13.md`): the cursor never offers a
//! `private_source` key, and a cached file passes the full filter before
//! it may be stored, as a BEP 9 fetch does.

use lodestar_bencode::{OwnedValue, to_vec};
use lodestar_filter::{Action, Core, CsList, Engine, Handle, Lists, Switches, TermSet};
use lodestar_metainfo::{Info, InfoHash};
use lodestar_pipeline::torrent_cache::{self, Cursor, Judged, PAGE, Refusal};
use lodestar_pipeline::unverified::VERIFY_ROUND_SECS;
use lodestar_store::claims::Claims;
use lodestar_store::unverified::{Network, NewUnverified};
use lodestar_store::{Store, StoreConfig};

const T: i64 = 1_700_000_000;

/// The core with stand-in term lists: `lexa` is sexual, `rolea` a child
/// role, so "rolea lexa" is a CS pattern hit.
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

fn info_bytes(name: &str) -> Vec<u8> {
    let b = |s: &[u8]| OwnedValue::Bytes(s.to_vec());
    let e: Vec<(Vec<u8>, OwnedValue)> = vec![
        (b"length".to_vec(), OwnedValue::Int(900 << 20)),
        (b"name".to_vec(), b(name.as_bytes())),
        (b"piece length".to_vec(), OwnedValue::Int(1 << 20)),
        (b"pieces".to_vec(), b(&[0; 20 * 900])),
    ];
    to_vec(&OwnedValue::Dict(e.into_iter().collect()))
}

fn judge(filter: &Handle, name: &str) -> (InfoHash, Judged) {
    let info = Info::parse(&info_bytes(name)).unwrap();
    let key = info.primary_hash();
    let store = Store::open_in_memory(StoreConfig::default()).unwrap();
    let mut preparer = store.preparer();
    let j = torrent_cache::judge(&filter.current(), &mut preparer, key, &info, T);
    (key, j)
}

#[test]
fn a_clean_file_is_admitted_as_verified_metadata() {
    let (key, j) = judge(&stand_in(), "Sintel 2010 1080p.mkv");
    let Judged::Store(a) = j else {
        panic!("not admitted: {j:?}");
    };
    assert_eq!(a.key, key);
    assert_eq!(a.torrent.info_hash, key);
    assert!(a.torrent.enrichment.is_some(), "enrichment ran");
    assert!(a.verdict.decision.is_none());
    assert_eq!(torrent_cache::PROVENANCE, "torrent cache");
}

#[test]
fn a_cs_pattern_name_is_held_never_stored() {
    let (_, j) = judge(&stand_in(), "rolea lexa.mp4");
    let Judged::Held { verdict, .. } = j else {
        panic!("a CS hit was admitted: {j:?}");
    };
    assert!(matches!(
        verdict.decision.map(|d| d.action),
        Some(Action::Drop | Action::Quarantine)
    ));
}

#[test]
fn a_name_your_rules_drop_is_held() {
    let filter = stand_in();
    rule(&filter, "bunny", Action::Drop);
    let (key, j) = judge(&filter, "Big Buck Bunny.mkv");
    let Judged::Held {
        hashes, version, ..
    } = j
    else {
        panic!("a dropped name was admitted: {j:?}");
    };
    assert_eq!(hashes, [key.0]);
    assert_eq!(version, filter.current().version);
    // A weaker rule leaves it stored, marked.
    let filter = stand_in();
    rule(&filter, "bunny", Action::Flag);
    assert!(matches!(
        judge(&filter, "Big Buck Bunny.mkv").1,
        Judged::Store(_)
    ));
}

#[test]
fn a_file_for_another_key_is_refused() {
    let info = Info::parse(&info_bytes("Sintel.mkv")).unwrap();
    let store = Store::open_in_memory(StoreConfig::default()).unwrap();
    let j = torrent_cache::judge(
        &stand_in().current(),
        &mut store.preparer(),
        InfoHash([9; 20]),
        &info,
        T,
    );
    assert!(matches!(j, Judged::Refused(Refusal::Mismatch)));
}

fn claims(name: &str) -> Claims {
    Claims {
        name: Some(name.into()),
        ..Claims::default()
    }
}

#[test]
fn the_cursor_never_offers_a_private_source_key() {
    let mut store = Store::open_in_memory(StoreConfig::default()).unwrap();
    let src = store.register_source("nyaa", "1", T).unwrap();
    let c = claims("Plain Film 2020");
    let keys: Vec<([u8; 20], bool)> = (1..=5u8).map(|i| ([i; 20], i % 2 == 0)).collect();
    let items: Vec<NewUnverified<'_>> = keys
        .iter()
        .map(|(k, private)| NewUnverified {
            network: Network::Bt,
            key: k,
            source: src,
            private_source: *private,
            claims: Some(&c),
            at: T,
        })
        .collect();
    store.put_unverified(&items).unwrap();

    let mut cursor = Cursor::default();
    let page = cursor.next(&store, 2, T).unwrap();
    assert_eq!(page.keys, [InfoHash([1; 20])]);
    assert_eq!(page.private, 1);
    let page = cursor.next(&store, PAGE, T).unwrap();
    assert_eq!(page.keys, [InfoHash([3; 20]), InfoHash([5; 20])]);
    assert_eq!(page.private, 1);

    // The end: the round rests, then starts over.
    assert!(cursor.next(&store, PAGE, T).unwrap().keys.is_empty());
    assert!(cursor.next(&store, PAGE, T + 60).unwrap().keys.is_empty());
    let again = cursor.next(&store, PAGE, T + VERIFY_ROUND_SECS).unwrap();
    assert_eq!(again.keys.len(), 3);
    assert!(
        !again
            .keys
            .iter()
            .any(|k| keys.iter().any(|(p, private)| *private && *p == k.0))
    );
}

#[test]
fn a_key_on_the_floors_hash_list_is_refused_before_preparing() {
    let info = Info::parse(&info_bytes("Sintel.mkv")).unwrap();
    let key = info.primary_hash();
    let mut cs = CsList::default();
    cs.add_hash(key.0, lodestar_filter::Origin::Imported);
    let lists = Lists {
        sexual: TermSet::of(&["lexa"]),
        child_roles: TermSet::of(&["rolea"]),
        codes: TermSet::of(&["codex"]),
    };
    let engine = Engine::new(Core::new(lists, cs, Switches::default()));
    let store = Store::open_in_memory(StoreConfig::default()).unwrap();
    let j = torrent_cache::judge(&engine, &mut store.preparer(), key, &info, T);
    assert!(matches!(j, Judged::Refused(Refusal::Blocked)));
}

/// The plugin's recorded cache reply, judged whole: the file the
/// plugin's contract test verifies is the file the filter judges.
fn fixture_file() -> lodestar_metainfo::TorrentFile {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../sources-plugins/fixtures/torrent_cache/hit/002.body");
    lodestar_metainfo::TorrentFile::parse(&std::fs::read(path).unwrap()).unwrap()
}

#[test]
fn the_recorded_file_is_admitted_and_a_rule_on_its_name_holds_it() {
    let file = fixture_file();
    let key: InfoHash = "695b0b3debb14473d741f7a3a86561e42009d6dc".parse().unwrap();
    let store = Store::open_in_memory(StoreConfig::default()).unwrap();
    let filter = stand_in();
    let j = torrent_cache::judge(&filter.current(), &mut store.preparer(), key, &file.info, T);
    assert!(matches!(j, Judged::Store(_)), "{j:?}");
    rule(&filter, "sintel", Action::Drop);
    let j = torrent_cache::judge(&filter.current(), &mut store.preparer(), key, &file.info, T);
    assert!(matches!(j, Judged::Held { .. }), "{j:?}");
}
