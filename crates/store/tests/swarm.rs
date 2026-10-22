//! Swarm health storage (M4): scrape results, per-tracker detail,
//! history, the scheduler's lanes, last-seen and hot-column rebuilds.

use lodestar_bencode::{OwnedValue, to_vec};
use lodestar_metainfo::{Info, InfoHash};
use lodestar_store::{
    Col, HotColumns, LAST_SEEN_WRITE_EVERY, NEVER, NewTorrent, SourceId, Store, StoreConfig,
    SwarmUpdate, Touch, TrackerCounts, WriteBatch, flags, swarm_flags, tracker_state,
};

const DHT: SourceId = SourceId::new(0).unwrap();
const T: i64 = 1_700_000_000;

fn dict(name: &str, private: bool) -> Vec<u8> {
    let mut e: Vec<(Vec<u8>, OwnedValue)> = vec![
        (b"length".to_vec(), OwnedValue::Int(5)),
        (
            b"name".to_vec(),
            OwnedValue::Bytes(name.as_bytes().to_vec()),
        ),
        (b"piece length".to_vec(), OwnedValue::Int(16_384)),
        (b"pieces".to_vec(), OwnedValue::Bytes(vec![0; 20])),
    ];
    if private {
        e.push((b"private".to_vec(), OwnedValue::Int(1)));
    }
    to_vec(&OwnedValue::Dict(e.into_iter().collect()))
}

/// Inserts a torrent named `name` and returns (id, hash).
fn add(store: &mut Store, name: &str, private: bool, extra_flags: i64) -> (i64, InfoHash) {
    let raw = dict(name, private);
    let info = Info::parse(&raw).unwrap();
    let id = store
        .insert(&[NewTorrent {
            info: &info,
            raw_info: Some(&raw),
            discovered_at: T,
            metadata_at: T,
            extra_flags,
            source: DHT,
        }])
        .unwrap()[0]
        .id();
    (id, info.primary_hash())
}

fn mem() -> Store {
    Store::open_in_memory(StoreConfig::default()).unwrap()
}

fn update(id: i64, at: i64, seeders: u32) -> SwarmUpdate {
    SwarmUpdate {
        torrent_id: id,
        at,
        seeders,
        leechers: seeders / 2,
        completed: seeders * 10,
        trackers_reporting: u16::from(seeders > 0),
        trackers_checked: 3,
        dht: Some((seeders, 1)),
        per_tracker: vec![
            TrackerCounts {
                tracker: 1,
                seeders,
                leechers: seeders / 2,
                completed: seeders * 10,
            },
            // Zeros: "unknown here", never stored.
            TrackerCounts {
                tracker: 2,
                seeders: 0,
                leechers: 0,
                completed: 0,
            },
        ],
        next_scrape_at: at + 21_600,
        flags: 0,
        interest: false,
    }
}

fn apply(store: &mut Store, updates: &[SwarmUpdate]) -> Vec<bool> {
    store
        .write(&WriteBatch {
            swarm: updates,
            ..WriteBatch::default()
        })
        .unwrap()
        .swarm
}

#[test]
fn swarm_updates_upsert_and_read_back() {
    let mut store = mem();
    let (a, _) = add(&mut store, "a", false, 0);
    assert_eq!(store.swarm(a).unwrap(), None);
    assert_eq!(
        apply(&mut store, &[update(a, T, 12), update(999, T, 1)]),
        [true, false],
        "a deleted torrent is skipped"
    );

    let row = store.swarm(a).unwrap().unwrap();
    assert_eq!((row.seeders, row.leechers, row.completed), (12, 6, 120));
    assert_eq!((row.trackers_reporting, row.trackers_checked), (1, 3));
    assert_eq!(row.dht, Some((12, 1)));
    assert_eq!((row.last_scrape_at, row.next_scrape_at), (T, T + 21_600));
    let per = store.swarm_trackers(a).unwrap();
    assert_eq!(per.len(), 1, "only reporting trackers are kept");
    assert_eq!(per[0].counts.tracker, 1);
    assert_eq!(per[0].at, T);
    let h = store.history(a).unwrap().unwrap();
    assert_eq!(h.daily().next().unwrap().1, Some((12, 6)));

    // The swarm dies: the row updates, per-tracker detail goes, and the
    // history follows it down to zero.
    let mut dead = update(a, T + 86_400, 0);
    dead.dht = None;
    dead.flags = swarm_flags::SUSPECT;
    apply(&mut store, &[dead]);
    let row = store.swarm(a).unwrap().unwrap();
    assert_eq!(
        (row.seeders, row.dht, row.flags),
        (0, None, swarm_flags::SUSPECT)
    );
    assert!(store.swarm_trackers(a).unwrap().is_empty());
    let days: Vec<_> = store.history(a).unwrap().unwrap().daily().take(2).collect();
    assert_eq!(days[0].1, Some((0, 0)));
    assert_eq!(days[1].1, Some((12, 6)));
}

#[test]
fn history_only_for_live_or_interesting_swarms() {
    let mut store = mem();
    let (dead, _) = add(&mut store, "dead", false, 0);
    let (viewed, _) = add(&mut store, "viewed", false, 0);
    let mut zero = update(dead, T, 0);
    zero.dht = Some((0, 0));
    let mut looked = update(viewed, T, 0);
    looked.interest = true;
    apply(&mut store, &[zero, looked]);
    assert_eq!(store.history(dead).unwrap(), None);
    assert!(store.history(viewed).unwrap().is_some());
}

#[test]
fn unanswered_scrapes_keep_what_we_knew() {
    let mut store = mem();
    let (id, _) = add(&mut store, "kept", false, 0);
    let (fresh, _) = add(&mut store, "fresh", false, 0);
    apply(&mut store, &[update(id, T, 8)]);
    let unanswered = |id, at| SwarmUpdate {
        trackers_reporting: 0,
        trackers_checked: 0,
        dht: None,
        seeders: 0,
        leechers: 0,
        completed: 0,
        per_tracker: Vec::new(),
        ..update(id, at, 0)
    };
    assert!(unanswered(id, T).hot().is_none());
    assert_eq!(
        apply(
            &mut store,
            &[unanswered(id, T + 100), unanswered(fresh, T + 100)]
        ),
        [true, true]
    );
    let row = store.swarm(id).unwrap().unwrap();
    assert_eq!((row.seeders, row.trackers_checked), (8, 3), "counts kept");
    assert_eq!(row.last_scrape_at, T, "still the last answer");
    assert_eq!(row.next_scrape_at, T + 100 + 21_600, "schedule moved");
    assert_eq!(store.swarm_trackers(id).unwrap().len(), 1);
    // A first scrape nobody answered still leaves the new lane, but reads
    // as never scraped.
    let row = store.swarm(fresh).unwrap().unwrap();
    assert_eq!((row.trackers_checked, row.dht), (0, None));
    assert!(store.history(fresh).unwrap().is_none());
    assert!(store.unscraped_after(0, 10).unwrap().is_empty());
    assert_eq!(store.max_id().unwrap(), fresh);
}

#[test]
fn lanes_skip_private_torrents() {
    let mut store = mem();
    let (a, ha) = add(&mut store, "a", false, 0);
    let (b, hb) = add(&mut store, "b", false, 0);
    add(&mut store, "private", true, 0);
    add(&mut store, "private-source", false, flags::PRIVATE_SOURCE);
    let (e, he) = add(&mut store, "e", false, 0);

    let new = store.unscraped_after(0, 10).unwrap();
    assert_eq!(
        new.iter().map(|r| (r.0, r.1)).collect::<Vec<_>>(),
        [(a, ha), (b, hb), (e, he)]
    );
    assert_eq!(store.unscraped_after(b, 1).unwrap()[0].0, e);

    let mut ua = update(a, T, 1);
    ua.next_scrape_at = T + 100;
    let mut ub = update(b, T, 1);
    ub.next_scrape_at = T + 50;
    apply(&mut store, &[ua, ub]);
    assert_eq!(
        store.unscraped_after(0, 10).unwrap().len(),
        1,
        "scraped ones leave the new lane"
    );
    assert!(store.due_for_scrape(T + 49, 10).unwrap().is_empty());
    let due: Vec<i64> = store
        .due_for_scrape(T + 100, 10)
        .unwrap()
        .iter()
        .map(|r| r.0)
        .collect();
    assert_eq!(due, [b, a], "most overdue first");
    assert_eq!(store.due_for_scrape(T + 100, 1).unwrap().len(), 1);
}

#[test]
fn due_lane_query_uses_the_index() {
    let store = mem();
    let plan: Vec<String> = store
        .connection()
        .prepare(
            "EXPLAIN QUERY PLAN SELECT s.torrent_id, t.info_hash, t.flags FROM swarm s
             JOIN torrents t ON t.id = s.torrent_id
             WHERE s.next_scrape_at <= 1 AND t.flags & 2 = 0
             ORDER BY s.next_scrape_at LIMIT 5",
        )
        .unwrap()
        .query_map([], |r| r.get(3))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert!(plan.iter().any(|s| s.contains("swarm_due")), "{plan:?}");
    assert!(!plan.iter().any(|s| s.contains("TEMP B-TREE")), "{plan:?}");
}

fn touch(store: &mut Store, hash: InfoHash, at: i64, public: bool) {
    store
        .write(&WriteBatch {
            touches: &[Touch {
                hash,
                sources: 1,
                at,
                public,
            }],
            ..WriteBatch::default()
        })
        .unwrap();
}

#[test]
fn last_seen_is_written_at_most_hourly() {
    let mut store = mem();
    let (id, h) = add(&mut store, "a", false, 0);
    let seen = |s: &Store| s.torrent(id).unwrap().unwrap().last_seen_at;
    assert_eq!(seen(&store), T);
    touch(&mut store, h, T + 600, true);
    assert_eq!(seen(&store), T, "under an hour: the hot column has it");
    touch(&mut store, h, T + LAST_SEEN_WRITE_EVERY, true);
    assert_eq!(seen(&store), T + LAST_SEEN_WRITE_EVERY);
    touch(&mut store, h, T, true);
    assert_eq!(seen(&store), T + LAST_SEEN_WRITE_EVERY, "never backwards");
}

#[test]
fn public_sightings_clear_private_source() {
    let mut store = mem();
    let (id, h) = add(&mut store, "a", false, flags::PRIVATE_SOURCE);
    let private =
        |s: &Store, id: i64| s.torrent(id).unwrap().unwrap().flags & flags::PRIVATE_SOURCE != 0;
    assert!(private(&store, id));
    touch(&mut store, h, T, false);
    assert!(private(&store, id), "another private sighting keeps it");
    touch(&mut store, h, T, true);
    assert!(!private(&store, id));
    // A private commit of a stored public torrent doesn't make it private.
    add(&mut store, "a", false, flags::PRIVATE_SOURCE);
    assert!(!private(&store, id));

    // A public commit of a private-source torrent makes it public.
    let (id2, _) = add(&mut store, "b", false, flags::PRIVATE_SOURCE);
    assert!(private(&store, id2));
    add(&mut store, "b", false, 0);
    assert!(!private(&store, id2));
}

#[test]
fn trackers_get_stable_ids_and_state() {
    let mut store = mem();
    let a = store.upsert_tracker("udp://a.example:1337", T).unwrap();
    let b = store
        .upsert_tracker("http://b.example/announce", T + 1)
        .unwrap();
    assert_eq!(
        store.upsert_tracker("udp://a.example:1337", T + 2).unwrap(),
        a
    );
    assert_ne!(a, b);
    store
        .set_tracker_state(b, tracker_state::BANNED, T + 5, Some("rate limited"))
        .unwrap();
    store.set_tracker_enabled(a, false).unwrap();
    let rows = store.trackers().unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(
        (rows[0].id, rows[0].enabled, rows[0].added_at),
        (a, false, T)
    );
    assert_eq!(rows[1].state, tracker_state::BANNED);
    assert_eq!(rows[1].state_at, Some(T + 5));
    assert_eq!(rows[1].message.as_deref(), Some("rate limited"));
    // Ids aren't reused after a delete.
    store
        .connection()
        .execute("DELETE FROM trackers WHERE id = ?1", [b])
        .unwrap();
    let c = store.upsert_tracker("udp://c.example:1", T).unwrap();
    assert!(c > b);
}

#[test]
fn deleting_a_torrent_removes_its_swarm_rows() {
    let mut store = mem();
    let (id, _) = add(&mut store, "a", false, 0);
    apply(&mut store, &[update(id, T, 5)]);
    store
        .connection()
        .execute("DELETE FROM torrents WHERE id = ?1", [id])
        .unwrap();
    for table in ["swarm", "swarm_trackers", "swarm_history"] {
        let n: i64 = store
            .connection()
            .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 0, "{table}");
    }
}

#[test]
fn load_hot_rebuilds_the_columns() {
    let mut store = mem();
    let (a, ha) = add(&mut store, "a", false, 0);
    let (b, _) = add(&mut store, "b", false, 0);
    let (c, _) = add(&mut store, "c", false, 0);
    let mut ub = update(b, T, 0);
    ub.dht = None;
    apply(&mut store, &[update(a, T, 9), ub]);
    touch(&mut store, ha, T + 7200, true);

    let hot = HotColumns::new();
    assert_eq!(store.load_hot(&hot).unwrap(), 2);
    let id = |i: i64| i as u64;
    assert_eq!(hot.get(Col::Seeders, id(a)), 9);
    assert_eq!(hot.get(Col::Leechers, id(a)), 4);
    assert_eq!(hot.get(Col::Trackers, id(a)), 1);
    assert_eq!(hot.get(Col::Dht, id(a)), 10);
    assert_eq!(hot.get(Col::LastSeen, id(a)), (T + 7200) as u32);
    assert_eq!(hot.get(Col::Seeders, id(b)), 0, "scraped zero");
    assert_eq!(hot.get(Col::Dht, id(b)), NEVER, "no DHT estimate");
    assert_eq!(hot.get(Col::Seeders, id(c)), NEVER, "never scraped");
    assert_eq!(hot.get(Col::LastSeen, id(c)), T as u32);
}

#[test]
fn migrates_a_v3_database_with_data() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("lodestar.db");
    let id = {
        let mut store = Store::open(&path, StoreConfig::default()).unwrap();
        let (id, _) = add(&mut store, "kept", false, 0);
        // Roll the file back to v3, as an M3 build left it.
        store
            .connection()
            .execute_batch(
                "DROP TABLE swarm_history; DROP TABLE swarm_trackers; DROP TABLE swarm;
                 DROP TABLE trackers; DELETE FROM schema_migrations WHERE version = 4;",
            )
            .unwrap();
        id
    };
    let mut store = Store::open(&path, StoreConfig::default()).unwrap();
    assert!(
        dir.path().join("lodestar.pre-v4.db").exists(),
        "snapshot before migrating"
    );
    assert_eq!(store.torrent(id).unwrap().unwrap().name, "kept");
    assert_eq!(store.unscraped_after(0, 10).unwrap().len(), 1);
    apply(&mut store, &[update(id, T, 3)]);
    assert_eq!(store.swarm(id).unwrap().unwrap().seeders, 3);
}
