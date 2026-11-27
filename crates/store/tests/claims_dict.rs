//! The claims dictionary (X-claims-size): codec 2 blobs, training once,
//! restarts, readers, and failing closed on a dictionary that isn't there.

use lodestar_store::claims::{
    self, AltTitle, Claims, ClaimsError, Details, ExternalId, Inflate, Inflated, ZSTD_DICT,
};
use lodestar_store::claims_dict::{self, MIN_ROWS};
use lodestar_store::identify_sweep::{SweepState, TERMS_KEY};
use lodestar_store::{SourceId, Store, StoreConfig};
use rusqlite::params;

const T: i64 = 1_800_000_000;
/// `Terms::record` for a plugin whose answers may be passed on.
const TERMS: [u8; 10] = {
    let r = (180u64 * 86_400).to_be_bytes();
    [1, 0b110, r[0], r[1], r[2], r[3], r[4], r[5], r[6], r[7]]
};

/// An identification answer, varied by `i` as a catalogue's are.
fn answer(i: u64) -> Claims {
    const LANGS: [&str; 6] = ["de", "fr", "ja", "ru", "es", "zh-hant"];
    const WORDS: [&str; 8] = [
        "night", "river", "stone", "garden", "winter", "empire", "shadow", "light",
    ];
    let w = |k: u64| WORDS[(k % WORDS.len() as u64) as usize];
    let title = format!("The {} {} {}", w(i), w(i / 7), i % 97);
    Claims {
        ids: vec![
            ExternalId {
                scheme: "imdb".into(),
                value: format!("tt{:07}", 100_000 + i),
            },
            ExternalId {
                scheme: "wikidata".into(),
                value: format!("Q{}", 1000 + i * 13),
            },
        ],
        details: Some(Box::new(Details {
            canonical_title: Some(title.clone()),
            alt_titles: (0..(i % 6) as usize)
                .map(|k| AltTitle {
                    language: LANGS[k].into(),
                    title: format!("{title} {}", w(i + k as u64)),
                })
                .collect(),
            genres: vec!["drama".into(), w(i).into()],
            year: Some(1950 + (i % 70) as u16),
            match_confidence: Some(60 + (i % 40) as u8),
            ..Details::default()
        })),
        ..Claims::default()
    }
}

fn torrent(store: &Store, id: i64) {
    let mut hash = [0u8; 20];
    hash[..8].copy_from_slice(&id.to_be_bytes());
    store
        .connection()
        .execute(
            "INSERT INTO torrents (id, info_hash, name, size, piece_len, piece_count,
                 file_count, info_size, flags, discovered_at, metadata_at, last_seen_at)
             VALUES (?1, ?2, 't', 0, 0, 0, 0, 0, 0, 0, 0, 0)",
            params![id, &hash[..]],
        )
        .unwrap();
}

/// The DHT (0) and an identify source (1) with a terms record.
fn sources(store: &mut Store) -> SourceId {
    store.register_source("dht", "1", T).unwrap();
    let s = store.register_source("wikidata", "1", T).unwrap();
    store.set_plugin_state(s, TERMS_KEY, Some(&TERMS)).unwrap();
    s
}

/// Stores answers on torrents `from..to`.
fn answers(store: &mut Store, source: SourceId, from: u64, to: u64) {
    for i in from..to {
        torrent(store, i as i64);
        assert!(
            store
                .put_claims_at(i as i64, source, &answer(i), false, T)
                .unwrap()
        );
    }
}

fn codec_of(store: &Store, id: i64) -> u8 {
    store
        .connection()
        .query_row(
            "SELECT data FROM torrent_claims WHERE torrent_id = ?1",
            [id],
            |r| r.get::<_, Vec<u8>>(0),
        )
        .unwrap()[1]
}

fn claims_dicts(store: &Store) -> i64 {
    store
        .connection()
        .query_row(
            "SELECT count(*) FROM zstd_dicts WHERE kind = 'claims'",
            [],
            |r| r.get(0),
        )
        .unwrap()
}

fn fixed_compressor(dict: &[u8]) -> zstd::bulk::Compressor<'static> {
    claims_dict::compressor(dict, 19).unwrap()
}

/// `bytes` as dictionary `id`.
#[allow(non_snake_case)]
fn Fixed(id: i64, bytes: Vec<u8>) -> FixedDict {
    FixedDict(id, bytes)
}

/// A raw-content dictionary: zstd takes any bytes as one.
struct FixedDict(i64, Vec<u8>);

impl Inflate for FixedDict {
    fn inflate(&mut self, id: i64, frame: &[u8], capacity: usize) -> Inflated {
        claims::Dictionary {
            id: self.0,
            bytes: &self.1,
        }
        .inflate(id, frame, capacity)
    }
}

fn sample_dict() -> Vec<u8> {
    let mut d = Vec::new();
    for i in 0..64 {
        d.extend(claims::encode_plain(&answer(i)).unwrap());
    }
    d
}

#[test]
fn every_codec_round_trips() {
    let dict = sample_dict();
    let mut z = fixed_compressor(&dict);
    for i in 0..200 {
        let c = answer(i);
        let raw_or_zstd = claims::encode(&c, 3).unwrap();
        assert!(matches!(raw_or_zstd[1], 0 | 1));
        assert_eq!(claims::decode(&raw_or_zstd).unwrap(), c);
        let blob = claims::encode_dict(&c, 7, &mut z, 19).unwrap();
        assert_eq!(blob[0], claims::VERSION_DETAILS, "the version stays first");
        assert_eq!(blob[1], ZSTD_DICT);
        assert_eq!(claims::dictionary_of(&blob), Some(7));
        assert!(blob.len() < raw_or_zstd.len(), "the dictionary saves");
        assert_eq!(
            claims::decode_with(&blob, &mut Fixed(7, dict.clone())).unwrap(),
            c
        );
        // Codecs 0 and 1 read the same through any dictionaries.
        assert_eq!(
            claims::decode_with(&raw_or_zstd, &mut Fixed(7, dict.clone())).unwrap(),
            c
        );
    }
    // Claims a dictionary doesn't shrink are stored raw.
    let tiny = Claims {
        details: Some(Box::new(Details {
            year: Some(2001),
            ..Details::default()
        })),
        ..Claims::default()
    };
    let blob = claims::encode_dict(&tiny, 7, &mut z, 19).unwrap();
    assert_eq!(blob[1], 0);
    assert!(blob.len() >= claims::MIN_STORED);
    assert_eq!(claims::decode(&blob).unwrap(), tiny);
}

#[test]
fn an_unknown_dictionary_fails_closed() {
    let dict = sample_dict();
    let blob = claims::encode_dict(&answer(3), 7, &mut fixed_compressor(&dict), 19).unwrap();
    // No dictionaries at all, or only another one: refused, never read.
    assert_eq!(claims::decode(&blob), Err(ClaimsError::Dictionary(7)));
    assert_eq!(
        claims::decode_with(&blob, &mut Fixed(8, dict.clone())),
        Err(ClaimsError::Dictionary(7))
    );
    // Another dictionary under the right id doesn't yield other claims:
    // the frame fails, or the body does.
    let other = sample_dict().into_iter().rev().collect::<Vec<u8>>();
    assert!(claims::decode_with(&blob, &mut Fixed(7, other)).is_err());
    // A malformed id is malformed, not a lookup.
    for bad in [&[3u8, 2, 0][..], &[3, 2, 0x80, 0], &[3, 2], &[3, 2, 0x81]] {
        assert!(claims::decode_with(bad, &mut Fixed(7, dict.clone())).is_err());
    }
}

#[test]
fn training_waits_for_enough_answers() {
    let mut store = Store::open_in_memory(StoreConfig::default()).unwrap();
    let source = sources(&mut store);
    answers(&mut store, source, 1, MIN_ROWS as u64);
    assert_eq!(store.claims_dict_samples().unwrap(), None, "one short");
    assert_eq!(store.train_claims_dict().unwrap(), None);
    assert_eq!(store.active_claims_dict(), None);
    // Claims of a source with no terms record aren't answers.
    let feed = store.register_source("nyaa", "1", T).unwrap();
    for i in 5000..5010 {
        torrent(&store, i);
        store
            .put_claims_at(i, feed, &answer(i as u64), false, T)
            .unwrap();
    }
    assert_eq!(store.train_claims_dict().unwrap(), None);
    assert_eq!(claims_dicts(&store), 0);

    answers(&mut store, source, MIN_ROWS as u64, MIN_ROWS as u64 + 1);
    let t = store
        .train_claims_dict()
        .unwrap()
        .expect("due at the threshold");
    assert_eq!(t.samples, MIN_ROWS);
    assert!(t.bytes > 0 && t.bytes <= claims_dict::DICT_SIZE);
    assert_eq!(store.active_claims_dict(), Some(t.id));
    assert_eq!(claims_dicts(&store), 1);
    // Once: nothing trains again.
    assert_eq!(store.train_claims_dict().unwrap(), None);
    assert_eq!(claims_dicts(&store), 1);

    // New answers use it; those before keep their blobs; both read.
    let before = codec_of(&store, 1);
    assert_eq!(before, 1);
    answers(&mut store, source, 2000, 2010);
    assert_eq!(codec_of(&store, 2005), ZSTD_DICT);
    assert_eq!(codec_of(&store, 1), before, "no rewrite");
    for id in [1i64, 500, 2005] {
        let got = store.torrent_claims(id).unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].claims.as_ref(), Some(&answer(id as u64)));
    }
    // Feed claims stay on codec 0 or 1.
    torrent(&store, 6000);
    let named = Claims {
        name: Some("A.Name.2001.1080p".repeat(8)),
        ..Claims::default()
    };
    store.put_claims_at(6000, feed, &named, false, T).unwrap();
    assert!(codec_of(&store, 6000) <= 1);
    // A refreshed answer is rewritten with the dictionary.
    store
        .put_claims_at(1, source, &answer(1), false, T + 1)
        .unwrap();
    assert_eq!(codec_of(&store, 1), ZSTD_DICT);
    // Search reads them as served answers.
    assert_eq!(store.served_identifications(2005).unwrap().len(), 1);
}

#[test]
fn a_restart_keeps_the_dictionary_and_never_trains_again() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s.db");
    let id = {
        let mut store = Store::open(&path, StoreConfig::default()).unwrap();
        let source = sources(&mut store);
        answers(&mut store, source, 1, MIN_ROWS as u64 + 1);
        let id = store.train_claims_dict().unwrap().unwrap().id;
        answers(&mut store, source, 2000, 2003);
        id
    };
    // A reader opened now, and the writer reopened, both read codec 2.
    let reader = Store::open_reader(&path, StoreConfig::default()).unwrap();
    let mut store = Store::open(&path, StoreConfig::default()).unwrap();
    assert_eq!(store.active_claims_dict(), Some(id));
    assert_eq!(store.train_claims_dict().unwrap(), None);
    assert_eq!(store.claims_dict_samples().unwrap(), None);
    assert_eq!(claims_dicts(&store), 1);
    for s in [&reader, &store] {
        assert_eq!(
            s.torrent_claims(2001).unwrap()[0].claims.as_ref(),
            Some(&answer(2001))
        );
    }
    // The reopened writer writes with it.
    let source = SourceId::new(1).unwrap();
    answers(&mut store, source, 3000, 3001);
    assert_eq!(codec_of(&store, 3000), ZSTD_DICT);
}

#[test]
fn a_reader_opened_before_training_finds_the_dictionary() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s.db");
    let mut store = Store::open(&path, StoreConfig::default()).unwrap();
    let source = sources(&mut store);
    answers(&mut store, source, 1, MIN_ROWS as u64 + 1);
    let reader = Store::open_reader(&path, StoreConfig::default()).unwrap();
    assert_eq!(reader.active_claims_dict(), None);
    store.train_claims_dict().unwrap().unwrap();
    answers(&mut store, source, 2000, 2001);
    assert_eq!(codec_of(&store, 2000), ZSTD_DICT);
    assert_eq!(
        reader.torrent_claims(2000).unwrap()[0].claims.as_ref(),
        Some(&answer(2000))
    );
}

#[test]
fn a_setting_naming_a_missing_dictionary_is_refused_at_open() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s.db");
    drop(Store::open(&path, StoreConfig::default()).unwrap());
    rusqlite::Connection::open(&path)
        .unwrap()
        .execute(
            "INSERT INTO settings (key, value) VALUES ('claims_dict', 41)",
            [],
        )
        .unwrap();
    assert!(matches!(
        Store::open(&path, StoreConfig::default()),
        Err(lodestar_store::Error::Corrupt(_))
    ));
}

#[test]
fn another_connection_training_first_wins() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s.db");
    let mut a = Store::open(&path, StoreConfig::default()).unwrap();
    let source = sources(&mut a);
    answers(&mut a, source, 1, MIN_ROWS as u64 + 1);
    let mut b = Store::open(&path, StoreConfig::default()).unwrap();
    let samples = a.claims_dict_samples().unwrap().unwrap();
    let dict = claims_dict::train(&samples, 16 << 10).unwrap();
    let first = b
        .install_claims_dict(samples.len(), &dict)
        .unwrap()
        .unwrap();
    // `a` sampled before `b` installed: its install stores nothing.
    assert_eq!(a.install_claims_dict(samples.len(), &dict).unwrap(), None);
    assert_eq!(claims_dicts(&a), 1);
    assert_eq!(b.active_claims_dict(), Some(first));
}

/// A codec 2 row naming a dictionary that isn't stored: unreadable
/// everywhere, never served, and the sweeps pass it.
#[test]
fn a_row_naming_an_unknown_dictionary_is_unreadable_and_passed_over() {
    let mut store = Store::open_in_memory(StoreConfig::default()).unwrap();
    let source = sources(&mut store);
    answers(&mut store, source, 1, 4);
    let dict = sample_dict();
    let blob = claims::encode_dict(&answer(2), 99, &mut fixed_compressor(&dict), 19).unwrap();
    store
        .connection()
        .execute(
            "UPDATE torrent_claims SET data = ?1 WHERE torrent_id = 2",
            [&blob],
        )
        .unwrap();
    assert_eq!(store.decode_claims(&blob), Err(ClaimsError::Dictionary(99)));
    assert!(store.torrent_claims(2).unwrap().is_empty(), "left out");
    assert!(
        store.served_identifications(2).unwrap().is_empty(),
        "never served"
    );
    assert_eq!(store.served_identifications(3).unwrap().len(), 1);
    // The re-judge pages read past it.
    let page = store
        .claims_after((0, SourceId::new(0).unwrap()), 10)
        .unwrap();
    assert_eq!(page.len(), 3);
    assert!(
        page.iter()
            .find(|c| c.torrent_id == 2)
            .unwrap()
            .claims
            .is_none()
    );
    // The identify sweep logs and moves on: it ends, it doesn't spin.
    let mut steps = 0;
    while store.identify_sweep_state().unwrap() != SweepState::Done {
        store.identify_sweep_step(1).unwrap();
        steps += 1;
        assert!(steps < 50, "the sweep must end");
    }
    // Dropping it counts as shaping (the safe side) and succeeds.
    assert!(store.drop_claims(2, source).unwrap());
}

/// A store written by `main` (codec 0 and 1 rows, no dictionary) opens,
/// reads, trains and rewrites.
#[test]
fn a_store_without_a_dictionary_opens_reads_and_rewrites() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s.db");
    {
        let mut store = Store::open(&path, StoreConfig::default()).unwrap();
        let source = sources(&mut store);
        for i in 1..=MIN_ROWS as i64 {
            torrent(&store, i);
        }
        // Blobs as main's encoder wrote them, straight into the table.
        for i in 1..=MIN_ROWS as i64 {
            let blob = claims::encode(&answer(i as u64), 19).unwrap();
            store
                .connection()
                .execute(
                    "INSERT INTO torrent_claims (torrent_id, source_id, flags, data, stored_at)
                     VALUES (?1, ?2, 0, ?3, ?4)",
                    params![i, source.get(), blob, T],
                )
                .unwrap();
        }
    }
    let mut store = Store::open(&path, StoreConfig::default()).unwrap();
    assert_eq!(store.active_claims_dict(), None);
    assert_eq!(
        store.torrent_claims(17).unwrap()[0].claims.as_ref(),
        Some(&answer(17))
    );
    store.train_claims_dict().unwrap().unwrap();
    let source = SourceId::new(1).unwrap();
    store
        .put_claims_at(17, source, &answer(17), false, T + 5)
        .unwrap();
    assert_eq!(codec_of(&store, 17), ZSTD_DICT);
    assert!(codec_of(&store, 18) <= 1, "main's blob stays as written");
    for id in [17i64, 18] {
        assert_eq!(
            store.torrent_claims(id).unwrap()[0].claims.as_ref(),
            Some(&answer(id as u64))
        );
    }
}

/// The training queries go through an index, and the claims indexes'
/// queries keep their plans (no schema change).
#[test]
fn the_queries_keep_their_plans() {
    let store = Store::open_in_memory(StoreConfig::default()).unwrap();
    let plan = |sql: &str| -> String {
        let mut stmt = store
            .connection()
            .prepare(&format!("EXPLAIN QUERY PLAN {sql}"))
            .unwrap();
        stmt.query_map([], |r| r.get::<_, String>(3))
            .unwrap()
            .map(Result::unwrap)
            .collect::<Vec<_>>()
            .join(" | ")
    };
    for (sql, index) in [
        (
            "SELECT data FROM torrent_claims
             WHERE source_id IN (SELECT source_id FROM source_state WHERE key = 'k')
               AND substr(data, 1, 1) = x'03' LIMIT 10",
            "torrent_claims_",
        ),
        (
            "SELECT torrent_id, source_id, flags, data FROM torrent_claims
             WHERE torrent_id = 1 ORDER BY source_id",
            "sqlite_autoindex_torrent_claims_1",
        ),
        (
            "SELECT torrent_id FROM torrent_claims WHERE source_id = 1 AND stored_at < 5",
            "torrent_claims_stored",
        ),
        (
            "UPDATE torrent_parsed SET parser_ver = 0 WHERE torrent_id IN
                 (SELECT torrent_id FROM torrent_claims WHERE source_id = 1)",
            "COVERING INDEX torrent_claims_source",
        ),
    ] {
        let p = plan(sql);
        assert!(p.contains(index), "{sql}: {p}");
        assert!(!p.contains("SCAN torrent_claims"), "{sql}: {p}");
    }
}

/// A dictionary lookup that fails (here, its table is out of reach) fails
/// the read instead of making a readable row look unreadable; it reads
/// again once the store does.
#[test]
fn a_failed_dictionary_lookup_fails_the_read() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s.db");
    let mut store = Store::open(&path, StoreConfig::default()).unwrap();
    let source = sources(&mut store);
    answers(&mut store, source, 1, MIN_ROWS as u64 + 1);
    store.train_claims_dict().unwrap().unwrap();
    answers(&mut store, source, 2000, 2001);
    drop(store);
    // A fresh connection has no decompressor yet.
    let store = Store::open(&path, StoreConfig::default()).unwrap();
    let raw = rusqlite::Connection::open(&path).unwrap();
    raw.execute_batch("ALTER TABLE zstd_dicts RENAME TO zstd_dicts_away")
        .unwrap();
    assert!(
        store.torrent_claims(2000).is_err(),
        "not read as unreadable"
    );
    assert!(store.served_identifications(2000).is_err());
    // Rows that need no dictionary still read.
    assert_eq!(store.torrent_claims(1).unwrap().len(), 1);
    raw.execute_batch("ALTER TABLE zstd_dicts_away RENAME TO zstd_dicts")
        .unwrap();
    assert_eq!(
        store.torrent_claims(2000).unwrap()[0].claims.as_ref(),
        Some(&answer(2000))
    );
}

/// An install that fails keeps the trained dictionary: the next call
/// installs it without sampling or training again.
#[test]
fn a_failed_install_is_retried_without_training() {
    let mut store = Store::open_in_memory(StoreConfig::default()).unwrap();
    let source = sources(&mut store);
    answers(&mut store, source, 1, MIN_ROWS as u64 + 1);
    store
        .connection()
        .execute_batch(
            "CREATE TRIGGER refuse BEFORE INSERT ON settings WHEN new.key = 'claims_dict'
             BEGIN SELECT RAISE(ABORT, 'refused'); END;",
        )
        .unwrap();
    assert!(store.train_claims_dict().is_err());
    assert_eq!(store.active_claims_dict(), None);
    assert_eq!(claims_dicts(&store), 0, "nothing half-installed");
    // With no answers left a new training would find nothing to train on.
    store
        .connection()
        .execute_batch("DROP TRIGGER refuse; DELETE FROM torrent_claims;")
        .unwrap();
    let t = store
        .train_claims_dict()
        .unwrap()
        .expect("the kept dictionary");
    assert_eq!(t.samples, MIN_ROWS);
    assert_eq!(store.active_claims_dict(), Some(t.id));
}

/// A connection that finds another's dictionary active takes it up.
#[test]
fn a_connection_adopts_a_dictionary_installed_elsewhere() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s.db");
    let mut a = Store::open(&path, StoreConfig::default()).unwrap();
    let source = sources(&mut a);
    answers(&mut a, source, 1, MIN_ROWS as u64 + 1);
    let mut b = Store::open(&path, StoreConfig::default()).unwrap();
    let id = b.train_claims_dict().unwrap().unwrap().id;
    assert_eq!(a.active_claims_dict(), None);
    assert_eq!(a.train_claims_dict().unwrap(), None);
    assert_eq!(a.active_claims_dict(), Some(id));
    answers(&mut a, source, 2000, 2001);
    assert_eq!(codec_of(&a, 2000), ZSTD_DICT);
    assert_eq!(claims_dicts(&a), 1);
}
