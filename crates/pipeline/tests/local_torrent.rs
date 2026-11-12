//! Uploaded `.torrent` files through a running pipeline (UP-0, plan §5
//! and §4.3, `docs/decisions/2026-11-12-UP-0.md`): judged as a cache's
//! verified file is, stored under the built-in `upload` source with only
//! what a BEP 9 fetch would store plus the claims from outside the info
//! dict, never stored when held or on the floor, and a `private=1` file
//! kept from every DHT-facing path.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use lodestar_bencode::{OwnedValue, to_vec};
use lodestar_filter::{Core, CsList, Engine, Handle, Lists, Switches, TermSet};
use lodestar_metainfo::{Info, InfoHash};
use lodestar_pipeline::adult_enrich::{self, GateCtx, Ineligible};
use lodestar_pipeline::local_torrent::{self, Outcome};
use lodestar_pipeline::source_adult::AdultSources;
use lodestar_pipeline::torrent_cache::{Gate, Withheld};
use lodestar_pipeline::{Pipeline, PipelineConfig};
use lodestar_sources::enrich::QueryForm;
use lodestar_sources::{Consent, Kind, Manifest, Network as Net, Priority, Risk};
use lodestar_store::{SourceState, Store, StoreConfig};

const T: i64 = 1_700_000_000;
const TRACKER: &str = "udp://tracker.example:6969/announce";
const PRIVATE_TRACKER: &str = "https://private.example/announce";

fn stand_in(cs: CsList) -> Arc<Handle> {
    let lists = Lists {
        sexual: TermSet::of(&["lexa"]),
        child_roles: TermSet::of(&["rolea"]),
        codes: TermSet::of(&["codex"]),
    };
    Arc::new(Handle::new(Engine::new(Core::new(
        lists,
        cs,
        Switches::default(),
    ))))
}

fn dict(entries: Vec<(&str, OwnedValue)>) -> OwnedValue {
    OwnedValue::Dict(
        entries
            .into_iter()
            .map(|(k, v)| (k.as_bytes().to_vec(), v))
            .collect(),
    )
}

fn b(s: &str) -> OwnedValue {
    OwnedValue::Bytes(s.as_bytes().to_vec())
}

#[derive(Clone, Copy, Debug)]
enum Form {
    V1,
    V2,
    Hybrid,
}

fn info(name: &str, form: Form, private: bool) -> OwnedValue {
    let leaf = dict(vec![
        ("length", OwnedValue::Int(100)),
        ("pieces root", OwnedValue::Bytes(vec![5; 32])),
    ]);
    let tree = || dict(vec![(name, dict(vec![("", leaf.clone())]))]);
    let mut e = vec![("name", b(name)), ("piece length", OwnedValue::Int(16_384))];
    match form {
        Form::V1 => {
            e.push(("length", OwnedValue::Int(100)));
            e.push(("pieces", OwnedValue::Bytes(vec![9; 20])));
        }
        Form::V2 => {
            e.push(("file tree", tree()));
            e.push(("meta version", OwnedValue::Int(2)));
        }
        Form::Hybrid => {
            e.push(("file tree", tree()));
            e.push(("meta version", OwnedValue::Int(2)));
            e.push(("length", OwnedValue::Int(100)));
            e.push(("pieces", OwnedValue::Bytes(vec![9; 20])));
        }
    }
    if private {
        e.push(("private", OwnedValue::Int(1)));
    }
    dict(e)
}

/// A whole file: the info dict, a tracker, a comment, a creation date,
/// `created by` and a web seed.
fn file(name: &str, form: Form, private: bool) -> (Vec<u8>, InfoHash) {
    let i = info(name, form, private);
    let key = Info::parse(&to_vec(&i)).unwrap().primary_hash();
    let tracker = if private { PRIVATE_TRACKER } else { TRACKER };
    let body = to_vec(&dict(vec![
        ("info", i),
        ("announce", b(tracker)),
        (
            "announce-list",
            OwnedValue::List(vec![OwnedValue::List(vec![
                b(tracker),
                b("http://192.168.1.10/announce"),
            ])]),
        ),
        ("comment", b("Ripped with care")),
        ("created by", b("SomeMaker 2.0")),
        ("creation date", OwnedValue::Int(T - 86_400)),
        ("url-list", b("https://webseed.example/file")),
    ]));
    (body, key)
}

fn config(filter: Arc<Handle>) -> PipelineConfig {
    PipelineConfig {
        flush_every: Duration::from_millis(100),
        batch_wait: Duration::from_millis(20),
        filter,
        sealer: Some(Arc::new(lodestar_filter::Sealer::new(
            &lodestar_filter::Sealer::generate_key().unwrap(),
        ))),
        ..PipelineConfig::default()
    }
}

/// Polls for 10 s (standards §2), so the slowest runner passes.
async fn until(what: &str, mut f: impl FnMut() -> bool) {
    for _ in 0..400 {
        if f() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("timed out waiting for {what}");
}

fn stored(p: &Pipeline) -> u64 {
    p.writer_stats()
        .torrent_cache
        .stored
        .load(Ordering::Relaxed)
}

/// Uploads `bodies` to a pipeline over `store` and waits for `commits`
/// of them to be stored; the store afterwards, and each outcome.
async fn upload(
    store: Store,
    filter: Arc<Handle>,
    bodies: Vec<Vec<u8>>,
    commits: u64,
) -> (Store, Vec<Outcome>) {
    let pipeline = Pipeline::start(config(filter), store, Vec::new())
        .await
        .unwrap();
    let uploads = pipeline.uploads();
    let mut outcomes = Vec::new();
    for body in bodies {
        outcomes.push(uploads.submit(body).await.unwrap());
    }
    until("the uploads stored", || stored(&pipeline) >= commits).await;
    assert_eq!(stored(&pipeline), commits);
    let n = outcomes.len() as u64;
    assert_eq!(uploads.stats().received.load(Ordering::Relaxed), n);
    (pipeline.shutdown().await.unwrap(), outcomes)
}

fn upload_source(store: &Store) -> lodestar_store::SourceId {
    store
        .sources()
        .unwrap()
        .into_iter()
        .find(|r| r.plugin == local_torrent::SOURCE)
        .expect("the upload source")
        .id
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn v1_v2_and_hybrid_files_are_stored_with_their_claims() {
    let files: Vec<_> = [Form::V1, Form::V2, Form::Hybrid]
        .into_iter()
        .enumerate()
        .map(|(i, f)| file(&format!("Sintel {i} 2010 1080p.mkv"), f, false))
        .collect();
    let store = Store::open_in_memory(StoreConfig::default()).unwrap();
    let (store, outcomes) = upload(
        store,
        stand_in(CsList::default()),
        files.iter().map(|(b, _)| b.clone()).collect(),
        3,
    )
    .await;
    let source = upload_source(&store);
    for ((_, key), outcome) in files.iter().zip(outcomes) {
        assert_eq!(
            outcome,
            Outcome::Accepted {
                key: *key,
                private: false
            }
        );
        let id = store.find(key).unwrap().expect("stored");
        let row = store.torrent(id).unwrap().unwrap();
        assert_eq!(
            row.sources,
            1 << source.get(),
            "the upload is its only source"
        );
        // The claims from outside the info dict, as the upload's: the
        // public tracker (the LAN one dropped) and the comment.
        let claims = store.torrent_claims(id).unwrap();
        let mine = claims.iter().find(|c| c.source == source).expect("claims");
        let c = mine.claims.as_ref().unwrap();
        assert_eq!(c.trackers, [TRACKER]);
        assert_eq!(c.description.as_deref(), Some("Ripped with care"));
        assert_eq!(c.provenance.published_at, Some(T - 86_400));
        // `url-list` and `created by` are kept nowhere.
        let shown = format!("{c:?}");
        assert!(
            !shown.contains("webseed") && !shown.contains("SomeMaker"),
            "{shown}"
        );
        assert!(!mine.private_source);
    }
    no_text_anywhere(&store, &["webseed.example", "SomeMaker"]);
    // The search finds what is stored, so its misses mean something.
    assert!(anywhere(&store, "Sintel 0 2010 1080p.mkv"));
}

/// Whether any table's text or blobs hold `needle`.
fn anywhere(store: &Store, needle: &str) -> bool {
    use lodestar_store::rusqlite::types::ValueRef;
    let conn = store.connection();
    let tables: Vec<String> = conn
        .prepare("SELECT name FROM sqlite_master WHERE type = 'table'")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    for t in tables {
        let mut stmt = conn.prepare(&format!("SELECT * FROM \"{t}\"")).unwrap();
        let cols = stmt.column_count();
        let mut rows = stmt.query([]).unwrap();
        while let Some(row) = rows.next().unwrap() {
            for i in 0..cols {
                if let ValueRef::Text(b) | ValueRef::Blob(b) = row.get_ref(i).unwrap()
                    && b.windows(needle.len()).any(|w| w == needle.as_bytes())
                {
                    return true;
                }
            }
        }
    }
    false
}

/// None of `needles` is stored anywhere.
fn no_text_anywhere(store: &Store, needles: &[&str]) {
    for n in needles {
        assert!(!anywhere(store, n), "{n} is stored");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_private_file_never_reaches_a_dht_facing_path() {
    let (private, pkey) = file("Private Cut 2021 1080p.mkv", Form::Hybrid, true);
    let (public, key) = file("Public Cut 2021 1080p.mkv", Form::V1, false);
    let mut store = Store::open_in_memory(StoreConfig::default()).unwrap();
    // The cache and an enrichment plugin are on, so their gates answer
    // for privacy, not for being off.
    let cache = "torrent_cache";
    store
        .register_source_as(cache, "1", SourceState::Enabled, T)
        .unwrap();
    let enr = store
        .register_source_as(ENRICH.id, "1", SourceState::Enabled, T)
        .unwrap();
    let filter = stand_in(CsList::default());
    let (mut store, outcomes) = upload(store, filter.clone(), vec![private, public], 2).await;
    assert_eq!(
        outcomes,
        [
            Outcome::Accepted {
                key: pkey,
                private: true
            },
            Outcome::Accepted {
                key,
                private: false
            }
        ]
    );
    let pid = store.find(&pkey).unwrap().expect("stored");
    let id = store.find(&key).unwrap().expect("stored");
    let hybrid: Vec<InfoHash> = Info::parse(&to_vec(&info(
        "Private Cut 2021 1080p.mkv",
        Form::Hybrid,
        true,
    )))
    .unwrap()
    .swarm_hashes()
    .collect();
    assert_eq!(hybrid.len(), 2);

    // Scrapes (trackers and BEP 33 over the DHT): never offered.
    let new = store.unscraped_after(0, 100).unwrap();
    assert!(new.iter().any(|(i, ..)| *i == id), "the public one is");
    assert!(
        !new.iter().any(|(i, ..)| *i == pid),
        "the private one isn't"
    );
    assert!(
        !store
            .due_for_scrape(i64::MAX / 2, 100)
            .unwrap()
            .iter()
            .any(|(i, ..)| *i == pid)
    );
    // A refetch through public peer finding: never.
    assert!(
        !store
            .partial_hashes(100)
            .unwrap()
            .iter()
            .any(|h| hybrid.contains(h))
    );
    // A cache lookup, by either of the hybrid's hashes.
    let engine = filter.current();
    let gate = Gate {
        engine: &engine,
        plugin: cache,
    };
    for h in &hybrid {
        assert_eq!(gate.check(&store, *h).unwrap(), Err(Withheld::Private));
    }
    // An adult-enrichment lookup (ENR-1): refused as private, where the
    // public one goes on to the next reason.
    let flags = Arc::new(On(AtomicBool::new(true)));
    let ctx = GateCtx {
        manifest: &ENRICH,
        source: enr,
        form: QueryForm::Name,
        adult: AdultSources::default(),
        switches: &*flags,
    };
    assert_eq!(
        adult_enrich::check(&mut store, &engine, ctx, pid, T).unwrap(),
        Err(Ineligible::PrivateSource)
    );
    assert_eq!(
        adult_enrich::check(&mut store, &engine, ctx, id, T).unwrap(),
        Err(Ineligible::NotAdult)
    );
    // Its trackers and comment are kept nowhere: a private tracker's
    // address carries the uploader's passkey.
    let source = upload_source(&store);
    let claims = store.torrent_claims(pid).unwrap();
    assert!(claims.iter().filter(|c| c.source == source).all(|c| {
        c.claims
            .as_ref()
            .is_none_or(|c| c.trackers.is_empty() && c.description.is_none())
    }));
    no_text_anywhere(&store, &["private.example"]);
}

static ENRICH: Manifest = Manifest {
    id: "test-enrich",
    name: "Test enrichment",
    version: "1",
    kinds: &[Kind::Enrichment],
    networks: &[Net::Bt],
    endpoints: &[],
    risk: Risk::Low,
    consent: Consent::None,
    default_priority: Priority::Normal,
    enabled_by_default: true,
    always_adult: false,
    finds_private: false,
};

/// "Show adult content" on, the kill switch not tripped.
struct On(AtomicBool);

impl adult_enrich::Switches for On {
    fn show_adult(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
    fn kill_switch_tripped(&self) -> bool {
        false
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn floor_blocked_and_held_files_are_never_stored() {
    let (blocked, bkey) = file("Plain Film 2020.mkv", Form::V1, false);
    let (held, hkey) = file("rolea lexa.mp4", Form::V1, false);
    let mut cs = CsList::default();
    cs.add_hash(bkey.0, lodestar_filter::Origin::Imported);
    let store = Store::open_in_memory(StoreConfig::default()).unwrap();
    let (store, outcomes) = upload(store, stand_in(cs), vec![blocked, held], 0).await;
    assert_eq!(outcomes, [Outcome::Refused, Outcome::Held]);
    for k in [bkey, hkey] {
        assert!(store.find(&k).unwrap().is_none(), "never stored");
    }
    assert!(store.count().unwrap() == 0);
    no_text_anywhere(&store, &["Ripped with care", "tracker.example"]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_info_dict_is_archived_minus_pieces_only_when_the_setting_is_on() {
    for archive_raw in [false, true] {
        let (body, key) = file("Archive Test 2019.mkv", Form::V1, false);
        let cfg = StoreConfig {
            archive_raw,
            ..StoreConfig::default()
        };
        let store = Store::open_in_memory(cfg).unwrap();
        let (store, _) = upload(store, stand_in(CsList::default()), vec![body], 1).await;
        let id = store.find(&key).unwrap().unwrap();
        let raw = store.raw_info(id).unwrap();
        assert_eq!(raw.is_some(), archive_raw);
        if let Some(raw) = raw {
            // The store's own form for a fetched dict: no `pieces`, and
            // nothing from outside the info dict.
            let text = String::from_utf8_lossy(&raw);
            assert!(!text.contains("6:pieces"), "{text}");
            assert!(text.contains("Archive Test 2019.mkv"));
            assert!(!text.contains("announce") && !text.contains("comment"));
        }
        // The whole file is kept nowhere either way.
        no_text_anywhere(&store, &["webseed.example", "SomeMaker", "announce-list"]);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn malformed_files_are_refused_and_nothing_is_stored() {
    let (good, _) = file("Good 2020.mkv", Form::V1, false);
    let mut bodies: Vec<Vec<u8>> = vec![
        Vec::new(),
        b"not bencode".to_vec(),
        b"d4:infoi1ee".to_vec(),
        // Multipart sent under the right type is just a malformed file.
        b"------x\r\nContent-Disposition: form-data; name=\"f\"; filename=\"a.torrent\"\r\n\r\n"
            .to_vec(),
        good[..good.len() / 2].to_vec(),
    ];
    // The same file with its info dict cut short.
    let mut cut = good.clone();
    cut.truncate(good.len() - 3);
    bodies.push(cut);
    let store = Store::open_in_memory(StoreConfig::default()).unwrap();
    let n = bodies.len();
    let (store, outcomes) = upload(store, stand_in(CsList::default()), bodies, 0).await;
    assert_eq!(outcomes, vec![Outcome::Malformed; n]);
    assert_eq!(store.count().unwrap(), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn after_shutdown_uploads_are_closed() {
    let store = Store::open_in_memory(StoreConfig::default()).unwrap();
    let pipeline = Pipeline::start(config(stand_in(CsList::default())), store, Vec::new())
        .await
        .unwrap();
    let uploads = pipeline.uploads();
    pipeline.shutdown().await.unwrap();
    let (body, _) = file("Late 2020.mkv", Form::V1, false);
    assert_eq!(uploads.submit(body).await, Err(local_torrent::Closed));
}

proptest::proptest! {
    #![proptest_config(proptest::prelude::ProptestConfig::with_cases(2048))]

    /// Arbitrary bodies, and valid files with bytes flipped or cut: `read`
    /// never panics, and a body it takes has a key from its own dict.
    #[test]
    fn read_never_panics(
        noise in proptest::collection::vec(proptest::prelude::any::<u8>(), 0..512),
        flips in proptest::collection::vec((0usize..4096, proptest::prelude::any::<u8>()), 0..8),
        cut in 0usize..4096,
    ) {
        let _ = local_torrent::read(&noise);
        let (mut good, _) = file("Fuzz 2020.mkv", Form::Hybrid, false);
        for (i, v) in flips {
            let n = good.len();
            good[i % n] ^= v;
        }
        let n = good.len();
        if let Ok(l) = local_torrent::read(&good[..n - (cut % n)]) {
            let info = Info::parse(&l.info_raw).unwrap();
            proptest::prop_assert_eq!(info.primary_hash(), l.key);
        }
    }
}
