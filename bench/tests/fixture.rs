//! The fixture builder (`lodestar_bench::fixture`): deterministic for one
//! anchor, at the store's current schema, and its manifest agrees with
//! what's stored, counted independently.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::OnceLock;

use lodestar_bench::fixture::{self, LOSSES, Manifest, stage};
use lodestar_search::{SearchIndex, index_dir, parse};
use lodestar_store::rusqlite::Connection;
use lodestar_store::{Store, StoreConfig, flags};

/// 2026-03-14T09:00:00Z.
const ANCHOR: i64 = 1_773_478_800;

struct Built {
    dir: tempfile::TempDir,
    manifest: Manifest,
}

/// One build shared by the read-only tests.
fn built() -> &'static Built {
    static BUILT: OnceLock<Built> = OnceLock::new();
    BUILT.get_or_init(|| {
        let dir = tempfile::tempdir().unwrap();
        let manifest = fixture::build(dir.path(), ANCHOR).unwrap();
        Built { dir, manifest }
    })
}

fn db(dir: &Path) -> Connection {
    Connection::open(dir.join(fixture::DB)).unwrap()
}

fn count(conn: &Connection, sql: &str) -> u64 {
    conn.query_row(sql, [], |r| r.get::<_, i64>(0)).unwrap() as u64
}

/// Every query of the fixed set, as ids the index matches, with Adult and
/// filter-hidden torrents left in (the answers, before visibility).
fn answers(dir: &Path) -> Vec<Vec<u64>> {
    let index = SearchIndex::open(&index_dir(&dir.join("index"))).unwrap();
    fixture::QUERIES
        .iter()
        .chain(&["type:adult", "discovered:<7d", "ext:flac"])
        .map(|q| {
            let node = parse(q).unwrap();
            let (mut ids, over) = index
                .matching_ids(&index.query(&node, ANCHOR), 100_000)
                .unwrap();
            assert!(!over);
            ids.sort_unstable();
            ids
        })
        .collect()
}

/// The stored rows that make up the data set, in a stable order.
fn dump(dir: &Path) -> Vec<String> {
    let conn = db(dir);
    let mut out = Vec::new();
    for sql in [
        "SELECT id, hex(info_hash), name, size, file_count, flags, discovered_at, metadata_at, sources FROM torrents ORDER BY id",
        "SELECT torrent_id, seeders, leechers, completed, trackers_reporting, trackers_checked, dht_seeds, dht_peers, last_scrape_at, flags FROM swarm ORDER BY torrent_id",
        "SELECT torrent_id, hex(data) FROM swarm_history ORDER BY torrent_id",
        "SELECT hour, metric, dim, key, value FROM rollup_hour ORDER BY hour, metric, dim, key",
        "SELECT cohort, source, stage, reason, count FROM cohort_hour ORDER BY cohort, source, stage, reason",
        "SELECT at, kind, detail FROM events ORDER BY at, kind",
        "SELECT hour, last_id FROM index_watermark_hour ORDER BY hour",
        "SELECT saved_search_id, torrent_id, created_at, read_at FROM alerts ORDER BY id",
        "SELECT torrent_id, hex(data) FROM torrent_parsed ORDER BY torrent_id",
    ] {
        let mut stmt = conn.prepare(sql).unwrap();
        let n = stmt.column_count();
        let rows = stmt
            .query_map([], |r| {
                Ok((0..n)
                    .map(|i| format!("{:?}", r.get_ref(i).unwrap()))
                    .collect::<Vec<_>>()
                    .join("|"))
            })
            .unwrap();
        out.extend(rows.map(Result::unwrap));
    }
    out
}

#[test]
fn two_builds_with_one_anchor_are_identical() {
    let a = built();
    let b = tempfile::tempdir().unwrap();
    let manifest = fixture::build(b.path(), ANCHOR).unwrap();
    assert_eq!(manifest, a.manifest);
    let text = |d: &Path| std::fs::read_to_string(d.join(fixture::MANIFEST)).unwrap();
    assert_eq!(text(b.path()), text(a.dir.path()));
    assert_eq!(text(b.path()), manifest.to_json());
    assert_eq!(dump(b.path()), dump(a.dir.path()));
    assert_eq!(answers(b.path()), answers(a.dir.path()));
    // Another anchor moves every time with it, and nothing else.
    let c = tempfile::tempdir().unwrap();
    let later = fixture::build(c.path(), ANCHOR + 86_400).unwrap();
    assert_eq!(later.counts, manifest.counts);
    assert_eq!(later.queries, manifest.queries);
    assert_eq!(later.cohorts.len(), manifest.cohorts.len());
    for (x, y) in later.torrents.iter().zip(&manifest.torrents) {
        assert_eq!(x.discovered_at, y.discovered_at + 86_400, "{}", x.key);
    }
}

#[test]
fn refuses_a_directory_that_isnt_empty_or_an_anchor_off_the_hour() {
    let d = tempfile::tempdir().unwrap();
    std::fs::write(d.path().join("keep.txt"), "mine").unwrap();
    assert!(fixture::build(d.path(), ANCHOR).is_err());
    assert_eq!(std::fs::read_dir(d.path()).unwrap().count(), 1);
    let e = tempfile::tempdir().unwrap();
    assert!(fixture::build(e.path(), ANCHOR + 1).is_err());
    assert_eq!(std::fs::read_dir(e.path()).unwrap().count(), 0);
}

#[test]
fn the_store_opens_at_the_current_schema_version() {
    let b = built();
    let version = |c: &Connection| -> i64 {
        c.query_row("SELECT max(version) FROM schema_migrations", [], |r| {
            r.get(0)
        })
        .unwrap()
    };
    let fresh = Store::open_in_memory(StoreConfig::default()).unwrap();
    let want = version(fresh.connection());
    assert!(want > 0);
    assert_eq!(version(&db(b.dir.path())), want);
    // A reader opens only a store at exactly this build's version; the
    // writer opens it as is, with no migration to run.
    Store::open_reader(b.dir.path().join(fixture::DB), StoreConfig::default()).unwrap();
    let store = Store::open(b.dir.path().join(fixture::DB), StoreConfig::default()).unwrap();
    assert_eq!(store.count().unwrap(), b.manifest.counts.total);
}

#[test]
fn manifest_counts_match_independent_counts() {
    let b = built();
    let c = &b.manifest.counts;
    let conn = db(b.dir.path());
    let flagged = |bit: i64| {
        count(
            &conn,
            &format!("SELECT count(*) FROM torrents WHERE flags & {bit} != 0"),
        )
    };
    assert_eq!(count(&conn, "SELECT count(*) FROM torrents"), c.total);
    assert_eq!(c.total, c.hand + c.generated + c.imported);
    assert_eq!(c.stored + c.imported, c.total);
    assert_eq!(flagged(flags::HIDDEN), c.filter_hidden);
    assert_eq!(flagged(flags::PRIVATE_SOURCE), c.private_source);
    assert_eq!(flagged(flags::PRIVATE), c.private);
    assert_eq!(flagged(flags::V2), c.v2);
    assert_eq!(flagged(flags::HYBRID), c.hybrid);
    assert_eq!(flagged(flags::PADDING), c.padding);
    assert_eq!(flagged(flags::IMPORTED), c.imported);
    assert_eq!(flagged(flags::BACKFILL), c.backfill);
    assert_eq!(
        count(
            &conn,
            "SELECT count(*) FROM torrents WHERE file_count > 1000"
        ),
        c.large_packs
    );
    assert_eq!(
        count(
            &conn,
            "SELECT count(*) FROM torrents WHERE name_raw IS NOT NULL"
        ),
        c.non_utf8
    );
    assert_eq!(
        count(
            &conn,
            "SELECT count(*) FROM class_corrections WHERE adult = 1"
        ),
        c.adult_corrected
    );
    assert_eq!(
        count(
            &conn,
            "SELECT count(*) FROM torrent_parsed WHERE resolved_at IS NOT NULL"
        ),
        c.resolved_by_files
    );
    let a = ANCHOR;
    assert_eq!(
        count(
            &conn,
            &format!(
                "SELECT count(*) FROM torrents WHERE discovered_at >= {}",
                a - 86_400
            )
        ),
        c.discovered_24h
    );
    assert_eq!(
        count(
            &conn,
            &format!(
                "SELECT count(*) FROM torrents WHERE discovered_at >= {}",
                a - 7 * 86_400
            )
        ),
        c.discovered_7d
    );
    assert_eq!(
        count(
            &conn,
            "SELECT count(*) FROM torrents WHERE discovered_at >= 0 AND metadata_at < 0"
        ),
        0
    );

    // The index holds every torrent; Adult is its stored class.
    let index = SearchIndex::open(&index_dir(&b.dir.path().join("index"))).unwrap();
    assert_eq!(index.num_docs(), c.searchable);
    let adult = parse("type:adult").unwrap();
    let (ids, _) = index
        .matching_ids(&index.query(&adult, a), 100_000)
        .unwrap();
    assert_eq!(ids.len() as u64, c.adult);
    let browse = &b.manifest.queries[0];
    assert!(browse.q.is_empty() && !browse.hidden && !browse.adult);
    assert_eq!(browse.total, c.visible);

    // Swarm: never scraped is no row, or a row nobody answered.
    let s = &b.manifest.swarm;
    let answered = count(
        &conn,
        "SELECT count(*) FROM swarm WHERE trackers_checked > 0 OR dht_seeds IS NOT NULL",
    );
    assert_eq!(answered, s.populated);
    assert_eq!(
        count(&conn, "SELECT count(*) FROM swarm") - answered,
        s.unanswered
    );
    assert_eq!(s.populated + s.never_scraped, c.total);
    assert_eq!(
        count(
            &conn,
            "SELECT count(*) FROM swarm WHERE trackers_checked > 0 AND seeders = 0"
        ),
        s.zero
    );
    assert_eq!(
        count(
            &conn,
            &format!(
                "SELECT count(*) FROM swarm WHERE flags & {} != 0",
                lodestar_store::swarm_flags::SUSPECT
            )
        ),
        s.suspect
    );
    assert_eq!(
        count(&conn, "SELECT count(*) FROM swarm_history"),
        s.with_history
    );
    // Private torrents are never scraped (§18.7).
    assert_eq!(
        count(
            &conn,
            &format!(
                "SELECT count(*) FROM swarm JOIN torrents t ON t.id = torrent_id WHERE t.flags & {} != 0",
                flags::PRIVATE | flags::PRIVATE_SOURCE
            )
        ),
        0
    );
    for bucket in ["≥1", "≥5", "≥50", "never_scraped"] {
        assert!(
            s.seeders.get(bucket).is_some_and(|&n| n > 0),
            "seeders {bucket}"
        );
        assert!(
            s.trackers.get(bucket).is_some_and(|&n| n > 0),
            "trackers {bucket}"
        );
    }
    assert!(s.zero > 0 && s.dht_only > 0 && s.suspect > 0 && s.unanswered > 0);

    // Insights.
    let r = &b.manifest.rollups;
    let sum = |metric: u32| -> i64 {
        conn.query_row(
            &format!("SELECT coalesce(sum(value), 0) FROM rollup_hour WHERE metric = {metric}"),
            [],
            |r| r.get(0),
        )
        .unwrap()
    };
    use lodestar_pipeline::metric;
    assert_eq!(sum(metric::HARVESTED), r.harvested);
    assert_eq!(sum(metric::REDISCOVERED), r.rediscovered);
    assert_eq!(sum(metric::DROPPED), r.dropped);
    assert_eq!(sum(metric::STORED), r.stored);
    assert_eq!(r.stored as u64, c.total, "every torrent was stored once");
    assert_eq!(
        count(&conn, "SELECT count(DISTINCT hour) FROM rollup_hour"),
        r.hours
    );
    assert!(r.hours >= 30 * 24, "at least 30 days");
    assert_eq!(
        count(
            &conn,
            "SELECT count(*) FROM index_watermark_hour WHERE hour > 0"
        ),
        r.watermark_hours
    );
    assert_eq!(
        count(&conn, "SELECT max(last_id) FROM index_watermark_hour"),
        count(&conn, "SELECT max(id) FROM torrents")
    );
    assert_eq!(
        count(&conn, "SELECT count(*) FROM events"),
        b.manifest.events.len() as u64
    );

    // User data.
    for s in &b.manifest.saved_searches {
        let id = count(
            &conn,
            &format!("SELECT id FROM saved_searches WHERE name = '{}'", s.name),
        );
        assert_eq!(
            count(
                &conn,
                &format!("SELECT count(*) FROM alerts WHERE saved_search_id = {id}")
            ),
            s.alerts,
            "{}",
            s.name
        );
        assert_eq!(
            count(
                &conn,
                &format!(
                    "SELECT count(*) FROM alerts WHERE saved_search_id = {id} AND read_at IS NULL"
                )
            ),
            s.unread,
            "{}",
            s.name
        );
    }
    let u = &b.manifest.user_items;
    assert_eq!(count(&conn, "SELECT count(*) FROM user_items"), u.items);
    assert_eq!(
        count(&conn, "SELECT count(*) FROM user_items WHERE favourite = 1"),
        u.favourite
    );
    assert_eq!(
        count(&conn, "SELECT count(*) FROM user_items WHERE hidden = 1"),
        u.hidden
    );
    assert_eq!(
        count(
            &conn,
            "SELECT count(*) FROM user_items WHERE note IS NOT NULL"
        ),
        u.noted
    );
    assert_eq!(
        count(&conn, "SELECT count(*) FROM api_keys"),
        0,
        "no API keys"
    );
    let store = Store::open_reader(b.dir.path().join(fixture::DB), StoreConfig::default()).unwrap();
    let cols = store.collections().unwrap();
    assert_eq!(cols.len(), b.manifest.collections.len());
    for want in &b.manifest.collections {
        let c = cols.iter().find(|c| c.name == want.name).unwrap();
        let items = store.collection_items(c.id, None, 100).unwrap();
        let hashes: Vec<String> = items
            .iter()
            .map(|i| {
                hex(store
                    .torrent(i.torrent_id)
                    .unwrap()
                    .unwrap()
                    .info_hash
                    .as_bytes())
            })
            .collect();
        assert_eq!(&hashes, &want.items, "{}: in order", c.name);
    }
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

#[test]
fn funnel_stages_never_increase_within_a_cohort() {
    let b = built();
    let store = Store::open_reader(b.dir.path().join(fixture::DB), StoreConfig::default()).unwrap();
    let rows = store.cohorts(0, i64::MAX / 3600).unwrap();
    let mut cells: BTreeMap<(i64, u8), BTreeMap<(u8, u8), i64>> = BTreeMap::new();
    for r in &rows {
        *cells
            .entry((r.cohort, r.source.get()))
            .or_default()
            .entry((r.stage, r.reason))
            .or_default() += r.count;
    }
    let planned: std::collections::BTreeSet<(i64, u8)> = b
        .manifest
        .cohorts
        .iter()
        .map(|c| (c.cohort, c.source))
        .collect();
    let stored: std::collections::BTreeSet<(i64, u8)> = cells.keys().copied().collect();
    assert_eq!(
        stored
            .symmetric_difference(&planned)
            .take(5)
            .collect::<Vec<_>>(),
        Vec::<&(i64, u8)>::new()
    );
    let mut finals = 0;
    let mut reasons = std::collections::BTreeSet::new();
    for c in &b.manifest.cohorts {
        let got = &cells[&(c.cohort, c.source)];
        let at = |s: u8| got.get(&(s, 0)).copied().unwrap_or(0);
        let stages = [
            at(stage::HARVESTED),
            at(stage::PEER_FOUND),
            at(stage::FETCHED),
            at(stage::STORED),
            at(stage::SEARCHABLE),
        ];
        assert_eq!(
            stages,
            [c.harvested, c.peer_found, c.fetched, c.stored, c.searchable],
            "cohort {}",
            c.cohort
        );
        assert!(
            stages.windows(2).all(|w| w[0] >= w[1]),
            "cohort {}: {stages:?}",
            c.cohort
        );
        let lost: i64 = LOSSES
            .iter()
            .map(|(code, name)| {
                let n = got.get(&(stage::LOST, *code)).copied().unwrap_or(0);
                assert_eq!(n, c.losses.get(name).copied().unwrap_or(0));
                if n > 0 {
                    reasons.insert(*code);
                }
                n
            })
            .sum();
        // Harvested is the searchable, the lost and those in flight.
        assert_eq!(
            c.harvested,
            c.searchable + lost + c.in_progress,
            "cohort {}",
            c.cohort
        );
        let ended = (c.cohort + 1) * 3600;
        assert_eq!(c.final_, ended <= ANCHOR - fixture::FINAL_AFTER);
        if c.final_ {
            finals += 1;
            assert_eq!(c.in_progress, 0);
        }
    }
    assert!(finals > 0 && finals < b.manifest.cohorts.len());
    assert!(
        b.manifest
            .cohorts
            .iter()
            .any(|c| !c.final_ && c.in_progress > 0)
    );
    assert_eq!(reasons.len(), LOSSES.len(), "every loss reason");
    let stored: i64 = b.manifest.cohorts.iter().map(|c| c.stored).sum();
    assert_eq!(stored as u64, b.manifest.counts.stored);
}

#[test]
fn every_edge_case_is_present() {
    let b = built();
    let c = &b.manifest.counts;
    assert!(c.hand >= 200, "a few hundred hand-specified: {}", c.hand);
    assert_eq!(c.generated, fixture::GENERATED as u64);
    for (name, n) in [
        ("adult", c.adult),
        ("adult_corrected", c.adult_corrected),
        ("filter_hidden", c.filter_hidden),
        ("private_source", c.private_source),
        ("v2", c.v2),
        ("hybrid", c.hybrid),
        ("padding", c.padding),
        ("non_utf8", c.non_utf8),
        ("large_packs", c.large_packs),
        ("resolved_by_files", c.resolved_by_files),
        ("imported", c.imported),
        ("backfill", c.backfill),
        ("discovered_24h", c.discovered_24h),
    ] {
        assert!(n > 0, "{name}");
    }
    assert!(c.v2 > c.hybrid, "a pure v2 one too");
    // Every content type appears.
    let types = &b.manifest.queries[1].facets["type"];
    assert!(b.manifest.queries[1].adult);
    for t in [
        "movie",
        "tv",
        "music",
        "audiobook",
        "ebook",
        "comic",
        "software",
        "game",
        "images",
        "adult",
    ] {
        assert!(types.get(t).is_some_and(|&n| n > 0), "{t}: {types:?}");
    }
    let key = |k: &str| b.manifest.torrents.iter().find(|t| t.key == k).unwrap();
    assert_eq!(key("adult-corrected").content_type, "adult");
    assert!(key("v2-pure").flags & flags::V2 != 0 && key("v2-pure").flags & flags::HYBRID == 0);
    assert!(key("v2-hybrid-0").flags & flags::PADDING != 0);
    assert_eq!(key("non-utf8").name, "星之海 第一季 国语中字 全集");
    assert_eq!(key("large-pack").files, 1200);
    assert_eq!(key("ambiguous-0").content_type, "movie");
    // Several versions of one title, for siblings and Compare.
    let lantern = b
        .manifest
        .torrents
        .iter()
        .filter(|t| t.key.starts_with("movie-00-"))
        .count();
    assert!(lantern >= 5);
    // Events: start, stop, pause, resume and intensity.
    use lodestar_pipeline::event;
    for kind in [
        event::START,
        event::STOP,
        event::PAUSE,
        event::RESUME,
        event::INTENSITY,
    ] {
        assert!(
            b.manifest.events.iter().any(|e| e.kind == kind),
            "event {kind}"
        );
    }
    // Saved searches: alerting, RSS-only and paused, with read and unread
    // alerts.
    let s = &b.manifest.saved_searches;
    assert!(s.iter().any(|s| s.alert
        && !s.rss
        && !s.paused
        && s.unread > 0
        && s.alerts > s.unread));
    assert!(
        s.iter()
            .any(|s| s.rss && !s.alert && !s.paused && s.alerts > 0)
    );
    assert!(s.iter().any(|s| s.paused && s.alerts == 0));
    let u = &b.manifest.user_items;
    assert!(u.favourite > 0 && u.hidden > 0 && u.labelled > 0 && u.noted > 0);
    assert_eq!(b.manifest.collections.len(), 2);
}

#[test]
fn every_name_is_invented() {
    // The generator's real group and tracker names never reach the store.
    let b = built();
    let conn = db(b.dir.path());
    for real in [
        "RARBG",
        "YIFY",
        "SPARKS",
        "FitGirl",
        "SubsPlease",
        "HorribleSubs",
        "Erai-raws",
        "fitgirl",
    ] {
        let n = count(
            &conn,
            &format!("SELECT count(*) FROM torrents WHERE instr(name, '{real}') > 0"),
        );
        assert_eq!(n, 0, "{real}");
    }
}
