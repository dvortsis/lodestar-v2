//! The fixture data directory behind `lodestar serve --fixture` (build
//! plan §1.3 seam 2): a store, search index and Insights tables with
//! known contents, so screens, e2e tests, the v0.5 exit and the user-doc
//! screenshots run against the real server with numbers that are known
//! in advance.
//!
//! [`build`] writes a fresh directory through the public store, pipeline
//! and search APIs only (the sequence `search-bench` uses: insert, enrich,
//! index), so it stays valid across migrations. It holds:
//!
//! - **Torrents:** about 200 hand-specified ones (several versions of one
//!   title for siblings and Compare, every content type, every edge case
//!   below) and [`GENERATED`] from [`Generator`] with a fixed seed. Every
//!   name is invented: the generator's real release-group and tracker
//!   names are swapped for invented ones ([`sanitize`]).
//! - **Edge cases, each a known count:** Adult (detected, and one by an
//!   Is adult correction), filter-hidden ([`flags::HIDDEN`]),
//!   [`flags::PRIVATE_SOURCE`], pure v2 and hybrid with padding files, a
//!   non-UTF-8 (GBK) name, a pack of more than 1,000 files, ambiguous
//!   names resolved by their files, imports.
//! - **Swarm health:** scrape results for some torrents (seeders,
//!   leechers, completed, trackers, a DHT estimate, a suspect swarm,
//!   history) and none for others, with every swarm bucket filled.
//! - **Insights:** `rollup_hour`, `cohort_hour` (final cohorts and
//!   incomplete ones, every loss reason), `events` and
//!   `index_watermark_hour` over [`DAYS`] days, consistent with the
//!   torrents.
//! - **User data:** three saved searches with read and unread alerts,
//!   user items and two ordered collections.
//!
//! Every timestamp is an offset from the anchor (the build time rounded
//! down to the hour, [`anchor`]); everything else depends only on it. The
//! crawler's gaps put nothing in the six hours after the 24 h, 72 h and
//! 7 d marks, so "24 h", "7 d", `discovered:<7d` and cohort finality give
//! the manifest's numbers for six hours after the anchor.
//!
//! The [`Manifest`] (`fixture.json` beside the database) is computed from
//! this specification, never by querying the index or the API: they're
//! what it checks. Content types come from the stored classification (the
//! input the index is built from); every other number from the specs.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::Path;

use lodestar_bencode::{OwnedValue, to_vec};
use lodestar_metainfo::{Info, Text, TextEncoding};
use lodestar_pipeline::enrich::{correction, enrich_corrected, enrich_stored, file_infos};
use lodestar_pipeline::{event, metric, strategy};
use lodestar_search::{SearchIndex, index_dir};
use lodestar_store::{
    ClassCorrection, CohortCount, Counter, Event, NewTorrent, Resolved, SavedSearchInput,
    SearchHits, SearchableCodes, SourceId, Store, StoreConfig, SwarmUpdate, TrackerCounts,
    UserItemUpdate, WriteBatch, flags, swarm_flags,
};

use crate::synth::{EPOCH, Generator};

/// The manifest's format. Bump it whenever the data set or the manifest
/// changes, so `serve --fixture DIR` rebuilds rather than reuses.
pub const FORMAT: u32 = 2;
/// The manifest's file name, beside the database.
pub const MANIFEST: &str = "fixture.json";
/// The database's file name (the server's `Config::db_path`).
pub const DB: &str = "lodestar.db";
/// The setting that marks a fixture's database, holding [`FORMAT`]: a
/// stray `fixture.json` can't make a real store a fixture.
pub const FORMAT_SETTING: &str = "fixture_format";
/// Generated torrents, after the hand-specified ones.
pub const GENERATED: usize = 2000;
/// The generator's seed.
pub const SEED: u64 = 0x10de_57a2;
/// Days of history before the anchor: the first run is this long ago.
pub const DAYS: i64 = 35;
/// A discovery cohort is final once it ended this long ago (§10.9.3).
pub const FINAL_AFTER: i64 = 72 * 3600;
/// The keyword of the fixed query set: in hand-specified names only.
pub const KEYWORD: &str = "lantern";

const HOUR: i64 = 3600;
const DAY: i64 = 86_400;
/// The pipeline's warm-up window (`PipelineConfig::backfill`).
const BACKFILL_WINDOW: i64 = 30 * DAY;
/// The pipeline's first-run setting, which `backfill_before` reads.
const FIRST_RUN_SETTING: &str = "first_run_at";

/// `cohort_hour.stage` (`lodestar_sources::funnel::Stage`; the server's
/// fixture test checks the codes match).
pub mod stage {
    pub const HARVESTED: u8 = 1;
    pub const PEER_FOUND: u8 = 2;
    pub const FETCHED: u8 = 3;
    pub const STORED: u8 = 4;
    pub const SEARCHABLE: u8 = 5;
    pub const LOST: u8 = 6;
}

/// `cohort_hour.reason` (`lodestar_sources::funnel::Loss`) with its API
/// name (`docs/metrics.md` §6.4), in code order.
pub const LOSSES: [(u8, &str); 10] = [
    (1, "no_peers"),
    (2, "timed_out"),
    (3, "refused"),
    (4, "encryption_failed"),
    (5, "oversize"),
    (6, "invalid"),
    (7, "blocked"),
    (8, "evicted"),
    (9, "gave_up"),
    (10, "unreachable"),
];
/// What `record_searchable` writes under (the server's
/// `insights::CODES`).
const SEARCHABLE: SearchableCodes = SearchableCodes {
    stage: stage::SEARCHABLE,
    newly: metric::NEWLY_SEARCHABLE,
    delay: metric::TIME_TO_SEARCHABLE,
};
/// Losses before a peer was found; the rest happen after.
const LOST_BEFORE_PEERS: [u8; 3] = [1, 8, 9];
/// Lost after the fetch (a blocklist drops it before storage).
const LOST_AFTER_FETCH: u8 = 7;

/// The anchor for a build at `now`: the hour it started in.
pub fn anchor(now: i64) -> i64 {
    now - now.rem_euclid(HOUR)
}

pub type Error = Box<dyn std::error::Error + Send + Sync>;

// ---- the manifest ----------------------------------------------------------

/// What a fixture directory holds, as `fixture.json` records it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Manifest {
    pub format: u32,
    pub anchor: i64,
    pub seed: u64,
    pub counts: Counts,
    /// The fixed query set's totals and facet counts.
    pub queries: Vec<QueryExpect>,
    /// Discover's Newest per tab, with Adult hidden.
    pub discover: Vec<DiscoverExpect>,
    pub swarm: SwarmExpect,
    pub cohorts: Vec<CohortExpect>,
    pub rollups: RollupExpect,
    pub events: Vec<EventExpect>,
    pub saved_searches: Vec<SavedExpect>,
    pub user_items: UserItemsExpect,
    pub collections: Vec<CollectionExpect>,
    /// Every hand-specified torrent.
    pub torrents: Vec<TorrentExpect>,
}

/// Torrent counts. "Visible" is what browse shows by default: neither
/// filter-hidden nor Adult.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Counts {
    /// Rows in `torrents`.
    pub total: u64,
    /// Stored by the crawler's sources (the funnel's Stored), not imports.
    pub stored: u64,
    /// Documents in the index: every torrent.
    pub searchable: u64,
    pub hand: u64,
    pub generated: u64,
    pub imported: u64,
    pub visible: u64,
    pub adult: u64,
    /// Adult by an Is adult correction rather than detection.
    pub adult_corrected: u64,
    pub filter_hidden: u64,
    pub private_source: u64,
    /// The `private` flag (a private tracker's torrent).
    pub private: u64,
    /// Pure v2 and hybrid.
    pub v2: u64,
    pub hybrid: u64,
    pub padding: u64,
    pub non_utf8: u64,
    /// More than 1,000 files.
    pub large_packs: u64,
    /// Ambiguous names resolved by their files.
    pub resolved_by_files: u64,
    pub backfill: u64,
    /// Discovered in the 24 hours and 7 days before the anchor.
    pub discovered_24h: u64,
    pub discovered_7d: u64,
}

/// One query of the fixed set: what `/search` and `/facets` return.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueryExpect {
    pub q: String,
    /// `hidden=true`.
    pub hidden: bool,
    /// "Show adult content" on.
    pub adult: bool,
    pub total: u64,
    /// Every value of the `type` and `ext` facets with its count
    /// (disjunctive: a facet ignores the query's own clause on it).
    pub facets: BTreeMap<String, BTreeMap<String, u64>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoverExpect {
    /// "Show adult content" on.
    pub adult: bool,
    /// The tab; `None` is every type.
    pub tab: Option<String>,
    pub total: u64,
    /// The first hashes of Newest, newest first.
    pub newest: Vec<String>,
}

/// Swarm health over every torrent. "Never scraped" is no swarm row, or a
/// row nobody answered (`docs/metrics.md` §7.1).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SwarmExpect {
    pub never_scraped: u64,
    pub populated: u64,
    /// Scraped, and really 0 seeders.
    pub zero: u64,
    /// A DHT estimate only (no tracker answered).
    pub dht_only: u64,
    pub suspect: u64,
    /// With a history ring.
    pub with_history: u64,
    /// A row whose only scrape nobody answered (never scraped all the same).
    pub unanswered: u64,
    /// The swarm buckets (`docs/metrics.md` §7.4) over the default browse
    /// set: `≥1`, `≥5`, `≥50` (cumulative) and `never_scraped`.
    pub seeders: BTreeMap<String, u64>,
    pub trackers: BTreeMap<String, u64>,
}

/// One discovery cohort (an hour) of one source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CohortExpect {
    pub cohort: i64,
    pub source: u8,
    /// Ended [`FINAL_AFTER`] or more before the anchor.
    pub final_: bool,
    pub harvested: i64,
    pub peer_found: i64,
    pub fetched: i64,
    pub stored: i64,
    pub searchable: i64,
    /// By API name; zeros left out.
    pub losses: BTreeMap<&'static str, i64>,
    pub in_progress: i64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RollupExpect {
    /// Sums over the whole history, per metric.
    pub harvested: i64,
    pub rediscovered: i64,
    pub dropped: i64,
    pub stored: i64,
    /// Hours with any rollup row.
    pub hours: u64,
    /// `STORED` in the 24 h and 7 d before the anchor (event time).
    pub stored_24h: i64,
    pub stored_7d: i64,
    /// Torrents that became searchable in those windows
    /// (`index_watermark_hour`).
    pub new_searchable_24h: u64,
    pub new_searchable_7d: u64,
    /// Rows in `index_watermark_hour`.
    pub watermark_hours: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventExpect {
    pub at: i64,
    pub kind: u32,
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SavedExpect {
    pub name: String,
    pub query: String,
    pub alert: bool,
    pub rss: bool,
    pub paused: bool,
    pub alerts: u64,
    pub unread: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UserItemsExpect {
    pub items: u64,
    pub favourite: u64,
    pub hidden: u64,
    pub labelled: u64,
    pub noted: u64,
    /// Each label with the torrents carrying it.
    pub labels: BTreeMap<String, u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CollectionExpect {
    pub name: String,
    /// Hashes in collection order.
    pub items: Vec<String>,
}

/// One hand-specified torrent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TorrentExpect {
    pub key: String,
    pub id: i64,
    pub hash: String,
    pub name: String,
    pub size: u64,
    /// Files, padding excluded.
    pub files: u64,
    pub content_type: String,
    /// The bits the fixture specifies (`private`, `v2`, `hybrid`,
    /// `padding`, `imported`, `backfill`, private source, filter-hidden);
    /// the server shows others too (`single_file`).
    pub flags: i64,
    pub discovered_at: i64,
    /// `None`: never scraped.
    pub swarm: Option<SwarmExpectRow>,
}

/// One torrent's swarm values as the API shows them once scraped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SwarmExpectRow {
    pub seeders: u32,
    pub leechers: u32,
    pub completed: u32,
    pub trackers_reporting: u16,
    pub trackers_checked: u16,
    pub last_scrape_at: i64,
    pub dht: Option<(u32, u32)>,
    pub suspect: bool,
}

// ---- building ---------------------------------------------------------------

/// Where a torrent came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Src {
    Dht,
    Import,
    Private,
}

/// The source ids the fixture registers, in this order.
const SOURCES: [(&str, Src); 3] = [
    ("dht", Src::Dht),
    ("bitmagnet", Src::Import),
    ("fixture-private-feed", Src::Private),
];

fn source_id(s: Src) -> SourceId {
    let i = SOURCES.iter().position(|(_, x)| *x == s).expect("listed");
    SourceId::new(i as u8).expect("valid")
}

/// How a torrent's swarm was scraped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Swarm {
    Never,
    /// A first scrape nobody answered: a row, still never scraped.
    Unanswered,
    /// Trackers answered: seeders, leechers, trackers reporting of
    /// checked.
    Trackers(u32, u32, u16, u16),
    /// Only the DHT estimate (seeds, peers).
    DhtOnly(u32, u32),
    /// One tracker reports a swarm nobody else sees.
    Suspect(u32),
}

/// One torrent of the specification.
#[derive(Debug, Clone)]
struct Spec {
    key: String,
    info: Vec<u8>,
    discovered_at: i64,
    metadata_at: i64,
    extra_flags: i64,
    src: Src,
    /// The content type the classifier must give a hand-specified one.
    expect: Option<&'static str>,
    adult_correction: bool,
    resolved: bool,
    swarm: Swarm,
    /// Daily history samples before the last scrape.
    history_days: u32,
}

/// What the manifest is computed from, per stored torrent.
#[derive(Debug, Clone)]
struct Fact {
    spec: usize,
    id: i64,
    hash: String,
    name: String,
    size: u64,
    files: u64,
    flags: i64,
    tokens: BTreeSet<String>,
    exts: BTreeSet<String>,
    ctype: String,
    adult: bool,
    /// The name isn't plain UTF-8 (the store keeps its raw bytes).
    non_utf8: bool,
}

/// Builds the fixture in `dir`, which must be empty or absent, and writes
/// its manifest there last: a directory without `fixture.json` is an
/// unfinished build.
pub fn build(dir: &Path, anchor: i64) -> Result<Manifest, Error> {
    if anchor.rem_euclid(HOUR) != 0 {
        return Err("the anchor must be a whole hour".into());
    }
    std::fs::create_dir_all(dir)?;
    if std::fs::read_dir(dir)?.next().is_some() {
        return Err(format!("{} is not empty", dir.display()).into());
    }
    let clock = Clock::new(anchor);
    let mut specs = hand_specs(&clock);
    let hand = specs.len();
    specs.extend(generated_specs(&clock, hand));
    specs.extend(imported_specs(&clock));
    for s in &mut specs {
        if s.src != Src::Import && s.discovered_at < clock.first_run + BACKFILL_WINDOW {
            s.extra_flags |= flags::BACKFILL;
        }
    }
    // Ids follow storage order, as the writer assigns them, so the index
    // watermark per hour is "every id stored by then".
    let mut order: Vec<usize> = (0..specs.len()).collect();
    order.sort_by_key(|&i| (specs[i].metadata_at, i));

    // A low zstd level: the fixture is small, and builds run in tests.
    let config = StoreConfig {
        compression_level: 3,
        ..StoreConfig::default()
    };
    let mut store = Store::open(dir.join(DB), config)?;
    for (plugin, _) in SOURCES {
        store.register_source(plugin, env!("CARGO_PKG_VERSION"), clock.first_run)?;
    }
    store.set_setting_i64(FIRST_RUN_SETTING, clock.first_run)?;
    store.set_setting_i64(FORMAT_SETTING, i64::from(FORMAT))?;

    let infos: Vec<Info> = specs
        .iter()
        .map(|s| Info::parse(&s.info).map_err(|e| format!("{}: {e}", s.key)))
        .collect::<Result<_, _>>()?;
    let mut ids = vec![0i64; specs.len()];
    for chunk in order.chunks(500) {
        let batch: Vec<NewTorrent<'_>> = chunk
            .iter()
            .map(|&i| NewTorrent {
                info: &infos[i],
                raw_info: None,
                discovered_at: specs[i].discovered_at,
                metadata_at: specs[i].metadata_at,
                extra_flags: specs[i].extra_flags,
                source: source_id(specs[i].src),
            })
            .collect();
        for (&i, inserted) in chunk.iter().zip(store.insert(&batch)?) {
            match inserted {
                lodestar_store::Inserted::New(id) => ids[i] = id,
                lodestar_store::Inserted::Existing(_) => {
                    return Err(format!("{}: a duplicate info hash", specs[i].key).into());
                }
            }
        }
    }

    // Enrichment (§6), as the writer's background pass stores it.
    let mut after = 0;
    while let Some((last, _)) = enrich_stored(&mut store, after, 1000)? {
        after = last;
    }
    // An Is adult correction (§20.9), applied the way the API applies one.
    let adult_fix = ClassCorrection {
        adult: Some(true),
        not_junk: false,
    };
    for (i, s) in specs.iter().enumerate() {
        if s.adult_correction {
            store.set_correction(ids[i], adult_fix)?;
            let row = store.torrent(ids[i])?.ok_or("stored")?;
            let tree = store.files(ids[i])?.ok_or("stored")?;
            let files = file_infos(&tree.files, tree.encoding);
            let (e, _) = enrich_corrected(&row.name, &files, correction(adult_fix));
            store.set_enrichments(&[(ids[i], e)])?;
        }
    }
    let resolved: Vec<Resolved> = specs
        .iter()
        .enumerate()
        .filter(|(_, s)| s.resolved)
        .map(|(i, s)| Resolved {
            torrent_id: ids[i],
            at: s.metadata_at + 60,
        })
        .collect();
    store.set_resolved_at(&resolved)?;

    let facts = facts(&mut store, &specs, &ids, adult_fix)?;
    let wrong: Vec<String> = facts
        .iter()
        .filter_map(|f| {
            let s = &specs[f.spec];
            let want = s.expect?;
            (f.ctype != want)
                .then(|| format!("{}: classified {}, specified {want}", s.key, f.ctype))
        })
        .collect();
    if !wrong.is_empty() {
        return Err(wrong.join("; ").into());
    }

    write_swarm(&mut store, &clock, &specs, &ids)?;
    let insights = Insights::plan(&clock, &specs, &ids);
    store.write(&WriteBatch {
        counters: &insights.counters,
        cohorts: &insights.cohort_rows,
        events: &insights.events,
        ..WriteBatch::default()
    })?;

    // The index (§8), then the hours each id became searchable in.
    let index = SearchIndex::open(&index_dir(&dir.join("index")))?;
    let mut indexer = index.writer(64 << 20)?;
    while indexer.catch_up(&mut store, 5000)? > 0 {}
    indexer.commit()?;
    indexer.finish()?;
    // As the server's recorder does on each index commit: the watermark,
    // newly searchable by type, cohort Searchable, the delay histogram and
    // the imported total. The watermark alone left the overview's "new
    // searchable" and searchable time empty outside the 72 h the repair
    // at start recounts.
    for &(hour, last_id) in &insights.watermarks {
        store.record_searchable(last_id, (hour + 1) * HOUR - 1, SEARCHABLE)?;
    }

    let user = write_user_data(&mut store, &clock, &specs, &ids, &facts)?;
    store.checkpoint()?;
    drop(store);

    let manifest = manifest(&clock, &specs, &ids, &facts, hand, &insights, user);
    let tmp = dir.join(format!("{MANIFEST}.tmp"));
    std::fs::write(&tmp, manifest.to_json())?;
    std::fs::rename(&tmp, dir.join(MANIFEST))?;
    Ok(manifest)
}

/// Times relative to the anchor, and the crawler's gaps.
#[derive(Debug, Clone)]
struct Clock {
    anchor: i64,
    first_run: i64,
    /// When the crawler ran: `[from, to)`, in order.
    active: Vec<(i64, i64)>,
}

impl Clock {
    fn new(anchor: i64) -> Clock {
        let first_run = anchor - DAYS * DAY;
        // Paused at 7 d and 24 h, restarted at 72 h: six quiet hours after
        // each mark keep the windows' numbers for six hours.
        let gaps = [
            (anchor - 7 * DAY, anchor - 7 * DAY + 6 * HOUR),
            (anchor - FINAL_AFTER, anchor - FINAL_AFTER + 6 * HOUR),
            (anchor - DAY, anchor - DAY + 6 * HOUR),
        ];
        let mut active = Vec::new();
        let mut from = first_run + HOUR;
        for (a, b) in gaps {
            active.push((from, a));
            from = b;
        }
        active.push((from, anchor));
        Clock {
            anchor,
            first_run,
            active,
        }
    }

    fn active_secs(&self) -> i64 {
        self.active.iter().map(|(a, b)| b - a).sum()
    }

    /// The time `p` seconds into the active periods.
    fn at(&self, mut p: i64) -> i64 {
        for &(a, b) in &self.active {
            if p < b - a {
                return a + p;
            }
            p -= b - a;
        }
        self.anchor - 1
    }

    fn is_active(&self, t: i64) -> bool {
        self.active.iter().any(|&(a, b)| t >= a && t < b)
    }

    /// A hand-specified discovery `age` seconds before the anchor, moved
    /// out of a gap if it falls in one; odd seconds, so it never meets a
    /// generated one (even).
    fn hand(&self, age: i64) -> i64 {
        let mut t = self.anchor - age;
        while !self.is_active(t) || !self.is_active(t + 600) {
            t -= HOUR;
        }
        t | 1
    }
}

/// Deterministic noise from a counter (SplitMix64).
fn mix(x: u64) -> u64 {
    let mut z = x.wrapping_add(0x9e37_79b9_7f4a_7c15);
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

// ---- the hand-specified torrents ----------------------------------------------

const MOVIES: [(&str, u16); 20] = [
    ("The Lantern Keeper", 2021),
    ("Saltmarsh Requiem", 2017),
    ("Quiet Engines", 2020),
    ("The Paper Cartographer", 2016),
    ("Harrow Fields", 2022),
    ("Amber Tideline", 2018),
    ("The Last Ferrywoman", 2015),
    ("Northgate Signal", 2023),
    ("Cinderbloom Avenue", 2019),
    ("Tallow and Thread", 2014),
    ("The Orrery Thief", 2024),
    ("Hollow Antlers", 2013),
    ("Midwinter Telegraph", 2020),
    ("The Brass Aviary", 2012),
    ("Kestrel Moonrise", 2022),
    ("Undertow Parish", 2016),
    ("The Clockwork Almanac", 2011),
    ("Fennel Street Ghosts", 2023),
    ("Driftglass Saints", 2019),
    ("The Ninth Lightship", 2018),
];

const SHOWS: [&str; 8] = [
    "Brackwater Station",
    "The Moth Assembly",
    "Ridgeline Postal",
    "Ashgrove Nights",
    "Copperfern Lane",
    "The Tin Parliament",
    "Sable Coast Rescue",
    "Low Tide Detectives",
];

const ALBUMS: [(&str, &str, u16); 16] = [
    ("Marrowlight", "Hollow Season", 2019),
    ("The Gilded Static", "Paper Satellites", 2021),
    ("Pellucid Ferns", "Rooms Without Clocks", 2017),
    ("Ninefold Harbor", "Lowland Hymns", 2020),
    ("Saffron Relay", "Night Market Radio", 2022),
    ("Tidal Orchard", "Brine and Blossom", 2018),
    ("Brumefield", "Quiet Weather", 2016),
    ("The Quarry Choir", "Granite Psalms", 2023),
    ("Lumen Driftwork", "Slow Signals", 2015),
    ("Velours Antique", "Faded Upholstery", 2014),
    ("Orrin Vale Ensemble", "Winter Cartography", 2019),
    ("Copper Mothwing", "Static Bloom", 2021),
    ("Hollowpine", "Ember Letters", 2020),
    ("The Kiln Sisters", "Glaze", 2017),
    ("Starling Freight", "Cargo of Small Hours", 2022),
    ("Mistral Dovecote", "Weathervanes", 2018),
];

const BOOKS: [(&str, &str, u16); 16] = [
    ("Ilsa Marrowfield", "The Lantern Keeper", 2018),
    ("Teodor Quillfeather", "A Grammar of Tides", 2016),
    ("Hesper Vantablake", "The Copper Orchard", 2019),
    ("Odile Brackenridge-Fay", "Salt Roads", 2020),
    ("Casimir Wrenhollow", "The Weight of Lighthouses", 2014),
    ("Mireille Ashcombe-Tull", "Glass Harvest", 2021),
    ("Barnaby Kettlewick", "Notes from the Ferry", 2017),
    ("Ysolde Pennyworth-Lane", "An Atlas of Small Rooms", 2022),
    ("Leopold Thistlecombe", "The Clockmaker's Debt", 2013),
    ("Aurelie Fenwright", "Winter Rations", 2015),
    ("Quentin Hollowmere", "The Signal Box", 2023),
    ("Ottilie Brambleton", "Copper Saints", 2018),
    ("Severin Ashgrove-Mott", "Field Guide to Fog", 2019),
    ("Philippa Wrexley-Dunn", "Paper Weather", 2020),
    ("Ignatius Harrowgate", "The Tin Bell", 2012),
    ("Rosalind Quarrington", "Moth Lanterns", 2021),
];

const PRODUCTS: [&str; 10] = [
    "Lumenforge Studio",
    "Quillpad Pro",
    "Harborsync",
    "Gridmason CAD",
    "Pixelloom",
    "Archivault",
    "Tracewise Debugger",
    "Notewell",
    "Cobalt Mixer",
    "Fernstone Backup",
];

const GAMES: [&str; 8] = [
    "Ember Courier",
    "Skyloom Tactics",
    "Rustvane Rally",
    "Tidewardens",
    "Quarry of Echoes",
    "Moss Knight Chronicles",
    "Glimmerdeep",
    "Bramble Siege",
];

/// Invented release groups (the relevance corpus's, plus a few).
const GROUPS: [&str; 8] = [
    "KSTRL", "NOVAQ", "TEALDK", "BRMBY", "PLNTH", "OAKUM", "ZEPHR0", "LUMN7",
];

fn dotted(s: &str) -> String {
    s.replace(' ', ".").replace('\'', "")
}

/// Adds a hand-specified torrent discovered `age` seconds before the
/// anchor.
fn push(
    out: &mut Vec<Spec>,
    clock: &Clock,
    key: String,
    info: Vec<u8>,
    age: i64,
    expect: Option<&'static str>,
) {
    let k = out.len() as i64;
    let discovered_at = clock.hand(age);
    out.push(Spec {
        key,
        info,
        discovered_at,
        metadata_at: discovered_at + 20 + k % 40,
        extra_flags: 0,
        src: Src::Dht,
        expect,
        adult_correction: false,
        resolved: false,
        swarm: Swarm::Never,
        history_days: 0,
    });
}

fn hand_specs(clock: &Clock) -> Vec<Spec> {
    let mut out = Vec::new();
    let o = &mut out;
    // Hand-specified discoveries spread over the history: the k-th at a
    // fixed fraction of it.
    let spread = |k: u64| -> i64 {
        let span = (DAYS - 1) * DAY;
        (mix(k ^ 0xfeed) % span as u64) as i64 + 2 * HOUR
    };
    let mut k = 0u64;
    let mut next_age = || {
        k += 1;
        spread(k)
    };

    // Movies: four versions each; The Lantern Keeper has six, three of
    // them recent (siblings, Compare, the alerting saved search).
    for (m, &(title, year)) in MOVIES.iter().enumerate() {
        let t = dotted(title);
        let g = |i: usize| GROUPS[(m + i) % GROUPS.len()];
        let gib = |x: u64| x << 30;
        let base = 1 + (m as u64 % 5);
        let versions: Vec<(String, Vec<u8>)> = vec![
            (
                "2160p".into(),
                single(
                    &format!(
                        "{t}.{year}.2160p.UHD.BluRay.x265.HDR.DTS-HD.MA.5.1-{}.mkv",
                        g(0)
                    ),
                    gib(40 + base * 3) + 123_456,
                ),
            ),
            (
                "1080p".into(),
                single(
                    &format!("{t}.{year}.1080p.BluRay.x264-{}.mkv", g(1)),
                    gib(8 + base) + 654_321,
                ),
            ),
            (
                "720p".into(),
                single(
                    &format!("{title} ({year}) 720p WEB-DL AAC2.0 H.264-{}.mp4", g(2)),
                    gib(2) + base * 77_777_777,
                ),
            ),
            (
                "web".into(),
                multi(
                    &format!("{t}.{year}.1080p.WEB-DL.DD+5.1.H.264-{}", g(3)),
                    &[
                        (
                            format!("{t}.{year}.1080p.WEB-DL.DD+5.1.H.264-{}.mkv", g(3)),
                            gib(4) + base * 11_111_111,
                        ),
                        (
                            format!("{t}.{year}.1080p.WEB-DL.DD+5.1.H.264-{}.nfo", g(3)),
                            3_210,
                        ),
                        (
                            format!("Sample/{}.{year}.sample.mkv", t.to_lowercase()),
                            140 << 20,
                        ),
                    ],
                ),
            ),
        ];
        for (v, (tag, info)) in versions.into_iter().enumerate() {
            let age = if m == 0 && v < 2 {
                // Two recent Lantern Keeper versions.
                [2 * HOUR + 600, 30 * HOUR][v]
            } else {
                next_age()
            };
            push(
                o,
                clock,
                format!("movie-{m:02}-{tag}"),
                info,
                age,
                Some("movie"),
            );
        }
        if m == 0 {
            push(
                o,
                clock,
                "movie-00-remux".into(),
                single(
                    &format!(
                        "{t}.{year}.2160p.UHD.BluRay.REMUX.HDR.HEVC.TrueHD.Atmos.7.1-{}.mkv",
                        g(4)
                    ),
                    gib(61) + 999,
                ),
                5 * HOUR,
                Some("movie"),
            );
            push(
                o,
                clock,
                "movie-00-cam".into(),
                single(
                    &format!("{t}.{year}.CAM.x264-{}.mkv", g(5)),
                    gib(1) + 400_000_000,
                ),
                next_age(),
                Some("movie"),
            );
            push(
                o,
                clock,
                "music-lantern-score".into(),
                multi(
                    &format!("{title} (Original Score) {year} [FLAC]"),
                    &[
                        ("01 - Wick and Glass.flac".into(), 38 << 20),
                        ("02 - The Keeper's Stair.flac".into(), 41 << 20),
                        ("03 - Lamp Oil.flac".into(), 29 << 20),
                        ("04 - Harbor at Dusk.flac".into(), 35 << 20),
                        ("cover.jpg".into(), 2 << 20),
                    ],
                ),
                2 * DAY + 12 * HOUR,
                Some("music"),
            );
        }
    }

    // Series: a season pack each for two seasons, and two single episodes.
    let tail = "1080p.WEB-DL.DD5.1.H.264";
    for (s, show) in SHOWS.iter().enumerate() {
        let t = dotted(show);
        let g = GROUPS[(s + 2) % GROUPS.len()];
        for (season, episodes) in [(1u32, 8u32), (2, 6)] {
            let name = format!("{t}.S{season:02}.{tail}-{g}");
            let files: Vec<(String, u64)> = (1..=episodes)
                .map(|e| {
                    (
                        format!("{t}.S{season:02}E{e:02}.{tail}-{g}.mkv"),
                        (1 << 30) + u64::from(e) * 13_000_000 + s as u64 * 1_000,
                    )
                })
                .collect();
            push(
                o,
                clock,
                format!("tv-{s}-s{season:02}"),
                multi(&name, &files),
                next_age(),
                Some("tv"),
            );
        }
        for e in [3u32, 4] {
            push(
                o,
                clock,
                format!("tv-{s}-s02e{e:02}"),
                single(
                    &format!("{t}.S02E{e:02}.720p.HDTV.x264-{g}.mkv"),
                    (600 << 20) + u64::from(e) * 1_234_567,
                ),
                next_age(),
                Some("tv"),
            );
        }
    }

    // Albums: FLAC, the odd ones MP3; two of the FLAC ones are recent
    // (the RSS-only saved search).
    for (a, &(artist, album, year)) in ALBUMS.iter().enumerate() {
        let flac = a % 2 == 0;
        let (fmt, ext, track) = if flac {
            ("FLAC", "flac", 31u64 << 20)
        } else {
            ("MP3 320", "mp3", 9u64 << 20)
        };
        let mut files: Vec<(String, u64)> = (1..=(8 + a % 5))
            .map(|n| {
                (
                    format!(
                        "{n:02} - {} {n}.{ext}",
                        album.split(' ').next().unwrap_or(album)
                    ),
                    track + n as u64 * 345_678,
                )
            })
            .collect();
        files.push(("cover.jpg".into(), 1_800_000 + a as u64));
        let age = if a == 2 || a == 4 {
            [20 * HOUR, 40 * HOUR][a / 3]
        } else {
            next_age()
        };
        push(
            o,
            clock,
            format!("music-{a:02}"),
            multi(&format!("{artist} - {album} ({year}) [{fmt}]"), &files),
            age,
            Some("music"),
        );
    }

    // Ebooks, one file each, and two packs.
    for (b, &(author, title, year)) in BOOKS.iter().enumerate() {
        let ext = ["epub", "mobi"][b % 2];
        push(
            o,
            clock,
            format!("ebook-{b:02}"),
            single(
                &format!("{author} - {title} ({year}).{ext}"),
                2_000_000 + b as u64 * 87_654,
            ),
            next_age(),
            Some("ebook"),
        );
    }
    for p in 0..2u64 {
        let topic = ["Coastal Navigation", "Bookbinding and Paper Craft"][p as usize];
        let files: Vec<(String, u64)> = (0..24)
            .map(|i| {
                let (author, title, _) = BOOKS[(i + p as usize * 5) % BOOKS.len()];
                (
                    format!("{author} - {title} Vol {i}.epub"),
                    1_500_000 + i as u64 * 10_000,
                )
            })
            .collect();
        push(
            o,
            clock,
            format!("ebook-pack-{p}"),
            multi(&format!("{topic} Ebook Collection 24 Books"), &files),
            next_age(),
            Some("ebook"),
        );
    }

    // Audiobooks.
    for b in 0..4 {
        let (author, title, year) = BOOKS[b + 4];
        let files: Vec<(String, u64)> = (1..=6)
            .map(|c| {
                (
                    format!("{title} - Chapter {c:02}.m4b"),
                    48_000_000 + c * 1_000,
                )
            })
            .collect();
        push(
            o,
            clock,
            format!("audiobook-{b}"),
            multi(
                &format!("{author} - {title} ({year}) [Unabridged Audiobook]"),
                &files,
            ),
            next_age(),
            Some("audiobook"),
        );
    }

    // Software.
    for (p, product) in PRODUCTS.iter().enumerate() {
        let t = dotted(product);
        let v = format!("v{}.{}.{}", 2 + p, p % 4, 100 + p * 7);
        let g = GROUPS[p % GROUPS.len()];
        let info = if p % 3 == 0 {
            single(
                &format!("{t}.{v}.x64-{g}.iso"),
                (700 << 20) + p as u64 * 9_999,
            )
        } else {
            multi(
                &format!("{t}.{v}.x64-{g}"),
                &[
                    (
                        format!("{}_setup.exe", t.to_lowercase()),
                        (90 << 20) + p as u64 * 4_321,
                    ),
                    ("readme.txt".into(), 2_048),
                    ("Docs/manual.pdf".into(), 3_400_000),
                ],
            )
        };
        push(
            o,
            clock,
            format!("software-{p}"),
            info,
            next_age(),
            Some("software"),
        );
    }

    // Games.
    for (n, game) in GAMES.iter().enumerate() {
        let mut files: Vec<(String, u64)> = vec![("setup.exe".into(), 4 << 20)];
        files.extend((1..=6).map(|i| (format!("data-{i:02}.bin"), (1 << 30) + i * 7_777)));
        files.push(("_Redist/dxsetup.exe".into(), 30 << 20));
        push(
            o,
            clock,
            format!("game-{n}"),
            multi(&format!("{game} v1.{n}.2 [Repack]"), &files),
            next_age(),
            Some("game"),
        );
    }

    // Photo sets and comics.
    for n in 0..4u64 {
        let files: Vec<(String, u64)> = (1..=60)
            .map(|i| {
                (
                    format!("IMG_{:04}.JPG", 2000 + n * 100 + i),
                    4_000_000 + i * 1_111,
                )
            })
            .collect();
        let place = [
            "Saltmarsh Coast",
            "Ridgeline Autumn",
            "Harbor Fog",
            "Old Mill Interiors",
        ];
        push(
            o,
            clock,
            format!("images-{n}"),
            multi(
                &format!("{} Photo Set {}", place[n as usize], 10 + n),
                &files,
            ),
            next_age(),
            Some("images"),
        );
    }
    for n in 0..4u64 {
        let series = [
            "Moth Lantern Patrol",
            "The Copper Kites",
            "Brine Witch",
            "Sable Deep",
        ];
        let files: Vec<(String, u64)> = (1..=8)
            .map(|i| {
                (
                    format!("{} {i:03} (2022).cbz", series[n as usize]),
                    40_000_000 + i,
                )
            })
            .collect();
        push(
            o,
            clock,
            format!("comic-{n}"),
            multi(
                &format!("{} v01-08 (2022) (Digital)", series[n as usize]),
                &files,
            ),
            next_age(),
            Some("comic"),
        );
    }

    // Adult, detected from the name (hidden by default, §20.9).
    for (n, name) in [
        "Velvetine.Studios.26.03.14.Night.Shift.XXX.1080p.MP4-PLNTH.mp4",
        "Moonlit.Sisters.Hentai.Collection.Uncensored.mp4",
        "Amberline.XXX.Parody.2020.DVDRip.x264.mp4",
        "Cinderhouse.Studios.XXX.720p.WEB.x264.mp4",
        "Saltwater.Studios.26.02.01.Poolside.XXX.2160p.MP4-OAKUM.mp4",
        "Gilded.Room.XXX.Compilation.1080p.mp4",
    ]
    .iter()
    .enumerate()
    {
        push(
            o,
            clock,
            format!("adult-{n}"),
            single(name, (900 << 20) + n as u64 * 55_555),
            next_age(),
            Some("adult"),
        );
    }
    // Adult by an Is adult correction: detection calls it video.
    push(
        o,
        clock,
        "adult-corrected".into(),
        multi(
            "Private Collection Vol 3",
            &[
                ("clip01.mp4".into(), 300 << 20),
                ("clip02.mp4".into(), 310 << 20),
                ("clip03.mp4".into(), 290 << 20),
            ],
        ),
        next_age(),
        None,
    );
    o.last_mut().expect("pushed").adult_correction = true;

    // Filter-hidden (a §20 rule's verdict).
    for n in 0..5u64 {
        let name = [
            "FREE.MOVIES.CLICK.HERE.Promo.Pack",
            "Totally.Legit.Codec.Installer.exe",
            "Spam.Bundle.Visit.Our.Site",
            "Fake.Release.Sample.Pack",
            "Promo.Video.Download.Now.mp4",
        ][n as usize];
        push(
            o,
            clock,
            format!("hidden-{n}"),
            single(name, 50_000_000 + n * 1_000),
            next_age(),
            None,
        );
        o.last_mut().expect("pushed").extra_flags |= flags::HIDDEN;
    }

    // Reported only by a private source (§18.7): never scraped.
    for n in 0..4usize {
        let (title, year) = MOVIES[n + 4];
        push(
            o,
            clock,
            format!("private-source-{n}"),
            single(
                &format!(
                    "{}.{year}.1080p.BluRay.DTS.x264-{}.mkv",
                    dotted(title),
                    GROUPS[n]
                ),
                (12 << 30) + n as u64 * 3_333,
            ),
            next_age(),
            Some("movie"),
        );
        let s = o.last_mut().expect("pushed");
        s.extra_flags |= flags::PRIVATE_SOURCE;
        s.src = Src::Private;
    }

    // v2: a pure v2 torrent and two hybrids with padding files.
    let eps = |show: &str, n: u64| -> Vec<(String, u64)> {
        (1..=n)
            .map(|e| {
                (
                    format!("{show}.S01E{e:02}.1080p.WEB-DL.x265-NOVAQ.mkv"),
                    (900 << 20) + e * 4_097,
                )
            })
            .collect()
    };
    push(
        o,
        clock,
        "v2-pure".into(),
        pure_v2(
            "Lowland.Hymns.Live.2022.1080p.WEB-DL.x265-NOVAQ",
            &eps("Lowland.Hymns.Live", 3),
        ),
        next_age(),
        None,
    );
    for n in 0..2u64 {
        let show = ["Brine.Lantern.Society", "Kiln.and.Ash"][n as usize];
        push(
            o,
            clock,
            format!("v2-hybrid-{n}"),
            hybrid(
                &format!("{show}.S01.1080p.WEB-DL.x265-NOVAQ"),
                &eps(show, 4 + n),
            ),
            next_age(),
            Some("tv"),
        );
    }

    // A legacy-encoded (GBK) name and file list, without UTF-8 keys.
    push(
        o,
        clock,
        "non-utf8".into(),
        gbk_multi(
            "星之海 第一季 国语中字 全集",
            &[
                "星之海 第01集 国语中字.mkv",
                "星之海 第02集 国语中字.mkv",
                "星之海 第03集 国语中字.mkv",
            ],
            700 << 20,
        ),
        next_age(),
        None,
    );

    // A pack of more than 1,000 files.
    let files: Vec<(String, u64)> = (0..1_200u64)
        .map(|i| {
            let place = ["Marsh", "Harbor", "Ridge", "Forest", "Quarry", "Mill"][(i % 6) as usize];
            (
                format!("{place} {:02}/{place} Recording {i:04}.wav", i / 40),
                8_000_000 + i * 17,
            )
        })
        .collect();
    push(
        o,
        clock,
        "large-pack".into(),
        multi("Field Recordings Archive 1200 Takes", &files),
        next_age(),
        None,
    );

    // Uninformative names whose files say what they are (§6).
    for n in 0..2usize {
        let (title, year) = MOVIES[n + 10];
        let name = ["a3f91c07e2b4", "7d0e44b19c2a5f"][n];
        push(
            o,
            clock,
            format!("ambiguous-{n}"),
            multi(
                name,
                &[(
                    format!(
                        "{}.{year}.1080p.BluRay.x264-{}.mkv",
                        dotted(title),
                        GROUPS[n + 3]
                    ),
                    (7 << 30) + n as u64 * 2_222,
                )],
            ),
            next_age(),
            Some("movie"),
        );
        o.last_mut().expect("pushed").resolved = true;
    }

    // Swarm health: a fixed rotation over the hand-specified torrents,
    // with history for every third scraped one. Private ones never are.
    let pattern = [
        Swarm::Never,
        Swarm::Trackers(0, 0, 0, 3),
        Swarm::Trackers(3, 1, 1, 2),
        Swarm::Trackers(17, 6, 2, 3),
        Swarm::Trackers(240, 35, 3, 3),
        Swarm::DhtOnly(12, 30),
        Swarm::Trackers(8, 2, 2, 2),
    ];
    for (i, s) in out.iter_mut().enumerate() {
        if s.extra_flags & flags::PRIVATE_SOURCE != 0 {
            continue;
        }
        s.swarm = pattern[i % pattern.len()];
        if matches!(s.swarm, Swarm::Trackers(..) | Swarm::DhtOnly(..)) && i % 3 == 0 {
            s.history_days = 10;
        }
    }
    let by_key = |out: &mut Vec<Spec>, key: &str, swarm: Swarm, days: u32| {
        let s = out.iter_mut().find(|s| s.key == key).expect("a hand key");
        s.swarm = swarm;
        s.history_days = days;
    };
    by_key(
        &mut out,
        "movie-00-2160p",
        Swarm::Trackers(512, 48, 50, 56),
        14,
    );
    by_key(
        &mut out,
        "movie-00-1080p",
        Swarm::Trackers(96, 12, 3, 3),
        10,
    );
    by_key(&mut out, "movie-00-720p", Swarm::Suspect(900), 0);
    by_key(&mut out, "movie-00-web", Swarm::Unanswered, 0);
    by_key(&mut out, "movie-00-remux", Swarm::Never, 0);
    out
}

// ---- generated and imported torrents ----------------------------------------------

fn generated_specs(clock: &Clock, first: usize) -> Vec<Spec> {
    let mut generator = Generator::new(SEED);
    let n = GENERATED as i64;
    // Even seconds spread evenly over the active periods, jittered by the
    // generator's own discovery time; never within ten minutes of a gap.
    let slot = (clock.active_secs() - 1200) / n;
    (0..GENERATED)
        .map(|i| {
            let t = generator.next_torrent();
            let jitter = (EPOCH - t.discovered_at).rem_euclid(slot.max(2));
            let at = clock.at(i as i64 * slot + jitter) & !1;
            let at = if clock.is_active(at + 600) {
                at
            } else {
                at - 1200
            };
            let k = (first + i) as u64;
            let r = mix(k);
            let info = sanitize(&t.info);
            // A private tracker's torrent is never scraped publicly (§18.7).
            let private = Info::parse(&info).is_ok_and(|i| i.private);
            let swarm = if i % 4 != 0 || private {
                Swarm::Never
            } else {
                match r % 5 {
                    0 => Swarm::Trackers(0, 0, 0, 2),
                    1 => Swarm::Trackers(1 + (r >> 8) as u32 % 4, (r >> 16) as u32 % 3, 1, 2),
                    2 => Swarm::Trackers(5 + (r >> 8) as u32 % 40, (r >> 16) as u32 % 20, 2, 3),
                    3 => Swarm::Trackers(50 + (r >> 8) as u32 % 900, (r >> 16) as u32 % 90, 3, 3),
                    _ => Swarm::DhtOnly(1 + (r >> 8) as u32 % 20, (r >> 16) as u32 % 40),
                }
            };
            Spec {
                key: format!("gen-{i:04}"),
                info,
                discovered_at: at,
                metadata_at: at + 10 + (r >> 32) as i64 % 50,
                extra_flags: 0,
                src: Src::Dht,
                expect: None,
                adult_correction: false,
                resolved: false,
                swarm,
                history_days: 0,
            }
        })
        .collect()
}

/// Torrents from a bitmagnet import (§18.3): discovered long ago by its
/// source, stored here in one import run.
fn imported_specs(clock: &Clock) -> Vec<Spec> {
    let at = clock.hand(20 * DAY) + 2;
    (0..6usize)
        .map(|n| {
            let (title, year) = MOVIES[19 - n];
            Spec {
                key: format!("imported-{n}"),
                info: single(
                    &format!(
                        "{}.{year}.720p.BluRay.x264-{}.mkv",
                        dotted(title),
                        GROUPS[7 - n]
                    ),
                    (4 << 30) + n as u64 * 6_543,
                ),
                discovered_at: clock.anchor - 400 * DAY + n as i64 * 37 * DAY + 1,
                metadata_at: at + n as i64,
                extra_flags: flags::IMPORTED,
                src: Src::Import,
                expect: Some("movie"),
                adult_correction: false,
                resolved: false,
                swarm: Swarm::Never,
                history_days: 0,
            }
        })
        .collect()
}

/// Real release-group and tracker names the generator uses, with the
/// invented ones that replace them, as whole tokens.
const RENAMES: &[(&str, &str)] = &[
    ("NTb", "QWLM"),
    ("FLUX", "FRNZL"),
    ("SPARKS", "SPRQL"),
    ("GECKOS", "GRVLT"),
    ("AMIABLE", "AMBRQ"),
    ("RARBG", "RQVLD"),
    ("YIFY", "YQRN"),
    ("EVO", "EQZ"),
    ("CMRG", "CQRT"),
    ("TEPES", "TQPLZ"),
    ("playWEB", "quillWEB"),
    ("KOGi", "KQRi"),
    ("SMURF", "SNQRL"),
    ("MZABI", "MZQRV"),
    ("ROVERS", "RQVNS"),
    ("DIMENSION", "DQMTRL"),
    ("LOL", "LQX"),
    ("KILLERS", "KQLVRS"),
    ("TBS", "TQV"),
    ("FGT", "FQZ"),
    ("ION10", "IQN12"),
    ("PSA", "PQZ"),
    ("QxR", "QzV"),
    ("Tigole", "Tqvelle"),
    ("HONE", "HQNZ"),
    ("BYNDR", "BQNDL"),
    ("EDITH", "EQWYN"),
    ("APEX", "AQSK"),
    ("MiNX", "MiQZ"),
    ("SYNCOPY", "SQLVAN"),
    ("FraMeSToR", "FrQmeLoRn"),
    ("CtrlHD", "QuillHD"),
    ("DON", "DQV"),
    ("HiFi", "HiQz"),
    ("W4NK3R", "W1QK3L"),
    ("TOMMY", "TQLLV"),
    ("ELiTE", "ELiQZ"),
    ("GalaxyRG", "NebulQRG"),
    ("PHOENiX", "PHLQZiX"),
    ("SubsPlease", "SubsHarbor"),
    ("Erai-raws", "Orqai-drafts"),
    ("HorribleSubs", "TolerableSubs"),
    ("Judas", "Quillon"),
    ("EMBER", "EMBRQ"),
    ("ASW", "QSW"),
    ("Ohys-Raws", "Oqys-Drafts"),
    ("Commie", "Comfrey"),
    ("FitGirl", "TidyKnit"),
    ("fitgirl", "tidyknit"),
    ("DODI", "DOQI"),
    ("ElAmigos", "ElVecinos"),
    ("KaOs", "KoQs"),
    ("xatab", "xqtab"),
    ("PTP", "FXA"),
    ("BTN", "FXB"),
    ("RED", "FXC"),
    ("OPS", "FXD"),
    ("HDB", "FXE"),
];

/// The generator's info dict with every real group and tracker name
/// replaced (names, paths, the v2 file tree and `source`). Legacy-encoded
/// strings are left alone; the generator puts no group in them.
pub fn sanitize(info: &[u8]) -> Vec<u8> {
    let value = lodestar_bencode::decode(info)
        .expect("the generator writes valid bencode")
        .to_owned_value();
    let OwnedValue::Dict(mut d) = value else {
        unreachable!("an info dict");
    };
    for key in [&b"name"[..], b"name.utf-8", b"source"] {
        if let Some(OwnedValue::Bytes(b)) = d.get_mut(key) {
            *b = rename(b);
        }
    }
    if let Some(OwnedValue::List(files)) = d.get_mut(&b"files"[..]) {
        for f in files {
            if let OwnedValue::Dict(f) = f {
                for key in [&b"path"[..], b"path.utf-8"] {
                    if let Some(OwnedValue::List(segs)) = f.get_mut(key) {
                        for s in segs {
                            if let OwnedValue::Bytes(b) = s {
                                *b = rename(b);
                            }
                        }
                    }
                }
            }
        }
    }
    if let Some(tree) = d.remove(&b"file tree"[..]) {
        d.insert(b"file tree".to_vec(), rename_tree(tree));
    }
    to_vec(&OwnedValue::Dict(d))
}

fn rename_tree(v: OwnedValue) -> OwnedValue {
    match v {
        OwnedValue::Dict(d) => OwnedValue::Dict(
            d.into_iter()
                .map(|(k, v)| {
                    // The empty key holds a file's length and root.
                    if k.is_empty() {
                        (k, v)
                    } else {
                        (rename(&k), rename_tree(v))
                    }
                })
                .collect(),
        ),
        other => other,
    }
}

fn rename(b: &[u8]) -> Vec<u8> {
    let Ok(s) = std::str::from_utf8(b) else {
        return b.to_vec();
    };
    let mut s = s.to_owned();
    for (from, to) in RENAMES {
        s = replace_token(&s, from, to);
    }
    s.into_bytes()
}

/// `from` replaced where it stands alone: not inside a longer word.
fn replace_token(s: &str, from: &str, to: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find(from) {
        let before = rest[..i].chars().next_back();
        let after = rest[i + from.len()..].chars().next();
        let alone =
            !before.is_some_and(char::is_alphanumeric) && !after.is_some_and(char::is_alphanumeric);
        out.push_str(&rest[..i]);
        out.push_str(if alone { to } else { from });
        rest = &rest[i + from.len()..];
    }
    out.push_str(rest);
    out
}

// ---- info dicts -------------------------------------------------------------

fn int(v: u64) -> OwnedValue {
    OwnedValue::Int(v as i64)
}

fn bytes(s: &str) -> OwnedValue {
    OwnedValue::Bytes(s.as_bytes().to_vec())
}

fn dict(entries: Vec<(&str, OwnedValue)>) -> OwnedValue {
    OwnedValue::Dict(
        entries
            .into_iter()
            .map(|(k, v)| (k.as_bytes().to_vec(), v))
            .collect(),
    )
}

/// The smallest power of two ≥ 16 KiB giving at most ~2,000 pieces (as
/// the generator picks it).
fn piece_length(total: u64) -> u64 {
    let mut len = 16u64 << 10;
    while total / len > 2000 && len < 16 << 20 {
        len *= 2;
    }
    len
}

/// Placeholder piece hashes; `pieces` is never stored.
fn pieces(total: u64, piece_len: u64) -> OwnedValue {
    OwnedValue::Bytes(vec![0; total.div_ceil(piece_len) as usize * 20])
}

fn single(name: &str, size: u64) -> Vec<u8> {
    let piece_len = piece_length(size);
    to_vec(&dict(vec![
        ("length", int(size)),
        ("name", bytes(name)),
        ("piece length", int(piece_len)),
        ("pieces", pieces(size, piece_len)),
    ]))
}

/// A multi-file torrent; paths are `/`-separated.
fn multi(name: &str, files: &[(String, u64)]) -> Vec<u8> {
    let total: u64 = files.iter().map(|(_, s)| s).sum();
    let piece_len = piece_length(total);
    let list = files
        .iter()
        .map(|(p, size)| {
            dict(vec![
                ("length", int(*size)),
                ("path", OwnedValue::List(p.split('/').map(bytes).collect())),
            ])
        })
        .collect();
    to_vec(&dict(vec![
        ("files", OwnedValue::List(list)),
        ("name", bytes(name)),
        ("piece length", int(piece_len)),
        ("pieces", pieces(total, piece_len)),
    ]))
}

fn gbk_multi(name: &str, files: &[&str], size: u64) -> Vec<u8> {
    let gbk = |s: &str| OwnedValue::Bytes(encoding_rs::GBK.encode(s).0.into_owned());
    let total = size * files.len() as u64;
    let piece_len = piece_length(total);
    let list = files
        .iter()
        .enumerate()
        .map(|(i, f)| {
            dict(vec![
                ("length", int(size + i as u64)),
                ("path", OwnedValue::List(vec![gbk(f)])),
            ])
        })
        .collect();
    to_vec(&dict(vec![
        ("files", OwnedValue::List(list)),
        ("name", gbk(name)),
        ("piece length", int(piece_len)),
        ("pieces", pieces(total + files.len() as u64, piece_len)),
    ]))
}

/// A stand-in Merkle root, distinct per file.
fn root(name: &str) -> OwnedValue {
    let mut r = [0u8; 32];
    for (i, b) in name.bytes().enumerate() {
        r[i % 32] = r[i % 32].wrapping_mul(31).wrapping_add(b);
    }
    r[0] |= 1;
    OwnedValue::Bytes(r.to_vec())
}

fn file_tree(files: &[(String, u64)]) -> OwnedValue {
    OwnedValue::Dict(
        files
            .iter()
            .map(|(f, size)| {
                (
                    f.as_bytes().to_vec(),
                    dict(vec![(
                        "",
                        dict(vec![("length", int(*size)), ("pieces root", root(f))]),
                    )]),
                )
            })
            .collect(),
    )
}

fn pure_v2(name: &str, files: &[(String, u64)]) -> Vec<u8> {
    let total: u64 = files.iter().map(|(_, s)| s).sum();
    to_vec(&dict(vec![
        ("file tree", file_tree(files)),
        ("meta version", int(2)),
        ("name", bytes(name)),
        ("piece length", int(piece_length(total).max(16 << 10))),
    ]))
}

/// A hybrid (BEP 52): the v2 tree, and v1 files with a padding file after
/// every file but the last.
fn hybrid(name: &str, files: &[(String, u64)]) -> Vec<u8> {
    let total: u64 = files.iter().map(|(_, s)| s).sum();
    let piece_len = piece_length(total).max(16 << 10);
    let mut v1 = Vec::new();
    let mut covered = 0;
    for (i, (f, size)) in files.iter().enumerate() {
        v1.push(dict(vec![
            ("length", int(*size)),
            ("path", OwnedValue::List(vec![bytes(f)])),
        ]));
        covered += size;
        let pad = (piece_len - size % piece_len) % piece_len;
        if pad > 0 && i + 1 < files.len() {
            v1.push(dict(vec![
                ("attr", bytes("p")),
                ("length", int(pad)),
                (
                    "path",
                    OwnedValue::List(vec![bytes(".pad"), bytes(&pad.to_string())]),
                ),
            ]));
            covered += pad;
        }
    }
    to_vec(&dict(vec![
        ("file tree", file_tree(files)),
        ("files", OwnedValue::List(v1)),
        ("meta version", int(2)),
        ("name", bytes(name)),
        ("piece length", int(piece_len)),
        ("pieces", pieces(covered, piece_len)),
    ]))
}

// ---- facts ------------------------------------------------------------------

fn hex(b: &[u8]) -> String {
    let mut s = String::with_capacity(b.len() * 2);
    for x in b {
        let _ = write!(s, "{x:02x}");
    }
    s
}

/// Lowercased runs of letters and digits.
fn tokens(s: &str, out: &mut BTreeSet<String>) {
    for w in s
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
    {
        out.insert(w.to_lowercase());
    }
}

/// A file name's lowercased extension: 1–10 ASCII letters or digits after
/// the last dot, with something before it.
fn extension(name: &str) -> Option<String> {
    let (stem, ext) = name.rsplit_once('.')?;
    (!stem.is_empty()
        && (1..=10).contains(&ext.len())
        && ext.chars().all(|c| c.is_ascii_alphanumeric()))
    .then(|| ext.to_ascii_lowercase())
}

/// The `torrents.flags` bits the fixture specifies (and the manifest
/// counts).
const SPECIFIED_FLAGS: i64 = flags::PRIVATE
    | flags::V2
    | flags::HYBRID
    | flags::PADDING
    | flags::IMPORTED
    | flags::BACKFILL
    | flags::PRIVATE_SOURCE
    | flags::HIDDEN;

fn facts(
    store: &mut Store,
    specs: &[Spec],
    ids: &[i64],
    adult_fix: ClassCorrection,
) -> Result<Vec<Fact>, Error> {
    let mut out = Vec::with_capacity(specs.len());
    for (i, s) in specs.iter().enumerate() {
        let row = store.torrent(ids[i])?.ok_or("stored")?;
        let tree = store.files(ids[i])?.ok_or("stored")?;
        let info = Info::parse(&s.info)?;
        let mut toks = BTreeSet::new();
        let mut exts = BTreeSet::new();
        tokens(&info.name.decode(), &mut toks);
        for f in info.files.iter().filter(|f| !f.is_padding()) {
            let segs: Vec<String> = f
                .path
                .iter()
                .map(|p| {
                    Text {
                        raw: p.clone(),
                        encoding: info.path_encoding,
                    }
                    .decode()
                    .into_owned()
                })
                .collect();
            for seg in &segs {
                tokens(seg, &mut toks);
            }
            if let Some(e) = segs.last().and_then(|n| extension(n)) {
                exts.insert(e);
            }
        }
        // The classification the stored enrichment holds (the index's
        // input), with the torrent's correction.
        let c = if s.adult_correction {
            correction(adult_fix)
        } else {
            correction(ClassCorrection::default())
        };
        let files = file_infos(&tree.files, tree.encoding);
        let (_, class) = enrich_corrected(&row.name, &files, c);
        let ctype = class.content_type.as_str().to_owned();
        // Everything else from the specification and the info dict it
        // wrote; the store must agree.
        let mut bits = s.extra_flags;
        if info.private {
            bits |= flags::PRIVATE;
        }
        if info.v2.is_some() {
            bits |= flags::V2;
            if info.v1.is_some() {
                bits |= flags::HYBRID;
            }
        }
        if info.files.iter().any(|f| f.is_padding()) {
            bits |= flags::PADDING;
        }
        let hash = match (info.v1, info.v2) {
            (Some(v1), _) => hex(v1.as_bytes()),
            (None, Some(v2)) => hex(v2.truncated().as_bytes()),
            (None, None) => return Err(format!("{}: no info hash", s.key).into()),
        };
        let file_count = info.files.iter().filter(|f| !f.is_padding()).count() as u64;
        let name = info.name.decode().into_owned();
        let stored = (
            hex(row.info_hash.as_bytes()),
            row.name.as_str(),
            row.size,
            row.file_count,
            row.flags & SPECIFIED_FLAGS,
        );
        if stored
            != (
                hash.clone(),
                name.as_str(),
                info.total_size,
                file_count,
                bits,
            )
        {
            return Err(format!("{}: the store holds {stored:?}", s.key).into());
        }
        out.push(Fact {
            spec: i,
            id: ids[i],
            hash,
            name,
            size: info.total_size,
            files: file_count,
            flags: bits,
            tokens: toks,
            exts,
            adult: ctype == "adult",
            ctype,
            non_utf8: info.name.encoding != TextEncoding::UTF8,
        });
    }
    Ok(out)
}

// ---- swarm ------------------------------------------------------------------

const TRACKERS: [&str; 3] = [
    "udp://tracker.alpha.invalid:6969/announce",
    "udp://tracker.beta.invalid:1337/announce",
    "https://tracker.gamma.invalid/announce",
];

/// The last scrape of a torrent (an hour or two before the anchor).
fn last_scrape(clock: &Clock, i: usize) -> i64 {
    clock.anchor - HOUR - (i as i64 % 50) * 60
}

fn update(s: &Spec, id: i64, at: i64, trackers: &[u16], scale: f64) -> SwarmUpdate {
    let sc = |v: u32| (f64::from(v) * scale).round() as u32;
    let mut u = SwarmUpdate {
        torrent_id: id,
        at,
        seeders: 0,
        leechers: 0,
        completed: 0,
        trackers_reporting: 0,
        trackers_checked: 0,
        dht: None,
        per_tracker: Vec::new(),
        next_scrape_at: at + 6 * HOUR,
        flags: 0,
        interest: false,
    };
    match s.swarm {
        Swarm::Never | Swarm::Unanswered => {}
        Swarm::Trackers(seeders, leechers, reporting, checked) => {
            u.seeders = sc(seeders);
            u.leechers = sc(leechers);
            u.completed = sc(seeders) * 7 + 3;
            u.trackers_reporting = reporting;
            u.trackers_checked = checked;
            // The first trackers carry the detail; more than three
            // reporting is counts only.
            for (n, &t) in trackers.iter().take(usize::from(reporting)).enumerate() {
                u.per_tracker.push(TrackerCounts {
                    tracker: t,
                    seeders: u.seeders.saturating_sub(n as u32),
                    leechers: u.leechers,
                    completed: u.completed,
                });
            }
        }
        Swarm::DhtOnly(seeds, peers) => {
            u.seeders = sc(seeds);
            u.leechers = sc(peers);
            u.dht = Some((sc(seeds), sc(peers)));
        }
        Swarm::Suspect(seeders) => {
            u.seeders = seeders;
            u.completed = 1;
            u.trackers_reporting = 1;
            u.trackers_checked = 3;
            u.dht = Some((0, 0));
            u.flags = swarm_flags::SUSPECT;
            u.per_tracker.push(TrackerCounts {
                tracker: trackers[0],
                seeders,
                leechers: 0,
                completed: 1,
            });
        }
    }
    u
}

fn expect_row(u: &SwarmUpdate) -> Option<SwarmExpectRow> {
    u.is_answered().then_some(SwarmExpectRow {
        seeders: u.seeders,
        leechers: u.leechers,
        completed: u.completed,
        trackers_reporting: u.trackers_reporting,
        trackers_checked: u.trackers_checked,
        last_scrape_at: u.at,
        dht: u.dht,
        suspect: u.flags & swarm_flags::SUSPECT != 0,
    })
}

/// The last scrape's update for spec `i` (what the swarm row ends with).
fn final_update(
    clock: &Clock,
    specs: &[Spec],
    ids: &[i64],
    trackers: &[u16],
    i: usize,
) -> SwarmUpdate {
    update(&specs[i], ids[i], last_scrape(clock, i), trackers, 1.0)
}

fn write_swarm(store: &mut Store, clock: &Clock, specs: &[Spec], ids: &[i64]) -> Result<(), Error> {
    let trackers: Vec<u16> = TRACKERS
        .iter()
        .map(|url| store.upsert_tracker(url, clock.first_run))
        .collect::<Result<_, _>>()?;
    let max_days = specs.iter().map(|s| s.history_days).max().unwrap_or(0);
    // Older rounds first, one batch per day, so history rings fill in
    // order; then the last round.
    for d in (1..=max_days).rev() {
        let round: Vec<SwarmUpdate> = specs
            .iter()
            .enumerate()
            .filter(|(_, s)| s.history_days >= d)
            .map(|(i, s)| {
                // A swarm that grew towards today.
                let scale = 1.0 - f64::from(d) * 0.05;
                update(
                    s,
                    ids[i],
                    last_scrape(clock, i) - i64::from(d) * DAY,
                    &trackers,
                    scale,
                )
            })
            .collect();
        store.write(&WriteBatch {
            swarm: &round,
            ..WriteBatch::default()
        })?;
    }
    let last: Vec<SwarmUpdate> = (0..specs.len())
        .filter(|&i| specs[i].swarm != Swarm::Never)
        .map(|i| final_update(clock, specs, ids, &trackers, i))
        .collect();
    store.write(&WriteBatch {
        swarm: &last,
        ..WriteBatch::default()
    })?;
    Ok(())
}

// ---- insights ---------------------------------------------------------------

/// The Insights tables' contents.
#[derive(Debug, Clone, Default)]
struct Insights {
    counters: Vec<Counter>,
    cohort_rows: Vec<CohortCount>,
    cohorts: Vec<CohortExpect>,
    events: Vec<Event>,
    /// `(hour, last id searchable by its end)`.
    watermarks: Vec<(i64, i64)>,
}

impl Insights {
    fn plan(clock: &Clock, specs: &[Spec], ids: &[i64]) -> Insights {
        let mut ins = Insights::default();
        let first_hour = clock.first_run.div_euclid(HOUR);
        let last_hour = clock.anchor.div_euclid(HOUR); // exclusive
        // Stored per (discovery hour, source) and per storage hour.
        let mut stored: BTreeMap<(i64, Src), i64> = BTreeMap::new();
        let mut stored_at: BTreeMap<(i64, Src), i64> = BTreeMap::new();
        for s in specs {
            if s.src != Src::Import {
                *stored
                    .entry((s.discovered_at.div_euclid(HOUR), s.src))
                    .or_default() += 1;
                // Imports are stored outside the pipeline: no counter.
                *stored_at
                    .entry((s.metadata_at.div_euclid(HOUR), s.src))
                    .or_default() += 1;
            }
        }
        let mut counter = |hour: i64, metric: u32, src: Src, key: i64, value: i64| {
            if value != 0 {
                ins.counters.push(Counter {
                    hour,
                    metric,
                    dim: u32::from(source_id(src).get()),
                    key,
                    value,
                });
            }
        };
        let mut cohorts = Vec::new();
        for hour in first_hour..last_hour {
            let start = hour * HOUR;
            let running = clock.is_active(start) || clock.is_active(start + HOUR - 1);
            let final_ = (hour + 1) * HOUR <= clock.anchor - FINAL_AFTER;
            for src in [Src::Dht, Src::Private] {
                let n_stored = stored.get(&(hour, src)).copied().unwrap_or(0);
                // The private feed only reports what it has.
                if (!running || src == Src::Private) && n_stored == 0 {
                    continue;
                }
                let r = mix(hour as u64 ^ ((src as u64) << 56));
                let mut losses = BTreeMap::new();
                let mut in_progress = 0;
                if src == Src::Dht && running {
                    let pick = |shift: u32, m: u64| ((r >> shift) % m) as i64;
                    let every = |n: i64| i64::from(hour.rem_euclid(n) == 0);
                    for (code, name) in LOSSES {
                        let v = match code {
                            1 => 3 + pick(0, 5),
                            2 => pick(8, 3),
                            3 => pick(12, 2),
                            4 => every(17),
                            5 => every(53),
                            6 => every(29),
                            7 => every(41),
                            8 => every(37),
                            9 => pick(16, 2),
                            _ => 1 + pick(20, 3),
                        };
                        if v > 0 {
                            losses.insert(name, v);
                        }
                    }
                    if !final_ {
                        in_progress = 2 + pick(24, 4);
                    }
                }
                let lost = |codes: &[u8]| -> i64 {
                    LOSSES
                        .iter()
                        .filter(|(c, _)| codes.contains(c))
                        .map(|(_, n)| losses.get(n).copied().unwrap_or(0))
                        .sum()
                };
                let after_peers: Vec<u8> = (2..=10)
                    .filter(|c| !LOST_BEFORE_PEERS.contains(c) && *c != LOST_AFTER_FETCH)
                    .collect();
                // Keys still in flight: half waiting for peers, half for
                // their metadata.
                let (waiting_peers, waiting_fetch) =
                    (in_progress / 2, in_progress - in_progress / 2);
                let fetched = n_stored + lost(&[LOST_AFTER_FETCH]);
                let peer_found = fetched + lost(&after_peers) + waiting_fetch;
                let harvested = peer_found + lost(&LOST_BEFORE_PEERS) + waiting_peers;
                cohorts.push(CohortExpect {
                    cohort: hour,
                    source: source_id(src).get(),
                    final_,
                    harvested,
                    peer_found,
                    fetched,
                    stored: n_stored,
                    searchable: n_stored,
                    losses,
                    in_progress,
                });
                // Event time: every key received (first harvests, the
                // rediscovered and the dropped), by strategy, as intake
                // counts them.
                let (rediscovered, dropped) = if src == Src::Dht {
                    (
                        5 + (r >> 40) as i64 % 20,
                        12 * i64::from(hour.rem_euclid(97) == 0),
                    )
                } else {
                    (0, 0)
                };
                let received = harvested + rediscovered + dropped;
                let sample = received / 2;
                counter(
                    hour,
                    metric::HARVESTED,
                    src,
                    i64::from(strategy::SAMPLE),
                    sample,
                );
                counter(
                    hour,
                    metric::HARVESTED,
                    src,
                    i64::from(strategy::GET_PEERS),
                    received - sample,
                );
                counter(
                    hour,
                    metric::REDISCOVERED,
                    src,
                    i64::from(strategy::SAMPLE),
                    rediscovered,
                );
                counter(hour, metric::DROPPED, src, 0, dropped);
            }
        }
        for (&(hour, src), &n) in &stored_at {
            counter(hour, metric::STORED, src, 0, n);
        }
        ins.counters
            .sort_by_key(|c| (c.hour, c.metric, c.dim, c.key));
        for c in &cohorts {
            let src = SourceId::new(c.source).expect("valid");
            let mut row = |stage: u8, reason: u8, count: i64| {
                if count > 0 {
                    ins.cohort_rows.push(CohortCount {
                        cohort: c.cohort,
                        source: src,
                        stage,
                        reason,
                        count,
                    });
                }
            };
            row(stage::HARVESTED, 0, c.harvested);
            row(stage::PEER_FOUND, 0, c.peer_found);
            row(stage::FETCHED, 0, c.fetched);
            row(stage::STORED, 0, c.stored);
            // Searchable comes from `record_searchable` (`build`).
            for (code, name) in LOSSES {
                row(stage::LOST, code, c.losses.get(name).copied().unwrap_or(0));
            }
        }
        ins.cohorts = cohorts;

        let a = clock.anchor;
        let version = format!("v{}", env!("CARGO_PKG_VERSION"));
        let ev = |at: i64, kind: u32, detail: &str| Event {
            at,
            kind,
            detail: detail.to_owned(),
        };
        ins.events = vec![
            ev(clock.first_run, event::START, &version),
            ev(a - 28 * DAY + 5 * HOUR, event::INTENSITY, "aggressive"),
            ev(a - 21 * DAY + 2 * HOUR, event::INTENSITY, "balanced"),
            ev(a - 7 * DAY, event::PAUSE, ""),
            ev(a - 7 * DAY + 6 * HOUR, event::RESUME, ""),
            ev(a - FINAL_AFTER, event::STOP, "0 keys queued"),
            ev(a - FINAL_AFTER + 6 * HOUR, event::START, &version),
            ev(a - DAY, event::PAUSE, ""),
            ev(a - DAY + 6 * HOUR, event::RESUME, ""),
        ];

        // Each id is searchable in the hour it was stored.
        let mut by_hour: BTreeMap<i64, i64> = BTreeMap::new();
        for (i, s) in specs.iter().enumerate() {
            let e = by_hour.entry(s.metadata_at.div_euclid(HOUR)).or_default();
            *e = (*e).max(ids[i]);
        }
        let mut high = 0;
        for (hour, last) in by_hour {
            if last > high {
                ins.watermarks.push((hour, last));
                high = last;
            }
        }
        ins
    }
}

// ---- user data --------------------------------------------------------------

/// The saved searches, with their ASTs as the API stores them.
const SAVED: [SavedSearchInput<'static>; 3] = [
    SavedSearchInput {
        name: "Lantern Keeper releases",
        query: KEYWORD,
        ast: r#"{"op":"word","field":"any","word":"lantern"}"#,
        sort: None,
        alert: true,
        rss: false,
        channels: 0,
        paused: false,
    },
    SavedSearchInput {
        name: "New FLAC albums",
        query: "ext:flac",
        ast: r#"{"op":"ext","any":["flac"]}"#,
        sort: Some("-discovered"),
        alert: false,
        rss: true,
        channels: 0,
        paused: false,
    },
    SavedSearchInput {
        name: "Films to check",
        query: "type:movie",
        ast: r#"{"op":"tag","key":"type","any":["movie"]}"#,
        sort: Some("-seeders"),
        alert: true,
        rss: false,
        channels: 0,
        paused: true,
    },
];

/// Whether a torrent matches one of the fixed queries (`""`, the
/// keyword, `type:…`, `ext:…`).
fn matches(f: &Fact, q: &str) -> bool {
    if q.is_empty() {
        true
    } else if let Some(t) = q.strip_prefix("type:") {
        f.ctype == t
    } else if let Some(e) = q.strip_prefix("ext:") {
        f.exts.contains(e)
    } else {
        f.tokens.contains(q)
    }
}

/// Shown by default: neither filter-hidden nor Adult.
fn visible(f: &Fact, hidden: bool, adult: bool) -> bool {
    (hidden || f.flags & flags::HIDDEN == 0) && (adult || !f.adult)
}

#[derive(Debug, Clone, Default)]
struct User {
    saved: Vec<SavedExpect>,
    items: UserItemsExpect,
    collections: Vec<CollectionExpect>,
}

fn write_user_data(
    store: &mut Store,
    clock: &Clock,
    specs: &[Spec],
    ids: &[i64],
    facts: &[Fact],
) -> Result<User, Error> {
    let a = clock.anchor;
    let id_by = |key: &str| -> i64 {
        let i = specs.iter().position(|s| s.key == key).expect("a hand key");
        ids[i]
    };
    // The highest id stored by `t`.
    let through = |t: i64| -> i64 {
        specs
            .iter()
            .zip(ids)
            .filter(|(s, _)| s.metadata_at < t)
            .map(|(_, &id)| id)
            .max()
            .unwrap_or(0)
    };
    let created = a - 3 * DAY - 3 * HOUR;
    let first_run = a - 2 * DAY;
    let second_run = a - HOUR;
    let mut user = User::default();
    let mut saved_ids = Vec::new();
    for s in &SAVED {
        saved_ids.push(store.create_saved_search(s, through(created), created)?);
    }
    // Two percolation runs; the first run's alerts have been read. A
    // paused search doesn't run.
    let hits = |q: &str, lo: i64, hi: i64| -> Vec<i64> {
        facts
            .iter()
            .filter(|f| f.id > lo && f.id <= hi && visible(f, false, false) && matches(f, q))
            .map(|f| f.id)
            .collect()
    };
    let mut alerts = vec![(0u64, 0u64); SAVED.len()];
    let mut lo = through(created);
    for (run, at) in [first_run, second_run].into_iter().enumerate() {
        let hi = through(at);
        let found: Vec<Vec<i64>> = SAVED.iter().map(|s| hits(s.query, lo, hi)).collect();
        let results: Vec<SearchHits<'_>> = SAVED
            .iter()
            .zip(&saved_ids)
            .zip(&found)
            .filter(|((s, _), _)| !s.paused)
            .map(|((_, &sid), f)| SearchHits {
                saved_search_id: sid,
                torrent_ids: f,
            })
            .collect();
        let created = store.record_percolation(hi, &results, at)?;
        for (n, s) in SAVED.iter().enumerate() {
            if s.paused {
                continue;
            }
            let k = found[n].len() as u64;
            alerts[n].0 += k;
            if run == 1 {
                alerts[n].1 += k;
            }
        }
        if run == 0
            && let Some(last) = created.iter().map(|a| a.id).max()
        {
            store.mark_alerts_read(None, last, at + 600)?;
        }
        lo = hi;
    }
    for (s, (alerts, unread)) in SAVED.iter().zip(alerts) {
        user.saved.push(SavedExpect {
            name: s.name.into(),
            query: s.query.into(),
            alert: s.alert,
            rss: s.rss,
            paused: s.paused,
            alerts,
            unread,
        });
    }

    // Your marks (§10.8): favourites, your own hidden, labels, a note.
    let at = a - 2 * DAY;
    let marks: [(&str, UserItemUpdate<'_>); 7] = [
        (
            "movie-00-2160p",
            UserItemUpdate {
                favourite: Some(true),
                labels: Some(&["To watch"]),
                note: Some(Some("The remux has the better audio track.")),
                ..UserItemUpdate::default()
            },
        ),
        (
            "movie-00-1080p",
            UserItemUpdate {
                favourite: Some(true),
                ..UserItemUpdate::default()
            },
        ),
        (
            "music-00",
            UserItemUpdate {
                favourite: Some(true),
                labels: Some(&["Archive", "Lossless"]),
                ..UserItemUpdate::default()
            },
        ),
        (
            "ebook-00",
            UserItemUpdate {
                labels: Some(&["To read"]),
                ..UserItemUpdate::default()
            },
        ),
        (
            "tv-0-s01",
            UserItemUpdate {
                favourite: Some(true),
                labels: Some(&["To watch"]),
                ..UserItemUpdate::default()
            },
        ),
        (
            "movie-00-cam",
            UserItemUpdate {
                hidden: Some(true),
                ..UserItemUpdate::default()
            },
        ),
        (
            "software-1",
            UserItemUpdate {
                hidden: Some(true),
                labels: Some(&["Archive"]),
                ..UserItemUpdate::default()
            },
        ),
    ];
    for (key, u) in &marks {
        store.set_user_item(id_by(key), u, at)?;
        let items = &mut user.items;
        items.items += 1;
        items.favourite += u64::from(u.favourite == Some(true));
        items.hidden += u64::from(u.hidden == Some(true));
        items.noted += u64::from(matches!(u.note, Some(Some(_))));
        if let Some(labels) = u.labels {
            items.labelled += 1;
            for l in labels {
                *items.labels.entry((*l).to_owned()).or_default() += 1;
            }
        }
    }

    // Two ordered collections.
    let hash_by = |key: &str| -> String {
        let id = id_by(key);
        facts
            .iter()
            .find(|f| f.id == id)
            .expect("stored")
            .hash
            .clone()
    };
    for (name, note, keys) in [
        (
            "Weekend films",
            Some("Shortlist for the projector night."),
            &[
                "movie-03-1080p",
                "movie-00-2160p",
                "movie-07-web",
                "movie-12-720p",
                "movie-05-1080p",
            ][..],
        ),
        (
            "Reading list",
            None,
            &["ebook-03", "ebook-00", "ebook-11", "audiobook-1"][..],
        ),
    ] {
        let c = store.create_collection(name, note, at)?;
        let members: Vec<i64> = keys.iter().map(|k| id_by(k)).collect();
        store.add_to_collection(c, &members, at + 60)?;
        user.collections.push(CollectionExpect {
            name: name.into(),
            items: keys.iter().map(|k| hash_by(k)).collect(),
        });
    }
    Ok(user)
}

// ---- the manifest's numbers ---------------------------------------------------

/// The fixed query set: browse, the keyword, a type and an extension, each
/// with Adult hidden and shown, and browse with `hidden=true`.
pub const QUERIES: [&str; 4] = ["", KEYWORD, "type:movie", "ext:mkv"];

fn query_expect(facts: &[Fact], q: &str, hidden: bool, adult: bool) -> QueryExpect {
    let shown: Vec<&Fact> = facts.iter().filter(|f| visible(f, hidden, adult)).collect();
    let total = shown.iter().filter(|f| matches(f, q)).count() as u64;
    let mut facets = BTreeMap::new();
    // Disjunctive: each facet ignores the query's own clause on it.
    let (q_type, q_ext) = (
        if q.starts_with("type:") { "" } else { q },
        if q.starts_with("ext:") { "" } else { q },
    );
    let mut types: BTreeMap<String, u64> = BTreeMap::new();
    for f in shown.iter().filter(|f| matches(f, q_type)) {
        *types.entry(f.ctype.clone()).or_default() += 1;
    }
    let mut exts: BTreeMap<String, u64> = BTreeMap::new();
    for f in shown.iter().filter(|f| matches(f, q_ext)) {
        for e in &f.exts {
            *exts.entry(e.clone()).or_default() += 1;
        }
    }
    facets.insert("type".to_owned(), types);
    facets.insert("ext".to_owned(), exts);
    QueryExpect {
        q: q.to_owned(),
        hidden,
        adult,
        total,
        facets,
    }
}

fn manifest(
    clock: &Clock,
    specs: &[Spec],
    ids: &[i64],
    facts: &[Fact],
    hand: usize,
    insights: &Insights,
    user: User,
) -> Manifest {
    let a = clock.anchor;
    let n = |p: &dyn Fn(&Fact) -> bool| facts.iter().filter(|f| p(f)).count() as u64;
    let spec = |f: &Fact| &specs[f.spec];
    let counts = Counts {
        total: facts.len() as u64,
        stored: n(&|f| spec(f).src != Src::Import),
        searchable: facts.len() as u64,
        hand: hand as u64,
        generated: GENERATED as u64,
        imported: n(&|f| spec(f).src == Src::Import),
        visible: n(&|f| visible(f, false, false)),
        adult: n(&|f| f.adult),
        adult_corrected: n(&|f| spec(f).adult_correction),
        filter_hidden: n(&|f| f.flags & flags::HIDDEN != 0),
        private_source: n(&|f| f.flags & flags::PRIVATE_SOURCE != 0),
        private: n(&|f| f.flags & flags::PRIVATE != 0),
        v2: n(&|f| f.flags & flags::V2 != 0),
        hybrid: n(&|f| f.flags & flags::HYBRID != 0),
        padding: n(&|f| f.flags & flags::PADDING != 0),
        non_utf8: n(&|f| f.non_utf8),
        large_packs: n(&|f| f.files > 1000),
        resolved_by_files: n(&|f| spec(f).resolved),
        backfill: n(&|f| f.flags & flags::BACKFILL != 0),
        discovered_24h: n(&|f| spec(f).discovered_at >= a - DAY),
        discovered_7d: n(&|f| spec(f).discovered_at >= a - 7 * DAY),
    };

    let mut queries = Vec::new();
    for q in QUERIES {
        for adult in [false, true] {
            queries.push(query_expect(facts, q, false, adult));
        }
    }
    for adult in [false, true] {
        queries.push(query_expect(facts, "", true, adult));
    }

    // Discover's Newest, with Adult hidden and shown: every type, then
    // each type's tab.
    let mut discover = Vec::new();
    for adult in [false, true] {
        let mut newest: Vec<&Fact> = facts.iter().filter(|f| visible(f, false, adult)).collect();
        newest.sort_by_key(|f| std::cmp::Reverse(spec(f).discovered_at));
        let types: BTreeSet<&str> = newest.iter().map(|f| f.ctype.as_str()).collect();
        let tabs = std::iter::once(None).chain(types.into_iter().map(Some));
        for t in tabs {
            let tab: Vec<&&Fact> = newest
                .iter()
                .filter(|f| t.is_none_or(|t| f.ctype == t))
                .collect();
            discover.push(DiscoverExpect {
                adult,
                tab: t.map(str::to_owned),
                total: tab.len() as u64,
                newest: tab.iter().take(5).map(|f| f.hash.clone()).collect(),
            });
        }
    }

    // Swarm, from each torrent's last scrape.
    let trackers = [1u16, 2, 3];
    let rows: Vec<Option<SwarmExpectRow>> = (0..specs.len())
        .map(|i| expect_row(&final_update(clock, specs, ids, &trackers, i)))
        .collect();
    let mut swarm = SwarmExpect::default();
    for (i, s) in specs.iter().enumerate() {
        match rows[i] {
            None => swarm.never_scraped += 1,
            Some(r) => {
                swarm.populated += 1;
                swarm.zero += u64::from(r.seeders == 0);
                swarm.dht_only += u64::from(r.trackers_checked == 0);
                swarm.suspect += u64::from(r.suspect);
            }
        }
        swarm.unanswered += u64::from(s.swarm == Swarm::Unanswered);
        // A ring is kept once a scrape found anyone (§7).
        let live = |r: SwarmExpectRow| {
            r.seeders > 0
                || r.leechers > 0
                || r.completed > 0
                || r.trackers_reporting > 0
                || r.dht.is_some_and(|(a, b)| a > 0 || b > 0)
        };
        swarm.with_history += u64::from(s.history_days > 0 || rows[i].is_some_and(live));
    }
    for f in facts.iter().filter(|f| visible(f, false, false)) {
        let r = rows[f.spec];
        let bucket = |map: &mut BTreeMap<String, u64>, v: Option<u32>| match v {
            None => *map.entry("never_scraped".into()).or_default() += 1,
            Some(v) => {
                for (name, min) in [("≥1", 1), ("≥5", 5), ("≥50", 50)] {
                    if v >= min {
                        *map.entry(name.into()).or_default() += 1;
                    }
                }
            }
        };
        bucket(&mut swarm.seeders, r.map(|r| r.seeders));
        bucket(
            &mut swarm.trackers,
            r.map(|r| u32::from(r.trackers_reporting)),
        );
    }

    let mut rollups = RollupExpect::default();
    let mut hours = BTreeSet::new();
    for c in &insights.counters {
        hours.insert(c.hour);
        let sum = match c.metric {
            metric::HARVESTED => &mut rollups.harvested,
            metric::REDISCOVERED => &mut rollups.rediscovered,
            metric::DROPPED => &mut rollups.dropped,
            _ => &mut rollups.stored,
        };
        *sum += c.value;
        if c.metric == metric::STORED {
            if c.hour * HOUR >= a - DAY {
                rollups.stored_24h += c.value;
            }
            if c.hour * HOUR >= a - 7 * DAY {
                rollups.stored_7d += c.value;
            }
        }
    }
    rollups.hours = hours.len() as u64;
    rollups.watermark_hours = insights.watermarks.len() as u64;
    let searchable_since = |t: i64| -> u64 {
        let before = insights
            .watermarks
            .iter()
            .filter(|(h, _)| h * HOUR < t)
            .map(|(_, id)| *id)
            .max()
            .unwrap_or(0);
        facts.iter().filter(|f| f.id > before).count() as u64
    };
    rollups.new_searchable_24h = searchable_since(a - DAY);
    rollups.new_searchable_7d = searchable_since(a - 7 * DAY);

    let torrents = facts
        .iter()
        .filter(|f| f.spec < hand)
        .map(|f| TorrentExpect {
            key: spec(f).key.clone(),
            id: f.id,
            hash: f.hash.clone(),
            name: f.name.clone(),
            size: f.size,
            files: f.files,
            content_type: f.ctype.clone(),
            flags: f.flags,
            discovered_at: spec(f).discovered_at,
            swarm: rows[f.spec],
        })
        .collect();

    Manifest {
        format: FORMAT,
        anchor: a,
        seed: SEED,
        counts,
        queries,
        discover,
        swarm,
        cohorts: insights.cohorts.clone(),
        rollups,
        events: insights
            .events
            .iter()
            .map(|e| EventExpect {
                at: e.at,
                kind: e.kind,
                detail: e.detail.clone(),
            })
            .collect(),
        saved_searches: user.saved,
        user_items: user.items,
        collections: user.collections,
        torrents,
    }
}

// ---- JSON -------------------------------------------------------------------

/// A JSON value, written with keys in the order given.
#[derive(Debug, Clone, PartialEq, Eq)]
enum J {
    Null,
    Bool(bool),
    Int(i64),
    Str(String),
    Arr(Vec<J>),
    Obj(Vec<(String, J)>),
}

impl J {
    fn obj(entries: Vec<(&str, J)>) -> J {
        J::Obj(
            entries
                .into_iter()
                .map(|(k, v)| (k.to_owned(), v))
                .collect(),
        )
    }

    fn u(v: u64) -> J {
        J::Int(v as i64)
    }

    fn s(v: &str) -> J {
        J::Str(v.to_owned())
    }

    fn opt<T>(v: Option<T>, f: impl FnOnce(T) -> J) -> J {
        v.map_or(J::Null, f)
    }

    fn counts<K: AsRef<str>>(m: &BTreeMap<K, u64>) -> J {
        J::Obj(
            m.iter()
                .map(|(k, v)| (k.as_ref().to_owned(), J::u(*v)))
                .collect(),
        )
    }

    fn is_flat(&self) -> bool {
        match self {
            J::Arr(v) => v.iter().all(|x| !matches!(x, J::Arr(_) | J::Obj(_))),
            J::Obj(v) => v.iter().all(|(_, x)| match x {
                J::Arr(_) | J::Obj(_) => x.is_flat() && x.len() <= 8,
                _ => true,
            }),
            _ => true,
        }
    }

    fn len(&self) -> usize {
        match self {
            J::Arr(v) => v.len(),
            J::Obj(v) => v.len(),
            _ => 1,
        }
    }

    fn write(&self, out: &mut String, indent: usize) {
        let pad = |out: &mut String, n: usize| out.extend(std::iter::repeat_n(' ', n));
        match self {
            J::Null => out.push_str("null"),
            J::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
            J::Int(v) => {
                let _ = write!(out, "{v}");
            }
            J::Str(s) => {
                out.push('"');
                for c in s.chars() {
                    match c {
                        '"' => out.push_str("\\\""),
                        '\\' => out.push_str("\\\\"),
                        '\n' => out.push_str("\\n"),
                        c if (c as u32) < 0x20 => {
                            let _ = write!(out, "\\u{:04x}", c as u32);
                        }
                        c => out.push(c),
                    }
                }
                out.push('"');
            }
            J::Arr(v) if v.is_empty() => out.push_str("[]"),
            J::Obj(v) if v.is_empty() => out.push_str("{}"),
            // Small all-scalar values on one line, so per-row records
            // (cohorts, torrents) stay one line each.
            J::Arr(v) if self.is_flat() => {
                out.push('[');
                for (i, x) in v.iter().enumerate() {
                    if i > 0 {
                        out.push_str(", ");
                    }
                    x.write(out, indent);
                }
                out.push(']');
            }
            J::Obj(v) if self.is_flat() && v.len() <= 16 => {
                out.push('{');
                for (i, (k, x)) in v.iter().enumerate() {
                    if i > 0 {
                        out.push_str(", ");
                    }
                    J::Str(k.clone()).write(out, indent);
                    out.push_str(": ");
                    x.write(out, indent);
                }
                out.push('}');
            }
            J::Arr(v) => {
                out.push_str("[\n");
                for (i, x) in v.iter().enumerate() {
                    pad(out, indent + 2);
                    x.write(out, indent + 2);
                    out.push_str(if i + 1 < v.len() { ",\n" } else { "\n" });
                }
                pad(out, indent);
                out.push(']');
            }
            J::Obj(v) => {
                out.push_str("{\n");
                for (i, (k, x)) in v.iter().enumerate() {
                    pad(out, indent + 2);
                    J::Str(k.clone()).write(out, indent + 2);
                    out.push_str(": ");
                    x.write(out, indent + 2);
                    out.push_str(if i + 1 < v.len() { ",\n" } else { "\n" });
                }
                pad(out, indent);
                out.push('}');
            }
        }
    }
}

impl Manifest {
    /// `fixture.json`'s text: deterministic, keys in a fixed order.
    pub fn to_json(&self) -> String {
        let c = &self.counts;
        let counts = J::obj(vec![
            ("total", J::u(c.total)),
            ("stored", J::u(c.stored)),
            ("searchable", J::u(c.searchable)),
            ("hand", J::u(c.hand)),
            ("generated", J::u(c.generated)),
            ("imported", J::u(c.imported)),
            ("visible", J::u(c.visible)),
            ("adult", J::u(c.adult)),
            ("adult_corrected", J::u(c.adult_corrected)),
            ("filter_hidden", J::u(c.filter_hidden)),
            ("private_source", J::u(c.private_source)),
            ("private", J::u(c.private)),
            ("v2", J::u(c.v2)),
            ("hybrid", J::u(c.hybrid)),
            ("padding", J::u(c.padding)),
            ("non_utf8", J::u(c.non_utf8)),
            ("large_packs", J::u(c.large_packs)),
            ("resolved_by_files", J::u(c.resolved_by_files)),
            ("backfill", J::u(c.backfill)),
            ("discovered_24h", J::u(c.discovered_24h)),
            ("discovered_7d", J::u(c.discovered_7d)),
        ]);
        let queries = J::Arr(
            self.queries
                .iter()
                .map(|q| {
                    J::obj(vec![
                        ("q", J::s(&q.q)),
                        ("hidden", J::Bool(q.hidden)),
                        ("adult", J::Bool(q.adult)),
                        ("total", J::u(q.total)),
                        (
                            "facets",
                            J::Obj(
                                q.facets
                                    .iter()
                                    .map(|(k, v)| (k.clone(), J::counts(v)))
                                    .collect(),
                            ),
                        ),
                    ])
                })
                .collect(),
        );
        let discover = J::Arr(
            self.discover
                .iter()
                .map(|d| {
                    J::obj(vec![
                        ("adult", J::Bool(d.adult)),
                        ("type", J::opt(d.tab.as_deref(), J::s)),
                        ("total", J::u(d.total)),
                        ("newest", J::Arr(d.newest.iter().map(|h| J::s(h)).collect())),
                    ])
                })
                .collect(),
        );
        let s = &self.swarm;
        let swarm = J::obj(vec![
            ("never_scraped", J::u(s.never_scraped)),
            ("populated", J::u(s.populated)),
            ("zero", J::u(s.zero)),
            ("dht_only", J::u(s.dht_only)),
            ("suspect", J::u(s.suspect)),
            ("with_history", J::u(s.with_history)),
            ("unanswered", J::u(s.unanswered)),
            (
                "buckets",
                J::obj(vec![
                    ("seeders", J::counts(&s.seeders)),
                    ("trackers", J::counts(&s.trackers)),
                ]),
            ),
        ]);
        let cohorts = J::Arr(
            self.cohorts
                .iter()
                .map(|c| {
                    J::obj(vec![
                        ("cohort", J::Int(c.cohort)),
                        ("source", J::Int(i64::from(c.source))),
                        ("final", J::Bool(c.final_)),
                        ("harvested", J::Int(c.harvested)),
                        ("peer_found", J::Int(c.peer_found)),
                        ("fetched", J::Int(c.fetched)),
                        ("stored", J::Int(c.stored)),
                        ("searchable", J::Int(c.searchable)),
                        (
                            "losses",
                            J::Obj(
                                c.losses
                                    .iter()
                                    .map(|(k, v)| ((*k).to_owned(), J::Int(*v)))
                                    .collect(),
                            ),
                        ),
                        ("in_progress", J::Int(c.in_progress)),
                    ])
                })
                .collect(),
        );
        let r = &self.rollups;
        let rollups = J::obj(vec![
            ("harvested", J::Int(r.harvested)),
            ("rediscovered", J::Int(r.rediscovered)),
            ("dropped", J::Int(r.dropped)),
            ("stored", J::Int(r.stored)),
            ("hours", J::u(r.hours)),
            ("stored_24h", J::Int(r.stored_24h)),
            ("stored_7d", J::Int(r.stored_7d)),
            ("new_searchable_24h", J::u(r.new_searchable_24h)),
            ("new_searchable_7d", J::u(r.new_searchable_7d)),
            ("watermark_hours", J::u(r.watermark_hours)),
        ]);
        let events = J::Arr(
            self.events
                .iter()
                .map(|e| {
                    J::obj(vec![
                        ("at", J::Int(e.at)),
                        ("kind", J::Int(i64::from(e.kind))),
                        ("detail", J::s(&e.detail)),
                    ])
                })
                .collect(),
        );
        let saved = J::Arr(
            self.saved_searches
                .iter()
                .map(|s| {
                    J::obj(vec![
                        ("name", J::s(&s.name)),
                        ("query", J::s(&s.query)),
                        ("alert", J::Bool(s.alert)),
                        ("rss", J::Bool(s.rss)),
                        ("paused", J::Bool(s.paused)),
                        ("alerts", J::u(s.alerts)),
                        ("unread", J::u(s.unread)),
                    ])
                })
                .collect(),
        );
        let u = &self.user_items;
        let user_items = J::obj(vec![
            ("items", J::u(u.items)),
            ("favourite", J::u(u.favourite)),
            ("hidden", J::u(u.hidden)),
            ("labelled", J::u(u.labelled)),
            ("noted", J::u(u.noted)),
            ("labels", J::counts(&u.labels)),
        ]);
        let collections = J::Arr(
            self.collections
                .iter()
                .map(|c| {
                    J::obj(vec![
                        ("name", J::s(&c.name)),
                        ("items", J::Arr(c.items.iter().map(|h| J::s(h)).collect())),
                    ])
                })
                .collect(),
        );
        let torrents = J::Arr(
            self.torrents
                .iter()
                .map(|t| {
                    let swarm = J::opt(t.swarm, |r| {
                        J::obj(vec![
                            ("seeders", J::Int(i64::from(r.seeders))),
                            ("leechers", J::Int(i64::from(r.leechers))),
                            ("completed", J::Int(i64::from(r.completed))),
                            (
                                "trackers_reporting",
                                J::Int(i64::from(r.trackers_reporting)),
                            ),
                            ("trackers_checked", J::Int(i64::from(r.trackers_checked))),
                            ("last_scrape_at", J::Int(r.last_scrape_at)),
                            ("dht_seeds", J::opt(r.dht, |d| J::Int(i64::from(d.0)))),
                            ("dht_peers", J::opt(r.dht, |d| J::Int(i64::from(d.1)))),
                            ("suspect", J::Bool(r.suspect)),
                        ])
                    });
                    J::obj(vec![
                        ("key", J::s(&t.key)),
                        ("id", J::Int(t.id)),
                        ("hash", J::s(&t.hash)),
                        ("name", J::s(&t.name)),
                        ("size", J::u(t.size)),
                        ("files", J::u(t.files)),
                        ("content_type", J::s(&t.content_type)),
                        ("flags", J::Int(t.flags)),
                        ("discovered_at", J::Int(t.discovered_at)),
                        ("swarm", swarm),
                    ])
                })
                .collect(),
        );
        let root = J::obj(vec![
            ("format", J::Int(i64::from(self.format))),
            ("anchor", J::Int(self.anchor)),
            ("seed", J::u(self.seed)),
            ("counts", counts),
            ("queries", queries),
            ("discover", discover),
            ("swarm", swarm),
            ("cohorts", cohorts),
            ("rollups", rollups),
            ("events", events),
            ("saved_searches", saved),
            ("user_items", user_items),
            ("collections", collections),
            ("torrents", torrents),
        ]);
        let mut out = String::new();
        root.write(&mut out, 0);
        out.push('\n');
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn anchors_are_whole_hours() {
        assert_eq!(anchor(7_200), 7_200);
        assert_eq!(anchor(7_201), 7_200);
        assert_eq!(anchor(10_799), 7_200);
    }

    #[test]
    fn the_clock_leaves_six_quiet_hours_after_each_mark() {
        let a = 1_800_000_000 - 1_800_000_000 % HOUR;
        let c = Clock::new(a);
        for mark in [a - DAY, a - FINAL_AFTER, a - 7 * DAY] {
            for t in (mark..mark + 6 * HOUR).step_by(60) {
                assert!(!c.is_active(t), "{t}");
            }
            assert!(c.is_active(mark - 1) && c.is_active(mark + 6 * HOUR));
        }
        assert!(!c.is_active(a));
        assert_eq!(c.at(0), c.first_run + HOUR);
        assert_eq!(c.at(c.active_secs()), a - 1);
        let t = c.hand(DAY + HOUR);
        assert!(c.is_active(t) && t % 2 == 1, "moved out of the gap");
    }

    #[test]
    fn tokens_are_replaced_only_where_they_stand_alone() {
        assert_eq!(
            replace_token("Film.2020-RARBG.mkv", "RARBG", "X"),
            "Film.2020-X.mkv"
        );
        assert_eq!(replace_token("LOLA.LOL", "LOL", "Q"), "LOLA.Q");
        assert_eq!(replace_token("[Erai-raws] x", "Erai-raws", "E"), "[E] x");
        assert_eq!(
            rename(b"FitGirl fitgirl-bins.md5"),
            b"TidyKnit tidyknit-bins.md5"
        );
        // Legacy bytes pass through.
        assert_eq!(
            rename(&[0xd0, 0xc7, b'L', b'O', b'L']),
            vec![0xd0, 0xc7, b'L', b'O', b'L']
        );
    }

    #[test]
    fn sanitized_generator_names_hold_no_real_group() {
        let mut generator = Generator::new(SEED);
        for _ in 0..3000 {
            let info = Info::parse(&sanitize(&generator.next_torrent().info)).unwrap();
            let mut text = info.name.decode().into_owned();
            for f in &info.files {
                for p in &f.path {
                    text.push('/');
                    text.push_str(&String::from_utf8_lossy(p));
                }
            }
            if let Some(s) = &info.source {
                text.push_str(&String::from_utf8_lossy(s));
            }
            for (real, _) in RENAMES {
                assert!(
                    replace_token(&text, real, "") == text,
                    "{real} left in {text}"
                );
            }
        }
    }

    #[test]
    fn extensions_follow_the_index_rule() {
        assert_eq!(extension("a.MKV").as_deref(), Some("mkv"));
        assert_eq!(extension(".hidden"), None);
        assert_eq!(extension("noext"), None);
        assert_eq!(extension("a.tar-gz"), None);
        assert_eq!(extension("a.b.flac").as_deref(), Some("flac"));
    }

    #[test]
    fn json_is_escaped_and_ordered() {
        let mut out = String::new();
        J::obj(vec![("b", J::s("a\"\\\n\u{1}")), ("a", J::Null)]).write(&mut out, 0);
        assert_eq!(out, r#"{"b": "a\"\\\n\u0001", "a": null}"#);
    }
}
