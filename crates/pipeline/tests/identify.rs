//! The general enrichment runtime (ENR-10, `docs/decisions/2026-11-12-ENR-10.md`):
//! what may be sent to a service such as Wikidata or TMDB, and what
//! becomes of the answer.
//!
//! - no ineligible item is ever handed to a plugin (a property test over
//!   stored items and switches, through the running runtime): private,
//!   held, hidden, filtered, Adult, unverified, partial, unconsented, off,
//!   behind a tripped kill switch;
//! - only the parsed title, year and type are sent, never the hash, the
//!   raw name or a file name;
//! - the day's cap, the cache (hit and miss), interest before the
//!   backfill, and a restart going on where it stopped;
//! - an answer is judged by the core before storage, stricter only, and a
//!   held one is never stored;
//! - the refile's judged-bytes budget in its Items phase.
//!
//! No fixed sleeps: every wait is for a condition, with a 10 s deadline.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use lodestar_bencode::{OwnedValue, to_vec};
use lodestar_classify::{ContentType, Record, ResolvedVia};
use lodestar_filter::{
    Action, Core, CsList, Engine, Handle, Lists, Switches as CoreSwitches, TermSet,
};
use lodestar_metainfo::{Info, InfoHash};
use lodestar_pipeline::enrich::{enrich_with_rule, file_infos};
use lodestar_pipeline::identify::{
    self, Config, IdentifySource, IdentifyStats, Ineligible, Interest, STRATEGY, Switches,
};
use lodestar_pipeline::source_adult::AdultSources;
use lodestar_pipeline::unverified::{self, Ctx, Phase, REFILE_JUDGED_BYTES, UnverifiedStats};
use lodestar_sources::claims::{self, AltTitle, ExternalId};
use lodestar_sources::identify::{Identification, Identifier, Lookup, MediaKind, Query};
use lodestar_sources::{
    BoxFuture, Consent, Discovery, Emitter, Fetch, Found, HostLimiter, HttpError, Kind, Kit,
    Manifest, MemoryState, Network, PluginError, PluginState as _, Policy, Priority, Request,
    Response, Risk, Shutdown, SourceCtx, SourcePlugin,
};
use lodestar_store::unverified::{Network as Net, NewUnverified};
use lodestar_store::{
    ClassCorrection, NewTorrent, SourceId, SourceState, Store, StoreConfig, flags,
};
use proptest::prelude::*;
use tokio::sync::mpsc;

const T: i64 = 1_700_000_000;
const DEADLINE: Duration = Duration::from_secs(10);

static MANIFEST: Manifest = Manifest {
    id: "test-identify",
    name: "Test identification",
    version: "1",
    kinds: &[Kind::Enrichment],
    networks: &[Network::Bt],
    endpoints: &[],
    risk: Risk::Low,
    consent: Consent::Account,
    default_priority: Priority::Normal,
    enabled_by_default: false,
    always_adult: false,
    finds_private: false,
};

/// An info dict named `name` holding one 900 MB video file.
fn info_dict(name: &str) -> Vec<u8> {
    let mut f = BTreeMap::new();
    f.insert(b"length".to_vec(), OwnedValue::Int(900 << 20));
    let file = format!("{}.mkv", name.replace(' ', "."));
    f.insert(
        b"path".to_vec(),
        OwnedValue::List(vec![OwnedValue::from(file.as_str())]),
    );
    let mut d = BTreeMap::new();
    d.insert(
        b"files".to_vec(),
        OwnedValue::List(vec![OwnedValue::Dict(f)]),
    );
    d.insert(b"name".to_vec(), OwnedValue::from(name));
    d.insert(b"piece length".to_vec(), OwnedValue::Int(1 << 18));
    d.insert(b"pieces".to_vec(), OwnedValue::Bytes(vec![0; 20 * 3600]));
    to_vec(&OwnedValue::Dict(d))
}

/// Stores a verified torrent named `name`, classified Adult when `adult`
/// (as an Is adult rule makes it), with `extra` flags.
fn put(
    store: &mut Store,
    name: &str,
    source: SourceId,
    extra: i64,
    adult: bool,
) -> (i64, InfoHash) {
    let raw = info_dict(name);
    let info = Info::parse(&raw).unwrap();
    let id = store
        .insert(&[NewTorrent {
            info: &info,
            raw_info: None,
            discovered_at: T,
            metadata_at: T,
            extra_flags: extra,
            source,
        }])
        .unwrap()[0]
        .id();
    let files = file_infos(&info.files, info.path_encoding);
    let e = enrich_with_rule(name, &files, adult.then_some(true));
    store.set_enrichments(&[(id, e)]).unwrap();
    (id, info.swarm_hashes().next().unwrap())
}

/// A movie's name the parser reads a title and year from.
fn movie(i: usize, word: &str) -> String {
    format!("Item{i} {word} Film 2010 1080p BluRay x264")
}

/// A store with the plugin's source registered, enabled and consented to
/// (its Account credential stored), and a DHT source.
fn store() -> (Store, SourceId, SourceId) {
    let mut s = Store::open_in_memory(StoreConfig::default()).unwrap();
    let dht = s.register_source("dht", "1", 0).unwrap();
    let src = s.register_source(MANIFEST.id, "1", 0).unwrap();
    s.update_source_secrets(
        src,
        &[("api_key", b"sealed")],
        &[],
        Some(SourceState::Enabled),
    )
    .unwrap();
    (s, dht, src)
}

/// The core with stand-in term lists: `lexa` is sexual, `rolea` a child
/// role, so "rolea lexa" is a CS pattern hit.
fn stand_in() -> Arc<Handle> {
    let lists = Lists {
        sexual: TermSet::of(&["lexa"]),
        child_roles: TermSet::of(&["rolea"]),
        codes: TermSet::of(&["codex"]),
    };
    Arc::new(Handle::new(Engine::new(Core::new(
        lists,
        CsList::default(),
        CoreSwitches::default(),
    ))))
}

fn rules(filter: &Handle, rows: &[(&str, Action)]) {
    let rows = rows
        .iter()
        .enumerate()
        .map(|(n, (q, a))| lodestar_store::RuleRow {
            id: n as i64 + 1,
            query: (*q).into(),
            action: a.code(),
            enabled: true,
            shadow_until: None,
            unless_query: None,
        })
        .collect();
    filter.update_engine(|e| {
        let notes = e.load_rules(&lodestar_store::RuleSet {
            rules: rows,
            ..Default::default()
        });
        assert!(notes.is_empty(), "{notes:?}");
        e.version += 1;
    });
}

#[derive(Default)]
struct Flags {
    tripped: AtomicBool,
}

impl Switches for Flags {
    fn kill_switch_tripped(&self) -> bool {
        self.tripped.load(Ordering::SeqCst)
    }
}

/// A plugin that records every query, sends each as one request through
/// its kit, and answers with `answer`.
struct Recorder {
    queries: Mutex<Vec<Query>>,
    answer: Mutex<Option<Identification>>,
    kinds: Vec<MediaKind>,
    fail: AtomicBool,
}

impl Recorder {
    fn new(answer: Option<Identification>) -> Arc<Recorder> {
        Arc::new(Recorder {
            queries: Mutex::default(),
            answer: Mutex::new(answer),
            kinds: MediaKind::ALL.to_vec(),
            fail: AtomicBool::new(false),
        })
    }

    fn titles(&self) -> Vec<String> {
        self.queries
            .lock()
            .unwrap()
            .iter()
            .map(|q| q.title().to_owned())
            .collect()
    }
}

impl Identifier for Recorder {
    fn manifest(&self) -> &Manifest {
        &MANIFEST
    }

    fn kinds(&self) -> &[MediaKind] {
        &self.kinds
    }

    fn min_interval(&self) -> Duration {
        Duration::ZERO
    }

    fn lookup<'a>(
        &'a self,
        kit: &'a Kit,
        query: &'a Query,
    ) -> BoxFuture<'a, Result<Lookup, PluginError>> {
        Box::pin(async move {
            self.queries.lock().unwrap().push(query.clone());
            let url = format!(
                "https://api.example.test/search?q={}&y={}",
                query.title().replace(' ', "+"),
                query.year().unwrap_or(0)
            );
            let _ = kit.http.send(&Request::get(&url)).await;
            if self.fail.load(Ordering::SeqCst) {
                return Err(PluginError("down".into()));
            }
            Ok(match &*self.answer.lock().unwrap() {
                Some(a) => Lookup::Match(a.clone()),
                None => Lookup::NoMatch,
            })
        })
    }
}

/// A [`Fetch`] that records every request and answers 200.
#[derive(Default)]
struct Wire(Mutex<Vec<Request>>);

impl Fetch for Wire {
    fn fetch<'a>(&'a self, req: &'a Request) -> BoxFuture<'a, Result<Response, HttpError>> {
        self.0.lock().unwrap().push(req.clone());
        Box::pin(async {
            Ok(Response {
                status: 200,
                headers: Vec::new(),
                body: b"{}".to_vec(),
            })
        })
    }
}

/// The runtime running as a source, with its context's parts.
struct Running {
    task: tokio::task::JoinHandle<Result<(), PluginError>>,
    stop: tokio::sync::watch::Sender<bool>,
    found: mpsc::Receiver<Discovery>,
    interest: Interest,
    stats: Arc<IdentifyStats>,
    wire: Arc<Wire>,
}

impl Running {
    async fn stop(self) -> mpsc::Receiver<Discovery> {
        let _ = self.stop.send(true);
        let _ = self.task.await;
        self.found
    }
}

#[allow(clippy::too_many_arguments)]
fn run(
    plugin: Arc<Recorder>,
    store: Arc<Mutex<Store>>,
    filter: Arc<Handle>,
    adult: AdultSources,
    switches: Arc<Flags>,
    source: SourceId,
    config: Config,
    state: Arc<MemoryState>,
    opened: &[i64],
) -> Running {
    let (src, interest) =
        IdentifySource::new(plugin, store, filter, adult, switches, config).unwrap();
    // Opened before the runtime starts, so they're first in line.
    for &id in opened {
        assert!(interest.opened(id));
    }
    let stats = src.stats();
    let (tx, found) = mpsc::channel(256);
    let (stop, shutdown) = Shutdown::new();
    let wire = Arc::new(Wire::default());
    let kit = Kit::with_limiter(
        state,
        wire.clone(),
        // Fast enough for tests; the runtime's own pacing is what's tested.
        HostLimiter::new(fast().limits(&Default::default())),
    );
    let ctx = SourceCtx {
        source,
        emitter: Emitter::new(source, tx),
        peer_requests: None,
        peer_replies: None,
        shutdown,
        kit,
    };
    let task = tokio::spawn(async move { src.run(ctx).await });
    Running {
        task,
        stop,
        found,
        interest,
        stats,
        wire,
    }
}

fn fast() -> Policy {
    Policy {
        caps: lodestar_sources::Caps {
            per_host: lodestar_sources::HostRate::per_second(1000),
            ..lodestar_sources::Caps::STRICT
        },
        ..Policy::STRICT
    }
}

/// Fast pacing, no background: only what the test opens is looked up.
fn interest_only() -> Config {
    Config {
        background: false,
        idle: Duration::from_millis(10),
        ..Config::default()
    }
}

fn background() -> Config {
    Config {
        idle: Duration::from_millis(10),
        ..Config::default()
    }
}

/// Waits for `cond`, failing with `what` after the deadline.
async fn until(what: &str, mut cond: impl FnMut() -> bool) {
    let deadline = tokio::time::Instant::now() + DEADLINE;
    while !cond() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for {what}"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

fn n(c: &AtomicU64) -> u64 {
    c.load(Ordering::SeqCst)
}

fn answer(canonical: &str) -> Identification {
    Identification {
        ids: vec![
            ExternalId {
                scheme: "imdb".into(),
                value: "tt0111161".into(),
            },
            ExternalId {
                scheme: "wikidata".into(),
                value: "Q172241".into(),
            },
        ],
        canonical_title: Some(canonical.into()),
        alt_titles: vec![AltTitle {
            language: "ja".into(),
            title: "某映画".into(),
        }],
        genres: vec!["Drama".into()],
        year: Some(1994),
        confidence: 92,
    }
}

/// The facts one generated item is stored with.
#[derive(Debug, Clone)]
struct Spec {
    adult: bool,
    /// Found by an always-adult source.
    masked: bool,
    hidden: bool,
    private: bool,
    bep27: bool,
    floor: bool,
    not_adult: bool,
    unverified: bool,
    partial: bool,
    /// Your rule matches it now: hide, drop or quarantine.
    rule: Option<Action>,
}

fn spec() -> impl Strategy<Value = Spec> {
    let rule = prop_oneof![
        3 => Just(None),
        1 => Just(Some(Action::Hide)),
        1 => Just(Some(Action::Drop)),
        1 => Just(Some(Action::Quarantine)),
    ];
    (
        proptest::collection::vec(proptest::bool::weighted(0.2), 10),
        rule,
    )
        .prop_map(|(b, rule)| Spec {
            adult: b[0],
            masked: b[1],
            hidden: b[2],
            private: b[3],
            bep27: b[4],
            floor: b[5],
            not_adult: b[6],
            unverified: b[7],
            partial: b[8],
            rule,
        })
}

fn rule_word(a: Option<Action>) -> &'static str {
    match a {
        None => "Plain",
        Some(Action::Hide) => "Hidex",
        Some(Action::Drop) => "Dropx",
        Some(_) => "Quarx",
    }
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 32, ..ProptestConfig::default() })]

    /// Whatever is stored and whatever the switches say, the plugin is
    /// handed exactly the eligible items, through the running runtime.
    #[test]
    fn no_ineligible_item_is_ever_handed_to_a_plugin(
        specs in proptest::collection::vec(spec(), 1..7),
        tripped in proptest::bool::weighted(0.2),
        enabled in proptest::bool::weighted(0.8),
        consented in proptest::bool::weighted(0.8),
    ) {
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        rt.block_on(async move {
            let (mut s, dht, src) = store();
            let sukebei = s.register_source("sukebei", "1", 0).unwrap();
            let mask = AdultSources(1 << sukebei.get());
            if !consented {
                s.update_source_secrets(src, &[], &["api_key"], None).unwrap();
            }
            if !enabled {
                s.set_source_state(src, SourceState::Disabled).unwrap();
            }
            let filter = stand_in();
            let mut ids = Vec::new();
            let mut want = Vec::new();
            for (i, sp) in specs.iter().enumerate() {
                let name = movie(i, rule_word(sp.rule));
                if sp.unverified {
                    let key = [i as u8 + 100; 20];
                    let c = lodestar_store::claims::Claims {
                        name: Some(name.clone()),
                        ..Default::default()
                    };
                    let put = s.put_unverified(&[NewUnverified {
                        network: Net::Bt, key: &key, source: dht, private_source: false,
                        claims: Some(&c), at: T,
                    }]).unwrap();
                    let lodestar_store::unverified::Put::New(id) = put[0] else { panic!("new") };
                    assert!(s.torrent(id).unwrap().is_none());
                    ids.push(id);
                    continue;
                }
                let extra = if sp.hidden { flags::HIDDEN } else { 0 }
                    | if sp.private { flags::PRIVATE_SOURCE } else { 0 }
                    | if sp.bep27 {
                        if i % 2 == 0 { flags::PRIVATE } else { flags::REMOTE_UNVERIFIED }
                    } else {
                        0
                    }
                    | if sp.partial { flags::FILES_PARTIAL | flags::IMPORTED } else { 0 };
                let source = if sp.masked { sukebei } else { dht };
                let (id, key) = put(&mut s, &name, source, extra, sp.adult);
                if sp.not_adult {
                    s.set_correction(id, ClassCorrection { adult: Some(false), not_junk: false })
                        .unwrap();
                }
                if sp.floor {
                    filter.update(|core| core.cs.add_hash(key.0, lodestar_filter::Origin::Imported));
                }
                ids.push(id);
                // Not adult undoes an always-adult source, never your Is
                // adult classification of the torrent itself.
                let adult = sp.adult || (sp.masked && !sp.not_adult);
                let ok = !tripped && enabled && consented && !adult
                    && !sp.hidden && !sp.private && !sp.bep27 && !sp.partial
                    && !sp.floor && sp.rule.is_none();
                if ok {
                    want.push(format!("Item{i} Plain Film"));
                }
            }
            rules(&filter, &[
                ("hidex", Action::Hide),
                ("dropx", Action::Drop),
                ("quarx", Action::Quarantine),
            ]);
            let flags = Flags::default();
            flags.tripped.store(tripped, Ordering::SeqCst);
            let plugin = Recorder::new(None);
            let r = run(plugin.clone(), Arc::new(Mutex::new(s)), filter, mask,
                Arc::new(flags), src, interest_only(), Arc::new(MemoryState::new()), &ids);
            let n = ids.len() as u64;
            until("every item", || r.stats.done.load(Ordering::SeqCst) >= n).await;
            r.stop().await;
            let mut got = plugin.titles();
            got.sort();
            want.sort();
            assert_eq!(got, want);
        });
    }
}

/// The gate's reasons, one condition at a time, from the store: private,
/// held, Adult, consent not given.
#[test]
fn each_refusal_has_its_reason() {
    let (mut s, dht, src) = store();
    let filter = stand_in();
    let engine = filter.current();
    let flags = Flags::default();
    let kinds = MediaKind::ALL;
    let ctx = identify::GateCtx {
        manifest: &MANIFEST,
        source: src,
        kinds: &kinds,
        adult: AdultSources::default(),
        switches: &flags,
    };
    let (plain, _) = put(&mut s, &movie(0, "Plain"), dht, 0, false);
    let (private, _) = put(
        &mut s,
        &movie(1, "Plain"),
        dht,
        flags::PRIVATE_SOURCE,
        false,
    );
    let (adult, _) = put(&mut s, &movie(2, "Plain"), dht, 0, true);
    let (held, h) = put(&mut s, &movie(3, "Plain"), dht, 0, false);
    filter.update(|core| core.cs.add_hash(h.0, lodestar_filter::Origin::Imported));
    let engine2 = filter.current();
    let (unnamed, _) = put(&mut s, "New Folder", dht, 0, false);
    let why = |s: &mut Store, e: &Engine, id| identify::check(s, e, ctx, id, T).unwrap().err();
    let c = identify::check(&mut s, &engine, ctx, plain, T)
        .unwrap()
        .unwrap();
    assert_eq!(
        (c.query.title(), c.query.year(), c.query.kind()),
        ("Item0 Plain Film", Some(2010), MediaKind::Movie)
    );
    assert_eq!(
        why(&mut s, &engine, private),
        Some(Ineligible::PrivateSource)
    );
    assert_eq!(why(&mut s, &engine, adult), Some(Ineligible::Adult));
    assert_eq!(
        why(&mut s, &engine, held),
        None,
        "the old engine didn't hold it"
    );
    assert_eq!(why(&mut s, &engine2, held), Some(Ineligible::Held));
    assert_eq!(why(&mut s, &engine2, unnamed), Some(Ineligible::NoQuery));
    // A kind the plugin doesn't serve sends nothing.
    let music_only = identify::GateCtx {
        kinds: &[MediaKind::Music],
        ..ctx
    };
    assert_eq!(
        identify::check(&mut s, &engine2, music_only, plain, T)
            .unwrap()
            .err(),
        Some(Ineligible::NoQuery)
    );
    // An Is adult rule now, though stored as a film.
    filter.update_engine(|e| e.load_class_rules(&[(1, "plain".into(), true)]));
    assert_eq!(
        why(&mut s, &filter.current(), plain),
        Some(Ineligible::Adult)
    );
    // Consent withdrawn: plugin-wide, before anything about the item.
    s.update_source_secrets(src, &[], &["api_key"], None)
        .unwrap();
    assert_eq!(why(&mut s, &engine, plain), Some(Ineligible::NoConsent));
    flags.tripped.store(true, Ordering::SeqCst);
    assert_eq!(why(&mut s, &engine, plain), Some(Ineligible::KillSwitch));
}

/// Only the parsed title and year leave: never the hash (hex or raw), the
/// raw name or a file name, in the query or on the wire.
#[tokio::test]
async fn only_the_parsed_title_and_year_are_sent() {
    let (mut s, dht, src) = store();
    let name = movie(0, "Plain");
    let (id, key) = put(&mut s, &name, dht, 0, false);
    let plugin = Recorder::new(None);
    let r = run(
        plugin.clone(),
        Arc::new(Mutex::new(s)),
        stand_in(),
        AdultSources::default(),
        Arc::default(),
        src,
        interest_only(),
        Arc::new(MemoryState::new()),
        &[id],
    );
    until("the lookup", || n(&r.stats.done) >= 1).await;
    let wire = r.wire.clone();
    r.stop().await;
    assert_eq!(plugin.titles(), vec!["Item0 Plain Film"]);
    let hex: String = key.0.iter().map(|b| format!("{b:02x}")).collect();
    let sent = wire.0.lock().unwrap().clone();
    assert_eq!(sent.len(), 1);
    for req in sent {
        let text = format!("{req:?}");
        for bad in [
            hex.as_str(),
            &hex.to_uppercase(),
            "1080p",
            "BluRay",
            "x264",
            ".mkv",
        ] {
            assert!(!text.contains(bad), "{bad} in {text}");
        }
        assert!(
            req.url.contains("Item0+Plain+Film") && req.url.contains("y=2010"),
            "{}",
            req.url
        );
    }
}

/// A match is emitted under the key as ids and a details section that
/// holds the confidence, cached as a hit; a miss is cached too, so neither
/// is asked again while fresh.
#[tokio::test]
async fn hits_and_misses_are_cached() {
    let (mut s, dht, src) = store();
    let (hit, key) = put(&mut s, &movie(0, "Plain"), dht, 0, false);
    let (miss, _) = put(&mut s, &movie(1, "Plain"), dht, 0, false);
    let store = Arc::new(Mutex::new(s));
    let state = Arc::new(MemoryState::new());
    let plugin = Recorder::new(Some(answer("The Film")));
    let mut r = run(
        plugin.clone(),
        store.clone(),
        stand_in(),
        AdultSources::default(),
        Arc::default(),
        src,
        interest_only(),
        state.clone(),
        &[hit],
    );
    let d = tokio::time::timeout(DEADLINE, r.found.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        (d.source, d.found.key, d.found.strategy),
        (src, key, STRATEGY)
    );
    assert!(!d.found.private_source);
    let c = d.found.claims.as_deref().unwrap();
    let det = c.details.as_deref().unwrap();
    assert_eq!(det.match_confidence, Some(92));
    assert_eq!(det.canonical_title.as_deref(), Some("The Film"));
    assert_eq!(det.genres, vec!["drama"]);
    assert_eq!(c.ids.len(), 2);
    assert!(c.name.is_none() && c.files.is_empty());
    *plugin.answer.lock().unwrap() = None;
    assert!(r.interest.opened(miss));
    until("the miss", || n(&r.stats.missed) == 1).await;
    // Both again: both cached, nothing sent.
    assert!(r.interest.opened(hit));
    assert!(r.interest.opened(miss));
    until("the cache", || n(&r.stats.cached) == 2).await;
    r.stop().await;
    assert_eq!(plugin.titles().len(), 2);
    // A failed lookup isn't cached: it's asked again.
    let plugin = Recorder::new(None);
    plugin.fail.store(true, Ordering::SeqCst);
    let (fresh, _) = put(
        &mut store.lock().unwrap(),
        &movie(2, "Plain"),
        dht,
        0,
        false,
    );
    let r = run(
        plugin.clone(),
        store,
        stand_in(),
        AdultSources::default(),
        Arc::default(),
        src,
        interest_only(),
        state,
        &[fresh, fresh],
    );
    until("two failures", || n(&r.stats.failed) == 2).await;
    r.stop().await;
}

/// Opened items go first; the background then walks the store, all
/// within one day's cap, and nothing goes once it's spent, an opened item
/// included.
#[tokio::test]
async fn interest_comes_before_the_capped_backfill() {
    let (mut s, dht, src) = store();
    let ids: Vec<i64> = (0..6)
        .map(|i| put(&mut s, &movie(i, "Plain"), dht, 0, false).0)
        .collect();
    let plugin = Recorder::new(None);
    let config = Config {
        per_day: 5,
        ..background()
    };
    let r = run(
        plugin.clone(),
        Arc::new(Mutex::new(s)),
        stand_in(),
        AdultSources::default(),
        Arc::default(),
        src,
        config,
        Arc::new(MemoryState::new()),
        &[ids[5], ids[4]],
    );
    // Two opened, three background, then the fourth is spent.
    until("the cap", || {
        n(&r.stats.looked_up) == 5 && n(&r.stats.done) >= 6
    })
    .await;
    // Spent: an opened item waits too, however long the runtime runs.
    assert!(r.interest.opened(ids[3]));
    let spent = n(&r.stats.done);
    until("more turns", || n(&r.stats.done) >= spent + 3).await;
    assert_eq!(n(&r.stats.looked_up), 5);
    r.stop().await;
    let got = plugin.titles();
    assert_eq!(
        got,
        [
            "Item5 Plain Film",
            "Item4 Plain Film",
            "Item0 Plain Film",
            "Item1 Plain Film",
            "Item2 Plain Film"
        ],
        "interest first"
    );
}

/// A restart goes on where the background stopped, with the day's count
/// it had.
#[tokio::test]
async fn a_restart_resumes_the_backfill() {
    let (mut s, dht, src) = store();
    for i in 0..4 {
        put(&mut s, &movie(i, "Plain"), dht, 0, false);
    }
    let store = Arc::new(Mutex::new(s));
    let state = Arc::new(MemoryState::new());
    let first = Recorder::new(None);
    let r = run(
        first.clone(),
        store.clone(),
        stand_in(),
        AdultSources::default(),
        Arc::default(),
        src,
        Config {
            per_day: 2,
            ..background()
        },
        state.clone(),
        &[],
    );
    // Two looked up, then the third is spent: both saved by then.
    until("the day's two", || {
        n(&r.stats.looked_up) == 2 && n(&r.stats.done) >= 3
    })
    .await;
    r.stop().await;
    assert_eq!(first.titles(), ["Item0 Plain Film", "Item1 Plain Film"]);
    assert!(state.get(identify::CURSOR_KEY).await.unwrap().is_some());
    // Restarted with a cap of 3 the same day: one more, after the cursor.
    let again = Recorder::new(None);
    let r = run(
        again.clone(),
        store,
        stand_in(),
        AdultSources::default(),
        Arc::default(),
        src,
        Config {
            per_day: 3,
            ..background()
        },
        state,
        &[],
    );
    until("one more, then spent", || {
        n(&r.stats.looked_up) == 1 && n(&r.stats.done) >= 2
    })
    .await;
    r.stop().await;
    assert_eq!(
        again.titles(),
        ["Item2 Plain Film"],
        "the count and cursor survived"
    );
}

/// Runs `found` through the writer's ingest, with `enrichers` the
/// registered enrichment sources (none listed as adult enrichers).
fn ingest(
    store: &mut Store,
    filter: &Handle,
    found: &[Discovery],
    enrichers: u64,
) -> UnverifiedStats {
    ingest_out(store, filter, found, enrichers).0
}

fn ingest_out(
    store: &mut Store,
    filter: &Handle,
    found: &[Discovery],
    enrichers: u64,
) -> (UnverifiedStats, unverified::Ingested) {
    let stats = UnverifiedStats::default();
    let ctx = Ctx {
        quarantine_secs: 86_400,
        enrichers: lodestar_pipeline::adult_enrich::EnrichSources {
            listed: 0,
            kind: enrichers,
        },
        ..Ctx::default()
    };
    let out =
        unverified::ingest(store, &filter.current(), ctx, found, &stats, T, &|_, _| {}).unwrap();
    (stats, out)
}

fn discovery(key: InfoHash, source: SourceId, a: Identification) -> Discovery {
    let mut found = Found::key(key, STRATEGY).with_claims(a.into_claims());
    found.clamp();
    Discovery {
        source,
        found,
        at: T,
        reply: false,
    }
}

/// An answer is judged before storage with the torrent's verified name:
/// a clean one is stored as the plugin's v3 claims; a child-safety hit in
/// any field, alone or with the name, holds the hash and stores nothing;
/// your rule on it drops the plugin's claims only.
#[test]
fn answers_are_judged_before_storage() {
    let (mut s, dht, src) = store();
    let filter = stand_in();
    let bit = 1u64 << src.get();
    let (clean, ck) = put(&mut s, &movie(0, "Plain"), dht, 0, false);
    let st = ingest(
        &mut s,
        &filter,
        &[discovery(ck, src, answer("The Film"))],
        bit,
    );
    assert_eq!(n(&st.held), 0);
    let row = s.torrent_claims(clean).unwrap();
    let c = row
        .iter()
        .find(|c| c.source == src)
        .and_then(|c| c.claims.clone())
        .unwrap();
    assert_eq!(
        c.details.as_deref().and_then(|d| d.match_confidence),
        Some(92)
    );
    assert_eq!(c.ids.len(), 2);

    // A core pattern split between the canonical title and the genre.
    let (_, hk) = put(&mut s, &movie(1, "Plain"), dht, 0, false);
    let bad = Identification {
        genres: vec!["Lexa".into()],
        ..answer("A Rolea Story")
    };
    let st = ingest(&mut s, &filter, &[discovery(hk, src, bad)], bit);
    assert_eq!(n(&st.held), 1);
    assert!(s.held_among(&[hk.0]).unwrap()[0], "held");
    assert!(
        s.find(&hk).unwrap().is_none(),
        "the torrent is gone with it"
    );

    // Completed only by the verified name: still held.
    let (_, nk) = put(&mut s, "Lexa Film 2010 1080p BluRay x264", dht, 0, false);
    let st = ingest(&mut s, &filter, &[discovery(nk, src, answer("Rolea"))], bit);
    assert_eq!(n(&st.held), 1);
    assert!(s.held_among(&[nk.0]).unwrap()[0]);

    // Your rule drops the plugin's claims, holds nothing (stricter only:
    // the torrent's own metadata is judged on its own).
    rules(&filter, &[("badword", Action::Drop)]);
    let (rid, rk) = put(&mut s, &movie(3, "Plain"), dht, 0, false);
    let st = ingest(
        &mut s,
        &filter,
        &[discovery(rk, src, answer("Badword"))],
        bit,
    );
    assert_eq!((n(&st.held), n(&st.dropped)), (0, 1));
    assert!(s.torrent(rid).unwrap().is_some());
    assert!(
        s.torrent_claims(rid)
            .unwrap()
            .iter()
            .all(|c| c.source != src)
    );
}

/// The effective title of an ambiguous torrent resolves via the plugin's
/// stored answer; an informative name keeps its own.
#[test]
fn an_identification_resolves_an_ambiguous_title() {
    let (mut s, dht, src) = store();
    let filter = stand_in();
    let bit = 1u64 << src.get();
    let (id, key) = put(&mut s, "New Folder", dht, 0, false);
    ingest(
        &mut s,
        &filter,
        &[discovery(key, src, answer("The Film"))],
        bit,
    );
    let row = s.torrent(id).unwrap().unwrap();
    let tree = s.files(id).unwrap().unwrap();
    let files = file_infos(&tree.files, tree.encoding);
    let class = lodestar_classify::classify(&row.name, &files);
    let before = s.enrichment(id).unwrap().unwrap();
    let after = identify::with_identified(&s, &row, &class, before.clone()).unwrap();
    let rec = Record::decode(&after.data).unwrap();
    assert_eq!(rec.release("title"), Some("The Film"));
    assert_eq!(rec.release("year"), Some("1994"));
    assert_eq!(rec.resolved, Some(ResolvedVia::Plugin));
    assert_eq!(
        (after.content_type, after.junk_score),
        (before.content_type, before.junk_score)
    );
    // Not from a source that found the torrent: a feed's canonical title
    // names nothing.
    let finder = lodestar_store::TorrentRow {
        sources: row.sources | bit,
        ..row.clone()
    };
    assert_eq!(
        identify::with_identified(&s, &finder, &class, before.clone()).unwrap(),
        before
    );
    // An informative name keeps its own title.
    let (fid, fkey) = put(&mut s, &movie(0, "Plain"), dht, 0, false);
    ingest(
        &mut s,
        &filter,
        &[discovery(fkey, src, answer("The Film"))],
        bit,
    );
    let row = s.torrent(fid).unwrap().unwrap();
    let class = lodestar_classify::classify(&row.name, &[]);
    let before = s.enrichment(fid).unwrap().unwrap();
    assert_eq!(
        identify::with_identified(&s, &row, &class, before.clone()).unwrap(),
        before
    );
    assert_eq!(
        ContentType::from_code(before.content_type),
        Some(ContentType::Movie)
    );
}

/// The refile's judged-bytes budget holds in its Items phase too:
/// unverified items with full sections and long claimed names end steps
/// early, every step moves on, and the sweep reaches every item
/// (`docs/decisions/2026-11-07-runner-followups.md`).
#[test]
fn an_items_refile_step_stops_at_its_judging_budget() {
    let (mut s, _, feed) = store();
    let section = claims::Details {
        alt_titles: (0..claims::MAX_ALT_TITLES)
            .map(|i| AltTitle {
                language: format!(
                    "x{}{}",
                    char::from(b'a' + (i % 26) as u8),
                    "x".repeat(i / 26)
                ),
                title: format!("title {i}"),
            })
            .collect(),
        genres: (0..claims::MAX_GENRES)
            .map(|i| format!("genre {i}"))
            .collect(),
        canonical_title: Some("Plain Show".into()),
        match_confidence: Some(90),
        ..claims::Details::default()
    };
    let long = |i: usize| format!("{i:03} {}", "word ".repeat(200));
    let mut one = claims::Claims {
        name: Some(long(0)),
        details: Some(Box::new(section.clone())),
        ..claims::Claims::default()
    };
    one.clamp();
    let cost = {
        let d = Discovery {
            source: feed,
            found: Found::key(InfoHash([1; 20]), 0).with_claims(one),
            at: T,
            reply: false,
        };
        unverified::judged_len(&unverified::store_claims(&d).unwrap(), None)
    };
    let rows = 2 * REFILE_JUDGED_BYTES / cost + 2;
    assert!(rows < 1000, "each row costs {cost}");
    let filter = stand_in();
    let batch: Vec<Discovery> = (0..rows)
        .map(|i| {
            let mut c = claims::Claims {
                name: Some(long(i)),
                details: Some(Box::new(section.clone())),
                ..claims::Claims::default()
            };
            c.clamp();
            let mut k = [0u8; 20];
            k[..8].copy_from_slice(&(i as u64 + 1).to_be_bytes());
            Discovery {
                source: feed,
                found: Found::key(InfoHash(k), 0).with_claims(c),
                at: T,
                reply: false,
            }
        })
        .collect();
    let st = ingest(&mut s, &filter, &batch, 0);
    assert_eq!(n(&st.stored), rows as u64);
    let ctx = Ctx {
        quarantine_secs: 86_400,
        ..Ctx::default()
    };
    let version = filter.current().version;
    let (mut after, mut steps) = (0, 0);
    loop {
        let st = unverified::refile_step(
            &mut s,
            &filter,
            version,
            Phase::Items,
            after,
            (0, 0),
            ctx,
            T,
            &|_, _| {},
        )
        .unwrap();
        assert!(!st.rejected);
        if st.end {
            break;
        }
        assert_ne!(st.after_item, after, "every step goes on");
        after = st.after_item;
        steps += 1;
    }
    assert!(steps >= 3, "{rows} rows of {cost} bytes took {steps} steps");
    // A rule now reaches every item.
    rules(&filter, &[("plain", Action::Drop)]);
    let version = filter.current().version;
    let (mut after, mut removed) = (0, 0);
    loop {
        let st = unverified::refile_step(
            &mut s,
            &filter,
            version,
            Phase::Items,
            after,
            (0, 0),
            ctx,
            T,
            &|_, _| {},
        )
        .unwrap();
        removed += st.removed;
        if st.end {
            break;
        }
        after = st.after_item;
    }
    assert_eq!(removed, rows);
}

/// An identification is an answer about a stored torrent, never a find:
/// no source bit, no rediscovery, no unverified item, and PRIVACY is never
/// cleared (runner, ENR-10).
#[test]
fn an_identification_is_never_a_find() {
    let (mut s, dht, src) = store();
    let filter = stand_in();
    let bit = 1u64 << src.get();
    let (id, key) = put(&mut s, &movie(0, "Plain"), dht, 0, false);
    let before = s.torrent(id).unwrap().unwrap();
    let (st, out) = ingest_out(
        &mut s,
        &filter,
        &[discovery(key, src, answer("The Film"))],
        bit,
    );
    assert_eq!(n(&st.on_torrent), 1);
    assert!(out.touched.is_empty(), "not a rediscovery");
    assert_eq!(out.identified, vec![id]);
    let after = s.torrent(id).unwrap().unwrap();
    assert_eq!(
        (after.sources, after.flags),
        (before.sources, before.flags),
        "no bit, no flag change"
    );
    // Marked so the backfill resolves the title.
    assert_ne!(
        s.enrichment(id).unwrap().map(|e| e.version),
        Some(lodestar_classify::ENRICHMENT_VERSION)
    );

    // The torrent deleted after the lookup: nothing is created.
    let (_, gk) = put(&mut s, &movie(1, "Plain"), dht, 0, false);
    s.delete_torrents(&[gk.0]).unwrap();
    let (st, out) = ingest_out(
        &mut s,
        &filter,
        &[discovery(gk, src, answer("The Film"))],
        bit,
    );
    assert_eq!((n(&st.refused), n(&st.stored)), (1, 0));
    assert!(out.identified.is_empty());
    assert!(
        s.unverified(Net::Bt, &gk.0).unwrap().is_none(),
        "never an unverified item"
    );

    // A private torrent: refused, its flags unchanged.
    for f in [flags::PRIVATE_SOURCE, flags::PRIVATE] {
        let (pid, pk) = put(&mut s, &movie(2, &format!("P{f}")), dht, f, false);
        let before = s.torrent(pid).unwrap().unwrap();
        let (st, _) = ingest_out(
            &mut s,
            &filter,
            &[discovery(pk, src, answer("The Film"))],
            bit,
        );
        assert_eq!(n(&st.refused), 1);
        let after = s.torrent(pid).unwrap().unwrap();
        assert_eq!((after.flags, after.sources), (before.flags, before.sources));
        assert!(s.torrent_claims(pid).unwrap().is_empty());
    }

    // Through a private source: refused.
    let (_, qk) = put(&mut s, &movie(3, "Plain"), dht, 0, false);
    let mut d = discovery(qk, src, answer("The Film"));
    d.found.private_source = true;
    let (st, _) = ingest_out(&mut s, &filter, &[d], bit);
    assert_eq!(n(&st.refused), 1);

    // A source that isn't an enrichment source takes the usual path.
    let (fid, fk) = put(&mut s, &movie(4, "Plain"), dht, 0, false);
    let (_, out) = ingest_out(
        &mut s,
        &filter,
        &[discovery(fk, src, answer("The Film"))],
        0,
    );
    assert!(out.identified.is_empty());
    assert_eq!(out.touched.len(), 1, "a feed's claims are a sighting");
    assert_ne!(s.torrent(fid).unwrap().unwrap().sources & bit, 0);
}

/// A details section is charged at what ingest judges for it, joined with
/// a worst-case verified name, so the writer's step budget bounds the
/// judged bytes (runner #557).
#[test]
fn the_claims_charge_counts_the_judged_bytes() {
    let src = SourceId::new(3).unwrap();
    let full = Identification {
        alt_titles: (0..claims::MAX_ALT_TITLES)
            .map(|i| AltTitle {
                language: "en".into(),
                title: format!("title {i} {}", "x".repeat(200)),
            })
            .collect(),
        genres: (0..claims::MAX_GENRES)
            .map(|i| format!("genre {i}"))
            .collect(),
        ..answer("The Film")
    };
    let d = discovery(InfoHash([1; 20]), src, full);
    let c = d.found.claims.as_deref().unwrap();
    let judged = unverified::details_judged_len(
        c.details.as_deref().unwrap(),
        &[None, Some(&"n".repeat(unverified::JUDGED_NAME_MAX))],
    );
    let kib = unverified::claims_kib(&d) as usize;
    assert!(kib * 1024 >= judged, "{kib} KiB for {judged} judged bytes");
    assert!(kib as u32 <= unverified::CLAIMS_STEP_KIB);
    // A step of `CLAIMS_STEP_KIB` holds a bounded number of these.
    let per_step = unverified::CLAIMS_STEP_KIB as usize / kib + 1;
    assert!(per_step * judged <= 2 * (unverified::CLAIMS_STEP_KIB as usize) * 1024);
    // No section: charged as before.
    let plain = Discovery {
        source: src,
        found: Found::key(InfoHash([2; 20]), 0),
        at: T,
        reply: false,
    };
    assert_eq!(unverified::claims_kib(&plain), 1);
}

/// Consent is read right before each lookup: a missing record, or one for
/// an older consent text or other endpoints, refuses.
#[test]
fn consent_must_cover_the_plugin_as_it_is() {
    static EXTERNAL: Manifest = Manifest {
        id: "test-external",
        consent: Consent::External,
        ..MANIFEST
    };
    let mut s = Store::open_in_memory(StoreConfig::default()).unwrap();
    let dht = s.register_source("dht", "1", 0).unwrap();
    let src = s.register_source(EXTERNAL.id, "1", 0).unwrap();
    let (id, _) = put(&mut s, &movie(0, "Plain"), dht, 0, false);
    let filter = stand_in();
    let flags = Flags::default();
    let kinds = MediaKind::ALL;
    let ctx = identify::GateCtx {
        manifest: &EXTERNAL,
        source: src,
        kinds: &kinds,
        adult: AdultSources::default(),
        switches: &flags,
    };
    let why = |s: &mut Store| {
        identify::check(s, &filter.current(), ctx, id, T)
            .unwrap()
            .err()
    };
    s.enable_source(src, None).unwrap();
    assert_eq!(why(&mut s), Some(Ineligible::NoConsent), "no record");
    s.enable_source(src, Some("external|t0 stale|1")).unwrap();
    assert_eq!(why(&mut s), Some(Ineligible::NoConsent), "an older text");
    let current = format!(
        "external|{}|1",
        lodestar_sources::consent::version(&EXTERNAL)
    );
    s.enable_source(
        src,
        Some(&format!(
            "external|{}|0",
            lodestar_sources::consent::version(&EXTERNAL)
        )),
    )
    .unwrap();
    assert_eq!(why(&mut s), Some(Ineligible::NoConsent), "declined");
    s.enable_source(src, Some(&current)).unwrap();
    assert_eq!(why(&mut s), None);
    s.set_source_state(src, SourceState::Disabled).unwrap();
    assert_eq!(why(&mut s), Some(Ineligible::SourceOff));
}
